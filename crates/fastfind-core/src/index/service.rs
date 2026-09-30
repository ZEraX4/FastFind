//! The indexing pipeline.
//!
//! ```text
//!  scan requests ─► coordinator (one scan at a time; user-added roots jump the queue)
//!                     │ parallel DirectoryScanner (prunes exclusions)
//!                     ▼
//!                  differ (catalog snapshot: path-hash → size/mtime/hash)
//!                     │ new / modified files only        deletions + renames after the scan
//!          ┌──────────┼──────────────┐
//!          ▼          ▼              ▼
//!   small lane    large lane     pdf lane          (bounded channels = back-pressure)
//!          └────┬─────┘              │
//!        parser worker pool     PDF threads (1 per PDFium helper process)
//!               └──────────┬─────────┘
//!                          ▼
//!                  single writer thread ── Tantivy commit → text store → catalog
//! ```
//!
//! Small files are always preferred (one worker is biased to the large lane so big files
//! still make progress). Commits happen every few seconds while indexing, so search sees
//! newly indexed files long before a scan completes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, select, Receiver, Sender};
use parking_lot::{Condvar, Mutex, RwLock};
use tantivy::schema::Value;
use tantivy::{TantivyDocument, Term};

use super::catalog::{CatOp, Catalog, FileRecord, Status};
use super::schema::Fields;
use super::store::IndexStore;
use super::textstore::{self, TextOp, TextStore};
use crate::config::{BackgroundMode, CpuPreference, Settings};
use crate::fs::watcher::Change;
use crate::fs::{scan, FileEntry, PathFilter, ScanOptions};
use crate::model::{flags, IndexProgress};
use crate::parsers::registry::kind_for_ext;
use crate::parsers::sniff::read_header;
use crate::parsers::{DocMeta, Kind, Limits, ParseContext, ParseError, ParserRegistry, TextMode, TextSink};
use crate::util::{self, extension_of, filter_form, lower_thread_priority, path_hash, path_str, CancelToken};

/// Files at or above this size go to the large lane.
const LARGE_FILE: u64 = 2 << 20;
/// Only files this large are held back for rename detection (parsing small files again is
/// cheaper than delaying them).
const MOVE_MIN_SIZE: u64 = 64 * 1024;
/// Files above this size are not content-hashed.
const HASH_MAX_SIZE: u64 = 256 << 20;
/// Writer queue length (documents in flight between workers and the writer).
const WRITER_QUEUE: usize = 1024;
/// Pending compressed text is written to the text store in chunks of about this size rather
/// than held until the next index commit (only the catalog must be committed last).
const TEXT_FLUSH_BYTES: usize = 32 << 20;
/// Commit at least every this many documents.
const COMMIT_DOCS: u64 = 20_000;

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub root: i64,
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
    pub prev_hash: Option<u64>,
    pub prev_size: Option<u64>,
    /// Rename/move source: reuse its stored text instead of re-parsing when possible.
    pub reuse_from: Option<String>,
}

pub enum TextAction {
    Put(Vec<u8>),
    Delete,
}

pub struct Processed {
    pub record: FileRecord,
    pub doc: Option<TantivyDocument>,
    pub text: TextAction,
    pub delete_old: Option<String>,
}

enum WriterMsg {
    Doc(Box<Processed>),
    Delete { path: String },
    DeleteUnder { dir: String },
    DeleteRoot { root_id: i64, ack: Sender<()> },
    Touch { path: String, size: u64, mtime_ns: i64 },
    Commit { ack: Option<Sender<()>> },
    ClearAll { ack: Sender<()> },
    Shutdown,
}

#[derive(Debug, Clone)]
struct ScanReq {
    root: i64,
    subdir: Option<PathBuf>,
}

#[derive(Default)]
pub struct Progress {
    pub discovered: AtomicU64,
    pub dispatched: AtomicU64,
    pub processed: AtomicU64,
    pub bytes: AtomicU64,
    pub writer_sent: AtomicU64,
    pub writer_applied: AtomicU64,
    pub writer_dirty: AtomicBool,
    pub scanning: AtomicUsize,
    pub current: Mutex<Option<String>>,
    session: Mutex<Option<(Instant, u64)>>,
    /// Stage timers (nanoseconds, summed over threads) for profiling / diagnostics.
    pub parse_ns: AtomicU64,
    pub writer_busy_ns: AtomicU64,
    pub commit_ns: AtomicU64,
    pub commits: AtomicU64,
}

/// Where indexing time went (summed across threads).
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageStats {
    pub parse_secs: f64,
    pub writer_busy_secs: f64,
    pub commit_secs: f64,
    pub commits: u64,
}

type Listener = Arc<dyn Fn(u64) + Send + Sync>;

pub struct Inner {
    pub settings: RwLock<Settings>,
    pub filter: RwLock<Arc<PathFilter>>,
    pub catalog: Arc<Catalog>,
    pub store: Arc<IndexStore>,
    pub texts: Arc<TextStore>,
    pub registry: Arc<ParserRegistry>,
    pub progress: Progress,
    small: (Sender<WorkItem>, Receiver<WorkItem>),
    large: (Sender<WorkItem>, Receiver<WorkItem>),
    pdf: (Sender<WorkItem>, Receiver<WorkItem>),
    writer_tx: Sender<WriterMsg>,
    paused: AtomicBool,
    shutdown: AtomicBool,
    target_workers: AtomicUsize,
    pub max_workers: usize,
    scan_queue: Mutex<VecDeque<ScanReq>>,
    scan_cv: Condvar,
    active_scan: Mutex<Option<(i64, CancelToken)>>,
    roots: RwLock<HashMap<i64, PathBuf>>,
    listeners: Mutex<Vec<Listener>>,
    pub rotational: Option<bool>,
    pdf_dedicated: bool,
    threads: Mutex<Vec<JoinHandle<()>>>,
    ocr_wakeup: Condvar,
    ocr_lock: Mutex<()>,
}

#[derive(Clone)]
pub struct IndexService {
    pub inner: Arc<Inner>,
}

