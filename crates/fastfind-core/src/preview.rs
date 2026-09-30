//! Preview panel content: match-centred sections with highlights and locations
//! (page / slide / sheet!cell / line), built lazily for one document at a time.

use std::ops::ControlFlow;

use crate::model::{flags, Preview, PreviewSection};
use crate::search::matcher::Highlighter;
use crate::search::snippet::{excerpt, STREAM_SCAN_BYTES};
use crate::textsource::TextSource;

pub const MAX_PREVIEW_MATCHES: usize = 500;
const CONTEXT: usize = 320;
const MAX_SECTION_BYTES: usize = 256 * 1024;
const HEAD_BYTES: usize = 12 * 1024;

/// Fill `p.sections` (or `p.head`) from the document text.
pub fn build(p: &mut Preview, source: &TextSource, hl: &dyn Highlighter) {
    if let Some(r) = source.unavailable_reason() {
        p.notes.push(format!("Text not available: {r}"));
        return;
    }
    let mut total = 0usize;
    let mut capped = false;
    let mut section_bytes = 0usize;
    let mut buf = Vec::new();
    source.for_each_chunk(STREAM_SCAN_BYTES, |chunk, pos| {
        if pos.base == 0 && p.head.is_none() {
            let (t, _) = excerpt(chunk, 0, HEAD_BYTES.min(chunk.len()), &[]);
            p.head = Some(t);
        }
        buf.clear();
        hl.find(chunk, &mut buf, MAX_PREVIEW_MATCHES + 1 - total);
        let mut i = 0;
        while i < buf.len() {
            if total >= MAX_PREVIEW_MATCHES {
                capped = true;
                break;
            }
            let ws = buf[i].0.saturating_sub(CONTEXT);
            let mut we = (buf[i].1 + CONTEXT).min(chunk.len());
            // Merge matches whose windows overlap into one section.
            let mut j = i + 1;
            while j < buf.len() && buf[j].0 <= we && total + (j - i) < MAX_PREVIEW_MATCHES {
                we = (buf[j].1 + CONTEXT).min(chunk.len());
                j += 1;
            }
            let (text, highlights) = excerpt(chunk, ws, we, &buf[i..j]);
            section_bytes += text.len();
            p.sections.push(PreviewSection {
                location: source.locate(chunk, pos, buf[i].0),
                first_match: total as u32,
                highlights,
                text,
            });
            total += j - i;
            i = j;
            if section_bytes > MAX_SECTION_BYTES {
                capped = true;
                break;
            }
        }
        if capped { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
    });
    // Global match numbering for "match 3 of 12" navigation.
    let mut n = 0u32;
    for s in &mut p.sections {
        s.first_match = n;
        n += s.highlights.len() as u32;
    }
    p.total_matches = n;
    p.matches_capped = capped;
    if !p.sections.is_empty() {
        p.head = None;
    }
}

/// Explanatory notes derived from index flags.
pub fn flag_notes(p: &mut Preview) {
    let f = p.flags;
    if f & flags::NEEDS_OCR != 0 {
        p.notes.push("Scanned document without a text layer — OCR is required to search its content (enable OCR in Settings → Indexing).".into());
    }
    if f & flags::OCR != 0 {
        p.notes.push("Text recognised with OCR; it may contain recognition errors.".into());
    }
    if f & flags::ENCRYPTED != 0 {
        p.notes.push("Password-protected or encrypted — only the file name is indexed.".into());
    }
    if f & flags::FAILED != 0 {
        p.notes.push("The content could not be extracted (damaged or unsupported file) — only the file name is indexed.".into());
    }
    if f & flags::NAME_ONLY != 0 && f & (flags::ENCRYPTED | flags::FAILED | flags::NEEDS_OCR) == 0 {
        p.notes.push("Only the file name is indexed for this file type.".into());
    }
    if f & flags::TRUNCATED != 0 {
        p.notes.push("Very large document: only the first part of its text was indexed.".into());
    }
    if f & flags::APPROX_PAGES != 0 {
        p.notes.push("Page numbers are approximate (based on Word's last layout).".into());
    }
}
