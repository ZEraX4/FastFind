//! `fastfind benchmark <dir>`: reproducible end-to-end measurements.
//!
//! Uses a fresh temporary data directory so results do not depend on earlier runs. Reports
//! scan and index throughput, index size, startup time, query latency percentiles (with the
//! result cache disabled), snippet latency, peak memory and average CPU.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use fastfind_core::config::{AppPaths, Settings};
use fastfind_core::fs::{scan, PathFilter, ScanOptions};
use fastfind_core::model::{SearchMode, SearchRequest};
use fastfind_core::util::CancelToken;
use fastfind_core::{Engine, EngineOptions};
use serde::Serialize;

#[derive(Serialize, Default)]
struct QueryStats {
    query: String,
    mode: String,
    results: u64,
    avg_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

#[derive(Serialize, Default)]
struct Report {
    files: u64,
    bytes: u64,
    scan_secs: f64,
    scan_files_per_sec: f64,
    index_secs: f64,
    index_files_per_sec: f64,
    index_mb_per_sec: f64,
    index_bytes: u64,
    text_store_bytes: u64,
    startup_ms_empty: f64,
    startup_ms_existing: f64,
    peak_rss_bytes: u64,
    avg_cpu_percent: f32,
    workers: usize,
    pdf_engine: String,
    queries: Vec<QueryStats>,
    snippet_page_ms: f64,
    all_queries_avg_ms: f64,
    all_queries_p95_ms: f64,
    all_queries_p99_ms: f64,
    stages: Option<fastfind_core::index::service::StageStats>,
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Samples RSS and CPU of this process in the background.
struct Sampler {
    stop: Arc<AtomicBool>,
    peak: Arc<Mutex<(u64, f32, u32)>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(Mutex::new((0u64, 0f32, 0u32)));
        let (s, p) = (stop.clone(), peak.clone());
        let handle = std::thread::spawn(move || {
            let pid = sysinfo::get_current_pid().ok();
            let mut sys = sysinfo::System::new();
            while !s.load(Ordering::Relaxed) {
                if let Some(pid) = pid {
                    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
                    if let Some(proc_) = sys.process(pid) {
                        let mut g = p.lock().unwrap();
                        g.0 = g.0.max(proc_.memory());
                        g.1 += proc_.cpu_usage();
                        g.2 += 1;
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        Self { stop, peak, handle: Some(handle) }
    }

    fn finish(mut self) -> (u64, f32) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let g = self.peak.lock().unwrap();
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
        (g.0, if g.2 > 1 { g.1 / (g.2 - 1) as f32 / cores } else { 0.0 })
    }
}

pub fn run(dir: &Path, iterations: usize, json: Option<&Path>, keep: bool) -> Result<()> {
    let tmp = tempfile::Builder::new().prefix("fastfind-bench").tempdir()?;
    let data = tmp.path().to_path_buf();
    let mut rep = Report::default();
    println!("FastFind benchmark\n  corpus : {}\n  index  : {}", dir.display(), data.display());

    // 1. Raw scan throughput (directory walk only).
    let filter = Arc::new(PathFilter::from_settings(&Settings::default().indexing, &data));
    let opts = ScanOptions { filter, threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8), follow_symlinks: false, hot_dirs: Default::default() };
    let t = Instant::now();
    let bytes = std::sync::atomic::AtomicU64::new(0);
    let summary = scan(dir, &opts, &CancelToken::new(), |e| {
        bytes.fetch_add(e.size, Ordering::Relaxed);
    });
    rep.scan_secs = t.elapsed().as_secs_f64();
    rep.files = summary.files;
    rep.bytes = bytes.into_inner();
    rep.scan_files_per_sec = rep.files as f64 / rep.scan_secs.max(1e-9);
    println!("\nScan     : {} files, {:.1} MB in {:.2}s → {:.0} files/s", rep.files, rep.bytes as f64 / 1048576.0, rep.scan_secs, rep.scan_files_per_sec);

    // 2. Full indexing.
    let t = Instant::now();
    let e = Engine::open(AppPaths::new(data.clone()), EngineOptions { pdfium_dirs: crate::pdfium_dirs(), watch: false, scan_on_start: false, pdf_worker_exe: std::env::current_exe().ok() })?;
    rep.startup_ms_empty = t.elapsed().as_secs_f64() * 1000.0;
    rep.workers = e.index.worker_count();
    rep.pdf_engine = e.diagnostics().pdf_engine;
    let sampler = Sampler::start();
    let t = Instant::now();
    e.add_root(&dir.to_string_lossy())?;
    let mut last_print = Instant::now();
    while !e.wait_idle(Duration::from_millis(200)) {
        if last_print.elapsed() > Duration::from_secs(2) {
            let p = e.index.progress();
            eprint!("\r  indexing: {} processed, {:.0} files/s   ", p.processed, p.files_per_sec);
            last_print = Instant::now();
        }
    }
    rep.index_secs = t.elapsed().as_secs_f64();
    let (peak, cpu) = sampler.finish();
    rep.peak_rss_bytes = peak;
    rep.avg_cpu_percent = cpu;
    let st = e.status()?;
    rep.index_bytes = st.index_bytes;
    rep.text_store_bytes = st.text_store_bytes;
    rep.index_files_per_sec = rep.files as f64 / rep.index_secs.max(1e-9);
    rep.index_mb_per_sec = rep.bytes as f64 / 1048576.0 / rep.index_secs.max(1e-9);
    eprintln!();
    println!(
        "Index    : {:.2}s → {:.0} files/s, {:.1} MB/s ({} workers, {})",
        rep.index_secs, rep.index_files_per_sec, rep.index_mb_per_sec, rep.workers, rep.pdf_engine
    );
    println!(
        "           {} content, {} name-only, {} failed · index {:.1} MB + text store {:.1} MB",
        st.indexed, st.name_only, st.failed, st.index_bytes as f64 / 1048576.0, st.text_store_bytes as f64 / 1048576.0
    );
    println!("Resources: peak RSS {:.0} MB, avg CPU {:.0}% of all cores", peak as f64 / 1048576.0, cpu);
    let stages = e.index.stage_stats();
    println!(
        "Stages   : parse {:.1}s (summed over {} workers → {:.1}s wall at full parallelism), writer busy {:.1}s, {} commits ({:.1}s)",
        stages.parse_secs, rep.workers, stages.parse_secs / rep.workers.max(1) as f64, stages.writer_busy_secs, stages.commits, stages.commit_secs
    );
    rep.stages = Some(stages);

    // 3. Query latency (result cache cleared before every run).
    let queries: Vec<(&str, SearchMode)> = vec![
        ("fastfind", SearchMode::Smart),
        ("invoice", SearchMode::Smart),
        ("zeppelin", SearchMode::Smart),
        ("\"total was approved\"", SearchMode::Smart),
        ("invoice AND customer", SearchMode::Smart),
        ("invoice OR quarterly", SearchMode::Smart),
        ("invoice -customer", SearchMode::Smart),
        ("quart", SearchMode::Smart),
        ("invoice ext:pdf", SearchMode::Smart),
        ("customer modified:>2000-01-01", SearchMode::Smart),
        ("filename:file00012", SearchMode::Smart),
        ("file0001", SearchMode::Filename),
        ("The invoice total", SearchMode::Exact),
        ("zeppel[a-z]+ total", SearchMode::Regex),
    ];
    let mut all = Vec::new();
    println!("\n{:<34} {:>8} {:>9} {:>9} {:>9} {:>9}", "query", "results", "avg ms", "p95 ms", "p99 ms", "max ms");
    for (q, mode) in queries {
        let mut req = SearchRequest::new(q);
        req.mode = mode;
        let mut times = Vec::with_capacity(iterations);
        let mut results = 0;
        for _ in 0..iterations.max(1) {
            e.search.clear_caches();
            let t = Instant::now();
            let r = e.search(&req, &CancelToken::new())?;
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            results = r.total;
        }
        all.extend(times.iter().copied());
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let qs = QueryStats {
            query: q.into(),
            mode: format!("{mode:?}").to_lowercase(),
            results,
            avg_ms: times.iter().sum::<f64>() / times.len() as f64,
            p50_ms: pct(&times, 0.5),
            p95_ms: pct(&times, 0.95),
            p99_ms: pct(&times, 0.99),
            max_ms: *times.last().unwrap(),
        };
        println!("{:<34} {:>8} {:>9.2} {:>9.2} {:>9.2} {:>9.2}", format!("{q} [{}]", qs.mode), qs.results, qs.avg_ms, qs.p95_ms, qs.p99_ms, qs.max_ms);
        rep.queries.push(qs);
    }
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    rep.all_queries_avg_ms = all.iter().sum::<f64>() / all.len().max(1) as f64;
    rep.all_queries_p95_ms = pct(&all, 0.95);
    rep.all_queries_p99_ms = pct(&all, 0.99);
    println!("{:<34} {:>8} {:>9.2} {:>9.2} {:>9.2}", "ALL", "", rep.all_queries_avg_ms, rep.all_queries_p95_ms, rep.all_queries_p99_ms);

    // 4. Snippets for a full first page.
    let req = SearchRequest::new("invoice");
    let r = e.search(&req, &CancelToken::new())?;
    let paths: Vec<String> = r.items.iter().map(|i| i.path.clone()).collect();
    e.search.clear_caches();
    let t = Instant::now();
    e.snippets(&req, &paths)?;
    rep.snippet_page_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("\nSnippets : {} results in {:.1} ms", paths.len(), rep.snippet_page_ms);

    // 5. Startup with an existing index.
    e.shutdown();
    drop(e);
    let t = Instant::now();
    let e = Engine::open(AppPaths::new(data.clone()), EngineOptions { pdfium_dirs: crate::pdfium_dirs(), watch: false, scan_on_start: false, pdf_worker_exe: std::env::current_exe().ok() })?;
    rep.startup_ms_existing = t.elapsed().as_secs_f64() * 1000.0;
    println!("Startup  : {:.0} ms (empty index), {:.0} ms (existing index)", rep.startup_ms_empty, rep.startup_ms_existing);
    e.shutdown();
    drop(e);

    if let Some(path) = json {
        std::fs::write(path, serde_json::to_vec_pretty(&rep)?)?;
        println!("\nwrote {}", path.display());
    }
    if keep {
        let kept = tmp.keep();
        println!("index kept at {}", kept.display());
    }
    Ok(())
}