/// Worker count policy: CPU preference × cores, reduced for spinning disks (random reads
/// collapse HDD throughput) and for low available memory.
pub fn compute_workers(s: &Settings, rotational: Option<bool>, available_mem: u64) -> usize {
    if s.indexing.worker_count > 0 {
        return s.indexing.worker_count as usize;
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut n = match s.performance.cpu {
        CpuPreference::Low => (cores / 4).max(1),
        CpuPreference::Balanced => (cores / 2).max(2),
        CpuPreference::High => cores.saturating_sub(1).max(2),
    };
    if rotational == Some(true) {
        n = n.min(2);
    }
    if available_mem > 0 && available_mem < (2 << 30) {
        n = n.min(2);
    } else if available_mem > 0 && available_mem < (4 << 30) {
        n = n.min(4);
    }
    n.clamp(1, 32)
}

impl IndexService {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        settings: Settings,
        filter: Arc<PathFilter>,
        catalog: Arc<Catalog>,
        store: Arc<IndexStore>,
        texts: Arc<TextStore>,
        registry: Arc<ParserRegistry>,
        rotational: Option<bool>,
        pdf_threads: usize,
    ) -> Self {
        let pdf_dedicated = pdf_threads > 0;
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let workers = compute_workers(&settings, rotational, sys.available_memory());
        let max_workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(workers, 32);
        // Bounded: when the writer falls behind, parser workers wait instead of piling up
        // finished documents (each can hold megabytes of text) in memory.
        let (writer_tx, writer_rx) = bounded(WRITER_QUEUE);
        let paused = settings.performance.background == BackgroundMode::Paused;
        let inner = Arc::new(Inner {
            settings: RwLock::new(settings),
            filter: RwLock::new(filter),
            catalog,
            store,
            texts,
            registry,
            progress: Progress::default(),
            small: bounded(2048),
            large: bounded(64),
            pdf: bounded(512),
            writer_tx,
            paused: AtomicBool::new(paused),
            shutdown: AtomicBool::new(false),
            target_workers: AtomicUsize::new(workers),
            max_workers,
            scan_queue: Mutex::new(VecDeque::new()),
            scan_cv: Condvar::new(),
            active_scan: Mutex::new(None),
            roots: RwLock::new(HashMap::new()),
            listeners: Mutex::new(Vec::new()),
            rotational,
            pdf_dedicated,
            threads: Mutex::new(Vec::new()),
            ocr_wakeup: Condvar::new(),
            ocr_lock: Mutex::new(()),
        });
        let svc = IndexService { inner: inner.clone() };
        svc.refresh_roots();
        let mut threads = Vec::new();
        {
            let i = inner.clone();
            threads.push(spawn("ff-writer", move || writer_loop(i, writer_rx)));
        }
        for idx in 0..max_workers {
            let i = inner.clone();
            threads.push(spawn(&format!("ff-worker-{idx}"), move || worker_loop(i, idx)));
        }
        // PDFs have their own lane served by exactly as many threads as there are PDF engines
        // (1 in-process PDFium, or one per helper process), so parser workers never block
        // waiting for a PDF engine.
        for k in 0..pdf_threads {
            let i = inner.clone();
            threads.push(spawn(&format!("ff-pdf-{k}"), move || pdf_loop(i)));
        }
        {
            let i = inner.clone();
            threads.push(spawn("ff-scan", move || coordinator_loop(i)));
        }
        {
            let i = inner.clone();
            threads.push(spawn("ff-ocr", move || crate::ocr::ocr_loop(i)));
        }
        *inner.threads.lock() = threads;
        tracing::info!(workers, max_workers, ?rotational, pdf_threads, "indexing service started");
        svc
    }

    pub fn refresh_roots(&self) {
        if let Ok(rows) = self.inner.catalog.roots() {
            *self.inner.roots.write() = rows.into_iter().map(|r| (r.id, PathBuf::from(r.path))).collect();
        }
    }

    pub fn add_listener(&self, f: impl Fn(u64) + Send + Sync + 'static) {
        self.inner.listeners.lock().push(Arc::new(f));
    }

    /// Queue a scan. `front` puts it ahead of queued work (user-requested directories).
    pub fn request_scan(&self, root: i64, subdir: Option<PathBuf>, front: bool) {
        let mut q = self.inner.scan_queue.lock();
        if q.iter().any(|r| r.root == root && (r.subdir.is_none() || r.subdir == subdir)) {
            return;
        }
        let req = ScanReq { root, subdir };
        if front {
            q.push_front(req);
        } else {
            q.push_back(req);
        }
        self.inner.scan_cv.notify_all();
    }

    pub fn rescan_all(&self) {
        let roots: Vec<i64> = self.inner.roots.read().keys().copied().collect();
        for r in roots {
            self.request_scan(r, None, false);
        }
    }

    /// Cancel scans of a root and remove all of its documents (blocking until done).
    pub fn delete_root(&self, root_id: i64) {
        self.inner.scan_queue.lock().retain(|r| r.root != root_id);
        if let Some((r, tok)) = self.inner.active_scan.lock().as_ref() {
            if *r == root_id {
                tok.cancel();
            }
        }
        let (tx, rx) = bounded(1);
        self.send(WriterMsg::DeleteRoot { root_id, ack: tx });
        let _ = rx.recv_timeout(Duration::from_secs(600));
        self.inner.roots.write().remove(&root_id);
    }

    pub fn set_paused(&self, paused: bool) {
        self.inner.paused.store(paused, Ordering::Relaxed);
        self.inner.scan_cv.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Relaxed)
    }

    pub fn apply_settings(&self, s: &Settings, filter: Arc<PathFilter>) {
        *self.inner.filter.write() = filter;
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let n = compute_workers(s, self.inner.rotational, sys.available_memory()).min(self.inner.max_workers);
        self.inner.target_workers.store(n, Ordering::Relaxed);
        self.set_paused(s.performance.background == BackgroundMode::Paused);
        *self.inner.settings.write() = s.clone();
        self.inner.ocr_wakeup.notify_all();
    }

    pub fn stage_stats(&self) -> StageStats {
        let p = &self.inner.progress;
        let s = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1e9;
        StageStats {
            parse_secs: s(&p.parse_ns),
            writer_busy_secs: s(&p.writer_busy_ns),
            commit_secs: s(&p.commit_ns),
            commits: p.commits.load(Ordering::Relaxed),
        }
    }

    pub fn worker_count(&self) -> usize {
        self.inner.target_workers.load(Ordering::Relaxed)
    }

    fn send(&self, m: WriterMsg) {
        self.inner.progress.writer_sent.fetch_add(1, Ordering::AcqRel);
        let _ = self.inner.writer_tx.send(m);
    }

    /// Force a commit and wait for it.
    pub fn commit_now(&self) {
        let (tx, rx) = bounded(1);
        self.send(WriterMsg::Commit { ack: Some(tx) });
        let _ = rx.recv_timeout(Duration::from_secs(120));
    }

    /// Drop the whole index (keeps roots) and re-index everything.
    pub fn clear_all(&self) {
        if let Some((_, tok)) = self.inner.active_scan.lock().as_ref() {
            tok.cancel();
        }
        self.inner.scan_queue.lock().clear();
        // Drain queued work so nothing stale is written after the clear.
        while self.inner.small.1.try_recv().is_ok() || self.inner.large.1.try_recv().is_ok() || self.inner.pdf.1.try_recv().is_ok() {
            self.inner.progress.processed.fetch_add(1, Ordering::Relaxed);
        }
        let (tx, rx) = bounded(1);
        self.send(WriterMsg::ClearAll { ack: tx });
        let _ = rx.recv_timeout(Duration::from_secs(600));
        self.rescan_all();
    }

    /// True when no scan is queued or running, all dispatched files are processed and the
    /// writer has committed everything.
    pub fn is_idle(&self) -> bool {
        let p = &self.inner.progress;
        self.inner.scan_queue.lock().is_empty()
            && p.scanning.load(Ordering::Acquire) == 0
            && p.dispatched.load(Ordering::Acquire) == p.processed.load(Ordering::Acquire)
            && p.writer_sent.load(Ordering::Acquire) == p.writer_applied.load(Ordering::Acquire)
            && !p.writer_dirty.load(Ordering::Acquire)
    }

    /// Wait until idle (used by the CLI, benchmarks and tests).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        // Give freshly queued scans a moment to register as active.
        std::thread::sleep(Duration::from_millis(50));
        loop {
            if self.is_idle() {
                // Double-check after a short pause (a scan may be between states).
                std::thread::sleep(Duration::from_millis(100));
                if self.is_idle() {
                    return true;
                }
            }
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn progress(&self) -> IndexProgress {
        let p = &self.inner.progress;
        let dispatched = p.dispatched.load(Ordering::Relaxed);
        let processed = p.processed.load(Ordering::Relaxed);
        let scanning = p.scanning.load(Ordering::Relaxed) > 0 || !self.inner.scan_queue.lock().is_empty();
        let active = scanning || dispatched > processed || p.writer_dirty.load(Ordering::Relaxed);
        let mut session = p.session.lock();
        if active && session.is_none() {
            *session = Some((Instant::now(), processed));
        } else if !active {
            *session = None;
        }
        let files_per_sec = session
            .map(|(t, base)| {
                let secs = t.elapsed().as_secs_f64();
                if secs > 0.5 { (processed - base) as f64 / secs } else { 0.0 }
            })
            .unwrap_or(0.0);
        let percent = if !scanning && dispatched > 0 && active {
            Some((processed as f32 / dispatched as f32 * 100.0).min(100.0))
        } else {
            None
        };
        IndexProgress {
            active,
            scanning,
            paused: self.is_paused(),
            discovered: p.discovered.load(Ordering::Relaxed),
            queued: dispatched.saturating_sub(processed),
            processed,
            bytes_processed: p.bytes.load(Ordering::Relaxed),
            files_per_sec,
            percent,
            current_path: if active { p.current.lock().clone() } else { None },
            ocr_pending: 0,
        }
    }

    pub fn is_scanning(&self, root: i64) -> bool {
        self.inner.active_scan.lock().as_ref().map(|(r, _)| *r == root).unwrap_or(false)
            || self.inner.scan_queue.lock().iter().any(|r| r.root == root)
    }

    /// React to debounced watcher changes.
    pub fn handle_changes(&self, changes: Vec<Change>) {
        if self.inner.settings.read().performance.background != BackgroundMode::Automatic {
            return;
        }
        for c in changes {
            match c {
                Change::Upsert { root, path } => self.check_file(root, &path, None),
                Change::Remove { path, .. } => {
                    let p = path_str(&path);
                    self.send(WriterMsg::Delete { path: p.clone() });
                    self.send(WriterMsg::DeleteUnder { dir: p });
                }
                Change::Rename { root, from, to } => self.check_file(root, &to, Some(path_str(&from))),
                Change::ScanDir { root, path } => self.request_scan(root, Some(path), false),
                Change::Rescan { root } => self.request_scan(root, None, false),
            }
        }
    }

    /// Re-evaluate one file (watcher path).
    pub fn check_file(&self, root: i64, path: &Path, reuse_from: Option<String>) {
        let Some(root_path) = self.inner.roots.read().get(&root).cloned() else { return };
        let filter = self.inner.filter.read().clone();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let md = std::fs::symlink_metadata(path);
        let allowed = filter.path_allowed(&root_path, path) && filter.file_allowed(&name, hidden_attr(md.as_ref().ok()), system_attr(md.as_ref().ok()));
        let Some(md) = md.ok().filter(|m| m.is_file() && allowed) else {
            // Excluded, vanished or not a regular file: make sure it is not in the index.
            self.send(WriterMsg::Delete { path: path_str(path) });
            if let Some(old) = reuse_from {
                self.send(WriterMsg::Delete { path: old });
            }
            return;
        };
        let size = md.len();
        let mtime_ns = md.modified().map(util::system_time_ns).unwrap_or(0);
        let p = path_str(path);
        let prev = self.inner.catalog.get(&p).ok().flatten();
        if reuse_from.is_none() {
            if let Some(r) = &prev {
                if r.size == size && r.mtime_ns == mtime_ns {
                    return;
                }
            }
        }
        dispatch(&self.inner, WorkItem {
            root,
            path: path.to_path_buf(),
            size,
            mtime_ns,
            prev_hash: prev.as_ref().and_then(|r| r.hash),
            prev_size: prev.map(|r| r.size),
            reuse_from,
        });
    }

    pub fn shutdown(&self) {
        if self.inner.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some((_, tok)) = self.inner.active_scan.lock().as_ref() {
            tok.cancel();
        }
        self.inner.scan_cv.notify_all();
        self.inner.ocr_wakeup.notify_all();
        let _ = self.inner.writer_tx.send(WriterMsg::Shutdown);
        let threads = std::mem::take(&mut *self.inner.threads.lock());
        for t in threads {
            let _ = t.join();
        }
        tracing::info!("indexing service stopped");
    }

    /// Send an externally produced document (OCR) to the writer.
    pub(crate) fn submit(inner: &Inner, p: Processed) {
        inner.progress.writer_sent.fetch_add(1, Ordering::AcqRel);
        let _ = inner.writer_tx.send(WriterMsg::Doc(Box::new(p)));
    }
}

