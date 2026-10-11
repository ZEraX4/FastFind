//! Recovering a document's text for snippets, previews and verification without keeping it
//! all in memory:
//!
//! * `Raw` formats (plain text, code, CSV…) are streamed from the original file in
//!   line-aligned chunks (a 2 GB log is never loaded at once).
//! * `Reparse` formats (HTML, XML, JSON) are re-extracted on demand — cheap and avoids storing
//!   a second copy.
//! * `Stored` formats (PDF, Office, ODF, EPUB, RTF) come from the compressed text store; if the
//!   stored copy is missing (e.g. store rebuilt), the file is re-parsed.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::index::TextStore;
use crate::parsers::locmap::describe;
use crate::parsers::plain::stream_text;
use crate::parsers::sniff::read_header;
use crate::parsers::{Limits, Loc, ParseContext, ParserRegistry, TextMode, TextSink};
use crate::util::{extension_of, path_hash};

/// Upper bound for on-demand re-extraction.
const REPARSE_MAX_BYTES: usize = 32 << 20;
const REPARSE_TIMEOUT: Duration = Duration::from_secs(20);
/// How much of the previous chunk a streamed chunk repeats, so a phrase or regex match
/// crossing a chunk boundary is still seen whole. Longer matches across a boundary are missed.
const CHUNK_OVERLAP: usize = 16 << 10;

pub enum TextSource {
    Mem { text: String, locs: Vec<Loc>, line_based: bool },
    File { path: PathBuf },
    Unavailable(String),
}

#[derive(Clone, Copy)]
pub struct ChunkPos {
    /// Byte offset of the chunk within the whole document text.
    pub base: usize,
    /// Newlines before the chunk.
    pub lines_before: u64,
    /// Leading bytes repeated from the end of the previous chunk.
    pub overlap: usize,
}

impl ChunkPos {
    /// A match ending inside the repeated bytes was already reported with the previous chunk.
    pub fn seen(&self, end: usize) -> bool {
        end <= self.overlap
    }
}

/// Where the part of `chunk` that the next chunk repeats begins: at a line start (or else
/// after a whitespace) within the last `CHUNK_OVERLAP` bytes, so the repeat never begins
/// mid-word. Returns `chunk.len()` when there is no such place.
fn overlap_start(chunk: &str) -> usize {
    let mut from = chunk.len().saturating_sub(CHUNK_OVERLAP);
    while !chunk.is_char_boundary(from) {
        from += 1;
    }
    let tail = &chunk[from..];
    // Chunks end with a line break, so look for the one before it.
    let body = tail.strip_suffix('\n').unwrap_or(tail);
    let start = body.rfind('\n').map(|i| i + 1).or_else(|| {
        let (i, c) = tail.char_indices().find(|(_, c)| c.is_whitespace())?;
        Some(i + c.len_utf8())
    });
    start.map_or(chunk.len(), |i| from + i)
}

impl TextSource {
    pub fn load(path: &str, mode: TextMode, texts: &TextStore, registry: &ParserRegistry) -> TextSource {
        let p = Path::new(path);
        match mode {
            TextMode::Raw => {
                if p.is_file() {
                    TextSource::File { path: p.to_path_buf() }
                } else {
                    TextSource::Unavailable("file no longer exists".into())
                }
            }
            TextMode::Stored => match texts.get(path_hash(path)) {
                Ok(Some((text, locs))) => TextSource::Mem { text, locs, line_based: false },
                Ok(None) => Self::reparse(p, registry),
                Err(e) => {
                    tracing::warn!(path, error = %e, "text store read failed; re-parsing");
                    Self::reparse(p, registry)
                }
            },
            TextMode::Reparse => Self::reparse(p, registry),
        }
    }

