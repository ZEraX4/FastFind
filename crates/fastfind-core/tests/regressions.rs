//! Inputs that once crashed a reader or made it allocate without bound (most found by the
//! fuzzers in `fuzz/`). Each file in `tests/regressions/` is parsed by the parser its extension
//! selects; it may fail to parse, but must not panic, and no single allocation may exceed
//! 256 MB (this test binary's allocator enforces that).

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastfind_core::parsers::sniff::read_header;
use fastfind_core::parsers::{Limits, ParseContext, ParserRegistry, TextSink};

struct CappedAlloc;

const MAX_ALLOCATION: usize = 256 << 20;

unsafe impl GlobalAlloc for CappedAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > MAX_ALLOCATION {
            // Panicking inside the allocator is not allowed; report and stop.
            eprintln!("allocation of {} bytes exceeds the regression limit", layout.size());
            std::process::abort();
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: CappedAlloc = CappedAlloc;

#[test]
fn past_crash_inputs_are_handled() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/regressions");
    let reg = ParserRegistry::new();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        let ext = p.extension().unwrap().to_string_lossy().to_lowercase();
        let header = read_header(&p).unwrap();
        let Some(r) = reg.resolve(&p, &ext, &header, true) else { continue };
        let limits = Limits { deadline: Some(Instant::now() + Duration::from_secs(20)), ..Limits::default() };
        let mut sink = TextSink::new(limits.max_text_bytes, limits.deadline);
        let t = Instant::now();
        let result = r.parser.extract(&p, &ParseContext { limits: &limits }, &mut sink);
        eprintln!("{}: {} in {:?}", p.file_name().unwrap().to_string_lossy(), if result.is_ok() { "parsed" } else { "rejected" }, t.elapsed());
        assert!(t.elapsed() < Duration::from_secs(20), "{} took too long", p.display());
        checked += 1;
    }
    assert!(checked >= 2);
}
