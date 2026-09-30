//! Parser registry: routes a file to a parser by extension, verifies with magic bytes, and
//! falls back to signature detection for mislabelled files.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use super::doc::DocParser;
use super::epub::EpubParser;
use super::html::HtmlParser;
use super::json::JsonParser;
use super::odf::OdfParser;
use super::ooxml::{DocxParser, PptxParser, XlsxParser};
use super::pdf::PdfParser;
use super::plain::PlainTextParser;
use super::ppt::PptParser;
use super::rtf::RtfParser;
use super::sniff::{cfb_kind, sniff, CfbKind, Sniffed};
use super::xls::XlsParser;
use super::xml::XmlParser;
use super::{DocMeta, DocumentParser, Kind, ParseContext, ParseError, ParseResult, TextMode, TextSink};

/// Reports encrypted OOXML packages (a CFB container with `EncryptionInfo`) uniformly.
struct EncryptedPackage;

impl DocumentParser for EncryptedPackage {
    fn name(&self) -> &'static str {
        "encrypted-ooxml"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }
    fn text_mode(&self) -> TextMode {
        TextMode::Stored
    }
    fn extract(&self, _: &Path, _: &ParseContext, _: &mut TextSink) -> ParseResult<DocMeta> {
        Err(ParseError::Encrypted)
    }
}

pub type ParserRef = Arc<dyn DocumentParser>;

pub struct Resolved {
    pub parser: ParserRef,
}

pub struct ParserRegistry {
    by_ext: HashMap<&'static str, ParserRef>,
    plain: ParserRef,
    html: ParserRef,
    xml: ParserRef,
    rtf: ParserRef,
    pdf: ParserRef,
    doc: ParserRef,
    xls: ParserRef,
    ppt: ParserRef,
    encrypted: ParserRef,
}

impl Default for ParserRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ParserRegistry {
    pub fn new() -> Self {
        let plain: ParserRef = Arc::new(PlainTextParser);
        let html: ParserRef = Arc::new(HtmlParser);
        let xml: ParserRef = Arc::new(XmlParser);
        let rtf: ParserRef = Arc::new(RtfParser);
        let pdf: ParserRef = Arc::new(PdfParser);
        let doc: ParserRef = Arc::new(DocParser);
        let xls: ParserRef = Arc::new(XlsParser);
        let ppt: ParserRef = Arc::new(PptParser);
        let all: Vec<ParserRef> = vec![
            plain.clone(), html.clone(), xml.clone(), rtf.clone(), pdf.clone(), doc.clone(), xls.clone(),
            ppt.clone(), Arc::new(JsonParser), Arc::new(DocxParser), Arc::new(XlsxParser),
            Arc::new(PptxParser), Arc::new(OdfParser), Arc::new(EpubParser),
        ];
        let mut by_ext = HashMap::new();
        for p in &all {
            for e in p.extensions() {
                by_ext.insert(*e, p.clone());
            }
        }
        Self { by_ext, plain, html, xml, rtf, pdf, doc, xls, ppt, encrypted: Arc::new(EncryptedPackage) }
    }

