//! Optional OCR for scanned PDFs and images via a locally installed Tesseract.
//!
//! * Off by default; nothing leaves the machine (Tesseract runs locally).
//! * Runs on its own background thread and only while regular indexing is idle, so it never
//!   slows down normal indexing or searching.
//! * PDF pages are rendered with PDFium at a fixed width, written to a temp PNG and passed to
//!   `tesseract <png> stdout`; each call has a timeout and is killed if exceeded.

use std::ffi::OsStr;
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
use serde::Serialize;

const PAGE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_OCR_PAGES: u32 = 500;
const RENDER_WIDTH: i32 = 2000;

/// Locate the Tesseract executable (explicit setting, PATH, common install locations). An
/// explicit path must name the Tesseract program itself, so the setting cannot be used to make
/// FastFind run some other executable.
pub fn find_tesseract(explicit: &str) -> Option<PathBuf> {
    if !explicit.trim().is_empty() {
        let p = PathBuf::from(explicit.trim());
        return (p.is_file() && is_tesseract_name(&p)).then_some(p);
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

fn is_tesseract_name(p: &Path) -> bool {
    p.file_stem().is_some_and(|s| s.eq_ignore_ascii_case("tesseract"))
}

/// Why OCR did not produce a result.
#[derive(Debug, Clone, PartialEq)]
pub enum OcrError {
    /// Tesseract is missing, cannot start or lacks a configured language. No file is at fault:
    /// files stay queued until the setup is fixed.
    Setup(String),
    /// This file could not be recognised; it is marked failed.
    File(String),
    /// Stopped for shutdown or because regular indexing resumed; the file is retried later.
    Interrupted,
}

impl std::fmt::Display for OcrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OcrError::Setup(m) | OcrError::File(m) => f.write_str(m),
            OcrError::Interrupted => f.write_str("OCR was interrupted"),
        }
    }
}

struct Output {
    success: bool,
    status: String,
    stdout: String,
    stderr: String,
}

/// Run Tesseract with a timeout, capturing stdout and (bounded) stderr. Failing to start the
/// program is a setup problem; a timeout is reported as `timeout_error`.
fn run_tesseract(tesseract: &Path, args: &[&OsStr], timeout: Duration, timeout_error: OcrError) -> Result<Output, OcrError> {
    let mut cmd = Command::new(tesseract);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| OcrError::Setup(format!("Tesseract could not be started ({}): {e}", tesseract.display())))?;
    let out = read_bounded(child.stdout.take(), 64 << 20);
    let err = read_bounded(child.stderr.take(), 64 << 10);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output { success: status.success(), status: status.to_string(), stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() });
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timeout_error);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(OcrError::File(e.to_string())),
        }
    }
}

fn read_bounded<R: Read + Send + 'static>(r: Option<R>, limit: u64) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(r) = r {
            let _ = r.take(limit).read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Tesseract's messages when its language data cannot be loaded, i.e. the setup (not the
/// image) is at fault.
fn is_setup_failure(stderr: &str) -> bool {
    ["Failed loading language", "Error opening data file", "Could not initialize tesseract"].iter().any(|m| stderr.contains(m))
}

/// Run Tesseract on one image file with a timeout.
pub fn ocr_image(tesseract: &Path, image: &Path, languages: &str) -> Result<String, OcrError> {
    let args: [&OsStr; 4] = [image.as_os_str(), "stdout".as_ref(), "-l".as_ref(), languages.as_ref()];
    let out = run_tesseract(tesseract, &args, PAGE_TIMEOUT, OcrError::File("OCR timed out".into()))?;
    if out.success {
        return Ok(out.stdout);
    }
    let detail = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    if is_setup_failure(&out.stderr) {
        let missing: Vec<&str> = out.stderr.lines().filter_map(|l| l.trim().strip_prefix("Failed loading language '")?.strip_suffix('\'')).collect();
        return Err(OcrError::Setup(if missing.is_empty() {
            format!("Tesseract could not load its language data: {detail}")
        } else {
            format!("Tesseract has no data for language “{}”.", missing.join("”, “"))
        }));
    }
    Err(OcrError::File(if detail.is_empty() { format!("tesseract exited with {}", out.status) } else { format!("tesseract exited with {}: {detail}", out.status) }))
}

