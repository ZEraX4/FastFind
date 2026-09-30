//! Out-of-process PDF extraction with the real `fastfind-cli` binary as helper: parallel
//! helpers index many PDFs, and a helper that crashes on one file only fails that file.

use std::path::PathBuf;
use std::time::Duration;

use fastfind_core::config::AppPaths;
use fastfind_core::gen::write_pdf;
use fastfind_core::model::SearchRequest;
use fastfind_core::util::CancelToken;
use fastfind_core::{Engine, EngineOptions};

fn pdfium_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/pdfium")
}

#[test]
fn helper_processes_index_pdfs_and_isolate_crashes() {
    let lib = pdfium_dir().join(if cfg!(windows) { "pdfium.dll" } else if cfg!(target_os = "macos") { "libpdfium.dylib" } else { "libpdfium.so" });
    if !lib.exists() {
        eprintln!("PDFium not present ({}); run scripts/fetch-pdfium first — skipping", lib.display());
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pdfs");
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..40 {
        write_pdf(&root.join(format!("doc{i:02}.pdf")), "T", &[vec![format!("marker{i} common okapi")], vec![format!("second page {i}")]]).unwrap();
    }
    write_pdf(&root.join("poison-crash.pdf"), "T", &[vec!["poison okapi".into()]]).unwrap();
    // The helper aborts (like a PDFium crash) whenever it is asked to parse this file.
    std::env::set_var("FASTFIND_TEST_PDF_CRASH_ON", "poison-crash");

    let engine = Engine::open(
        AppPaths::new(tmp.path().join("data")),
        EngineOptions {
            pdfium_dirs: vec![pdfium_dir()],
            watch: false,
            scan_on_start: false,
            pdf_worker_exe: Some(PathBuf::from(env!("CARGO_BIN_EXE_fastfind-cli"))),
        },
    )
    .unwrap();
    assert!(fastfind_core::parsers::pdfworker::pool().is_some(), "helper pool configured");
    engine.add_root(root.to_str().unwrap()).unwrap();
    assert!(engine.wait_idle(Duration::from_secs(180)));

    let r = engine.search(&SearchRequest::new("okapi"), &CancelToken::new()).unwrap();
    assert_eq!(r.total, 40, "all good PDFs indexed by content despite the crashing helper");
    let r = engine.search(&SearchRequest::new("poison"), &CancelToken::new()).unwrap();
    assert_eq!(r.total, 1, "the crashing file stays findable by name");
    let r = engine.search(&SearchRequest::new("marker37"), &CancelToken::new()).unwrap();
    assert_eq!(r.items.len(), 1);
    let sn = engine.snippets(&SearchRequest::new("second"), &[r.items[0].path.clone()]).unwrap();
    assert_eq!(sn[0].snippets[0].location.as_deref(), Some("Page 2"), "page anchors survive the process boundary");

    let failed = engine.problems(Some("failed"), "poison", 0, 10).unwrap();
    assert_eq!(failed.items.len(), 1, "the crashing file is recorded as failed");
    assert!(failed.items[0].reason.as_deref().unwrap_or("").contains("crashed"));
    engine.shutdown();
}
