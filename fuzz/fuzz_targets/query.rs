#![no_main]
//! Search input is typed by users: the query parser, tokenizer and matchers must accept
//! anything.
use fastfind_core::analysis::{analyze, normalize, words};
use fastfind_core::search::matcher::{build_regex, exact_pattern, Highlighter, PositiveTokenHighlighter};
use fastfind_core::search::query::parse;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let _ = analyze(s);
    for (a, b) in words(s) {
        let _ = normalize(&s[a..b]);
    }
    let _ = build_regex(&exact_pattern(s, true), false);
    let _ = build_regex(s, true);
    if let Ok(Some(ast)) = parse(s) {
        let mut out = Vec::new();
        PositiveTokenHighlighter::new(&ast, false, false).find(s, &mut out, 1000);
    }
});
