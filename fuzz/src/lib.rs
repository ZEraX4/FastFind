//! Shared harness for the fuzz targets: feed arbitrary bytes to one parser, the way the
//! indexer would (from a file, with the default limits and a deadline).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastfind_core::parsers::{DocumentParser, Limits, ParseContext, TextSink};

/// Write `data` to a per-process scratch file with extension `ext` and extract it.
/// Errors are expected for most inputs; only panics, hangs, stack overflows and runaway
/// memory use count as findings.
pub fn parse(parser: &dyn DocumentParser, ext: &str, data: &[u8]) {
    let path = scratch(ext);
    std::fs::write(&path, data).unwrap();
    let limits = Limits { deadline: Some(Instant::now() + Duration::from_secs(10)), ..Limits::default() };
    let mut sink = TextSink::new(limits.max_text_bytes, limits.deadline);
    let _ = parser.extract(&path, &ParseContext { limits: &limits }, &mut sink);
}

fn scratch(ext: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fastfind-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("input.{ext}"))
}
