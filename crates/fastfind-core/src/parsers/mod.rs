//! Pluggable document text extraction.
//!
//! Every format implements [`DocumentParser`]. Parsers write into a bounded [`TextSink`]
//! (hard caps on characters and time) and attach [`Loc`] anchors (page / slide / sheet / row /
//! section) so snippets can say *where* a match is. All parsers treat input as hostile: sizes,
//! nesting and decompression are bounded, and the pipeline additionally isolates each call
//! with `catch_unwind`.

pub mod doc;
pub mod epub;
pub mod html;
pub mod json;
pub mod locmap;
pub mod odf;
pub mod ooxml;
pub mod pdf;
pub mod pdfworker;
pub mod plain;
pub mod ppt;
pub mod registry;
pub mod rtf;
pub mod sink;
pub mod sniff;
pub mod xls;
pub mod xml;
pub mod zipsafe;

use std::io;
use std::path::Path;
use std::time::Instant;

pub use locmap::{Loc, LocKind};
pub use registry::{ParserRegistry, Resolved};
pub use sink::TextSink;

/// Format family used for the `type:` filter and UI icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Text,
    Code,
    Data,
    Web,
    Pdf,
    Word,
    Spreadsheet,
    Presentation,
    Ebook,
    Image,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Code => "code",
            Kind::Data => "data",
            Kind::Web => "web",
            Kind::Pdf => "pdf",
            Kind::Word => "word",
            Kind::Spreadsheet => "spreadsheet",
            Kind::Presentation => "presentation",
            Kind::Ebook => "ebook",
            Kind::Image => "image",
            Kind::Other => "other",
        }
    }

    pub fn all() -> &'static [Kind] {
        &[Kind::Text, Kind::Code, Kind::Data, Kind::Web, Kind::Pdf, Kind::Word, Kind::Spreadsheet,
          Kind::Presentation, Kind::Ebook, Kind::Image, Kind::Other]
    }

    pub fn parse(s: &str) -> Option<Kind> {
        let s = s.to_ascii_lowercase();
        Some(match s.as_str() {
            "text" | "txt" => Kind::Text,
            "code" | "source" => Kind::Code,
            "data" => Kind::Data,
            "web" | "html" => Kind::Web,
            "pdf" => Kind::Pdf,
            "word" | "document" | "doc" => Kind::Word,
            "spreadsheet" | "excel" | "sheet" => Kind::Spreadsheet,
            "presentation" | "powerpoint" | "slides" => Kind::Presentation,
            "ebook" | "epub" => Kind::Ebook,
            "image" => Kind::Image,
            "other" => Kind::Other,
            _ => return None,
        })
    }
}

/// How snippet/preview text is obtained later for documents produced by a parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMode {
    /// The file *is* its text: re-read and decode the original (never duplicated on disk).
    Raw = 0,
    /// Cheap markup (HTML/XML/JSON/RTF): re-run the parser on demand.
    Reparse = 1,
    /// Expensive binary format (PDF/Office/ODF/EPUB): extracted text is stored compressed.
    Stored = 2,
}

