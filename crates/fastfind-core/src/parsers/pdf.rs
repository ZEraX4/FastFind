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
//! Classification: pages with (almost) no text but with image objects count as scanned; a
//! document where at least half the pages are scanned is flagged `NEEDS_OCR` ("Scanned PDF
//! requiring OCR") as opposed to a text-searchable PDF.

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
    let mut text_chars = 0usize;
    let mut processed = 0u32;
    for (i, page) in pages.iter().enumerate() {
        if i as u32 >= ctx.limits.max_pdf_pages || sink.is_full() {
            break;
        }
        sink.check_deadline()?;
        processed += 1;
        sink.newline();
        sink.anchor(LocKind::Page(i as u32 + 1));
        let text = page.text().map(|t| t.all()).unwrap_or_default();
        let meaningful = text.chars().filter(|c| c.is_alphanumeric()).count();
        text_chars += meaningful;
        if meaningful < 16 && page.objects().iter().any(|o| o.object_type() == PdfPageObjectType::Image) {
            scanned += 1;
        }
        sink.push_str(&text);
    }
    if processed < count {
        tracing::debug!(path = %path.display(), processed, count, "PDF truncated at page limit");
    }
    if processed > 0 && scanned * 2 >= processed && text_chars / (processed as usize) < 64 {
        meta.flags |= flags::NEEDS_OCR;
    }
    Ok(meta)
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
