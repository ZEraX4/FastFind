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
                let _ = f(text, ChunkPos { base: 0, lines_before: 0 });
            }
            TextSource::File { path } => {
                let mut pos = ChunkPos { base: 0, lines_before: 0 };
                let _ = stream_text(path, |chunk| {
                    if pos.base as u64 >= max_bytes {
                        return ControlFlow::Break(());
                    }
                    let flow = f(chunk, pos);
                    pos.base += chunk.len();
                    pos.lines_before += chunk.bytes().filter(|&b| b == b'\n').count() as u64;
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
