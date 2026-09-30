//! HTML → text. A small, allocation-light tag stripper: skips `<script>`, `<style>`, comments
//! and markup, turns block elements into line breaks and table cells into tabs, decodes
//! entities, and captures `<title>` as metadata. Tolerant of broken markup by design.

use std::io::Read;
use std::path::Path;

use super::plain::detect_encoding;
use super::{DocMeta, DocumentParser, ParseContext, ParseError, ParseResult, TextMode, TextSink};

/// HTML is parsed in memory; bigger files are indexed only up to this many bytes.
const MAX_HTML_BYTES: u64 = 64 << 20;

pub struct HtmlParser;

impl DocumentParser for HtmlParser {
    fn name(&self) -> &'static str {
        "html"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["html", "htm", "xhtml", "shtml", "mht", "mhtml", "hta"]
    }

    fn text_mode(&self) -> TextMode {
        TextMode::Reparse
    }

    fn extract(&self, path: &Path, _ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?.take(MAX_HTML_BYTES).read_to_end(&mut bytes)?;
        let text = decode_html_bytes(&bytes).ok_or_else(|| ParseError::Unsupported("binary content".into()))?;
        let mut meta = DocMeta::default();
        html_to_text(&text, sink, &mut meta);
        Ok(meta)
    }
}

/// Decode HTML bytes honouring BOM, `<meta charset>` and falling back to detection.
pub fn decode_html_bytes(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    let lower = String::from_utf8_lossy(head).to_ascii_lowercase();
    let declared = lower.find("charset=").and_then(|i| {
        let rest = lower[i + 8..].trim_start_matches(['"', '\'']);
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')).unwrap_or(rest.len());
        encoding_rs::Encoding::for_label(&rest.as_bytes()[..end])
    });
    let (enc, bom) = match encoding_rs::Encoding::for_bom(bytes) {
        Some(x) => x,
        None => match declared {
            Some(e) => (e, 0),
            None => detect_encoding(&bytes[..bytes.len().min(64 * 1024)])?,
        },
    };
    let (cow, _) = enc.decode_without_bom_handling(&bytes[bom..]);
    Some(cow.into_owned())
}

const BLOCK_TAGS: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "tr", "table", "h1", "h2", "h3", "h4", "h5", "h6",
    "section", "article", "header", "footer", "nav", "aside", "main", "pre", "blockquote", "dt",
    "dd", "dl", "hr", "form", "fieldset", "figure", "figcaption", "address", "caption", "tbody",
    "thead", "tfoot", "summary", "details",
];

/// Strip `html` into `sink`. Also used by the EPUB parser for XHTML chapters.
pub fn html_to_text(html: &str, sink: &mut TextSink, meta: &mut DocMeta) {
    let b = html.as_bytes();
    let mut i = 0;
    let mut text_start = 0;
    let mut in_title = false;
    let mut title = String::new();
    while i < b.len() {
        if sink.is_full() {
            break;
        }
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        // Flush text before the tag.
        if i > text_start {
            let raw = &html[text_start..i];
            let decoded = html_escape::decode_html_entities(raw);
            if in_title {
                title.push_str(&decoded);
            } else {
                push_collapsed(sink, &decoded);
            }
        }
        // Comment.
        if b[i..].starts_with(b"<!--") {
            i = find(b, i + 4, b"-->").map(|p| p + 3).unwrap_or(b.len());
            text_start = i;
            continue;
        }
        let tag_end = match b[i..].iter().position(|&c| c == b'>') {
            Some(p) => i + p + 1,
            None => b.len(),
        };
        let inner = &html[i + 1..tag_end.saturating_sub(1).max(i + 1)];
        let closing = inner.starts_with('/');
        let name: String = inner
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        i = tag_end;
        match name.as_str() {
            "script" | "style" | "template" | "svg" | "math" if !closing && !inner.ends_with('/') => {
                let close = format!("</{name}");
                i = find_ci(b, i, close.as_bytes()).map(|p| {
                    b[p..].iter().position(|&c| c == b'>').map(|q| p + q + 1).unwrap_or(b.len())
                }).unwrap_or(b.len());
            }
            "title" => in_title = !closing,
            "td" | "th" if closing => sink.tab(),
            n if BLOCK_TAGS.contains(&n) => sink.newline(),
            _ => {}
        }
        text_start = i;
    }
    if text_start < b.len() && !sink.is_full() {
        push_collapsed(sink, &html_escape::decode_html_entities(&html[text_start..]));
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if !title.is_empty() {
        meta.title = Some(title);
    }
}

/// HTML whitespace collapsing: any whitespace run becomes a single space.
fn push_collapsed(sink: &mut TextSink, s: &str) {
    let mut first = true;
    for w in s.split_whitespace() {
        if first {
            if s.starts_with(char::is_whitespace) {
                sink.space();
            }
            first = false;
        } else {
            sink.push_char(' ');
        }
        sink.push_str(w);
    }
    if !first && s.ends_with(char::is_whitespace) {
        sink.space();
    }
}

fn find(h: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    h.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

fn find_ci(h: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    h.get(from..)?
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> (String, DocMeta) {
        let mut sink = TextSink::new(1 << 20, None);
        let mut meta = DocMeta::default();
        html_to_text(s, &mut sink, &mut meta);
        (sink.into_parts().0, meta)
    }

    #[test]
    fn strips_markup_scripts_and_decodes_entities() {
        let (t, m) = strip(
            "<html><head><title> My  Page </title><style>p{color:red}</style>\
             <script>var secret = 1 < 2;</script></head><body><p>Fish &amp; chips</p>\
             <!-- hidden --><div>caf&eacute;<br>next</div><table><tr><td>a</td><td>b</td></tr></table></body></html>",
        );
        assert_eq!(m.title.as_deref(), Some("My Page"));
        assert!(t.contains("Fish & chips"));
        assert!(t.contains("café\nnext"));
        assert!(t.contains("a\tb"));
        assert!(!t.contains("secret"));
        assert!(!t.contains("hidden"));
        assert!(!t.contains("color"));
    }

    #[test]
    fn tolerates_broken_markup() {
        let (t, _) = strip("<p>unclosed <b>bold <script>never closed");
        assert!(t.contains("unclosed bold"));
    }
}
