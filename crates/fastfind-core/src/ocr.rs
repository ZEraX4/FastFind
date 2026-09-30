//! Optional OCR for scanned PDFs and images via a locally installed Tesseract.
//!
//! * Off by default; nothing leaves the machine (Tesseract runs locally).
//! * Runs on its own background thread and only while regular indexing is idle, so it never
//!   slows down normal indexing or searching.
//! * PDF pages are rendered with PDFium at a fixed width, written to a temp PNG and passed to
//!   `tesseract <png> stdout`; each call has a timeout and is killed if exceeded.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::index::catalog::{FileRow, Status};
use crate::index::service::{base_doc, Inner, Processed, TextAction};
use crate::index::textstore;
use crate::index::{FileRecord, IndexService};
use crate::model::flags;
use crate::parsers::registry::kind_for_ext;
use crate::parsers::{LocKind, TextMode, TextSink};
use crate::util::{extension_of, lower_thread_priority};

const PAGE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_OCR_PAGES: u32 = 500;
const RENDER_WIDTH: i32 = 2000;

/// Locate the Tesseract executable (explicit setting, PATH, common install locations).
pub fn find_tesseract(explicit: &str) -> Option<PathBuf> {
    if !explicit.trim().is_empty() {
        let p = PathBuf::from(explicit.trim());
        return p.is_file().then_some(p);
    }
    let exe = if cfg!(windows) { "tesseract.exe" } else { "tesseract" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let c = dir.join(exe);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    let candidates: &[&str] = if cfg!(windows) {
        &["C:\\Program Files\\Tesseract-OCR\\tesseract.exe", "C:\\Program Files (x86)\\Tesseract-OCR\\tesseract.exe"]
    } else if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin/tesseract", "/usr/local/bin/tesseract"]
    } else {
        &["/usr/bin/tesseract", "/usr/local/bin/tesseract"]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// Run Tesseract on one image file with a timeout.
pub fn ocr_image(tesseract: &Path, image: &Path, languages: &str) -> Result<String, String> {
    let mut cmd = Command::new(tesseract);
    cmd.arg(image).arg("stdout").arg("-l").arg(languages).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot start tesseract: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().unwrap_or_default();
                return if status.success() { Ok(out) } else { Err(format!("tesseract exited with {status}")) };
            }
            Ok(None) if start.elapsed() > PAGE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("OCR timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// OCR a scanned PDF page by page into `sink` (with page anchors).
pub fn ocr_pdf(tesseract: &Path, pdf: &Path, languages: &str, sink: &mut TextSink, stop: &dyn Fn() -> bool) -> Result<u32, String> {
    let tmp = tempfile::Builder::new().prefix("fastfind-ocr").tempdir().map_err(|e| e.to_string())?;
    let mut err = None;
    let pages = crate::parsers::pdf::render_pages(pdf, MAX_OCR_PAGES, RENDER_WIDTH, |n, img| {
        if stop() {
            return false;
        }
        let png = tmp.path().join(format!("p{n}.png"));
        if let Err(e) = img.to_luma8().save(&png) {
            err = Some(e.to_string());
            return false;
        }
        match ocr_image(tesseract, &png, languages) {
            Ok(text) => {
                sink.newline();
                sink.anchor(LocKind::Page(n));
                sink.push_str(&text);
            }
            Err(e) => {
                err = Some(e);
                return false;
            }
        }
        let _ = std::fs::remove_file(&png);
        !sink.is_full()
    })
    .map_err(|e| e.to_string())?;
    match err {
        Some(e) if sink.is_empty() => Err(e),
        _ => Ok(pages),
    }
}

/// Background loop: picks files flagged `NeedsOcr` one at a time while indexing is idle.
pub(crate) fn ocr_loop(inner: Arc<Inner>) {
    lower_thread_priority(true);
    loop {
        if inner.is_shutdown() {
            return;
        }
        let settings = inner.settings.read().indexing.clone();
        let tesseract = if settings.ocr.enabled { find_tesseract(&settings.ocr.tesseract_path) } else { None };
        if tesseract.is_none() || inner.is_paused() || inner.busy() {
            inner.ocr_wait(Duration::from_secs(10));
            continue;
        }
        let tesseract = tesseract.unwrap();
        let next = inner.catalog.pending_ocr(1).unwrap_or_default();
        let Some(row) = next.into_iter().next() else {
            inner.ocr_wait(Duration::from_secs(30));
            continue;
        };
        let t0 = Instant::now();
        let p = ocr_file(&inner, &tesseract, &settings.ocr.languages, &row, settings.stored_text_kb, settings.max_indexed_text_mb);
        tracing::info!(path = %row.path, ms = t0.elapsed().as_millis() as u64, ok = p.record.status == Status::Indexed, "ocr finished");
        IndexService::submit(&inner, p);
        // Let the writer commit before picking the next file (the catalog status changes).
        inner.ocr_wait(Duration::from_secs(3));
    }
}

fn ocr_file(inner: &Inner, tesseract: &Path, languages: &str, row: &FileRow, stored_kb: u64, max_mb: u64) -> Processed {
    let path = PathBuf::from(&row.path);
    let ext = extension_of(&path);
    let kind = kind_for_ext(&ext);
    let mut sink = TextSink::new((max_mb as usize) << 20, None);
    let stop = || inner.is_shutdown() || inner.busy();
    let result = if ext == "pdf" {
        ocr_pdf(tesseract, &path, languages, &mut sink, &stop).map(Some)
    } else {
        ocr_image(tesseract, &path, languages).map(|t| {
            sink.push_str(&t);
            None
        })
    };
    let f = &inner.store.fields;
    let (status, reason, fl, pages) = match &result {
        Ok(pages) if !sink.is_empty() => (Status::Indexed, None, (row.flags & !flags::NEEDS_OCR & !flags::NAME_ONLY) | flags::OCR, *pages),
        Ok(pages) => (Status::Indexed, Some("OCR found no text".into()), (row.flags & !flags::NEEDS_OCR) | flags::OCR, *pages),
        Err(e) => (Status::Failed, Some(format!("OCR failed: {e}")), row.flags & !flags::NEEDS_OCR, None),
    };
    let (text, locs, _) = sink.into_parts();
    let mut doc = base_doc(f, row.root_id, &path, row.size, row.mtime_ns, kind, fl, pages, TextMode::Stored);
    doc.add_text(f.content, &text);
    let cap = (stored_kb as usize) << 10;
    let cut = crate::util::floor_char_boundary(&text, cap);
    Processed {
        record: FileRecord {
            root_id: row.root_id,
            path: row.path.clone(),
            size: row.size,
            mtime_ns: row.mtime_ns,
            hash: row.hash,
            kind: kind.as_str().into(),
            status,
            reason,
            flags: fl,
        },
        doc: Some(doc),
        text: if text.trim().is_empty() { TextAction::Delete } else { TextAction::Put(textstore::encode(&text[..cut], &locs)) },
        delete_old: None,
    }
}