impl Inner {
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// True while scanning or parsing is in progress (OCR yields to regular indexing).
    pub fn busy(&self) -> bool {
        let p = &self.progress;
        p.scanning.load(Ordering::Relaxed) > 0 || p.dispatched.load(Ordering::Relaxed) > p.processed.load(Ordering::Relaxed)
    }

    pub fn ocr_wait(&self, d: Duration) {
        let mut g = self.ocr_lock.lock();
        self.ocr_wakeup.wait_for(&mut g, d);
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new().name(name.into()).spawn(f).expect("spawn thread")
}

#[cfg(windows)]
fn hidden_attr(md: Option<&std::fs::Metadata>) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.map(|m| m.file_attributes() & 0x2 != 0).unwrap_or(false)
}
#[cfg(windows)]
fn system_attr(md: Option<&std::fs::Metadata>) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.map(|m| m.file_attributes() & 0x4 != 0).unwrap_or(false)
}
#[cfg(not(windows))]
fn hidden_attr(_: Option<&std::fs::Metadata>) -> bool {
    false
}
#[cfg(not(windows))]
fn system_attr(_: Option<&std::fs::Metadata>) -> bool {
    false
}

fn dispatch(inner: &Inner, item: WorkItem) {
    let ext = extension_of(&item.path);
    inner.progress.dispatched.fetch_add(1, Ordering::AcqRel);
    let lane = if inner.pdf_dedicated && ext == "pdf" {
        &inner.pdf.0
    } else if item.size >= LARGE_FILE {
        &inner.large.0
    } else {
        &inner.small.0
    };
    if lane.send(item).is_err() {
        inner.progress.processed.fetch_add(1, Ordering::AcqRel);
    }
}

