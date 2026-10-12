//! End-to-end: directory → scanner → parsers → index → query → results, plus incremental
//! updates, restarts, error handling and file watching.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fastfind_core::config::AppPaths;
use fastfind_core::gen::{write_docx, write_pdf, write_pptx, write_xlsx};
use fastfind_core::model::{flags, SearchFilters, SearchMode, SearchRequest, SortOrder};
use fastfind_core::util::CancelToken;
use fastfind_core::{Engine, EngineOptions};

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("docs");
    let data = tmp.path().join("data");
    fs::create_dir_all(root.join("reports/2026")).unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    fs::create_dir_all(root.join(".secret")).unwrap();

    fs::write(root.join("notes/meeting.txt"), "Meeting notes\nThe annual report is due Friday.\nInvoice INV-2024 was sent to the customer.\n").unwrap();
    fs::write(root.join("notes/Rust.md"), "# Rust notes\n\nRust ownership and tokio async runtime.\nCafé au lait.\n").unwrap();
    fs::write(root.join("notes/data.json"), r#"{"customer":"Acme Corp","total":1234.5,"status":"paid"}"#).unwrap();
    fs::write(root.join("notes/page.html"), "<html><head><title>Status Page</title><script>var hidden='zebra';</script></head><body><p>All systems operational</p></body></html>").unwrap();
    fs::write(root.join("notes/prices.csv"), "item,price\nwidget,10\ngadget,20\n").unwrap();
    fs::write(root.join("notes/legacy.rtf"), r"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\f0 Legacy rich text about gondolas\par}").unwrap();
    fs::write(root.join("node_modules/pkg/index.js"), "module.exports = 'invoice';").unwrap();
    fs::write(root.join(".secret/hidden.txt"), "invoice hidden").unwrap();
    fs::write(root.join("notes/binary.bin"), [0u8, 1, 2, 3, 0, 0, 255, 254]).unwrap();
    fs::write(root.join("notes/broken.docx"), b"PK\x03\x04 this is not really a zip archive").unwrap();

    write_docx(&root.join("reports/2026/quarterly.docx"), "Quarterly Review", &[
        "Quarterly review of the northern region.".into(),
        "Revenue grew by twelve percent year over year.".into(),
    ]).unwrap();
    write_xlsx(&root.join("reports/2026/budget.xlsx"), &[
        ("Budget".into(), vec![vec!["Item".into(), "Amount".into()], vec!["Rent".into(), "1200".into()], vec!["Catering".into(), "Pelican".into()]]),
    ]).unwrap();
    write_pptx(&root.join("reports/2026/deck.pptx"), &[
        ("Intro".into(), vec!["Welcome everyone".into()]),
        ("Roadmap".into(), vec!["Launch the albatross feature".into()]),
    ]).unwrap();
    write_pdf(&root.join("reports/manual.pdf"), "User Manual", &[
        vec!["Chapter one introduction".into()],
        vec!["Troubleshooting the flux capacitor".into()],
    ]).unwrap();
    Fixture { root, data, _tmp: tmp }
}

fn open(data: &Path, watch: bool) -> Arc<Engine> {
    Engine::open(AppPaths::new(data.to_path_buf()), EngineOptions { pdfium_dirs: vec![], watch, scan_on_start: !watch, pdf_worker_exe: None }).unwrap()
}

fn search(e: &Engine, q: &str) -> Vec<String> {
    search_req(e, SearchRequest::new(q))
}

fn search_req(e: &Engine, req: SearchRequest) -> Vec<String> {
    let r = e.search(&req, &CancelToken::new()).unwrap();
    let mut names: Vec<String> = r.items.into_iter().map(|i| i.name).collect();
    names.sort();
    names
}

fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn end_to_end_indexing_and_search() {
    let fx = fixture();
    let e = open(&fx.data, false);
    e.add_root(fx.root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)), "indexing did not finish");

    // --- basic, phrase, boolean -------------------------------------------------------------
    assert_eq!(search(&e, "invoice"), ["meeting.txt"], "node_modules and hidden dirs are excluded");
    assert_eq!(search(&e, "\"annual report\""), ["meeting.txt"]);
    assert_eq!(search(&e, "customer"), ["data.json", "meeting.txt"]);
    assert_eq!(search(&e, "customer AND acme"), ["data.json"]);
    assert_eq!(search(&e, "gondolas OR albatross"), ["deck.pptx", "legacy.rtf"]);
    assert_eq!(search(&e, "customer -acme"), ["meeting.txt"]);
    assert_eq!(search(&e, "cafe"), ["Rust.md"], "diacritics are folded");
    assert!(search(&e, "zebra").is_empty(), "script contents are not indexed");

    // --- formats ------------------------------------------------------------------------------
    assert_eq!(search(&e, "twelve percent"), ["quarterly.docx"]);
    assert_eq!(search(&e, "pelican"), ["budget.xlsx"]);
    assert_eq!(search(&e, "albatross"), ["deck.pptx"]);
    assert_eq!(search(&e, "capacitor"), ["manual.pdf"]);
    assert_eq!(search(&e, "operational"), ["page.html"]);

    // --- prefix / whole word ----------------------------------------------------------------------
    assert_eq!(search(&e, "gondol"), ["legacy.rtf"], "word beginnings match by default");
    let mut ww = SearchRequest::new("gondol");
    ww.whole_word = true;
    assert!(search_req(&e, ww).is_empty());

    // --- field filters ------------------------------------------------------------------------------
    assert_eq!(search(&e, "filename:quarterly"), ["quarterly.docx"]);
    assert_eq!(search(&e, "ext:pdf"), ["manual.pdf"]);
    assert_eq!(search(&e, "type:spreadsheet"), ["budget.xlsx", "prices.csv"]);
    assert_eq!(search(&e, "path:2026 ext:pptx"), ["deck.pptx"]);
    assert_eq!(search(&e, "customer modified:>2000-01-01"), ["data.json", "meeting.txt"]);
    assert!(search(&e, "customer modified:<2000-01-01").is_empty());
    assert_eq!(search(&e, "customer size:<1kb"), ["data.json", "meeting.txt"]);
    let reports = fx.root.join("reports");
    let mut in_dir = SearchRequest::new("review OR albatross OR customer");
    in_dir.filters = SearchFilters { dirs: vec![reports.to_string_lossy().into_owned()], ..Default::default() };
    assert_eq!(search_req(&e, in_dir), ["deck.pptx", "quarterly.docx"]);

    // --- case sensitivity (verified path) -------------------------------------------------------
    let mut cs = SearchRequest::new("Rust");
    cs.case_sensitive = true;
    assert_eq!(search_req(&e, cs), ["Rust.md"]);
    let mut cs = SearchRequest::new("rust");
    cs.case_sensitive = true;
    assert!(search_req(&e, cs).is_empty());

    // --- exact / regex / filename modes ---------------------------------------------------------
    let mut ex = SearchRequest::new("INV-2024 was");
    ex.mode = SearchMode::Exact;
    assert_eq!(search_req(&e, ex), ["meeting.txt"]);
    let mut ex = SearchRequest::new("INV 2024");
    ex.mode = SearchMode::Exact;
    assert!(search_req(&e, ex).is_empty(), "exact mode respects punctuation");
    let mut re = SearchRequest::new(r"INV-\d{4}");
    re.mode = SearchMode::Regex;
    let resp = e.search(&re, &CancelToken::new()).unwrap();
    assert_eq!(resp.items.len(), 1);
    let mut bad = SearchRequest::new("(unclosed");
    bad.mode = SearchMode::Regex;
    assert!(e.search(&bad, &CancelToken::new()).is_err());
    let mut fname = SearchRequest::new("quart");
    fname.mode = SearchMode::Filename;
    assert_eq!(search_req(&e, fname), ["quarterly.docx"]);
    let mut fname = SearchRequest::new("*.pptx");
    fname.mode = SearchMode::Filename;
    assert_eq!(search_req(&e, fname), ["deck.pptx"]);

    // --- sorting & paging --------------------------------------------------------------------------
    let mut sorted = SearchRequest::new("ext:txt,md,json,csv,html");
    sorted.sort = SortOrder::Name;
    sorted.limit = 2;
    let page1 = e.search(&sorted, &CancelToken::new()).unwrap();
    assert_eq!(page1.total, 5);
    assert_eq!(page1.items.len(), 2);
    sorted.offset = 2;
    let page2 = e.search(&sorted, &CancelToken::new()).unwrap();
    assert!(page2.items.iter().all(|i| !page1.items.iter().any(|j| j.path == i.path)));

    // --- snippets with locations -------------------------------------------------------------------
    let find_path = |name: &str| e.search(&SearchRequest::new(format!("filename:{name}")), &CancelToken::new()).unwrap().items[0].path.clone();
    let sn = e.snippets(&SearchRequest::new("pelican"), &[find_path("budget.xlsx")]).unwrap();
    assert_eq!(sn[0].match_count, 1);
    assert_eq!(sn[0].snippets[0].location.as_deref(), Some("Budget!B3"));
    let sn = e.snippets(&SearchRequest::new("albatross"), &[find_path("deck.pptx")]).unwrap();
    assert_eq!(sn[0].snippets[0].location.as_deref(), Some("Slide 2 — Roadmap"));
    let sn = e.snippets(&SearchRequest::new("capacitor"), &[find_path("manual.pdf")]).unwrap();
    assert_eq!(sn[0].snippets[0].location.as_deref(), Some("Page 2"));
    let sn = e.snippets(&SearchRequest::new("customer"), &[find_path("meeting.txt")]).unwrap();
    let s = &sn[0].snippets[0];
    assert_eq!(s.location.as_deref(), Some("Line 3"));
    let hl: String = s.text.encode_utf16().collect::<Vec<u16>>()[s.highlights[0][0] as usize..s.highlights[0][1] as usize]
        .iter()
        .map(|&u| char::from_u32(u as u32).unwrap())
        .collect();
    assert_eq!(hl, "customer");

    // --- preview ---------------------------------------------------------------------------------------
    let p = e.preview(&SearchRequest::new("review"), &find_path("quarterly.docx")).unwrap();
    assert_eq!(p.title.as_deref(), Some("Quarterly Review"));
    assert!(p.total_matches >= 1);
    assert!(p.sections[0].text.contains("review"));

    // --- errors are isolated and reported ------------------------------------------------------------
    let st = e.status().unwrap();
    assert!(st.failed >= 1, "broken.docx must be recorded as failed");
    let problems = e.problems(None, "broken", 0, 10).unwrap();
    assert_eq!(problems.items.len(), 1);
    assert!(problems.items[0].reason.is_some());
    let broken = e.search(&SearchRequest::new("filename:broken"), &CancelToken::new()).unwrap();
    assert!(broken.items[0].flags & flags::FAILED != 0, "failed files stay findable by name");
    assert_eq!(search(&e, "filename:binary"), ["binary.bin"], "unsupported files are name-only");
    e.shutdown();
}

