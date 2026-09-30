//! Snippet construction: match-centred excerpts with highlight ranges in UTF-16 units.

use std::ops::ControlFlow;

use super::matcher::Highlighter;
use crate::model::{Snippet, SnippetResult};
use crate::textsource::TextSource;
use crate::util::{ceil_char_boundary, floor_char_boundary, utf16_len};

/// How much of a streamed plain-text file is scanned for snippets and match counts.
pub const STREAM_SCAN_BYTES: u64 = 256 << 20;
const HEAD_BYTES: usize = 240;

pub struct SnippetOptions {
    pub max_snippets: usize,
    /// Context bytes on each side of a match.
    pub context: usize,
    pub count_cap: u32,
}

impl Default for SnippetOptions {
    fn default() -> Self {
        Self { max_snippets: 2, context: 90, count_cap: 999 }
    }
}

/// Excerpt `text[start..end]` around the matches it contains, trimmed to word boundaries,
/// single-line, with "…" markers. `matches` are absolute byte spans within `text`.
pub fn excerpt(text: &str, mut start: usize, mut end: usize, matches: &[(usize, usize)]) -> (String, Vec<[u32; 2]>) {
    start = floor_char_boundary(text, start);
    end = ceil_char_boundary(text, end.min(text.len()));
    // Don't cut words in half at the edges.
    if start > 0 {
        let first = matches.first().map(|m| m.0).unwrap_or(start);
        if let Some(sp) = text[start..first].find(char::is_whitespace) {
            start += sp + 1;
        }
    }
    if end < text.len() {
        let last = matches.last().map(|m| m.1).unwrap_or(end).min(end);
        if let Some(sp) = text[last..end].rfind(char::is_whitespace) {
            end = last + sp;
        }
    }
    let lead = start > 0;
    let trail = end < text.len();
    let mut out = String::with_capacity(end - start + 8);
    if lead {
        out.push('…');
    }
    let offset16 = if lead { 1 } else { 0 };
    let body = &text[start..end];
    // Collapse control whitespace to spaces 1:1 (byte offsets stay valid).
    out.extend(body.chars().map(|c| if c == '\n' || c == '\t' || c == '\r' { ' ' } else { c }));
    if trail {
        out.push('…');
    }
    let mut hl = Vec::new();
    for &(s, e) in matches {
        if s < start || e > end {
            continue;
        }
        let a = utf16_len(&text[start..s]) + offset16;
        let b = a + utf16_len(&text[s..e]);
        hl.push([a, b]);
    }
    (out, hl)
}

/// Build result-list snippets and a (capped) match count.
pub fn snippets(path: &str, source: &TextSource, hl: &dyn Highlighter, opt: &SnippetOptions) -> SnippetResult {
    let mut res = SnippetResult { path: path.to_string(), ..Default::default() };
    if let Some(r) = source.unavailable_reason() {
        res.note = Some(r.to_string());
        return res;
    }
    let mut count: u32 = 0;
    let mut capped = false;
    let mut buf = Vec::new();
    let mut head: Option<String> = None;
    source.for_each_chunk(STREAM_SCAN_BYTES, |chunk, pos| {
        if pos.base == 0 {
            let (t, _) = excerpt(chunk, 0, HEAD_BYTES.min(chunk.len()), &[]);
            head = Some(t);
        }
        buf.clear();
        let room = (opt.count_cap - count) as usize + 1;
        hl.find(chunk, &mut buf, room);
        let mut i = 0;
        while i < buf.len() && res.snippets.len() < opt.max_snippets {
            let (s, e) = buf[i];
            let ws = s.saturating_sub(opt.context);
            let we = (e + opt.context).min(chunk.len());
            // Group following matches that fall inside this window.
            let mut j = i + 1;
            while j < buf.len() && buf[j].1 <= we {
                j += 1;
            }
            let (text, highlights) = excerpt(chunk, ws, we, &buf[i..j]);
            res.snippets.push(Snippet { text, highlights, location: source.locate(chunk, pos, s) });
            i = j;
        }
        count += buf.len() as u32;
        if count > opt.count_cap {
            count = opt.count_cap;
            capped = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    if res.snippets.is_empty() {
        // Name or metadata hit: show how the document begins.
        if let Some(t) = head.filter(|t| !t.trim().is_empty()) {
            res.snippets.push(Snippet { text: t, highlights: vec![], location: None });
        }
    }
    res.match_count = count;
    res.match_count_capped = capped;
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_offsets_are_utf16() {
        let text = "héllo wörld the quick brown fox jumps";
        let s = text.find("quick").unwrap();
        let (out, hl) = excerpt(text, 3, s + 11, &[(s, s + 5)]);
        // Leading partial word dropped, ellipses added.
        assert!(out.starts_with('…'));
        let chars: Vec<u16> = out.encode_utf16().collect();
        let [a, b] = hl[0];
        assert_eq!(String::from_utf16(&chars[a as usize..b as usize]).unwrap(), "quick");
    }
}