// -------------------------------------------------------------------------------------------
// Scanning
// -------------------------------------------------------------------------------------------

fn coordinator_loop(inner: Arc<Inner>) {
    let mut last_periodic = Instant::now();
    loop {
        let req = {
            let mut q = inner.scan_queue.lock();
            loop {
                if inner.is_shutdown() {
                    return;
                }
                if !inner.is_paused() {
                    if let Some(r) = q.pop_front() {
                        // Mark as scanning while still holding the queue lock so idle checks
                        // never observe "empty queue and no scan".
                        inner.progress.scanning.fetch_add(1, Ordering::AcqRel);
                        break r;
                    }
                }
                inner.scan_cv.wait_for(&mut q, Duration::from_secs(30));
                // Periodic reconcile scans catch changes watchers cannot see.
                let interval = inner.settings.read().indexing.rescan_interval_min as u64;
                if interval > 0 && last_periodic.elapsed() >= Duration::from_secs(interval * 60) && q.is_empty() {
                    last_periodic = Instant::now();
                    for id in inner.roots.read().keys() {
                        q.push_back(ScanReq { root: *id, subdir: None });
                    }
                }
            }
        };
        let tok = CancelToken::new();
        *inner.active_scan.lock() = Some((req.root, tok.clone()));
        let started = Instant::now();
        let res = catch_unwind(AssertUnwindSafe(|| run_scan(&inner, &req, &tok)));
        if let Err(p) = res {
            tracing::error!(root = req.root, panic = ?p.downcast_ref::<&str>(), "scan panicked");
        }
        *inner.active_scan.lock() = None;
        inner.progress.scanning.fetch_sub(1, Ordering::AcqRel);
        tracing::info!(root = req.root, subdir = ?req.subdir, ms = started.elapsed().as_millis() as u64, "scan finished");
    }
}

