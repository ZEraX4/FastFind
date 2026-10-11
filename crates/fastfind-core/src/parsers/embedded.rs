//! Documents inside other documents: e-mail attachments and archive entries.
//!
//! An embedded document is written to a private temporary folder under an ASCII name (so
//! every parser and the PDF helper processes can open it) and read by the regular parser for
//! its type, with the container's remaining text budget and deadline. Its text is appended
//! after a `Part` anchor, with its own anchors re-based, so a match reads
//! "Page 2 · Attachment: report.pdf".
//!
//! Containers nest (a zip in an e-mail in a zip); `Limits::container_depth` counts the levels
//! across formats, and nothing deeper than `MAX_NESTING` is opened. That also stops archives
//! that contain themselves.

use std::path::Path;
use std::sync::OnceLock;

use super::registry::ParserRegistry;
use super::sniff::read_header;
use super::{Limits, Loc, LocKind, ParseContext, ParseError, ParseResult, TextSink};
use crate::util::extension_of;

/// Embedded documents larger than this are not extracted.
pub(super) const MAX_EMBEDDED_BYTES: usize = 64 << 20;
/// Containers inside containers are opened this deep.
pub(super) const MAX_NESTING: usize = 4;

/// Start a new part; text pushed after this is located by `label`.
pub(super) fn begin_part(sink: &mut TextSink, label: String) {
    sink.newline();
    sink.anchor(LocKind::Part(label));
}

pub(super) fn registry() -> &'static ParserRegistry {
    static REGISTRY: OnceLock<ParserRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ParserRegistry::new)
}

/// Run the regular parser for an embedded document named `name` and append its text and
/// anchors after a `label` part. Documents without a parser, too large, too deeply nested,
/// damaged or locked are skipped without failing the container.
pub(super) fn extract_embedded(name: &str, data: &[u8], label: String, limits: &Limits, sink: &mut TextSink) -> ParseResult<()> {
    let ext = extension_of(Path::new(name));
    if limits.container_depth >= MAX_NESTING || data.is_empty() || data.len() > MAX_EMBEDDED_BYTES || !registry().has_parser_for(&ext) {
        return Ok(());
    }
    let room = limits.max_text_bytes.saturating_sub(sink.len());
    if room == 0 {
        return Ok(());
    }
    let dir = tempfile::Builder::new().prefix("fastfind-embedded").tempdir()?;
    let path = dir.path().join(format!("embedded.{ext}"));
    std::fs::write(&path, data)?;
    let header = read_header(&path)?;
    let Some(resolved) = registry().resolve(&path, &ext, &header, false) else {
        return Ok(());
    };
    let child_limits = Limits { max_text_bytes: room, container_depth: limits.container_depth + 1, ..limits.clone() };
    let mut child = TextSink::new(room, limits.deadline);
    match resolved.parser.extract(&path, &ParseContext { limits: &child_limits }, &mut child) {
        Ok(_) | Err(ParseError::Timeout) => {}
        Err(e) => {
            tracing::debug!(embedded = %name, error = %e, "embedded document not extracted");
            return Ok(());
        }
    }
    let (text, locs, _) = child.into_parts();
    if text.trim().is_empty() {
        return Ok(());
    }
    begin_part(sink, label.clone());
    let base = sink.len() as u32;
    sink.push_str(&text);
    for loc in locs {
        let kind = match loc.kind {
            // Parts of the embedded document (its own attachments or entries) also name this
            // one: "In archive: notes.txt · Attachment: bundle.zip".
            LocKind::Part(inner) => LocKind::Part(format!("{inner} · {label}")),
            k => k,
        };
        sink.anchor_at(Loc { offset: base + loc.offset, kind });
    }
    sink.newline();
    Ok(())
}