impl TextMode {
    pub fn from_u64(v: u64) -> TextMode {
        match v {
            0 => TextMode::Raw,
            1 => TextMode::Reparse,
            _ => TextMode::Stored,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct DocMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub pages: Option<u32>,
    /// Extra [`crate::model::flags`] bits set by the parser (scanned, approximate pages…).
    pub flags: u64,
}

impl DocMeta {
    /// Metadata text indexed in the `meta` field.
    pub fn searchable(&self) -> String {
        [&self.title, &self.author, &self.subject, &self.keywords]
            .iter()
            .filter_map(|s| s.as_deref())
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("password protected or encrypted")]
    Encrypted,
    #[error("corrupt or malformed: {0}")]
    Corrupt(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("exceeds safety limit: {0}")]
    LimitExceeded(String),
    #[error("parse time limit exceeded")]
    Timeout,
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
}

impl ParseError {
    pub fn corrupt(e: impl std::fmt::Display) -> Self {
        ParseError::Corrupt(e.to_string())
    }
}

impl From<zip::result::ZipError> for ParseError {
    fn from(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::Io(io) => ParseError::Io(io),
            zip::result::ZipError::UnsupportedArchive(m) if m.contains("assword") || m.contains("ncrypt") => {
                ParseError::Encrypted
            }
            other => ParseError::Corrupt(other.to_string()),
        }
    }
}

impl From<quick_xml::Error> for ParseError {
    fn from(e: quick_xml::Error) -> Self {
        match e {
            quick_xml::Error::Io(io) => match std::sync::Arc::try_unwrap(io) {
                Ok(io) => ParseError::Io(io),
                Err(arc) => ParseError::Corrupt(arc.to_string()),
            },
            other => ParseError::Corrupt(other.to_string()),
        }
    }
}

pub type ParseResult<T> = std::result::Result<T, ParseError>;

/// Safety limits applied to a single extraction.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Max bytes of UTF-8 text the sink accepts.
    pub max_text_bytes: usize,
    pub deadline: Option<Instant>,
    /// Max entries in a ZIP container.
    pub max_zip_entries: usize,
    /// Max total decompressed bytes read from a ZIP container.
    pub max_zip_bytes: u64,
    /// Max compression ratio of any entry larger than 1 MB (zip-bomb guard).
    pub max_zip_ratio: u64,
    pub max_pdf_pages: u32,
    /// Max nesting depth for recursive formats (RTF groups, PPT records).
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_text_bytes: 16 << 20,
            deadline: None,
            max_zip_entries: 20_000,
            max_zip_bytes: 1 << 30,
            max_zip_ratio: 250,
            max_pdf_pages: 20_000,
            max_depth: 512,
        }
    }
}

pub struct ParseContext<'a> {
    pub limits: &'a Limits,
}

/// A document text extractor.
///
/// `extract` performs text and metadata extraction in one pass (opening a file twice would
/// double I/O). `extract_metadata` is a cheap metadata-only path with a default implementation.
pub trait DocumentParser: Send + Sync {
    fn name(&self) -> &'static str;

    /// Extensions (lowercase, no dot) this parser is registered for.
    fn extensions(&self) -> &'static [&'static str];

    /// Final say after extension routing, given the first bytes of the file. Returning `false`
    /// makes the registry fall back to signature-based detection (e.g. a `.doc` that is really
    /// RTF or HTML — very common in the wild).
    fn can_handle(&self, _ext: &str, _header: &[u8]) -> bool {
        true
    }

    fn text_mode(&self) -> TextMode;

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta>;

    fn extract_metadata(&self, path: &Path, ctx: &ParseContext) -> ParseResult<DocMeta> {
        let limits = Limits { max_text_bytes: 0, ..ctx.limits.clone() };
        let mut sink = TextSink::new(0, limits.deadline);
        self.extract(path, &ParseContext { limits: &limits }, &mut sink)
    }
}

/// Resolve XML predefined entities (quick-xml reports them separately from text).
pub(crate) fn xml_entity(name: &str) -> Option<char> {
    Some(match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => return None,
    })
}

/// Character for a quick-xml general reference (`&amp;`, `&#x41;`); unknown (custom DTD)
/// entities are dropped, never expanded.
pub(crate) fn ref_char(r: &quick_xml::events::BytesRef) -> Option<char> {
    match r.resolve_char_ref() {
        Ok(Some(c)) => Some(c),
        _ => xml_entity(r),
    }
}

/// Push a quick-xml general reference into a sink.
pub(crate) fn push_xml_ref(sink: &mut TextSink, r: &quick_xml::events::BytesRef) {
    if let Some(c) = ref_char(r) {
        sink.push_char(c);
    }
}

/// Attribute value with predefined entities resolved (XML 1.0 normalisation).
pub(crate) fn attr_value(a: &quick_xml::events::attributes::Attribute) -> Option<String> {
    a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned())
}