fn run_scan(inner: &Arc<Inner>, req: &ScanReq, cancel: &CancelToken) {
    let Some(root_path) = inner.roots.read().get(&req.root).cloned() else { return };
    if !root_path.exists() {
        // Unplugged drive / unmounted share: never treat as "everything deleted".
        let _ = inner.catalog.set_watch_status(req.root, "unavailable");
        tracing::warn!(root = %root_path.display(), "root unavailable; skipping scan");
        return;
    }
    let scan_dir = req.subdir.clone().unwrap_or_else(|| root_path.clone());
    let started = Instant::now();
    let under = req.subdir.as_ref().map(|d| path_str(d));
    let snapshot = match inner.catalog.snapshot(req.root, under.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "catalog snapshot failed");
            return;
        }
    };
    // (size, mtime) of large existing entries → rename candidates.
    let mut by_size_mtime: HashSet<(u64, i64)> = HashSet::new();
    for s in snapshot.values() {
        if s.size >= MOVE_MIN_SIZE {
            by_size_mtime.insert((s.size, s.mtime_ns));
        }
    }
    let snapshot = Mutex::new(snapshot);
    let held: Mutex<Vec<FileEntry>> = Mutex::new(Vec::new());
    let settings = inner.settings.read().clone();
    let filter = inner.filter.read().clone();
    let hot: HashSet<String> = inner.catalog.hot_dirs(200).unwrap_or_default().into_iter().collect();
    let threads = if inner.rotational == Some(true) { 1 } else { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8) };
    let opts = ScanOptions { filter, threads, follow_symlinks: settings.indexing.follow_symlinks, hot_dirs: Arc::new(hot) };
    let root = req.root;

    let summary = scan(&scan_dir, &opts, cancel, |e: FileEntry| {
        inner.progress.discovered.fetch_add(1, Ordering::Relaxed);
        let key = path_hash(&path_str(&e.path));
        let prev = snapshot.lock().remove(&key);
        match prev {
            Some(s) if s.size == e.size && s.mtime_ns == e.mtime_ns => {}
            Some(s) => dispatch(inner, WorkItem { root, path: e.path, size: e.size, mtime_ns: e.mtime_ns, prev_hash: s.hash, prev_size: Some(s.size), reuse_from: None }),
            None if e.size >= MOVE_MIN_SIZE && by_size_mtime.contains(&(e.size, e.mtime_ns)) => held.lock().push(e),
            None => dispatch(inner, WorkItem { root, path: e.path, size: e.size, mtime_ns: e.mtime_ns, prev_hash: None, prev_size: None, reuse_from: None }),
        }
    });
    let held = held.into_inner();
    if summary.cancelled || inner.is_shutdown() {
        return;
    }
    // Entries not seen during the scan were deleted, moved or excluded — unless they live
    // under a directory we could not read.
    let leftovers: Vec<i64> = snapshot.into_inner().values().map(|s| s.id).collect();
    let failed: Vec<String> = summary.failed_dirs.iter().map(|d| filter_form(&path_str(d))).collect();
    let gone = inner.catalog.paths_by_ids(&leftovers).unwrap_or_default();
    let mut held_by_key: HashMap<(u64, i64), Vec<FileEntry>> = HashMap::new();
    for e in held {
        held_by_key.entry((e.size, e.mtime_ns)).or_default().push(e);
    }
    let mut deleted = 0u64;
    let mut moved = 0u64;
    for (_, path, size, mtime) in gone {
        let f = filter_form(&path);
        if failed.iter().any(|d| f.starts_with(d.as_str()) && f[d.len()..].starts_with('/')) {
            continue;
        }
        if let Some(list) = held_by_key.get_mut(&(size, mtime)) {
            if let Some(e) = list.pop() {
                moved += 1;
                dispatch(inner, WorkItem { root, path: e.path, size, mtime_ns: mtime, prev_hash: None, prev_size: None, reuse_from: Some(path) });
                continue;
            }
        }
        deleted += 1;
        inner.progress.writer_sent.fetch_add(1, Ordering::AcqRel);
        let _ = inner.writer_tx.send(WriterMsg::Delete { path });
    }
    for e in held_by_key.into_values().flatten() {
        dispatch(inner, WorkItem { root, path: e.path, size: e.size, mtime_ns: e.mtime_ns, prev_hash: None, prev_size: None, reuse_from: None });
    }
    if req.subdir.is_none() {
        let _ = inner.catalog.set_root_scanned(root, util::now_secs(), started.elapsed().as_millis() as i64);
    }
    tracing::info!(root, files = summary.files, dirs = summary.dirs, errors = summary.errors, deleted, moved, "scan complete");
}

// -------------------------------------------------------------------------------------------
// Workers
// -------------------------------------------------------------------------------------------

fn worker_loop(inner: Arc<Inner>, idx: usize) {
    let low = inner.settings.read().performance.cpu == CpuPreference::Low;
    lower_thread_priority(low);
    let prefer_large = idx == 1;
    let (small, large) = (inner.small.1.clone(), inner.large.1.clone());
    loop {
        if inner.is_shutdown() {
            return;
        }
        if inner.is_paused() || idx >= inner.target_workers.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        let item = if prefer_large { large.try_recv().ok() } else { None }
            .or_else(|| small.try_recv().ok())
            .or_else(|| large.try_recv().ok())
            .or_else(|| {
                select! {
                    recv(small) -> m => m.ok(),
                    recv(large) -> m => m.ok(),
                    default(Duration::from_millis(250)) => None,
                }
            });
        if let Some(item) = item {
            handle_item(&inner, item);
        }
    }
}

fn pdf_loop(inner: Arc<Inner>) {
    lower_thread_priority(false);
    let rx = inner.pdf.1.clone();
    loop {
        if inner.is_shutdown() {
            return;
        }
        if inner.is_paused() {
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        if let Ok(item) = rx.recv_timeout(Duration::from_millis(250)) {
            handle_item(&inner, item);
        }
    }
}

enum Outcome {
    Doc(Processed),
    Delete(String),
    Touch { path: String, size: u64, mtime_ns: i64 },
}

fn handle_item(inner: &Inner, item: WorkItem) {
    *inner.progress.current.lock() = Some(path_str(&item.path));
    let size = item.size;
    let t0 = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| process(inner, &item))).unwrap_or_else(|_| {
        tracing::error!(path = %item.path.display(), "parser panicked");
        Outcome::Doc(name_only(inner, &item, Status::Failed, Some("parser crashed".into()), flags::FAILED))
    });
    inner.progress.parse_ns.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let msg = match outcome {
        Outcome::Doc(p) => WriterMsg::Doc(Box::new(p)),
        Outcome::Delete(path) => WriterMsg::Delete { path },
        Outcome::Touch { path, size, mtime_ns } => WriterMsg::Touch { path, size, mtime_ns },
    };
    inner.progress.writer_sent.fetch_add(1, Ordering::AcqRel);
    let _ = inner.writer_tx.send(msg);
    inner.progress.bytes.fetch_add(size, Ordering::Relaxed);
    inner.progress.processed.fetch_add(1, Ordering::AcqRel);
}

