//! Writes starting inputs for every fuzz target into `fuzz/corpus/<target>/`: the real Office
//! and Outlook fixtures, generated documents, and small hand-made samples. Run it once before
//! fuzzing (`cargo run --bin seed`); libFuzzer then mutates from these.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use fastfind_core::gen::{write_docx, write_pptx, write_xlsx};

fn main() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixtures = here.join("../crates/fastfind-core/tests/fixtures");
    let corpus = here.join("corpus");
    let put = |target: &str, name: &str, data: &[u8]| {
        let dir = corpus.join(target);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), data).unwrap();
    };
    let fixture = |name: &str| fs::read(fixtures.join(name)).unwrap();
    let tmp = std::env::temp_dir().join(format!("fastfind-seed-{}", std::process::id()));
    fs::create_dir_all(&tmp).unwrap();
    let generated = |name: &str, f: &dyn Fn(&Path)| -> Vec<u8> {
        let p: PathBuf = tmp.join(name);
        f(&p);
        fs::read(&p).unwrap()
    };

    // Legacy Office and Outlook: real files saved by Office.
    put("doc", "legacy.doc", &fixture("legacy.doc"));
    put("doc", "protected.doc", &fixture("protected.doc"));
    put("xls", "legacy.xls", &fixture("legacy.xls"));
    put("ppt", "legacy.ppt", &fixture("legacy.ppt"));
    put("msg", "unicode.msg", &fixture("outlook-unicode.msg"));
    put("msg", "ansi.msg", &fixture("outlook-ansi.msg"));

    // OOXML: real and generated.
    put("docx", "modern.docx", &fixture("modern.docx"));
    put("xlsx", "modern.xlsx", &fixture("modern.xlsx"));
    put("pptx", "modern.pptx", &fixture("modern.pptx"));
    put("docx", "gen.docx", &generated("g.docx", &|p| write_docx(p, "T", &["Hello world".into(), "Second".into()]).unwrap()));
    put("xlsx", "gen.xlsx", &generated("g.xlsx", &|p| write_xlsx(p, &[("S".into(), vec![vec!["a".into(), "1".into()]])]).unwrap()));
    put("pptx", "gen.pptx", &generated("g.pptx", &|p| write_pptx(p, &[("Title".into(), vec!["Body".into()])]).unwrap()));

    // ODF and EPUB: minimal packages.
    put("odf", "min.odt", &zip(&[
        ("mimetype", b"application/vnd.oasis.opendocument.text".as_slice()),
        ("content.xml", br#"<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:text><text:h>Title</text:h><text:p>Hello <text:span>world</text:span></text:p></office:text></office:body></office:document-content>"#),
        ("meta.xml", br#"<?xml version="1.0"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><dc:title>T</dc:title></office:meta></office:document-meta>"#),
    ]));
    put("epub", "min.epub", &zip(&[
        ("mimetype", b"application/epub+zip".as_slice()),
        ("META-INF/container.xml", br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/c.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#),
        ("OEBPS/c.opf", br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Book</dc:title></metadata><manifest><item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#),
        ("OEBPS/ch1.xhtml", b"<html><body><h1>Chapter</h1><p>Text</p></body></html>"),
    ]));

    // Archives: documents and a nested archive inside.
    let inner = zip(&[("deep/notes.txt", b"inner text".as_slice())]);
    put("zip", "mixed.zip", &zip(&[
        ("docs/", b"".as_slice()),
        ("docs/a.txt", b"plain text entry"),
        ("docs/b.docx", &fixture("modern.docx")),
        ("nested.zip", &inner),
    ]));

    // Text formats.
    put("rtf", "a.rtf", br"{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Arial;}}{\info{\title T}}\f0 Hello \b world\b0\par caf\'e9 \u8364? {\field{\*\fldinst HYPERLINK x}{\fldrslt link}}}");
    put("html", "a.html", b"<!doctype html><html><head><meta charset=utf-8><title>T</title><script>x</script></head><body><p>Hello <b>world</b></p><table><tr><td>1</td></tr></table></body></html>");
    put("xml", "a.xml", br#"<?xml version="1.0" encoding="UTF-8"?><root a="1"><item>Hello</item><![CDATA[raw]]><!-- c --></root>"#);
    put("json", "a.json", br#"{"name":"Hello","list":[1,2.5,true,null,{"deep":["x"]}],"esc":"\u00e9\n"}"#);
    put("plain", "utf8.txt", "Hello wörld, Geçiş izni, مرحبا\n".as_bytes());
    put("plain", "utf16.txt", &[0xFF, 0xFE, b'H', 0, b'i', 0]);
    put("eml", "a.eml", b"From: =?UTF-8?B?2KPYrdmF2K8=?= <a@example.com>\r\nSubject: =?windows-1254?Q?Ge=E7i=FE?=\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHello =C3=A9\r\n--B\r\nContent-Type: text/plain; name=\"n.txt\"\r\nContent-Disposition: attachment; filename=\"n.txt\"\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--B--\r\n");
    put("query", "a.txt", "\"annual report\" invoic* -draft OR (budget AND 2026) path:docs ext:pdf".as_bytes());
    put("query", "b.txt", "المدرسة كتـــاب Geçiş".as_bytes());

    let _ = fs::remove_dir_all(&tmp);
    println!("seeded {}", corpus.display());
}

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        if name.ends_with('/') {
            w.add_directory(*name, opts).unwrap();
        } else {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}
