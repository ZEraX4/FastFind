//! Parser tests against real files saved by Microsoft Office (see tests/fixtures/README.md).

use std::path::PathBuf;

use fastfind_core::parsers::locmap::describe;
use fastfind_core::parsers::sniff::read_header;
use fastfind_core::parsers::{Limits, Loc, ParseContext, ParseError, ParserRegistry, TextSink};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

struct Out {
    text: String,
    locs: Vec<Loc>,
    title: Option<String>,
    author: Option<String>,
    parser: &'static str,
}

fn extract(name: &str) -> Result<Out, ParseError> {
    let p = fixture(name);
    let reg = ParserRegistry::new();
    let ext = p.extension().unwrap().to_string_lossy().to_lowercase();
    let header = read_header(&p).unwrap();
    let r = reg.resolve(&p, &ext, &header, true).expect("a parser");
    let limits = Limits::default();
    let mut sink = TextSink::new(1 << 24, None);
    let meta = r.parser.extract(&p, &ParseContext { limits: &limits }, &mut sink)?;
    let (text, locs, _) = sink.into_parts();
    Ok(Out { text, locs, title: meta.title, author: meta.author, parser: r.parser.name() })
}

fn location_of(o: &Out, needle: &str) -> Option<String> {
    let off = o.text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {:?}", o.text));
    describe(&o.locs, &o.text, off, false)
}

#[test]
fn legacy_word_doc() {
    let o = extract("legacy.doc").unwrap();
    assert_eq!(o.parser, "doc");
    assert!(o.text.contains("marmalade factory"), "{}", o.text);
    assert!(o.text.contains("quokka habitats"));
    assert!(o.text.contains("tablecell alpha"));
    assert!(o.text.contains("tablecell omega"));
    assert!(o.text.contains("Header zanzibar"), "headers are extracted: {}", o.text);
    assert!(o.text.contains("Footer yellowstone"));
    assert_eq!(location_of(&o, "zanzibar").as_deref(), Some("Headers & footers"));
    assert_eq!(o.title.as_deref(), Some("Marmalade Report"));
    assert_eq!(o.author.as_deref(), Some("Fixture Author"));
}

#[test]
fn modern_word_docx() {
    let o = extract("modern.docx").unwrap();
    assert_eq!(o.parser, "docx");
    for w in ["marmalade factory", "quokka habitats", "tablecell alpha", "Header zanzibar", "Footer yellowstone"] {
        assert!(o.text.contains(w), "{w}: {}", o.text);
    }
    assert_eq!(location_of(&o, "yellowstone").as_deref(), Some("≈ Page 1 · Footer"));
    assert_eq!(o.title.as_deref(), Some("Marmalade Report"));
}

#[test]
fn legacy_excel_xls() {
    let o = extract("legacy.xls").unwrap();
    assert_eq!(o.parser, "xls");
    assert_eq!(location_of(&o, "Flamingo").as_deref(), Some("Budget!B3"));
    assert_eq!(location_of(&o, "4242").as_deref(), Some("Budget!D10"));
    assert_eq!(location_of(&o, "Pangolin").as_deref(), Some("Forecast!C2"));
}

#[test]
fn modern_excel_xlsx() {
    let o = extract("modern.xlsx").unwrap();
    assert_eq!(o.parser, "xlsx");
    assert_eq!(location_of(&o, "Flamingo").as_deref(), Some("Budget!B3"));
    assert_eq!(location_of(&o, "4242").as_deref(), Some("Budget!D10"));
    assert_eq!(location_of(&o, "Pangolin").as_deref(), Some("Forecast!C2"));
}

#[test]
fn legacy_powerpoint_ppt() {
    let o = extract("legacy.ppt").unwrap();
    assert_eq!(o.parser, "ppt");
    assert!(o.text.contains("aardvark programme"), "{}", o.text);
    assert!(o.text.contains("wombat release"));
    assert!(o.text.contains("Hire engineers"));
    assert!(o.text.contains("kiwis"), "speaker notes: {}", o.text);
    assert_eq!(location_of(&o, "aardvark").as_deref(), Some("Slide 1 — Kickoff Meeting"));
    assert_eq!(location_of(&o, "wombat").as_deref(), Some("Slide 2 — Roadmap Plans"));
    assert_eq!(location_of(&o, "kiwis").as_deref(), Some("Slide 2 — Roadmap Plans"));
    assert!(!o.text.contains("Click to edit"), "master placeholders are skipped");
}

#[test]
fn modern_powerpoint_pptx() {
    let o = extract("modern.pptx").unwrap();
    assert_eq!(o.parser, "pptx");
    assert_eq!(location_of(&o, "aardvark").as_deref(), Some("Slide 1 — Kickoff Meeting"));
    assert_eq!(location_of(&o, "wombat").as_deref(), Some("Slide 2 — Roadmap Plans"));
    assert!(o.text.contains("kiwis"), "speaker notes: {}", o.text);
}

#[test]
fn password_protected_files_are_reported_as_encrypted() {
    for f in ["protected.docx", "protected.doc", "protected.xls"] {
        match extract(f) {
            Err(ParseError::Encrypted) => {}
            other => panic!("{f}: expected Encrypted, got {:?}", other.map(|o| o.text)),
        }
    }
}