/// Common document fields (everything but content/meta).
#[allow(clippy::too_many_arguments)]
pub(crate) fn base_doc(f: &Fields, root: i64, path: &Path, size: u64, mtime_ns: i64, kind: Kind, flags_v: u64, pages: Option<u32>, mode: TextMode) -> TantivyDocument {
    let p = path_str(path);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
    let parent = path.parent().map(path_str).unwrap_or_default();
    let mut d = TantivyDocument::default();
    d.add_text(f.path, &p);
    d.add_u64(f.pid, path_hash(&p));
    d.add_text(f.name, &name);
    d.add_text(f.name_lc, name.to_lowercase());
    let mut dir = filter_form(&parent);
    if !dir.ends_with('/') {
        dir.push('/');
    }
    d.add_text(f.dir, &dir);
    d.add_text(f.path_t, &parent);
    d.add_text(f.ext, extension_of(path));
    d.add_text(f.kind, kind.as_str());
    d.add_u64(f.root, root as u64);
    d.add_u64(f.size, size);
    d.add_i64(f.mtime, mtime_ns.div_euclid(1_000_000_000));
    d.add_u64(f.flags, flags_v);
    d.add_u64(f.pages, pages.unwrap_or(0) as u64);
    d.add_u64(f.mode, mode as u64);
    d
}

/// Title/author for the preview panel (stored only).
pub(crate) fn doc_info_json(meta: &DocMeta) -> String {
    serde_json::json!({ "title": meta.title, "author": meta.author }).to_string()
}

fn record(item: &WorkItem, kind: Kind, status: Status, reason: Option<String>, flags_v: u64, hash: Option<u64>) -> FileRecord {
    FileRecord {
        root_id: item.root,
        path: path_str(&item.path),
        size: item.size,
        mtime_ns: item.mtime_ns,
        hash,
        kind: kind.as_str().into(),
        status,
        reason,
        flags: flags_v,
    }
}

fn name_only(inner: &Inner, item: &WorkItem, status: Status, reason: Option<String>, extra_flags: u64) -> Processed {
    let kind = kind_for_ext(&extension_of(&item.path));
    let fl = extra_flags | flags::NAME_ONLY;
    let doc = base_doc(&inner.store.fields, item.root, &item.path, item.size, item.mtime_ns, kind, fl, None, TextMode::Raw);
    Processed {
        record: record(item, kind, status, reason, fl, None),
        doc: Some(doc),
        text: TextAction::Delete,
        delete_old: item.reuse_from.clone(),
    }
}

fn process(inner: &Inner, item: &WorkItem) -> Outcome {
    let settings = inner.settings.read().clone();
    let filter = inner.filter.read().clone();
    let ext = extension_of(&item.path);
    let kind = kind_for_ext(&ext);

    if let Some(p) = try_reuse(inner, item, kind) {
        return Outcome::Doc(p);
    }
    let max = if ext == "pdf" { filter.max_pdf_size } else { filter.max_file_size };
    if item.size > max {
        return Outcome::Doc(name_only(inner, item, Status::Skipped, Some(format!("larger than {} MB limit", max >> 20)), 0));
    }
    if !filter.content_allowed(&ext) {
        return Outcome::Doc(name_only(inner, item, Status::Skipped, Some("file type excluded in settings".into()), 0));
    }
    let header = match read_header(&item.path) {
        Ok(h) => h,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Outcome::Delete(path_str(&item.path)),
        Err(e) => {
            let reason = if e.kind() == std::io::ErrorKind::PermissionDenied { "permission denied".to_string() } else { format!("cannot read: {e}") };
            return Outcome::Doc(name_only(inner, item, Status::Failed, Some(reason), flags::FAILED));
        }
    };
    let Some(resolved) = inner.registry.resolve(&item.path, &ext, &header, filter.detect_text) else {
        if kind == Kind::Image && settings.indexing.ocr.enabled && settings.indexing.ocr.images {
            return Outcome::Doc(name_only(inner, item, Status::NeedsOcr, Some("image: waiting for OCR".into()), flags::NEEDS_OCR));
        }
        if filter.index_all_filenames {
            return Outcome::Doc(name_only(inner, item, Status::NameOnly, None, 0));
        }
        return Outcome::Doc(Processed {
            record: record(item, kind, Status::Ignored, None, 0, None),
            doc: None,
            text: TextAction::Delete,
            delete_old: item.reuse_from.clone(),
        });
    };

    // Content hash: a touched-but-identical file only needs its catalog timestamp updated.
    // Only worth it for expensive formats — re-parsing plain text costs about as much as
    // hashing it, so text files skip the extra read.
    let cancel = CancelToken::new();
    let hash = if settings.indexing.content_hash && item.size <= HASH_MAX_SIZE && resolved.parser.text_mode() == TextMode::Stored {
        util::content_hash(&item.path, &cancel).ok()
    } else {
        None
    };
    if item.reuse_from.is_none() && hash.is_some() && item.prev_hash == hash && item.prev_size == Some(item.size) {
        return Outcome::Touch { path: path_str(&item.path), size: item.size, mtime_ns: item.mtime_ns };
    }

    let parser = resolved.parser;
    let limits = Limits {
        max_text_bytes: (settings.indexing.max_indexed_text_mb as usize) << 20,
        deadline: Some(Instant::now() + Duration::from_secs(settings.indexing.parse_timeout_secs)),
        ..Limits::default()
    };
    let mut sink = TextSink::new(limits.max_text_bytes, limits.deadline);
    let t0 = Instant::now();
    let res = catch_unwind(AssertUnwindSafe(|| parser.extract(&item.path, &ParseContext { limits: &limits }, &mut sink)));
    let elapsed = t0.elapsed();
    let mode = parser.text_mode();
    let (meta, mut status, mut reason, mut fl) = match res {
        Ok(Ok(meta)) => {
            let f = meta.flags;
            (meta, Status::Indexed, None, f)
        }
        // A timeout keeps whatever was extracted so far.
        Ok(Err(ParseError::Timeout)) if !sink.is_empty() => (DocMeta::default(), Status::Indexed, Some("parse time limit reached; partially indexed".into()), flags::TRUNCATED),
        Ok(Err(ParseError::Encrypted)) => return Outcome::Doc(name_only(inner, item, Status::Encrypted, Some("password protected or encrypted".into()), flags::ENCRYPTED)),
        Ok(Err(ParseError::Unsupported(m))) => return Outcome::Doc(name_only(inner, item, Status::NameOnly, Some(m), 0)),
        Ok(Err(ParseError::Io(e))) if e.kind() == std::io::ErrorKind::NotFound => return Outcome::Delete(path_str(&item.path)),
        Ok(Err(e)) => {
            tracing::info!(path = %item.path.display(), parser = parser.name(), error = %e, "extraction failed");
            return Outcome::Doc(name_only(inner, item, Status::Failed, Some(e.to_string()), flags::FAILED));
        }
        Err(_) => {
            tracing::error!(path = %item.path.display(), parser = parser.name(), "parser panicked");
            return Outcome::Doc(name_only(inner, item, Status::Failed, Some("parser crashed on malformed input".into()), flags::FAILED));
        }
    };
    if elapsed > Duration::from_secs(5) {
        tracing::info!(path = %item.path.display(), parser = parser.name(), ms = elapsed.as_millis() as u64, "slow extraction");
    }
    let (text, locs, truncated) = sink.into_parts();
    if truncated {
        fl |= flags::TRUNCATED;
        if reason.is_none() {
            reason = Some("text beyond the per-file limit was not indexed".into());
        }
    }
    if fl & flags::NEEDS_OCR != 0 {
        status = Status::NeedsOcr;
        reason = Some("scanned document: OCR required".into());
    }
    let f = &inner.store.fields;
    let mut doc = base_doc(f, item.root, &item.path, item.size, item.mtime_ns, kind, fl, meta.pages, mode);
    doc.add_text(f.content, &text);
    let meta_text = meta.searchable();
    if !meta_text.is_empty() {
        doc.add_text(f.meta, &meta_text);
        doc.add_text(f.info, doc_info_json(&meta));
    }
    let text_action = if mode == TextMode::Stored && !text.trim().is_empty() {
        let cap = (settings.indexing.stored_text_kb as usize) << 10;
        let cut = util::floor_char_boundary(&text, cap);
        let stored_locs: Vec<_> = locs.iter().filter(|l| (l.offset as usize) <= cut).cloned().collect();
        TextAction::Put(textstore::encode(&text[..cut], &stored_locs))
    } else {
        TextAction::Delete
    };
    Outcome::Doc(Processed {
        record: record(item, kind, status, reason, fl, hash),
        doc: Some(doc),
        text: text_action,
        delete_old: item.reuse_from.clone(),
    })
}