    /// All extensions with a dedicated parser (for the settings UI).
    pub fn supported_extensions(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.by_ext.keys().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn has_parser_for(&self, ext: &str) -> bool {
        self.by_ext.contains_key(ext)
    }

    /// Choose a parser for a file. `None` = index the name only.
    pub fn resolve(&self, path: &Path, ext: &str, header: &[u8], detect_text: bool) -> Option<Resolved> {
        if let Some(p) = self.by_ext.get(ext) {
            if p.can_handle(ext, header) {
                return Some(Resolved { parser: p.clone() });
            }
        } else if is_known_binary(ext) {
            return None;
        }
        let parser = match sniff(header) {
            Sniffed::Pdf => self.pdf.clone(),
            Sniffed::Rtf => self.rtf.clone(),
            Sniffed::Html => self.html.clone(),
            Sniffed::Xml => self.xml.clone(),
            Sniffed::Cfb => match cfb_kind(path) {
                CfbKind::Word => self.doc.clone(),
                CfbKind::Excel => self.xls.clone(),
                CfbKind::PowerPoint => self.ppt.clone(),
                CfbKind::EncryptedOoxml => self.encrypted.clone(),
                CfbKind::Unknown => return None,
            },
            Sniffed::Text | Sniffed::Utf16 if detect_text || self.by_ext.contains_key(ext) => self.plain.clone(),
            _ => return None,
        };
        Some(Resolved { parser })
    }
}

/// Format family for an extension (independent of which parser ends up reading it).
pub fn kind_for_ext(ext: &str) -> Kind {
    match ext {
        "pdf" | "ai" => Kind::Pdf,
        "doc" | "dot" | "docx" | "docm" | "dotx" | "dotm" | "odt" | "ott" | "fodt" | "rtf" | "wpd" | "pages" => Kind::Word,
        "xls" | "xlt" | "xla" | "xlsx" | "xlsm" | "xltx" | "xltm" | "xlam" | "xlsb" | "ods" | "ots" | "csv" | "tsv" | "tab" | "psv" | "numbers" => Kind::Spreadsheet,
        "ppt" | "pps" | "pot" | "pptx" | "pptm" | "potx" | "potm" | "ppsx" | "ppsm" | "odp" | "otp" | "key" => Kind::Presentation,
        "html" | "htm" | "xhtml" | "shtml" | "mht" | "mhtml" | "hta" => Kind::Web,
        "json" | "jsonl" | "ndjson" | "geojson" | "har" | "xml" | "xsd" | "xsl" | "xslt" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "config" | "properties" | "plist" | "rss" | "atom" | "kml" | "gpx" | "svg" | "sql" | "env" | "reg" => Kind::Data,
        "epub" | "fb2" | "mobi" | "azw" | "azw3" => Kind::Ebook,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "heic" | "heif" | "ico" | "psd" | "raw" | "cr2" | "nef" | "dng" => Kind::Image,
        "txt" | "text" | "md" | "markdown" | "mdown" | "rst" | "adoc" | "asciidoc" | "org" | "tex" | "bib" | "log" | "nfo" | "srt" | "vtt" | "eml" | "mbox" | "vcf" | "ics" => Kind::Text,
        e if super::plain::PLAIN_EXTENSIONS.contains(&e) => Kind::Code,
        _ => Kind::Other,
    }
}

/// Extensions that are never text: skipped without even reading their header.
pub fn is_known_binary(ext: &str) -> bool {
    matches!(ext,
        "exe" | "dll" | "so" | "dylib" | "o" | "obj" | "a" | "lib" | "bin" | "iso" | "img" | "dmg" | "zip"
        | "7z" | "rar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz4" | "jar" | "war" | "ear" | "class"
        | "pyc" | "pyo" | "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "ico"
        | "heic" | "heif" | "psd" | "raw" | "cr2" | "nef" | "dng" | "mp3" | "mp4" | "m4a" | "m4v"
        | "wav" | "flac" | "ogg" | "opus" | "aac" | "avi" | "mkv" | "mov" | "wmv" | "webm" | "flv"
        | "ttf" | "otf" | "woff" | "woff2" | "eot" | "sqlite" | "sqlite3" | "db" | "mdb" | "accdb"
        | "pst" | "ost" | "msi" | "msix" | "cab" | "apk" | "ipa" | "deb" | "rpm" | "vmdk" | "vhd"
        | "vhdx" | "qcow2" | "vdi" | "pdb" | "idb" | "ilk" | "blend" | "fbx" | "3ds" | "dwg" | "dxf"
        | "sys" | "drv" | "efi" | "wasm" | "node" | "pak" | "dat" | "idx" | "lock" | "swp" | "tmp"
        | "xlsb" | "one" | "msg" | "mobi" | "azw" | "azw3" | "numbers" | "pages" | "key"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_by_extension_and_signature() {
        let r = ParserRegistry::new();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.doc");
        // A ".doc" that is really RTF (very common) must go to the RTF parser.
        let rtf = b"{\\rtf1\\ansi hello}";
        std::fs::write(&p, rtf).unwrap();
        assert_eq!(r.resolve(&p, "doc", rtf, true).unwrap().parser.name(), "rtf");
        // A ".xls" that is really HTML (typical web export).
        let html = b"<html><body><table><tr><td>1</td></tr></table></body></html>";
        assert_eq!(r.resolve(&p, "xls", html, true).unwrap().parser.name(), "html");
        // Known binaries are never sniffed.
        assert!(r.resolve(&p, "png", b"hello", true).is_none());
        // Unknown extension with text content → plain when detection is on.
        assert_eq!(r.resolve(&p, "weird", b"just text", true).unwrap().parser.name(), "plain-text");
        assert!(r.resolve(&p, "weird", b"just text", false).is_none());
        assert_eq!(r.resolve(&p, "txt", b"plain", false).unwrap().parser.name(), "plain-text");
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_for_ext("docx"), Kind::Word);
        assert_eq!(kind_for_ext("csv"), Kind::Spreadsheet);
        assert_eq!(kind_for_ext("rs"), Kind::Code);
        assert_eq!(kind_for_ext("zzz"), Kind::Other);
    }
}
