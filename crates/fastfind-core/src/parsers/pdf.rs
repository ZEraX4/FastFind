//! PDF text extraction.
//!
//! Primary engine: **PDFium** (Chromium's PDF library) loaded dynamically from the application
//! bundle. PDFium opens documents lazily from the file (large PDFs are not loaded into RAM),
//! handles CID/Type3 fonts and broken xref tables well, and is heavily fuzzed.
//! PDFium is not thread-safe; `pdfium-render`'s `thread_safe` feature serialises calls, and the
//! indexing pipeline routes PDFs to a dedicated lane so other workers never block on it.
//!
//! Fallback engine: the pure-Rust `pdf-extract` crate, used only when the PDFium library cannot
//! be loaded (e.g. a development build without the bundled binary).
//!
//! Classification: a page mostly covered by images, with little text, is a scan. A document
//! with any scanned page is flagged `NEEDS_OCR`; OCR then recognises only those pages and
//! keeps the text layer of the others (`pages_for_ocr`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pdfium_render::prelude::*;

use super::sniff::{sniff, Sniffed};
use super::{DocMeta, DocumentParser, LocKind, ParseContext, ParseError, ParseResult, TextMode, TextSink};
use crate::model::flags;

pub struct PdfEngine {
    pdfium: Option<Pdfium>,
    description: String,
    /// Directory the PDFium library was loaded from (passed on to helper processes).
    dir: Option<PathBuf>,
}

impl PdfEngine {
    pub fn library_dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn pdfium(&self) -> Option<&Pdfium> {
        self.pdfium.as_ref()
    }
}

static ENGINE: OnceLock<PdfEngine> = OnceLock::new();

/// Initialise the PDF engine, searching `extra_dirs` first. Only the first call has effect.
pub fn init(extra_dirs: &[PathBuf]) -> &'static PdfEngine {
    ENGINE.get_or_init(|| {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Ok(d) = std::env::var("FASTFIND_PDFIUM_DIR") {
            dirs.push(PathBuf::from(d));
        }
        dirs.extend(extra_dirs.iter().cloned());
        if let Ok(exe) = std::env::current_exe() {
            if let Some(d) = exe.parent() {
                dirs.push(d.to_path_buf());
                dirs.push(d.join("resources"));
                dirs.push(d.join("pdfium"));
                dirs.push(d.join("../Resources")); // macOS .app bundle
                dirs.push(d.join("../Resources/resources"));
                dirs.push(d.join("../Frameworks"));
                dirs.push(d.join("../lib/fastfind")); // Linux .deb
                dirs.push(d.join("../lib/FastFind"));
                dirs.push(d.join("../lib"));
            }
        }
        for d in &dirs {
            let lib = Pdfium::pdfium_platform_library_name_at_path(d);
            if !lib.exists() {
                continue;
            }
            match Pdfium::bind_to_library(&lib) {
                Ok(b) => {
                    tracing::info!(path = %lib.display(), "PDFium loaded");
                    return PdfEngine { pdfium: Some(Pdfium::new(b)), description: format!("PDFium ({})", lib.display()), dir: Some(d.clone()) };
                }
                Err(e) => tracing::warn!(path = %lib.display(), error = %e, "failed to load PDFium"),
            }
        }
        match Pdfium::bind_to_system_library() {
            Ok(b) => {
                tracing::info!("PDFium loaded from system library path");
                PdfEngine { pdfium: Some(Pdfium::new(b)), description: "PDFium (system)".into(), dir: None }
            }
            Err(_) => {
                tracing::warn!("PDFium not found; using built-in fallback PDF extractor");
                PdfEngine { pdfium: None, description: "pdf-extract (fallback; PDFium not found)".into(), dir: None }
            }
        }
    })
}

pub fn engine() -> &'static PdfEngine {
    init(&[])
}

pub struct PdfParser;

impl DocumentParser for PdfParser {
    fn name(&self) -> &'static str {
        "pdf"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pdf", "ai"]
    }

    fn can_handle(&self, _ext: &str, header: &[u8]) -> bool {
        sniff(header) == Sniffed::Pdf
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        match super::pdfworker::pool() {
            Some(pool) => pool.extract(path, ctx, sink),
            None => extract_in_process(path, ctx, sink),
        }
    }
}

/// Extract in this process (used directly without a helper pool, and by the helpers).
pub fn extract_in_process(path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
    match engine().pdfium() {
        Some(p) => extract_pdfium(p, path, ctx, sink),
        None => extract_fallback(path, sink),
    }
}

fn map_err(e: PdfiumError) -> ParseError {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)
        | PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => ParseError::Encrypted,
        PdfiumError::IoError(io) => ParseError::Io(io),
        other => ParseError::corrupt(format!("{other:?}")),
    }
}