    /// Run the parser again (bounded) to recover text.
    pub fn reparse(p: &Path, registry: &ParserRegistry) -> TextSource {
        let Ok(header) = read_header(p) else {
            return TextSource::Unavailable("file no longer exists".into());
        };
        let ext = extension_of(p);
        let Some(r) = registry.resolve(p, &ext, &header, true) else {
            return TextSource::Unavailable("content not indexed".into());
        };
        let limits = Limits { max_text_bytes: REPARSE_MAX_BYTES, deadline: Some(Instant::now() + REPARSE_TIMEOUT), ..Limits::default() };
        let mut sink = TextSink::new(limits.max_text_bytes, limits.deadline);
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.parser.extract(p, &ParseContext { limits: &limits }, &mut sink)));
        match res {
            Ok(Ok(_)) | Ok(Err(crate::parsers::ParseError::Timeout)) => {
                let (text, locs, _) = sink.into_parts();
                TextSource::Mem { text, locs, line_based: false }
            }
            Ok(Err(e)) => TextSource::Unavailable(e.to_string()),
            Err(_) => TextSource::Unavailable("parser crashed".into()),
        }
    }

    /// Visit the text in chunks. `max_bytes` bounds how much of a streamed file is read.
    pub fn for_each_chunk(&self, max_bytes: u64, mut f: impl FnMut(&str, ChunkPos) -> ControlFlow<()>) {
        match self {
            TextSource::Mem { text, .. } => {
                let _ = f(text, ChunkPos { base: 0, lines_before: 0, overlap: 0 });
            }
            TextSource::File { path } => {
                // `next` is where the next fresh chunk starts in the document text.
                let (mut next, mut lines) = (0usize, 0u64);
                let mut tail = String::new();
                let mut joined = String::new();
                let _ = stream_text(path, |chunk| {
                    if next as u64 >= max_bytes {
                        return ControlFlow::Break(());
                    }
                    let overlap = tail.len();
                    let tail_lines = tail.bytes().filter(|&b| b == b'\n').count() as u64;
                    let pos = ChunkPos { base: next - overlap, lines_before: lines - tail_lines, overlap };
                    let text = if overlap == 0 {
                        chunk
                    } else {
                        joined.clear();
                        joined.push_str(&tail);
                        joined.push_str(chunk);
                        &joined
                    };
                    let flow = f(text, pos);
                    next += chunk.len();
                    lines += chunk.bytes().filter(|&b| b == b'\n').count() as u64;
                    tail.clear();
                    tail.push_str(&chunk[overlap_start(chunk)..]);
                    flow
                });
            }
            TextSource::Unavailable(_) => {}
        }
    }

    /// Human-readable location of `offset` within `chunk`.
    pub fn locate(&self, chunk: &str, pos: ChunkPos, offset: usize) -> Option<String> {
        match self {
            TextSource::Mem { text, locs, line_based } => describe(locs, text, pos.base + offset, *line_based),
            TextSource::File { .. } => {
                let o = offset.min(chunk.len());
                let line = pos.lines_before + chunk.as_bytes()[..o].iter().filter(|&&b| b == b'\n').count() as u64 + 1;
                Some(format!("Line {line}"))
            }
            TextSource::Unavailable(_) => None,
        }
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            TextSource::Unavailable(r) => Some(r),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::matcher::{build_regex, exact_pattern, RegexMatcher};
    use crate::search::snippet::{snippets, SnippetOptions};

    /// About 1.2 MB of numbered lines, so the file streams in several chunks.
    fn big_file(dir: &Path) -> (PathBuf, String) {
        let text: String = (0..40_000).map(|i| format!("line {i} ends here\n")).collect();
        let path = dir.join("big.log");
        std::fs::write(&path, &text).unwrap();
        (path, text)
    }

    #[test]
    fn streamed_chunks_overlap_and_report_true_positions() {
        let dir = tempfile::tempdir().unwrap();
        let (path, text) = big_file(dir.path());
        let mut chunks = Vec::new();
        TextSource::File { path }.for_each_chunk(u64::MAX, |c, pos| {
            assert_eq!(&text[pos.base..pos.base + c.len()], c);
            assert_eq!(pos.lines_before, text[..pos.base].matches('\n').count() as u64);
            assert!(pos.base == 0 || text[..pos.base].ends_with('\n'), "a repeat starts at a line start");
            chunks.push((pos.base, pos.base + c.len(), pos.overlap));
            ControlFlow::Continue(())
        });
        assert!(chunks.len() > 2);
        assert_eq!(chunks[0].2, 0);
        assert_eq!(chunks.last().unwrap().1, text.len());
        for w in chunks.windows(2) {
            assert!(w[1].2 > 0 && w[1].2 <= CHUNK_OVERLAP);
            assert_eq!(w[1].0 + w[1].2, w[0].1, "the fresh part starts where the previous chunk ended");
        }
    }

    #[test]
    fn matches_across_chunk_boundaries_are_found_once() {
        let dir = tempfile::tempdir().unwrap();
        let (path, text) = big_file(dir.path());
        let src = TextSource::File { path };
        let mut ends = Vec::new();
        src.for_each_chunk(u64::MAX, |c, pos| {
            ends.push(pos.base + c.len());
            ControlFlow::Continue(())
        });
        // An Exact phrase running from the last line of each chunk into the next one.
        for &end in &ends[..ends.len() - 1] {
            let last = text[..end - 1].rfind('\n').unwrap() + 1;
            let phrase = format!("{} {}", &text[last..end - 1], text[end..].lines().next().unwrap());
            let hl = RegexMatcher { re: build_regex(&exact_pattern(&phrase, true), false).unwrap() };
            let opt = SnippetOptions { count_cap: 100, ..Default::default() };
            assert_eq!(snippets("big.log", &src, &hl, &opt).match_count, 1, "{phrase}");
        }
        // Every line matches once: nothing in the repeated parts is counted twice.
        let hl = RegexMatcher { re: build_regex("ends", false).unwrap() };
        let opt = SnippetOptions { count_cap: 1_000_000, ..Default::default() };
        assert_eq!(snippets("big.log", &src, &hl, &opt).match_count, 40_000);
    }

    #[test]
    fn repeat_never_starts_mid_word() {
        assert_eq!(overlap_start("one\ntwo\n"), 4);
        let long = format!("{}\n", "word ".repeat(10_000));
        let at = overlap_start(&long);
        assert!(at < long.len() && long[..at].ends_with(' '));
        assert_eq!(overlap_start(&format!("{}\n", "x".repeat(CHUNK_OVERLAP + 10))), CHUNK_OVERLAP + 11);
    }
}