#[test]
fn incremental_updates_restart_and_renames() {
    let fx = fixture();
    let meeting = fx.root.join("notes/meeting.txt");
    {
        let e = open(&fx.data, false);
        e.add_root(fx.root.to_str().unwrap()).unwrap();
        assert!(e.wait_idle(Duration::from_secs(120)));
        let processed = e.status().unwrap().progress.processed;
        assert!(processed >= 10);

        // Modify, delete, add, rename.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&meeting, "Completely new content mentioning walrus.").unwrap();
        fs::remove_file(fx.root.join("notes/prices.csv")).unwrap();
        fs::write(fx.root.join("notes/new.txt"), "fresh narwhal file").unwrap();
        let big: String = (0..20_000).map(|i| format!("line {i} ocelot\n")).collect();
        fs::write(fx.root.join("notes/big.log"), &big).unwrap();
        e.rescan(None);
        assert!(e.wait_idle(Duration::from_secs(120)));
        assert_eq!(search(&e, "walrus"), ["meeting.txt"]);
        assert!(search(&e, "\"annual report\"").is_empty(), "old content removed");
        assert!(search(&e, "filename:prices").is_empty(), "deleted file removed");
        assert_eq!(search(&e, "narwhal"), ["new.txt"]);
        fs::rename(fx.root.join("notes/big.log"), fx.root.join("reports/moved.log")).unwrap();
        e.rescan(None);
        assert!(e.wait_idle(Duration::from_secs(120)));
        assert_eq!(search(&e, "ocelot"), ["moved.log"], "rename tracked");
        e.shutdown();
    }
    // Restart: nothing changed → nothing re-parsed.
    let e = open(&fx.data, false);
    assert!(e.wait_idle(Duration::from_secs(120)));
    assert_eq!(e.status().unwrap().progress.processed, 0, "unchanged files must not be re-indexed");
    assert_eq!(search(&e, "walrus"), ["meeting.txt"], "index persisted");
    // Touch without content change → content hash avoids re-parse and mtime is updated.
    let f = fs::OpenOptions::new().write(true).open(&meeting).unwrap();
    f.set_modified(std::time::SystemTime::now()).unwrap();
    drop(f);
    e.rescan(None);
    assert!(e.wait_idle(Duration::from_secs(60)));
    assert_eq!(search(&e, "walrus"), ["meeting.txt"]);
    // Removing the root removes its documents.
    let root = e.roots().unwrap()[0].id;
    e.remove_root(root).unwrap();
    assert!(search(&e, "walrus").is_empty());
    assert_eq!(e.status().unwrap().files_total, 0);
}

