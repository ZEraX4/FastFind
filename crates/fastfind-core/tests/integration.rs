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
