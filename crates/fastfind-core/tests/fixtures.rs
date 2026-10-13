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

#[test]
fn outlook_unicode_msg() {
    let o = extract("outlook-unicode.msg").unwrap();
    assert_eq!(o.parser, "msg");
    assert_eq!(o.title.as_deref(), Some("Geçiş izni — تقرير الميزانية"));
    for w in [
        "Subject: Geçiş izni — تقرير الميزانية",
        "Ayşe Yılmaz <ayse@example.com>",
        "Cc: team@example.com",
        "Geçiş izni onaylandı. zanzibarquota is the code word.",
        "الميزانية معتمدة للمدرسة",
    ] {
        assert!(o.text.contains(w), "{w}: {}", o.text);
    }
    // Attachments: a Word document, a text file and an attached Outlook item.
    assert_eq!(location_of(&o, "marmalade factory").as_deref(), Some("≈ Page 1 · Attachment: modern.docx"));
    assert_eq!(location_of(&o, "kestrelword").as_deref(), Some("Attachment: notes.txt"));
    assert_eq!(location_of(&o, "ospreyfigure").as_deref(), Some("Attached message: Original request"));
}

#[test]
fn outlook_ansi_msg_with_html_body() {
    let o = extract("outlook-ansi.msg").unwrap();
    assert_eq!(o.parser, "msg");
    assert_eq!(o.title.as_deref(), Some("Budget review"));
    assert!(o.text.contains("Jane Doe <jane@example.com>"), "{}", o.text);
    assert!(o.text.contains("The pelicanmargin figures are final."), "{}", o.text);
}

/// An archive made by Outlook (scripts/make-outlook-pst-fixture.ps1): one result per e-mail.
#[test]
fn outlook_archive_pst() {
    use fastfind_core::parsers::pst::{export_eml, read_archive, ArchivedMessage};
    use mail_parser::MimeHeaders;
    let p = fixture("outlook-archive.pst");
    let mut all: Vec<ArchivedMessage> = Vec::new();
    let summary = read_archive(&p, &Limits::default(), None, &|| true, |m| all.push(m)).unwrap();
    assert_eq!(summary.skipped, 0);
    let find = |subject: &str| all.iter().find(|m| m.subject == subject).unwrap_or_else(|| panic!("{subject:?} not among {:?}", all.iter().map(|m| &m.subject).collect::<Vec<_>>()));
    let at = |m: &ArchivedMessage, needle: &str| {
        let off = m.text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {:?}", m.text));
        describe(&m.locs, &m.text, off, false)
    };

    // Turkish and Arabic text, a Word attachment.
    let m = find("Geçiş izni — تقرير الميزانية");
    assert_eq!(m.folder, "Archive 2025");
    for w in ["Ayşe Yılmaz <ayse@example.com>", "albatrossledger", "الميزانية معتمدة للمدرسة"] {
        assert!(m.text.contains(w), "{w}: {}", m.text);
    }
    assert_eq!(at(m, "marmalade factory").as_deref(), Some("≈ Page 1 · Attachment: modern.docx"));
    assert!(m.time.is_some());

    // HTML body, a text attachment, in a subfolder.
    let m = find("Budget review");
    assert_eq!(m.folder, "Archive 2025 › Projects");
    assert!(m.text.contains("The condorbudget figures are final."), "{}", m.text);
    assert_eq!(at(m, "ibisinventory").as_deref(), Some("Attachment: notes.txt"));
    let eml = export_eml(&p, m.id).unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&eml).unwrap();
    assert_eq!(parsed.subject(), Some("Budget review"));
    assert!(parsed.body_html(0).unwrap().contains("condorbudget"));
    assert_eq!(parsed.attachments().filter_map(|a| a.attachment_name()).collect::<Vec<_>>(), ["notes.txt"]);

    // An attached Outlook item.
    let m = find("Forwarded request");
    assert!(m.text.contains("pelicanmemo"), "{}", m.text);
    assert_eq!(at(m, "ospreyfigure").as_deref(), Some("Attached message: Original request"));

    // Unicode subjects survive the export (RFC 2047).
    let m = find("Geçiş izni — تقرير الميزانية");
    let eml = export_eml(&p, m.id).unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&eml).unwrap();
    assert_eq!(parsed.subject(), Some("Geçiş izni — تقرير الميزانية"));
    assert_eq!(parsed.attachments().count(), 1);
}