#[test]
fn malicious_and_corrupt_inputs_are_contained() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("evil");
    fs::create_dir_all(&root).unwrap();
    // Zip bomb: 64 MB of zeros compresses to ~64 KB (ratio ≫ limit).
    {
        let mut z = zip::ZipWriter::new(fs::File::create(root.join("bomb.docx")).unwrap());
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        z.start_file("word/document.xml", opt).unwrap();
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..64 {
            z.write_all(&zeros).unwrap();
        }
        z.finish().unwrap();
    }
    // Truncated PDF, garbage "xlsx", deeply nested JSON and RTF, huge single-line text.
    fs::write(root.join("trunc.pdf"), b"%PDF-1.7\n1 0 obj << /Type /Catalog").unwrap();
    fs::write(root.join("junk.xlsx"), vec![0x50u8; 5000]).unwrap();
    fs::write(root.join("deep.json"), "[".repeat(200_000) + "\"deepword\"").unwrap();
    fs::write(root.join("deep.rtf"), format!("{{\\rtf1 {}x", "{".repeat(100_000))).unwrap();
    fs::write(root.join("ok.txt"), "survivor document").unwrap();
    let e = open(&tmp.path().join("data"), false);
    e.add_root(root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)));
    assert_eq!(search(&e, "survivor"), ["ok.txt"], "bad files never stop indexing");
    assert_eq!(search(&e, "deepword"), ["deep.json"]);
    let st = e.status().unwrap();
    assert!(st.failed >= 3, "bomb, pdf, rtf reported: {st:?}");
    let bomb = e.problems(Some("failed"), "bomb", 0, 10).unwrap();
    assert!(bomb.items[0].reason.as_deref().unwrap_or("").contains("limit"), "{:?}", bomb.items);
}

#[test]
fn file_watcher_picks_up_changes() {
    let fx = fixture();
    let e = open(&fx.data, true);
    e.add_root(fx.root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)));
    fs::write(fx.root.join("notes/live.txt"), "watcher sees the platypus").unwrap();
    assert!(wait_for(Duration::from_secs(20), || search(&e, "platypus") == ["live.txt"]), "created file indexed via watcher");
    fs::remove_file(fx.root.join("notes/live.txt")).unwrap();
    assert!(wait_for(Duration::from_secs(20), || search(&e, "platypus").is_empty()), "deleted file removed via watcher");
    fs::create_dir_all(fx.root.join("notes/newdir")).unwrap();
    fs::write(fx.root.join("notes/newdir/inner.txt"), "nested capybara").unwrap();
    assert!(wait_for(Duration::from_secs(20), || search(&e, "capybara") == ["inner.txt"]), "new directory scanned");
}