/// Result of checking the OCR setup, shown in Settings and used before any file is processed.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OcrSetup {
    /// The Tesseract executable that will be used.
    pub tesseract: Option<String>,
    pub version: Option<String>,
    /// Languages installed for that Tesseract.
    pub languages: Vec<String>,
    /// Configured languages that are not installed.
    pub missing_languages: Vec<String>,
    /// Why OCR cannot run, in words for the user. `None` when everything is ready.
    pub problem: Option<String>,
}

/// Check that Tesseract can be found and started and has every configured language.
pub fn probe(settings: &crate::config::OcrSettings) -> OcrSetup {
    let mut setup = OcrSetup::default();
    let explicit = settings.tesseract_path.trim();
    let Some(t) = find_tesseract(explicit) else {
        setup.problem = Some(if explicit.is_empty() {
            "Tesseract was not found. Install Tesseract OCR, or enter the location of the tesseract program below.".into()
        } else if Path::new(explicit).is_file() {
            format!("“{explicit}” is not the Tesseract program. Enter the path of tesseract{}.", if cfg!(windows) { ".exe" } else { "" })
        } else {
            format!("Tesseract was not found at “{explicit}”.")
        });
        return setup;
    };
    setup.tesseract = Some(crate::util::path_str(&t));
    let probe_timeout = OcrError::Setup("Tesseract did not respond.".into());
    match run_tesseract(&t, &["--version".as_ref()], Duration::from_secs(15), probe_timeout.clone()) {
        // Tesseract 4 prints its version to stderr, 5 to stdout.
        Ok(o) => setup.version = o.stdout.lines().chain(o.stderr.lines()).find_map(|l| l.trim().strip_prefix("tesseract ")).map(|v| v.trim_start_matches('v').to_string()),
        Err(e) => {
            setup.problem = Some(e.to_string());
            return setup;
        }
    }
    match run_tesseract(&t, &["--list-langs".as_ref()], Duration::from_secs(15), probe_timeout) {
        Ok(o) => {
            let text = if o.stdout.trim().is_empty() { o.stderr } else { o.stdout };
            setup.languages = text.lines().skip_while(|l| !l.contains("List of available languages")).skip(1).map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            setup.languages.sort();
        }
        Err(e) => {
            setup.problem = Some(e.to_string());
            return setup;
        }
    }
    let wanted = if settings.languages.trim().is_empty() { "eng" } else { settings.languages.trim() };
    setup.missing_languages = wanted.split('+').map(str::trim).filter(|l| !l.is_empty() && !setup.languages.iter().any(|a| a == l)).map(String::from).collect();
    if !setup.missing_languages.is_empty() {
        setup.problem = Some(format!(
            "Tesseract has no data for {} “{}”. Installed: {}.",
            if setup.missing_languages.len() == 1 { "language" } else { "languages" },
            setup.missing_languages.join("”, “"),
            if setup.languages.is_empty() { "none".to_string() } else { setup.languages.join(", ") },
        ));
    }
    setup
}

