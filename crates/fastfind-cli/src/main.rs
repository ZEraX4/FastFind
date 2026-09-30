//! `fastfind` command line.
//!
//! ```text
//! fastfind index ./docs                      index folders (incremental) and wait
//! fastfind search "annual report" --limit 20 query the index
//! fastfind status                            index statistics
//! fastfind problems                          files that could not be fully indexed
//! fastfind gen-data ./test-data --files 100000
//! fastfind benchmark ./test-data             reproducible performance measurements
//! ```

mod bench;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use fastfind_core::config::AppPaths;
use fastfind_core::gen::{self, GenOptions, Profile};
use fastfind_core::model::{SearchMode, SearchRequest, SortOrder};
use fastfind_core::util::CancelToken;
use fastfind_core::{Engine, EngineOptions};

#[derive(Parser)]
#[command(name = "fastfind-cli", version, about = "Fast local full-text file search")]
struct Cli {
    /// Data directory (index, catalog, settings). Defaults to the app's data directory.
    #[arg(long, global = true, env = "FASTFIND_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Log to stderr as well as the log file.
    #[arg(long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Smart,
    Exact,
    Regex,
    Filename,
}

#[derive(Clone, Copy, ValueEnum)]
enum Sort {
    Relevance,
    Modified,
    Size,
    Name,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add folders to the index (if needed) and index them incrementally.
    Index {
        dirs: Vec<PathBuf>,
        /// Keep running and follow file changes until Ctrl+C.
        #[arg(long)]
        watch: bool,
    },
    /// Search the index.
    Search {
        query: String,
        #[arg(long, value_enum, default_value = "smart")]
        mode: Mode,
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long)]
        whole_word: bool,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long, value_enum, default_value = "relevance")]
        sort: Sort,
        /// Print snippets under each result.
        #[arg(long)]
        snippets: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show index statistics.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List files that were skipped, failed, encrypted or need OCR.
    Problems {
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u64,
    },
    /// Remove a folder from the index.
    Remove { dir: PathBuf },
    /// Generate deterministic sample data.
    GenData {
        dir: PathBuf,
        #[arg(long, default_value_t = 10_000)]
        files: usize,
        /// mixed | small | large-text | office | pdf
        #[arg(long, default_value = "mixed")]
        profile: String,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Size of each file for the large-text profile.
        #[arg(long, default_value_t = 100)]
        large_mb: u64,
    },
    /// Index a directory into a fresh temporary index and measure throughput and latency.
    Benchmark {
        dir: PathBuf,
        /// Timed iterations per query.
        #[arg(long, default_value_t = 30)]
        iterations: usize,
        /// Write machine-readable results here.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Keep the benchmark index directory instead of deleting it.
        #[arg(long)]
        keep: bool,
    },
}

fn human_bytes(b: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < units.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{b} B") } else { format!("{v:.1} {}", units[u]) }
}

/// PDFium is looked up next to the executable (bundled builds) and in `FASTFIND_PDFIUM_DIR`.
pub fn pdfium_dirs() -> Vec<PathBuf> {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| vec![d.to_path_buf(), d.join("pdfium")])).unwrap_or_default()
}

fn paths(cli: &Cli) -> AppPaths {
    match &cli.data_dir {
        Some(d) => AppPaths::new(d.clone()),
        None => AppPaths::default_location(),
    }
}

fn open(cli: &Cli, watch: bool, scan: bool) -> Result<std::sync::Arc<Engine>> {
    let p = paths(cli);
    Engine::open(p, EngineOptions { pdfium_dirs: pdfium_dirs(), watch, scan_on_start: scan, pdf_worker_exe: std::env::current_exe().ok() }).context("opening index (is the FastFind app running with the same data directory?)")
}