#[test]
fn corrupted_index_is_rebuilt_on_start() {
    let fx = fixture();
    {
        let e = open(&fx.data, false);
        e.add_root(fx.root.to_str().unwrap()).unwrap();
        assert!(e.wait_idle(Duration::from_secs(120)));
        e.shutdown();
    }
    // Trash the index metadata.
    fs::write(fx.data.join("index/meta.json"), b"{ not json").unwrap();
    let e = open(&fx.data, false);
    assert!(e.wait_idle(Duration::from_secs(120)));
    assert_eq!(search(&e, "albatross"), ["deck.pptx"], "rebuilt automatically");
    assert!(fx.data.join("quarantine").read_dir().unwrap().count() >= 1, "damaged index kept for diagnosis");
}

/// Folder filters given through another name for an indexed folder (symlink, Windows junction,
/// 8.3 short name, macOS /var → /private/var) still match, because roots are stored resolved.
#[test]
fn folder_filter_resolves_links_to_indexed_folders() {
    let fx = fixture();
    let alias = fx._tmp.path().join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&fx.root, &alias).unwrap();
    #[cfg(windows)]
    {
        // A junction needs no special privileges, unlike a symlink.
        let ok = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&alias).arg(&fx.root).output().unwrap().status.success();
        assert!(ok, "mklink /J failed");
    }
    let e = open(&fx.data, false);
    e.add_root(fx.root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)));

    let via_alias = alias.join("reports").to_string_lossy().into_owned();
    let mut req = SearchRequest::new("review OR albatross OR customer");
    req.filters = SearchFilters { dirs: vec![via_alias.clone()], ..Default::default() };
    assert_eq!(search_req(&e, req), ["deck.pptx", "quarterly.docx"]);
    assert_eq!(search(&e, &format!("albatross in:\"{via_alias}\"")), ["deck.pptx"]);
    e.shutdown();
}

