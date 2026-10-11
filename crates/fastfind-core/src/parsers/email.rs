//! E-mail messages (`.eml`, RFC 5322 / MIME).
//!
//! Decoding is done by the `mail-parser` crate: encoded headers (RFC 2047), quoted-printable
//! and base64 bodies, every charset `encoding_rs` knows, and nested multiparts. Indexed text:
//!
//! * the From / To / Cc / Date / Subject headers (so a sender's name or address finds the mail);
//! * the message body (plain text, or HTML converted to text when there is no plain part);
//! * attachments, each after a `Part("Attachment: name")` anchor: text attachments
//!   directly, attached messages recursively, and documents (PDF, Word, Excel…) through the
//!   regular parsers, so their own page and sheet anchors are kept ("Page 2 · Attachment: x.pdf").
//!
//! Attachments are handed to those parsers as files in a private temporary folder, with the
//! remaining text budget and the same deadline. Formats without a parser (images, archives)
//! are skipped. If the file is not a parseable message at all, its raw text is indexed.

use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use mail_parser::{Address, Message, MessageParser, MimeHeaders};

use super::html::html_to_text;
use super::registry::ParserRegistry;
use super::sniff::read_header;
use super::{DocMeta, DocumentParser, Limits, LocKind, ParseContext, ParseResult, TextMode, TextSink};
use crate::util::extension_of;

/// Messages larger than this are read only up to here (the tail is usually attachments).
const MAX_EML_BYTES: u64 = 256 << 20;
/// Attachments larger than this are not extracted.
pub(super) const MAX_ATTACHMENT_BYTES: usize = 64 << 20;
/// Messages attached to messages attached to… are followed this deep.
pub(super) const MAX_NESTING: usize = 4;

pub struct EmailParser;

impl DocumentParser for EmailParser {
    fn name(&self) -> &'static str {
        "email"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["eml"]
    }

    fn text_mode(&self) -> TextMode {
        // Decoding (and attachment extraction) is too costly to repeat for every snippet.
        TextMode::Stored
    }

    fn extract(&self, path: &Path, ctx: &ParseContext, sink: &mut TextSink) -> ParseResult<DocMeta> {
        let mut data = Vec::new();
        let file = std::fs::File::open(path)?;
        let truncated = file.metadata()?.len() > MAX_EML_BYTES;
        file.take(MAX_EML_BYTES).read_to_end(&mut data)?;

        let mut meta = DocMeta::default();
        match MessageParser::default().parse(&data) {
            Some(msg) if msg.headers().iter().len() > 0 => {
                meta.title = msg.subject().map(str::to_string).filter(|s| !s.trim().is_empty());
                meta.author = msg.from().and_then(display_first);
                emit_message(&msg, ctx.limits, sink, 0)?;
            }
            // Not a message: index it as the text it is.
            _ => sink.push_str(&String::from_utf8_lossy(&data)),
        }
        if truncated {
            sink.mark_truncated();
        }
        Ok(meta)
    }
}

fn display_first(a: &Address) -> Option<String> {
    let first = a.first()?;
    match (first.name.as_deref(), first.address.as_deref()) {
        (Some(n), _) if !n.trim().is_empty() => Some(n.trim().to_string()),
        (_, Some(addr)) => Some(addr.to_string()),
        _ => None,
    }
}

fn format_addresses(a: &Address) -> String {
    a.iter()
        .filter_map(|x| match (x.name.as_deref(), x.address.as_deref()) {
            (Some(n), Some(addr)) if !n.trim().is_empty() => Some(format!("{} <{addr}>", n.trim())),
            (_, Some(addr)) => Some(addr.to_string()),
            (Some(n), None) => Some(n.trim().to_string()),
            (None, None) => None,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn emit_message(msg: &Message, limits: &Limits, sink: &mut TextSink, depth: usize) -> ParseResult<()> {
    let header = |sink: &mut TextSink, label: &str, value: Option<String>| {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            sink.push_str(label);
            sink.push_str(": ");
            sink.push_str(&v);
            sink.newline();
        }
    };
    header(sink, "From", msg.from().map(format_addresses));
    header(sink, "To", msg.to().map(format_addresses));
    header(sink, "Cc", msg.cc().map(format_addresses));
    header(sink, "Date", msg.date().map(|d| d.to_rfc3339()));
    header(sink, "Subject", msg.subject().map(str::to_string));
    sink.newline();

    for i in 0..msg.text_body_count() {
        sink.check_deadline()?;
        if let Some(text) = msg.body_text(i) {
            sink.push_str(&text);
            sink.newline();
        }
    }

    for part in msg.attachments() {
        if sink.is_full() {
            break;
        }
        sink.check_deadline()?;
        let name = part.attachment_name().map(str::to_string).filter(|n| !n.trim().is_empty());
        if let Some(inner) = part.message() {
            if depth < MAX_NESTING {
                let label = inner.subject().map(|s| format!("Attached message: {s}")).unwrap_or_else(|| "Attached message".into());
                begin_part(sink, label);
                emit_message(inner, limits, sink, depth + 1)?;
            }
            continue;
        }
        let label = format!("Attachment: {}", name.as_deref().unwrap_or("unnamed"));
        if part.is_text_html() {
            if let Some(html) = part.text_contents() {
                begin_part(sink, label);
                html_to_text(html, sink, &mut DocMeta::default());
            }
        } else if part.is_text() {
            if let Some(text) = part.text_contents() {
                begin_part(sink, label);
                sink.push_str(text);
            }
        } else if let Some(name) = name {
            extract_attachment(&name, part.contents(), label, limits, sink, depth)?;
        }
    }
    Ok(())
}

pub(super) fn begin_part(sink: &mut TextSink, label: String) {
    sink.newline();
    sink.anchor(LocKind::Part(label));
}

fn registry() -> &'static ParserRegistry {
    static REGISTRY: OnceLock<ParserRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ParserRegistry::new)
}