fn extract_pdfium(pdfium: &Pdfium, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
    let doc = pdfium.load_pdf_from_file(path, None).map_err(map_err)?;
    let mut meta = DocMeta::default();
    let md = doc.metadata();
    let tag = |t: PdfDocumentMetadataTagType| md.get(t).map(|v| v.value().trim().to_string()).filter(|v| !v.is_empty());
    meta.title = tag(PdfDocumentMetadataTagType::Title);
    meta.author = tag(PdfDocumentMetadataTagType::Author);
    meta.subject = tag(PdfDocumentMetadataTagType::Subject);
    meta.keywords = tag(PdfDocumentMetadataTagType::Keywords);
    let pages = doc.pages();
    let count = pages.len() as u32;
    meta.pages = Some(count);
    let mut scanned = 0u32;
    let mut processed = 0u32;
    for (i, page) in pages.iter().enumerate() {
        if i as u32 >= ctx.limits.max_pdf_pages || sink.is_full() {
            break;
        }
        sink.check_deadline()?;
        processed += 1;
        sink.newline();
        sink.anchor(LocKind::Page(i as u32 + 1));
        let (text, is_scan) = read_page(&page);
        scanned += is_scan as u32;
        sink.push_str(&text);
    }
    if processed < count {
        tracing::debug!(path = %path.display(), processed, count, "PDF truncated at page limit");
    }
    // One scanned page is enough: OCR then recognises just the scanned pages.
    if scanned > 0 {
        meta.flags |= flags::NEEDS_OCR;
    }
    Ok(meta)
}

/// A page is a scan when images cover at least half of it and it has little real text: fewer
/// than this many letters and digits (a stamp, a header line, a page number).
const SCAN_MAX_CHARS: usize = 64;
const SCAN_MIN_IMAGE_COVER: f32 = 0.5;

/// The page's text layer, and whether the page is a scan that needs OCR.
fn read_page(page: &PdfPage) -> (String, bool) {
    let text = page.text().map(|t| t.all()).unwrap_or_default();
    if text.chars().filter(|c| c.is_alphanumeric()).take(SCAN_MAX_CHARS).count() >= SCAN_MAX_CHARS {
        return (text, false);
    }
    let area = page.width().value * page.height().value;
    let mut images = 0.0f32;
    for o in page.objects().iter().filter(|o| o.object_type() == PdfPageObjectType::Image) {
        // An image whose size can't be read counts as covering the page.
        images += match (o.width(), o.height()) {
            (Ok(w), Ok(h)) => w.value * h.value,
            _ => area,
        };
    }
    (text, area > 0.0 && images >= area * SCAN_MIN_IMAGE_COVER)
}

/// A page as OCR sees it.
pub enum OcrPage {
    /// A page with a usable text layer, which is kept.
    Text(String),
    /// A scanned page, rendered for recognition, with whatever little text layer it has.
    Scan { text: String, image: image::DynamicImage },
}

/// Walk a PDF for OCR: pages with a usable text layer are passed as text, scanned pages are
/// rendered `width` pixels wide. Calls `f(page_number, page)`; stops when it returns false.
pub fn pages_for_ocr(path: &Path, max_pages: u32, width: i32, mut f: impl FnMut(u32, OcrPage) -> bool) -> ParseResult<u32> {
    let pdfium = engine().pdfium().ok_or_else(|| ParseError::Unsupported("OCR of PDFs requires PDFium".into()))?;
    let doc = pdfium.load_pdf_from_file(path, None).map_err(map_err)?;
    let cfg = PdfRenderConfig::new().set_target_width(width).set_maximum_height(width * 2);
    let mut n = 0;
    for (i, page) in doc.pages().iter().enumerate() {
        if i as u32 >= max_pages {
            break;
        }
        let (text, is_scan) = read_page(&page);
        let item = if is_scan {
            OcrPage::Scan { text, image: page.render_with_config(&cfg).and_then(|b| b.as_image()).map_err(map_err)? }
        } else {
            OcrPage::Text(text)
        };
        n += 1;
        if !f(i as u32 + 1, item) {
            break;
        }
    }
    Ok(n)
}

fn extract_fallback(path: &Path, sink: &mut TextSink) -> ParseResult<DocMeta> {
    let pages = pdf_extract::extract_text_by_pages(path).map_err(|e| {
        let msg = e.to_string();
        if msg.to_ascii_lowercase().contains("encrypt") || msg.to_ascii_lowercase().contains("password") {
            ParseError::Encrypted
        } else {
            ParseError::corrupt(msg)
        }
    })?;
    let mut meta = DocMeta { pages: Some(pages.len() as u32), ..Default::default() };
    let mut chars = 0usize;
    for (i, p) in pages.iter().enumerate() {
        if sink.is_full() {
            break;
        }
        sink.newline();
        sink.anchor(LocKind::Page(i as u32 + 1));
        chars += p.chars().filter(|c| c.is_alphanumeric()).count();
        sink.push_str(p);
    }
    if !pages.is_empty() && chars / pages.len() < 16 {
        // Without PDFium we cannot inspect page objects; little text on every page is the best
        // available signal for a scan.
        meta.flags |= flags::NEEDS_OCR;
    }
    Ok(meta)
}

/// Render pages to images for OCR. Calls `f(page_number, image)`; stops when it returns false.
pub fn render_pages(path: &Path, max_pages: u32, width: i32, mut f: impl FnMut(u32, image::DynamicImage) -> bool) -> ParseResult<u32> {
    let pdfium = engine().pdfium().ok_or_else(|| ParseError::Unsupported("OCR of PDFs requires PDFium".into()))?;
    let doc = pdfium.load_pdf_from_file(path, None).map_err(map_err)?;
    let cfg = PdfRenderConfig::new().set_target_width(width).set_maximum_height(width * 2);
    let mut n = 0;
    for (i, page) in doc.pages().iter().enumerate() {
        if i as u32 >= max_pages {
            break;
        }
        let img = page.render_with_config(&cfg).and_then(|b| b.as_image()).map_err(map_err)?;
        n += 1;
        if !f(i as u32 + 1, img) {
            break;
        }
    }
    Ok(n)
}