/// Arabic: letter variants, tatweel, digits and the definite article match the way readers
/// expect, in search, phrases, prefixes (search-as-you-type) and highlighting.
#[test]
fn arabic_text_is_found_regardless_of_spelling_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("مستندات");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("تقرير.txt"), "ذهب أحمد إلى المدرسة الكبيرة صباحاً.\nالفاتورة رقم ٢٠٢٦ مدفوعة.\n").unwrap();
    fs::write(root.join("رسالة.txt"), "زار الطالبُ مستشفى المدينة. كتـــاب جديد للمكتبة.\n").unwrap();
    write_docx(&root.join("محضر.docx"), "محضر الاجتماع", &["ناقش المجلس ميزانية الجامعة".into()]).unwrap();
    fs::write(root.join("other.txt"), "plain english text about invoices\n").unwrap();

    let e = open(&tmp.path().join("data"), false);
    e.add_root(root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)));

    // Alef forms, teh marbuta and the article.
    assert_eq!(search(&e, "احمد"), ["تقرير.txt"]);
    assert_eq!(search(&e, "مدرسة"), ["تقرير.txt"], "without the article finds the word with it");
    assert_eq!(search(&e, "المدرسه"), ["تقرير.txt"]);
    assert_eq!(search(&e, "\"مدرسة كبيرة\""), ["تقرير.txt"], "phrases match across article forms");
    assert_eq!(search(&e, "\"المدرسة الكبيرة\""), ["تقرير.txt"]);
    // Search as you type: partial words with the article still match.
    assert_eq!(search(&e, "المد"), ["تقرير.txt", "رسالة.txt"], "المدرسة and المدينة");
    assert_eq!(search(&e, "المدر"), ["تقرير.txt"]);
    assert_eq!(search(&e, "مدر"), ["تقرير.txt"]);
    // Alef maksura, tatweel, harakat, attached prepositions.
    assert_eq!(search(&e, "مستشفي"), ["رسالة.txt"]);
    assert_eq!(search(&e, "كتاب"), ["رسالة.txt"]);
    assert_eq!(search(&e, "الطالب"), ["رسالة.txt"]);
    assert_eq!(search(&e, "مكتبة"), ["رسالة.txt"], "للمكتبة");
    // Digits in either script.
    assert_eq!(search(&e, "2026"), ["تقرير.txt"]);
    assert_eq!(search(&e, "٢٠٢٦"), ["تقرير.txt"]);
    // Office documents and file names go through the same analysis.
    assert_eq!(search(&e, "جامعة"), ["محضر.docx"]);
    assert_eq!(search(&e, "تقرير"), ["تقرير.txt"]);
    // Short words keep the article: الله-style words are not reduced to two letters.
    assert!(search(&e, "له").is_empty());

    // Highlights cover the whole original word, article and all.
    let req = SearchRequest::new("مدرسة");
    let path = e.search(&req, &CancelToken::new()).unwrap().items.remove(0).path;
    let sn = e.snippets(&req, std::slice::from_ref(&path)).unwrap().remove(0);
    let s = &sn.snippets[0];
    let utf16: Vec<u16> = s.text.encode_utf16().collect();
    let [a, b] = s.highlights[0];
    assert_eq!(String::from_utf16(&utf16[a as usize..b as usize]).unwrap(), "المدرسة");

    // Exact mode stays literal.
    let mut ex = SearchRequest::new("المدرسة الكبيرة");
    ex.mode = SearchMode::Exact;
    assert_eq!(search_req(&e, ex), ["تقرير.txt"]);
    let mut ex = SearchRequest::new("مدرسة كبيرة");
    ex.mode = SearchMode::Exact;
    assert!(search_req(&e, ex).is_empty(), "Exact matches characters, not normalised words");
    e.shutdown();
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for (i, chunk) in data.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.push_str("\r\n"); // 76-character lines, as mail clients write them
        }
        let n = chunk.len();
        let v = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            out.push(if k <= n { T[(v >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// A realistic message: quoted-printable body, a Word and a PDF attachment, a text file, a
/// forwarded message and an image. Everything readable becomes searchable, with locations.
#[test]
fn email_bodies_and_attachments_are_searchable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("mail");
    fs::create_dir_all(&root).unwrap();
    let docx = tmp.path().join("a.docx");
    write_docx(&docx, "Budget", &["The kookaburra budget line was approved.".into()]).unwrap();
    let pdf = tmp.path().join("a.pdf");
    write_pdf(&pdf, "Report", &[vec!["First page about penguins.".into()], vec!["Second page mentions the narwhal.".into()]]).unwrap();
    let png = b"\x89PNG\r\n\x1a\nnot really an image but binary";

    let mime = format!(
        "From: =?UTF-8?Q?Ay=C5=9Fe_Y?= <ayse@example.com>\r\n\
To: team@example.com\r\n\
Subject: =?UTF-8?Q?Quarterly_numbers_=E2=80=93_flamingo?=\r\n\
Date: Mon, 5 Oct 2026 09:30:00 +0300\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"OUTER\"\r\n\r\n\
--OUTER\r\n\
Content-Type: multipart/alternative; boundary=\"ALT\"\r\n\r\n\
--ALT\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\r\n\
Please review the axolotl figures before Friday. Te=C5=9Fekk=C3=BCrler.\r\n\
--ALT\r\n\
Content-Type: text/html; charset=utf-8\r\n\r\n\
<p>Please review the <b>axolotl</b> figures before Friday.</p>\r\n\
--ALT--\r\n\
--OUTER\r\n\
Content-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document; name=\"budget.docx\"\r\n\
Content-Disposition: attachment; filename=\"budget.docx\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\n{}\r\n\
--OUTER\r\n\
Content-Type: application/pdf; name=\"report.pdf\"\r\n\
Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\n{}\r\n\
--OUTER\r\n\
Content-Type: text/plain; charset=utf-8; name=\"notes.txt\"\r\n\
Content-Disposition: attachment; filename=\"notes.txt\"\r\n\r\n\
Reminder about the quokka workshop.\r\n\
--OUTER\r\n\
Content-Type: message/rfc822\r\n\
Content-Disposition: attachment; filename=\"forwarded.eml\"\r\n\r\n\
From: boss@example.com\r\n\
Subject: Original request\r\n\
Content-Type: text/plain\r\n\r\n\
Can you check the pangolin numbers?\r\n\
--OUTER\r\n\
Content-Type: image/png; name=\"logo.png\"\r\n\
Content-Disposition: attachment; filename=\"logo.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\n{}\r\n\
--OUTER--\r\n",
        b64(&fs::read(&docx).unwrap()),
        b64(&fs::read(&pdf).unwrap()),
        b64(png),
    );
    fs::write(root.join("quarterly.eml"), mime).unwrap();
    fs::write(root.join("broken.eml"), b"\x00\x01 not a message \xff\xfe").unwrap();

    let e = open(&tmp.path().join("data"), false);
    e.add_root(root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)));

    for word in ["axolotl", "flamingo", "ayse", "kookaburra", "narwhal", "quokka", "pangolin", "tesekkurler"] {
        assert_eq!(search(&e, word), ["quarterly.eml"], "{word}");
    }
    let item = e.search(&SearchRequest::new("flamingo"), &CancelToken::new()).unwrap().items.remove(0);
    assert_eq!(item.kind, "text");

    // Locations name the attachment, and keep the attachment's own pages.
    let loc = |q: &str| {
        let sn = e.snippets(&SearchRequest::new(q), std::slice::from_ref(&item.path)).unwrap().remove(0);
        sn.snippets[0].location.clone()
    };
    assert_eq!(loc("kookaburra").as_deref(), Some("≈ Page 1 · Attachment: budget.docx"));
    assert_eq!(loc("narwhal").as_deref(), Some("Page 2 · Attachment: report.pdf"));
    assert_eq!(loc("quokka").as_deref(), Some("Attachment: notes.txt"));
    assert_eq!(loc("pangolin").as_deref(), Some("Attached message: Original request"));
    assert_eq!(loc("axolotl"), None, "the message body has no location");

    // Title and author come from the headers; a damaged file does not stop indexing.
    let pv = e.preview(&SearchRequest::new("axolotl"), &item.path).unwrap();
    assert_eq!(pv.title.as_deref(), Some("Quarterly numbers – flamingo"));
    assert_eq!(pv.author.as_deref(), Some("Ayşe Y"));
    assert_eq!(e.status().unwrap().failed, 0);
    e.shutdown();
}

#[test]
fn an_older_index_format_is_rebuilt_from_stored_text_without_reparsing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("docs");
    fs::create_dir_all(&root).unwrap();
    write_docx(&root.join("report.docx"), "Annual", &["The kookaburra census was completed.".into()]).unwrap();
    fs::write(root.join("notes.txt"), "Field notes about the platypus.\n").unwrap();
    fs::write(root.join("page.html"), "<html><body><p>The quoll survey.</p></body></html>").unwrap();
    fs::write(root.join("blob.bin"), [0u8, 1, 2, 3, 255]).unwrap();
    let data = tmp.path().join("data");
    let paths = AppPaths::new(data.clone());
    let e = open(&data, false);
    e.add_root(root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(60)));
    assert_eq!(search(&e, "kookaburra"), ["report.docx"]);
    e.shutdown();
    drop(e);

    // Pretend an older version wrote the index, and that an earlier upgrade was interrupted.
    let version_file = paths.index_dir.join("fastfind-schema-version");
    fs::write(&version_file, (fastfind_core::index::schema::SCHEMA_VERSION - 1).to_string()).unwrap();
    fs::create_dir_all(paths.index_dir.with_extension("upgrade").join("junk")).unwrap();
    assert!(fastfind_core::index::IndexStore::upgrade_pending(&paths.index_dir), "the app shows its progress window");
    // Wreck the Word file without changing its size or date: parsing it again would fail.
    let docx = root.join("report.docx");
    let modified = fs::metadata(&docx).unwrap().modified().unwrap();
    let size = fs::metadata(&docx).unwrap().len() as usize;
    fs::write(&docx, vec![0u8; size]).unwrap();
    fs::File::options().write(true).open(&docx).unwrap().set_modified(modified).unwrap();

    let e = Engine::open(paths.clone(), EngineOptions { pdfium_dirs: vec![], watch: false, scan_on_start: false, pdf_worker_exe: None }).unwrap();
    assert_eq!(fs::read_to_string(&version_file).unwrap().trim(), fastfind_core::index::schema::SCHEMA_VERSION.to_string());
    assert!(!paths.index_dir.with_extension("upgrade").exists(), "leftovers of an interrupted upgrade are removed");
    assert!(!fastfind_core::index::IndexStore::upgrade_pending(&paths.index_dir));
    // Before any scan: the Word document is back from its stored text, names are searchable.
    assert_eq!(search(&e, "kookaburra"), ["report.docx"]);
    assert_eq!(search(&e, "blob"), ["blob.bin"]);
    // Plain text and HTML are not stored; the scan reads them again.
    assert!(search(&e, "platypus").is_empty());
    e.rescan(None);
    assert!(wait_for(Duration::from_secs(60), || search(&e, "platypus") == ["notes.txt"] && search(&e, "quoll") == ["page.html"]));
    assert_eq!(search(&e, "kookaburra"), ["report.docx"], "not parsed again");
    assert_eq!(e.status().unwrap().failed, 0);
    e.shutdown();
}