/// Run the regular parser for an attached document and append its text and anchors.
pub(super) fn extract_attachment(name: &str, data: &[u8], label: String, limits: &Limits, sink: &mut TextSink, depth: usize) -> ParseResult<()> {
    let ext = extension_of(Path::new(name));
    if ext == "eml" {
        if depth < MAX_NESTING {
            if let Some(inner) = MessageParser::default().parse(data) {
                begin_part(sink, label);
                emit_message(&inner, limits, sink, depth + 1)?;
            }
        }
        return Ok(());
    }
    if data.is_empty() || data.len() > MAX_ATTACHMENT_BYTES || !registry().has_parser_for(&ext) {
        return Ok(());
    }
    // A private folder; the file name is ASCII so every parser and helper process can open it.
    let dir = tempfile::Builder::new().prefix("fastfind-attachment").tempdir()?;
    let path = dir.path().join(format!("attachment.{ext}"));
    std::fs::write(&path, data)?;
    let header = read_header(&path)?;
    let Some(resolved) = registry().resolve(&path, &ext, &header, false) else {
        return Ok(());
    };
    let room = limits.max_text_bytes.saturating_sub(sink.len());
    if room == 0 {
        return Ok(());
    }
    let child_limits = Limits { max_text_bytes: room, ..limits.clone() };
    let mut child = TextSink::new(room, limits.deadline);
    match resolved.parser.extract(&path, &ParseContext { limits: &child_limits }, &mut child) {
        Ok(_) | Err(super::ParseError::Timeout) => {}
        // A damaged or locked attachment does not fail the message.
        Err(e) => {
            tracing::debug!(attachment = %name, error = %e, "attachment not extracted");
            return Ok(());
        }
    }
    let (text, locs, _) = child.into_parts();
    if text.trim().is_empty() {
        return Ok(());
    }
    begin_part(sink, label);
    let base = sink.len() as u32;
    sink.push_str(&text);
    for loc in locs {
        sink.anchor_at(super::Loc { offset: base + loc.offset, kind: loc.kind });
    }
    sink.newline();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> (String, Vec<super::super::Loc>, DocMeta) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.eml");
        std::fs::write(&p, raw.replace('\n', "\r\n")).unwrap();
        let limits = Limits::default();
        let mut sink = TextSink::new(1 << 20, None);
        let meta = EmailParser.extract(&p, &ParseContext { limits: &limits }, &mut sink).unwrap();
        let (t, l, _) = sink.into_parts();
        (t, l, meta)
    }

    #[test]
    fn decodes_encoded_headers_and_base64_body() {
        // Subject "تقرير الميزانية" (RFC 2047, UTF-8 base64); body base64 UTF-8 Arabic.
        let raw = "From: =?UTF-8?B?2KPYrdmF2K8=?= <ahmed@example.com>\nTo: team@example.com\nSubject: =?UTF-8?B?2KrZgtix2YrYsSDYp9mE2YXZitiy2KfZhtmK2Kk=?=\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: base64\n\n2KfZhNmF2YrYstin2YbZitipINmF2LnYqtmF2K/YqQ==\n";
        let (t, _, meta) = parse(raw);
        assert_eq!(meta.title.as_deref(), Some("تقرير الميزانية"));
        assert_eq!(meta.author.as_deref(), Some("أحمد"));
        assert!(t.contains("الميزانية معتمدة"), "{t}");
        assert!(t.contains("ahmed@example.com"), "{t}");
    }

    #[test]
    fn decodes_quoted_printable_in_legacy_charsets() {
        // Turkish in windows-1254, quoted-printable: "Geçiş izni onaylandı".
        let raw = "From: ayse@example.com\nSubject: =?windows-1254?Q?Ge=E7i=FE_izni?=\nContent-Type: text/plain; charset=windows-1254\nContent-Transfer-Encoding: quoted-printable\n\nGe=E7i=FE izni onayland=FD.\n";
        let (t, _, meta) = parse(raw);
        assert_eq!(meta.title.as_deref(), Some("Geçiş izni"));
        assert!(t.contains("Geçiş izni onaylandı."), "{t}");
    }

    #[test]
    fn html_only_messages_become_text() {
        let raw = "From: a@example.com\nSubject: html\nContent-Type: text/html; charset=utf-8\n\n<html><body><p>Quarterly <b>numbers</b> attached</p><script>var x='hidden';</script></body></html>\n";
        let (t, _, _) = parse(raw);
        assert!(t.contains("Quarterly numbers attached"), "{t}");
        assert!(!t.contains("hidden"), "{t}");
    }

    #[test]
    fn not_a_message_is_indexed_as_text() {
        let (t, _, meta) = parse("just some notes, no headers here at all\n");
        assert!(t.contains("just some notes"));
        assert!(meta.title.is_none());
    }
}