/// OCR a scanned PDF page by page into `sink` (with page anchors). `stop` is polled before
/// each page; when it fires the result is `Interrupted`, never a partial document.
pub fn ocr_pdf(tesseract: &Path, pdf: &Path, languages: &str, sink: &mut TextSink, stop: &dyn Fn() -> bool) -> Result<u32, OcrError> {
    let tmp = tempfile::Builder::new().prefix("fastfind-ocr").tempdir().map_err(|e| OcrError::File(e.to_string()))?;
    let mut err = None;
    let mut stopped = false;
    let pages = crate::parsers::pdf::render_pages(pdf, MAX_OCR_PAGES, RENDER_WIDTH, |n, img| {
        if stop() {
            stopped = true;
            return false;
        }
        let png = tmp.path().join(format!("p{n}.png"));
        if let Err(e) = img.to_luma8().save(&png) {
            err = Some(OcrError::File(e.to_string()));
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
    .map_err(|e| OcrError::File(e.to_string()))?;
    if stopped {
        return Err(OcrError::Interrupted);
    }
    match err {
        // A setup problem always wins: the remaining pages were not attempted.
        Some(e @ OcrError::Setup(_)) => Err(e),
        Some(e) if sink.is_empty() => Err(e),
        _ => Ok(pages),
    }
}

/// Re-check a broken setup this often (picks up a Tesseract installed while FastFind runs).
const SETUP_RETRY: Duration = Duration::from_secs(60);
const SETUP_RECHECK: Duration = Duration::from_secs(600);

/// Background loop: picks files flagged `NeedsOcr` one at a time while indexing is idle, after
/// checking that Tesseract and its languages are available.
pub(crate) fn ocr_loop(inner: Arc<Inner>) {
    lower_thread_priority(true);
    // (path setting, languages) -> last probe
    let mut cache: Option<((String, String), Instant, OcrSetup)> = None;
    loop {
        if inner.is_shutdown() {
            return;
        }
        let settings = inner.settings.read().indexing.clone();
        if !settings.ocr.enabled {
            *inner.ocr_problem.write() = None;
            inner.ocr_wait(Duration::from_secs(10));
            continue;
        }
        let key = (settings.ocr.tesseract_path.clone(), settings.ocr.languages.clone());
        let setup = match &cache {
            Some((k, at, s)) if *k == key && at.elapsed() < if s.problem.is_some() { SETUP_RETRY } else { SETUP_RECHECK } => s.clone(),
            _ => {
                let s = probe(&settings.ocr);
                match &s.problem {
                    Some(p) => tracing::warn!(problem = %p, "OCR is enabled but cannot run"),
                    None => tracing::info!(tesseract = ?s.tesseract, version = ?s.version, "OCR ready"),
                }
                cache = Some((key.clone(), Instant::now(), s.clone()));
                s
            }
        };
        *inner.ocr_problem.write() = setup.problem.clone();
        let Some(tesseract) = setup.tesseract.as_ref().filter(|_| setup.problem.is_none()).map(PathBuf::from) else {
            inner.ocr_wait(Duration::from_secs(10));
            continue;
        };
        if inner.is_paused() || inner.busy() {
            inner.ocr_wait(Duration::from_secs(10));
            continue;
        }
        let next = inner.catalog.pending_ocr(1).unwrap_or_default();
        let Some(row) = next.into_iter().next() else {
            inner.ocr_wait(Duration::from_secs(30));
            continue;
        };
        let t0 = Instant::now();
        match ocr_file(&inner, &tesseract, &settings.ocr.languages, &row, settings.stored_text_kb, settings.max_indexed_text_mb) {
            Ok(p) => {
                tracing::info!(path = %row.path, ms = t0.elapsed().as_millis() as u64, ok = p.record.status == Status::Indexed, "ocr finished");
                IndexService::submit(&inner, p);
                // Let the writer commit before picking the next file (the catalog status changes).
                inner.ocr_wait(Duration::from_secs(3));
            }
            Err(OcrError::Setup(problem)) => {
                // The probe passed but Tesseract still failed for setup reasons (e.g. damaged
                // language data): keep the file queued, report the problem, retry later.
                tracing::warn!(path = %row.path, problem = %problem, "OCR setup problem; file stays queued");
                *inner.ocr_problem.write() = Some(problem.clone());
                cache = Some((key, Instant::now(), OcrSetup { problem: Some(problem), ..setup }));
            }
            Err(_) => {
                // Interrupted: regular indexing resumed or shutting down. Retried later.
                inner.ocr_wait(Duration::from_secs(10));
            }
        }
    }
}

fn ocr_file(inner: &Inner, tesseract: &Path, languages: &str, row: &FileRow, stored_kb: u64, max_mb: u64) -> Result<Processed, OcrError> {
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
    let (status, reason, fl, pages) = match result {
        Ok(pages) if !sink.is_empty() => (Status::Indexed, None, (row.flags & !flags::NEEDS_OCR & !flags::NAME_ONLY) | flags::OCR, pages),
        Ok(pages) => (Status::Indexed, Some("OCR found no text".into()), (row.flags & !flags::NEEDS_OCR) | flags::OCR, pages),
        Err(OcrError::File(e)) => (Status::Failed, Some(format!("OCR failed: {e}")), row.flags & !flags::NEEDS_OCR, None),
        Err(e) => return Err(e),
    };
    let (text, locs, _) = sink.into_parts();
    let mut doc = base_doc(f, row.root_id, &path, row.size, row.mtime_ns, kind, fl, pages, TextMode::Stored);
    doc.add_text(f.content, &text);
    let cap = (stored_kb as usize) << 10;
    let cut = crate::util::floor_char_boundary(&text, cap);
    Ok(Processed {
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
    })
}