#[test]
fn zip_archives_are_searchable_by_entry_name_and_content() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("archives");
    fs::create_dir_all(&root).unwrap();
    let docx = tmp.path().join("a.docx");
    write_docx(&docx, "Budget", &["The cassowary budget line was approved.".into()]).unwrap();
    let pdf = tmp.path().join("a.pdf");
    write_pdf(&pdf, "Report", &[vec!["First page.".into()], vec!["Second page mentions the dugong.".into()]]).unwrap();
    let eml = "From: a@example.com\r\nSubject: Hello\r\nContent-Type: text/plain\r\n\r\nThe tapir meeting moved.\r\n";

    let mut w = zip::ZipWriter::new(fs::File::create(root.join("bundle.zip")).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    for (name, data) in [
        ("reports/budget.docx", fs::read(&docx).unwrap()),
        ("reports/annual.pdf", fs::read(&pdf).unwrap()),
        ("mail/hello.eml", eml.as_bytes().to_vec()),
        ("scans/okapi-receipt.jpg", b"\xff\xd8\xff\xe0 binary".to_vec()),
    ] {
        w.start_file(name, opts).unwrap();
        w.write_all(&data).unwrap();
    }
    w.finish().unwrap();
    fs::write(root.join("broken.zip"), b"PK\x03\x04 truncated").unwrap();

    let e = open(&tmp.path().join("data"), false);
    e.add_root(root.to_str().unwrap()).unwrap();
    assert!(e.wait_idle(Duration::from_secs(120)));

    for word in ["cassowary", "dugong", "tapir", "okapi"] {
        assert_eq!(search(&e, word), ["bundle.zip"], "{word}");
    }
    let path = e.search(&SearchRequest::new("dugong"), &CancelToken::new()).unwrap().items.remove(0).path;
    let loc = |q: &str| e.snippets(&SearchRequest::new(q), std::slice::from_ref(&path)).unwrap().remove(0).snippets[0].location.clone();
    assert_eq!(loc("dugong").as_deref(), Some("Page 2 · In archive: reports/annual.pdf"));
    assert_eq!(loc("cassowary").as_deref(), Some("≈ Page 1 · In archive: reports/budget.docx"));
    assert_eq!(loc("tapir").as_deref(), Some("In archive: mail/hello.eml"));
    assert_eq!(loc("okapi").as_deref(), Some("In archive: scans/okapi-receipt.jpg"));
    assert_eq!(e.status().unwrap().failed, 1, "the damaged archive fails on its own");
    e.shutdown();
}
