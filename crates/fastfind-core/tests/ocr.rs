//! OCR with a locally installed Tesseract and the bundled PDFium.
//!
//! Test documents are genuine "scans": a text PDF is rendered to pixels with PDFium, and those
//! pixels are wrapped in an image-only PDF (or saved as a PNG) — no text layer, exactly like a
//! document scanner's output. Tests are skipped (with a message) when Tesseract or PDFium is not
//! installed, so CI without them still passes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fastfind_core::config::AppPaths;
use fastfind_core::gen::{write_pdf, write_scanned_pdf, ScanPage};
use fastfind_core::model::{flags, SearchRequest};
use fastfind_core::parsers::locmap::describe;
use fastfind_core::parsers::{pdf, TextSink};
use fastfind_core::util::CancelToken;
use fastfind_core::{ocr, Engine, EngineOptions};

fn pdfium_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/pdfium")
}

/// Tesseract + PDFium, or `None` (test skipped).
fn tools() -> Option<PathBuf> {
    let Some(t) = ocr::find_tesseract("") else {
        eprintln!("Tesseract not installed — skipping OCR test");
        return None;
    };
    if pdf::init(&[pdfium_dir()]).pdfium().is_none() {
        eprintln!("PDFium not available — skipping OCR test");
        return None;
    }
    Some(t)
}

/// Render each page's lines to a greyscale image (≈170 dpi), as a scanner would capture it.
fn scan_pages(dir: &Path, pages: &[&[&str]]) -> Vec<image::GrayImage> {
    let src = dir.join("source-text.pdf");
    let content: Vec<Vec<String>> = pages.iter().map(|p| p.iter().map(|l| l.to_string()).collect()).collect();
    write_pdf(&src, "source", &content).unwrap();
    let mut out = Vec::new();
    pdf::render_pages(&src, 100, 1400, |_, img| {
        out.push(img.to_luma8());
        true
    })
    .unwrap();
    std::fs::remove_file(src).unwrap();
    out
}

fn write_scan(path: &Path, imgs: &[image::GrayImage]) {
    let pages: Vec<ScanPage> = imgs.iter().map(|i| ScanPage { width: i.width(), height: i.height(), gray: i.as_raw() }).collect();
    write_scanned_pdf(path, &pages).unwrap();
}

fn contains_word(text: &str, word: &str) -> bool {
    text.to_lowercase().contains(word)
}

#[test]
fn tesseract_reads_a_rendered_page_image() {
    let Some(tess) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let img = &scan_pages(dir.path(), &[&["The platypus research station opened in March.", "Invoice number 4471 was paid."]])[0];
    let png = dir.path().join("page.png");
    img.save(&png).unwrap();
    let text = ocr::ocr_image(&tess, &png, "eng").expect("tesseract runs");
    assert!(contains_word(&text, "platypus"), "OCR output: {text:?}");
    assert!(contains_word(&text, "research"), "OCR output: {text:?}");
    assert!(text.contains("4471"), "digits recognised: {text:?}");
}

#[test]
fn scanned_pdf_is_recognised_page_by_page() {
    let Some(tess) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let imgs = scan_pages(dir.path(), &[&["Page one discusses the platypus colony."], &["Page two describes the kangaroo migration."]]);
    let scan = dir.path().join("scan.pdf");
    write_scan(&scan, &imgs);

    // Without OCR the document has no text and is classified as a scan.
    let limits = fastfind_core::parsers::Limits::default();
    let mut sink = TextSink::new(1 << 20, None);
    let meta = fastfind_core::parsers::DocumentParser::extract(&pdf::PdfParser, &scan, &fastfind_core::parsers::ParseContext { limits: &limits }, &mut sink).unwrap();
    assert!(sink.text().trim().is_empty());
    assert_ne!(meta.flags & flags::NEEDS_OCR, 0, "image-only PDF flagged as needing OCR");

    let mut sink = TextSink::new(1 << 20, None);
    let pages = ocr::ocr_pdf(&tess, &scan, "eng", &mut sink, &|| false).expect("OCR succeeds");
    assert_eq!(pages, 2);
    let (text, locs, _) = sink.into_parts();
    assert!(contains_word(&text, "platypus"), "{text:?}");
    let k = text.to_lowercase().find("kangaroo").unwrap_or_else(|| panic!("kangaroo not recognised: {text:?}"));
    assert_eq!(describe(&locs, &text, k, false).as_deref(), Some("Page 2"), "matches keep their page number");
}

fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn names(e: &Engine, q: &str) -> Vec<String> {
    let mut v: Vec<String> = e.search(&SearchRequest::new(q), &CancelToken::new()).unwrap().items.into_iter().map(|i| i.name).collect();
    v.sort();
    v
}

#[test]
fn enabling_ocr_makes_scans_and_images_searchable() {
    if tools().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("scans");
    std::fs::create_dir_all(&root).unwrap();
    let imgs = scan_pages(tmp.path(), &[&["Minutes of the platypus committee."], &["Budget for the kangaroo sanctuary."], &["Photo of a wombat near the river."]]);
    write_scan(&root.join("committee-scan.pdf"), &imgs[..2]);
    imgs[2].save(root.join("wombat-photo.png")).unwrap();
    std::fs::write(root.join("broken-image.png"), b"\x89PNG\r\n\x1a\n this is not really an image").unwrap();
    std::fs::write(root.join("notes.txt"), "ordinary text file").unwrap();

    let e = Engine::open(
        AppPaths::new(tmp.path().join("data")),
        EngineOptions { pdfium_dirs: vec![pdfium_dir()], watch: false, scan_on_start: false, pdf_worker_exe: None },
    )
    .unwrap();
    e.add_root(root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)));

    // OCR is off by default: the scan is flagged and findable by name only.
    assert!(names(&e, "platypus").is_empty(), "no OCR text yet");
    assert_eq!(e.status().unwrap().needs_ocr, 1, "the scanned PDF waits for OCR");
    let scan = e.search(&SearchRequest::new("filename:committee"), &CancelToken::new()).unwrap().items.remove(0);
    assert_ne!(scan.flags & flags::NEEDS_OCR, 0);
    assert!(e.preview(&SearchRequest::new("x"), &scan.path).unwrap().notes.iter().any(|n| n.contains("OCR is required")));

    // Turn OCR on, including images.
    let mut s = e.settings();
    s.indexing.ocr.enabled = true;
    s.indexing.ocr.images = true;
    s.indexing.ocr.languages = "eng".into();
    e.update_settings(s).unwrap();

    let done = wait_for(Duration::from_secs(180), || {
        e.status().map(|st| st.needs_ocr == 0).unwrap_or(false) && !names(&e, "platypus").is_empty() && !names(&e, "wombat").is_empty()
    });
    assert!(done, "OCR did not finish: status {:?}", e.status().unwrap());

    assert_eq!(names(&e, "platypus"), ["committee-scan.pdf"]);
    assert_eq!(names(&e, "kangaroo"), ["committee-scan.pdf"]);
    assert_eq!(names(&e, "wombat"), ["wombat-photo.png"]);
    let item = e.search(&SearchRequest::new("kangaroo"), &CancelToken::new()).unwrap().items.remove(0);
    assert_ne!(item.flags & flags::OCR, 0, "result marked as OCR text");
    assert_eq!(item.flags & flags::NEEDS_OCR, 0);
    let sn = e.snippets(&SearchRequest::new("kangaroo"), std::slice::from_ref(&item.path)).unwrap();
    assert_eq!(sn[0].snippets[0].location.as_deref(), Some("Page 2"));
    let pv = e.preview(&SearchRequest::new("kangaroo"), &item.path).unwrap();
    assert!(pv.total_matches >= 1);
    assert!(pv.notes.iter().any(|n| n.contains("OCR")), "preview explains the text came from OCR: {:?}", pv.notes);

    // The unreadable image did not block the queue; it is resolved one way or the other.
    let broken = e.search(&SearchRequest::new("filename:broken-image"), &CancelToken::new()).unwrap().items.remove(0);
    assert_eq!(broken.flags & flags::NEEDS_OCR, 0);
    e.shutdown();
}