/// Renamed/moved file with an unchanged, fully stored text: rebuild its document from the
/// text store instead of re-parsing (saves re-reading big PDFs after a folder move).
fn try_reuse(inner: &Inner, item: &WorkItem, kind: Kind) -> Option<Processed> {
    let old = item.reuse_from.as_ref()?;
    let row = inner.catalog.get(old).ok()??;
    if row.status != Status::Indexed || row.flags & flags::TRUNCATED != 0 || row.size != item.size {
        return None;
    }
    let f = &inner.store.fields;
    let searcher = inner.store.searcher();
    let q = tantivy::query::TermQuery::new(Term::from_field_text(f.path, old), tantivy::schema::IndexRecordOption::Basic);
    let hits = searcher.search(&q, &tantivy::collector::TopDocs::with_limit(1).order_by_score()).ok()?;
    let (_, addr) = hits.first()?;
    let old_doc: TantivyDocument = searcher.doc(*addr).ok()?;
    let mode = TextMode::from_u64(old_doc.get_first(f.mode).and_then(|v| v.as_u64()).unwrap_or(0));
    if mode != TextMode::Stored {
        return None;
    }
    let (text, locs) = inner.texts.get(path_hash(old)).ok()??;
    let pages = old_doc.get_first(f.pages).and_then(|v| v.as_u64()).filter(|&p| p > 0).map(|p| p as u32);
    let meta = old_doc.get_first(f.meta).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let info = old_doc.get_first(f.info).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut doc = base_doc(f, item.root, &item.path, item.size, item.mtime_ns, kind, row.flags, pages, mode);
    doc.add_text(f.content, &text);
    if !meta.is_empty() {
        doc.add_text(f.meta, &meta);
    }
    if !info.is_empty() {
        doc.add_text(f.info, &info);
    }
    Some(Processed {
        record: record(item, kind, Status::Indexed, None, row.flags, row.hash),
        doc: Some(doc),
        text: TextAction::Put(textstore::encode(&text, &locs)),
        delete_old: Some(old.clone()),
    })
}

// -------------------------------------------------------------------------------------------
// Writer
// -------------------------------------------------------------------------------------------

