//! PDF extraction with the PDFium engine (bundled library in `src-tauri/pdfium`) — including
//! page anchors, metadata and scanned-document classification. Falls back to the built-in
//! extractor when PDFium is not present, in which case PDFium-only checks are skipped.

use std::path::PathBuf;

use fastfind_core::gen::{write_image_pdf, write_pdf};
use fastfind_core::model::flags;
use fastfind_core::parsers::locmap::describe;
use fastfind_core::parsers::pdf::{self, PdfParser};
use fastfind_core::parsers::{DocumentParser, Limits, ParseContext, TextSink};

fn engine_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/pdfium")
}

fn extract(p: &std::path::Path) -> (String, Vec<fastfind_core::parsers::Loc>, fastfind_core::parsers::DocMeta) {
    pdf::init(&[engine_dir()]);
    let limits = Limits::default();
    let mut sink = TextSink::new(1 << 24, None);
    let meta = PdfParser.extract(p, &ParseContext { limits: &limits }, &mut sink).unwrap();
    let (t, l, _) = sink.into_parts();
    (t, l, meta)
}

fn has_pdfium() -> bool {
    pdf::init(&[engine_dir()]).pdfium().is_some()
}

#[test]
fn word_exported_pdf_pages_and_metadata() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/word-export.pdf");
    let (text, locs, meta) = extract(&p);
    assert!(text.contains("lemurs of Madagascar"), "{text}");
    assert!(text.contains("tapirs of Brazil"));
    assert_eq!(meta.pages, Some(2));
    let off = text.find("tapirs").unwrap();
    assert_eq!(describe(&locs, &text, off, false).as_deref(), Some("Page 2"));
    if has_pdfium() {
        assert_eq!(meta.title.as_deref(), Some("Wildlife Survey"));
    }
    assert_eq!(meta.flags & flags::NEEDS_OCR, 0, "text PDF is searchable");
}

#[test]
fn generated_pdf_many_pages() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("many.pdf");
    let pages: Vec<Vec<String>> = (1..=40).map(|i| vec![format!("page marker token{i}")]).collect();
    write_pdf(&p, "Many", &pages).unwrap();
    let (text, locs, meta) = extract(&p);
    assert_eq!(meta.pages, Some(40));
    let off = text.find("token37").unwrap();
    assert_eq!(describe(&locs, &text, off, false).as_deref(), Some("Page 37"));
}

#[test]
fn scanned_pdf_is_classified_as_needing_ocr() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("scan.pdf");
    write_image_pdf(&p, 3).unwrap();
    let (text, _, meta) = extract(&p);
    assert!(text.trim().is_empty());
    assert_ne!(meta.flags & flags::NEEDS_OCR, 0, "image-only PDF must be flagged for OCR");
}

#[test]
fn reports_which_engine_is_active() {
    let e = pdf::init(&[engine_dir()]);
    eprintln!("PDF engine: {}", e.description());
    if engine_dir().join(if cfg!(windows) { "pdfium.dll" } else if cfg!(target_os = "macos") { "libpdfium.dylib" } else { "libpdfium.so" }).exists() {
        assert!(e.pdfium().is_some(), "bundled PDFium present but failed to load: {}", e.description());
    }
}