fn main() -> Result<()> {
    // Helper-process mode for out-of-process PDF extraction (see pdfworker).
    if std::env::args().any(|a| a == fastfind_core::parsers::pdfworker::PDF_WORKER_ARG) {
        fastfind_core::parsers::pdfworker::run_worker(&pdfium_dirs());
    }
    let cli = Cli::parse();
    let p = paths(&cli);
    let _guard = fastfind_core::logging::init(&p.log_dir, cli.verbose);
    match &cli.cmd {
        Cmd::Index { dirs, watch } => {
            let e = open(&cli, *watch, true)?;
            let roots = e.roots()?;
            for d in dirs {
                let canon = fastfind_core::engine::simplify_path(std::fs::canonicalize(d).with_context(|| format!("{}", d.display()))?);
                if !roots.iter().any(|r| std::path::Path::new(&r.path) == canon) {
                    let r = e.add_root(&canon.to_string_lossy())?;
                    println!("added {}", r.path);
                }
            }
            let start = std::time::Instant::now();
            loop {
                if e.index.is_idle() && e.wait_idle(Duration::from_millis(10)) {
                    break;
                }
                let pr = e.index.progress();
                eprint!("\r{:>9} processed  {:>7.0} files/s  {:>9} queued   ", pr.processed, pr.files_per_sec, pr.queued);
                std::thread::sleep(Duration::from_millis(500));
            }
            let st = e.status()?;
            eprintln!();
            println!(
                "indexed {} files ({} content, {} name-only, {} skipped, {} failed, {} encrypted, {} need OCR) in {:.1}s — index {}",
                st.files_total, st.indexed, st.name_only, st.skipped, st.failed, st.encrypted, st.needs_ocr,
                start.elapsed().as_secs_f64(), human_bytes(st.index_bytes + st.text_store_bytes)
            );
            if *watch {
                println!("watching for changes — press Ctrl+C to stop");
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            e.shutdown();
        }
        Cmd::Search { query, mode, case_sensitive, whole_word, limit, sort, snippets, json } => {
            let e = open(&cli, false, false)?;
            let req = SearchRequest {
                query: query.clone(),
                mode: match mode {
                    Mode::Smart => SearchMode::Smart,
                    Mode::Exact => SearchMode::Exact,
                    Mode::Regex => SearchMode::Regex,
                    Mode::Filename => SearchMode::Filename,
                },
                case_sensitive: *case_sensitive,
                whole_word: *whole_word,
                sort: match sort {
                    Sort::Relevance => SortOrder::Relevance,
                    Sort::Modified => SortOrder::Modified,
                    Sort::Size => SortOrder::Size,
                    Sort::Name => SortOrder::Name,
                },
                limit: *limit,
                ..Default::default()
            };
            let r = e.search(&req, &CancelToken::new())?;
            let snips = if *snippets || *json {
                e.snippets(&req, &r.items.iter().map(|i| i.path.clone()).collect::<Vec<_>>())?
            } else {
                vec![]
            };
            if *json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "response": r, "snippets": snips }))?);
            } else {
                println!("{}{} results in {:.1} ms", r.total, if r.total_is_lower_bound { "+" } else { "" }, r.elapsed_ms);
                for w in &r.warnings {
                    println!("note: {w}");
                }
                for (i, it) in r.items.iter().enumerate() {
                    println!("{:>3}. {}  ({}, {})", i + 1, it.path, it.kind, human_bytes(it.size));
                    if let Some(s) = snips.get(i) {
                        for sn in &s.snippets {
                            println!("       {}{}", sn.location.as_ref().map(|l| format!("[{l}] ")).unwrap_or_default(), sn.text);
                        }
                    }
                }
            }
            e.shutdown();
        }
        Cmd::Status { json } => {
            let e = open(&cli, false, false)?;
            let st = e.status()?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!("Indexed files : {}", st.files_total);
                println!("  content     : {}", st.indexed);
                println!("  name only   : {}", st.name_only);
                println!("  skipped     : {}", st.skipped);
                println!("  failed      : {}", st.failed);
                println!("  encrypted   : {}", st.encrypted);
                println!("  needs OCR   : {}", st.needs_ocr);
                println!("Index size    : {}", human_bytes(st.index_bytes + st.text_store_bytes));
                for r in st.roots {
                    println!("Folder        : {} ({} files, {})", r.path, r.file_count, if r.watch_status.is_empty() { "-" } else { &r.watch_status });
                }
                let d = e.diagnostics();
                println!("PDF engine    : {}", d.pdf_engine);
                println!("OCR engine    : {}", d.ocr_engine.unwrap_or_else(|| "not installed".into()));
                println!("Data dir      : {}", d.data_dir);
            }
            e.shutdown();
        }
        Cmd::Problems { status, limit } => {
            let e = open(&cli, false, false)?;
            let page = e.problems(status.as_deref(), "", 0, *limit)?;
            println!("{} files", page.total);
            for f in page.items {
                println!("{:<10} {}  — {}", f.status, f.path, f.reason.unwrap_or_default());
            }
            e.shutdown();
        }
        Cmd::Remove { dir } => {
            let e = open(&cli, false, false)?;
            let canon = fastfind_core::engine::simplify_path(std::fs::canonicalize(dir)?);
            let Some(r) = e.roots()?.into_iter().find(|r| std::path::Path::new(&r.path) == canon) else {
                bail!("{} is not an indexed folder", dir.display());
            };
            e.remove_root(r.id)?;
            println!("removed {}", r.path);
            e.shutdown();
        }
        Cmd::GenData { dir, files, profile, seed, large_mb } => {
            let profile = Profile::parse(profile).context("profile must be mixed, small, large-text, office or pdf")?;
            let t = std::time::Instant::now();
            let s = gen::generate(dir, &GenOptions { files: *files, profile, seed: *seed, large_mb: *large_mb })?;
            println!(
                "generated {} files ({}) in {:.1}s — needles: invoice {}, customer {}, quarterly {}, zeppelin {}",
                s.files, human_bytes(s.bytes), t.elapsed().as_secs_f64(), s.invoice, s.customer, s.quarterly, s.zeppelin
            );
        }
        Cmd::Benchmark { dir, iterations, json, keep } => {
            bench::run(dir, *iterations, json.as_deref(), *keep)?;
        }
    }
    Ok(())
}