fn writer_loop(inner: Arc<Inner>, rx: Receiver<WriterMsg>) {
    let f = inner.store.fields;
    let mut cat_ops: Vec<CatOp> = Vec::new();
    let mut text_ops: Vec<TextOp> = Vec::new();
    let mut text_bytes = 0usize;
    let mut dirty = false;
    let mut docs_since_commit = 0u64;
    let mut docs_this_session = 0u64;
    let mut last_commit = Instant::now();
    let mut acks: Vec<Sender<()>> = Vec::new();

    let commit = |cat_ops: &mut Vec<CatOp>, text_ops: &mut Vec<TextOp>| {
        let t0 = Instant::now();
        if let Err(e) = inner.store.commit() {
            // Dropping the pending catalog updates is safe: those files are simply seen as
            // changed on the next scan and re-indexed.
            tracing::error!(error = %e, "index commit failed");
            cat_ops.clear();
            text_ops.clear();
            return;
        }
        if let Err(e) = inner.texts.apply(text_ops) {
            tracing::error!(error = %e, "text store commit failed");
            if let crate::error::Error::Db(db) = &e {
                if super::catalog::is_corruption(db) {
                    super::catalog::flag_corrupt(inner.texts.db_path());
                }
            }
        }
        if let Err(e) = inner.catalog.apply(cat_ops) {
            tracing::error!(error = %e, "catalog commit failed");
            if let crate::error::Error::Db(db) = &e {
                if super::catalog::is_corruption(db) {
                    super::catalog::flag_corrupt(inner.catalog.db_path());
                }
            }
        }
        tracing::debug!(ops = cat_ops.len(), ms = t0.elapsed().as_millis() as u64, "committed");
        inner.ocr_wakeup.notify_all();
        inner.progress.commit_ns.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        inner.progress.commits.fetch_add(1, Ordering::Relaxed);
        cat_ops.clear();
        text_ops.clear();
        let generation = inner.store.generation();
        for l in inner.listeners.lock().iter() {
            l(generation);
        }
    };

    loop {
        let msg = rx.recv_timeout(Duration::from_millis(250));
        let busy_start = Instant::now();
        let mut shutdown = false;
        let mut force = false;
        if let Ok(m) = msg {
            match m {
                WriterMsg::Doc(p) => {
                    let p = *p;
                    inner.store.with_writer(|w| {
                        if let Some(old) = &p.delete_old {
                            if *old != p.record.path {
                                w.delete_term(Term::from_field_text(f.path, old));
                                cat_ops.push(CatOp::Delete { path: old.clone() });
                                text_ops.push(TextOp::Delete { key: path_hash(old) });
                            }
                        }
                        w.delete_term(Term::from_field_text(f.path, &p.record.path));
                        if let Some(doc) = p.doc {
                            if let Err(e) = w.add_document(doc) {
                                tracing::error!(path = %p.record.path, error = %e, "add_document failed");
                            }
                        }
                    });
                    let key = path_hash(&p.record.path);
                    match p.text {
                        TextAction::Put(blob) => {
                            text_bytes += blob.len();
                            text_ops.push(TextOp::Put { key, blob });
                        }
                        TextAction::Delete => text_ops.push(TextOp::Delete { key }),
                    }
                    cat_ops.push(CatOp::Upsert(p.record));
                    docs_since_commit += 1;
                    docs_this_session += 1;
                    dirty = true;
                }
                WriterMsg::Delete { path } => {
                    inner.store.with_writer(|w| w.delete_term(Term::from_field_text(f.path, &path)));
                    text_ops.push(TextOp::Delete { key: path_hash(&path) });
                    cat_ops.push(CatOp::Delete { path });
                    dirty = true;
                }
                WriterMsg::DeleteUnder { dir } => {
                    let paths = inner.catalog.paths_under(&dir).unwrap_or_default();
                    if !paths.is_empty() {
                        inner.store.with_writer(|w| {
                            for p in &paths {
                                w.delete_term(Term::from_field_text(f.path, p));
                            }
                        });
                        text_ops.extend(paths.iter().map(|p| TextOp::Delete { key: path_hash(p) }));
                        cat_ops.push(CatOp::DeleteUnder { prefix: dir });
                        dirty = true;
                    }
                }
                WriterMsg::DeleteRoot { root_id, ack } => {
                    // By path (from the catalog) rather than by the `root` field, so roots can be
                    // merged by re-assigning catalog rows without touching the index.
                    let paths = inner.catalog.paths_of_root(root_id).unwrap_or_default();
                    inner.store.with_writer(|w| {
                        for p in &paths {
                            w.delete_term(Term::from_field_text(f.path, p));
                        }
                    });
                    text_ops.extend(paths.iter().map(|p| TextOp::Delete { key: path_hash(p) }));
                    cat_ops.push(CatOp::DeleteRoot { root_id });
                    dirty = true;
                    force = true;
                    acks.push(ack);
                }
                WriterMsg::Touch { path, size, mtime_ns } => {
                    cat_ops.push(CatOp::Touch { path, size, mtime_ns });
                    dirty = true;
                }
                WriterMsg::Commit { ack } => {
                    force = true;
                    if let Some(a) = ack {
                        acks.push(a);
                    }
                }
                WriterMsg::ClearAll { ack } => {
                    cat_ops.clear();
                    text_ops.clear();
                    if let Err(e) = inner.store.clear() {
                        tracing::error!(error = %e, "clearing index failed");
                    }
                    let _ = inner.texts.clear();
                    let _ = inner.catalog.clear_files();
                    dirty = false;
                    docs_since_commit = 0;
                    let _ = ack.send(());
                    for l in inner.listeners.lock().iter() {
                        l(inner.store.generation());
                    }
                }
                WriterMsg::Shutdown => {
                    shutdown = true;
                    force = true;
                }
            }
            inner.progress.writer_applied.fetch_add(1, Ordering::AcqRel);
        }
        inner.progress.writer_dirty.store(dirty, Ordering::Release);
        // Early in a session commit often (results appear quickly); later less often (fewer
        // small segments). Always commit as soon as the pipeline drains.
        let interval = if docs_this_session < 10_000 { Duration::from_secs(2) } else { Duration::from_secs(10) };
        let drained = rx.is_empty() && !inner.busy();
        if text_bytes >= TEXT_FLUSH_BYTES {
            if let Err(e) = inner.texts.apply(&text_ops) {
                tracing::error!(error = %e, "text store write failed");
            }
            text_ops.clear();
            text_bytes = 0;
        }
        let due = dirty && (docs_since_commit >= COMMIT_DOCS || last_commit.elapsed() >= interval || drained);
        if (force && (dirty || !acks.is_empty())) || due {
            if dirty {
                commit(&mut cat_ops, &mut text_ops);
                text_bytes = 0;
            }
            dirty = false;
            docs_since_commit = 0;
            last_commit = Instant::now();
            inner.progress.writer_dirty.store(false, Ordering::Release);
            for a in acks.drain(..) {
                let _ = a.send(());
            }
        }
        if !inner.busy() && rx.is_empty() && !dirty {
            docs_this_session = 0;
        }
        inner.progress.writer_busy_ns.fetch_add(busy_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        if shutdown {
            return;
        }
    }
}
