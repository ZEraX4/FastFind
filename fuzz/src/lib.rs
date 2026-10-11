//! Shared harness for the fuzz targets: feed arbitrary bytes to one parser, the way the
//! indexer would (from a file, with the default limits and a deadline).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastfind_core::parsers::{DocumentParser, Limits, ParseContext, TextSink};

/// Write `data` to a per-process scratch file with extension `ext` and extract it.
/// Errors are expected for most inputs; only panics, hangs, stack overflows and runaway
/// memory use count as findings.
pub fn parse(parser: &dyn DocumentParser, ext: &str, data: &[u8]) {
    contain_library_panics();
    let path = scratch(ext);
    std::fs::write(&path, data).unwrap();
    let limits = Limits { deadline: Some(Instant::now() + Duration::from_secs(10)), ..Limits::default() };
    let mut sink = TextSink::new(limits.max_text_bytes, limits.deadline);
    let _ = parser.extract(&path, &ParseContext { limits: &limits }, &mut sink);
}

/// libFuzzer aborts on every panic, even one that is caught. FastFind deliberately catches
/// panics raised inside calamine (see `parsers/xls.rs`), so let those unwind; any other panic
/// still aborts and is reported.
fn contain_library_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let abort = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.location().is_some_and(|l| l.file().contains("calamine")) {
                return;
            }
            abort(info);
        }));
    });
}

fn scratch(ext: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fastfind-fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("input.{ext}"))
}
