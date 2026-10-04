//! The engine facade used by the UI and the CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::config::{AppPaths, BackgroundMode, Settings};
use crate::error::{Error, Result};
use crate::fs::storage::is_rotational;
use crate::fs::watcher::WatcherService;
use crate::fs::PathFilter;
use crate::index::{Catalog, IndexService, IndexStore, Status, TextStore};
use crate::model::{Diagnostics, IndexStatus, Page, Preview, RootInfo, SearchRequest, SearchResponse, SkippedFile, SnippetResult};
use crate::parsers::ParserRegistry;
use crate::search::SearchService;
use crate::util::{self, filter_form, path_str, CancelToken};

#[derive(Clone, Debug)]
pub struct EngineOptions {
    /// Extra directories searched for the PDFium library (e.g. the app's resource dir).
    pub pdfium_dirs: Vec<PathBuf>,
    /// Start file-system watchers for roots.
    pub watch: bool,
    /// Reconcile every root against the file system at startup (only changes are indexed).
    pub scan_on_start: bool,
    /// Executable to start as PDF helper processes (it must call
    /// [`crate::parsers::pdfworker::run_worker`] when given
    /// [`crate::parsers::pdfworker::PDF_WORKER_ARG`]). `None` = parse PDFs in-process.
    pub pdf_worker_exe: Option<PathBuf>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self { pdfium_dirs: vec![], watch: true, scan_on_start: true, pdf_worker_exe: None }
    }
}

pub struct Engine {
    pub paths: AppPaths,
    settings: RwLock<Settings>,
    pub catalog: Arc<Catalog>,
    pub store: Arc<IndexStore>,
    pub texts: Arc<TextStore>,
    pub registry: Arc<ParserRegistry>,
    pub index: IndexService,
    pub search: SearchService,
    watcher: Mutex<Option<WatcherService>>,
    watch_status: Mutex<HashMap<i64, String>>,
    rotational: Option<bool>,
    sizes: Mutex<Option<(Instant, u64, u64)>>,
    /// Status and per-root counts (GROUP BY over the catalog) cached briefly: the UI polls
    /// every second while indexing and a million-row count is not free.
    counts: Arc<Mutex<Option<CountCache>>>,
    opts: EngineOptions,
}

type CountCache = (Instant, HashMap<Status, u64>, HashMap<i64, u64>);

pub use crate::util::simplify_path;

fn is_under(child: &str, parent: &str) -> bool {
    let c = filter_form(child);
    let p = filter_form(parent);
    let p = p.trim_end_matches('/');
    c.len() > p.len() && c.starts_with(p) && c[p.len()..].starts_with('/')
}

fn indexing_changed(a: &Settings, b: &Settings) -> bool {
    let (x, y) = (&a.indexing, &b.indexing);
    x.excluded_dirs != y.excluded_dirs
        || x.included_extensions != y.included_extensions
        || x.excluded_extensions != y.excluded_extensions
        || x.follow_symlinks != y.follow_symlinks
        || x.index_hidden != y.index_hidden
        || x.index_system != y.index_system
        || x.index_all_filenames != y.index_all_filenames
        || x.detect_text_files != y.detect_text_files
        || x.max_file_size_mb != y.max_file_size_mb
        || x.max_pdf_size_mb != y.max_pdf_size_mb
}

impl Engine {
    pub fn open(paths: AppPaths, opts: EngineOptions) -> Result<Arc<Engine>> {
        paths.ensure()?;
        let settings = Settings::load(&paths.settings_file);
        if !paths.settings_file.exists() {
            settings.save(&paths.settings_file)?;
        }
        let pdf = crate::parsers::pdf::init(&opts.pdfium_dirs);
        let (catalog, cat_new) = Catalog::open(&paths.catalog_db, &paths.quarantine_dir)?;
        if settings.indexing.ocr.enabled {
            // Up to 1.1.0 Tesseract was given file paths, which it cannot open on Windows when
            // they contain characters outside the legacy code page. Those files failed with
            // "cannot read input file"; give them another try now that images go via stdin.
            match catalog.requeue_failed_ocr_like("%cannot read input file%", settings.indexing.ocr.images) {
                Ok(0) => {}
                Ok(n) => tracing::info!(files = n, "retrying OCR for files whose path Tesseract could not open"),
                Err(e) => tracing::warn!(error = %e, "could not requeue OCR failures"),
            }
        }
        // Tantivy's own indexing threads tokenize and invert text; they are the throughput
        // ceiling for text-heavy corpora, so they scale with the CPU preference.
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let writer_threads = match settings.performance.cpu {
            crate::config::CpuPreference::Low => 1,
            crate::config::CpuPreference::Balanced => (cores / 4).clamp(2, 6),
            crate::config::CpuPreference::High => (cores / 2).clamp(2, 8),
        };
        let budget = ((settings.performance.memory_cache_mb as usize / 2) << 20).max(writer_threads * (24 << 20));
        let (store, index_new) = IndexStore::open(&paths.index_dir, &paths.quarantine_dir, budget, writer_threads)?;
        // Keep the two stores consistent after a rebuild of either one.
        if index_new && !cat_new {
            tracing::warn!("index was rebuilt; all files will be re-indexed");
            catalog.clear_files()?;
        }
        if cat_new && !index_new {
            tracing::warn!("catalog was rebuilt; clearing index to stay consistent");
            store.clear()?;
        }
        let texts = TextStore::open(&paths.text_db, &paths.quarantine_dir)?;
        if index_new || cat_new {
            texts.clear()?;
        }
        let catalog = Arc::new(catalog);
        let store = Arc::new(store);
        let texts = Arc::new(texts);
        let registry = Arc::new(ParserRegistry::new());
        let first_root = catalog.roots()?.first().map(|r| PathBuf::from(&r.path));
        let rotational = is_rotational(first_root.as_deref().unwrap_or(&paths.data_dir));
        let filter = Arc::new(PathFilter::from_settings(&settings.indexing, &paths.data_dir));
        // PDFium is single-threaded per process: use helper processes when possible (parallel
        // and crash-isolated), otherwise a dedicated in-process lane.
        let mut pool = None;
        if let (Some(exe), true) = (&opts.pdf_worker_exe, pdf.pdfium().is_some()) {
            let helpers = (cores / 4).clamp(1, 6);
            let dirs = pdf.library_dir().map(|d| vec![d.to_path_buf()]).unwrap_or_default();
            pool = crate::parsers::pdfworker::configure(exe.clone(), dirs, helpers);
        }
        let pdf_threads = match (&pool, pdf.pdfium().is_some()) {
            (Some(p), _) => p.max_workers(),
            (None, true) => 1,
            // Pure-Rust fallback is thread-safe: PDFs share the general lanes.
            (None, false) => 0,
        };
        let index = IndexService::start(settings.clone(), filter, catalog.clone(), store.clone(), texts.clone(), registry.clone(), rotational, pdf_threads);
        let search = SearchService::new(store.clone(), texts.clone(), registry.clone(), settings.performance.memory_cache_mb);
        // Every commit changes the counts: drop the cache so status is never stale.
        let counts: Arc<Mutex<Option<CountCache>>> = Arc::new(Mutex::new(None));
        {
            let c = counts.clone();
            index.add_listener(move |_| *c.lock() = None);
        }
        tracing::info!(data_dir = %paths.data_dir.display(), pdf = pdf.description(), pdf_helpers = pool.as_ref().map(|p| p.max_workers()).unwrap_or(0), ?rotational, "engine opened");
        let engine = Arc::new(Engine {
            paths,
            settings: RwLock::new(settings),
            catalog,
            store,
            texts,
            registry,
            index,
            search,
            watcher: Mutex::new(None),
            watch_status: Mutex::new(HashMap::new()),
            rotational,
            sizes: Mutex::new(None),
            counts,
            opts,
        });
        engine.sync_watchers();
        if engine.opts.scan_on_start {
            engine.index.rescan_all();
        }
        Ok(engine)
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn update_settings(&self, mut new: Settings) -> Result<Settings> {
        new.validate()?;
        let old = self.settings();
        new.save(&self.paths.settings_file)?;
        *self.settings.write() = new.clone();
        let filter = Arc::new(PathFilter::from_settings(&new.indexing, &self.paths.data_dir));
        self.index.apply_settings(&new, filter);
        self.search.clear_caches();
        if old.indexing.watch_changes != new.indexing.watch_changes || old.performance.background != new.performance.background {
            self.sync_watchers();
        }
        let ocr = &new.indexing.ocr;
        if ocr.enabled && ocr.images && !(old.indexing.ocr.enabled && old.indexing.ocr.images) {
            let n = self.catalog.queue_images_for_ocr()?;
            tracing::info!(images = n, "image OCR enabled; queued existing images");
            self.invalidate_counts();
        }
        let (o, n) = (&old.indexing.ocr, &new.indexing.ocr);
        if n.enabled && (!o.enabled || o.languages != n.languages || o.tesseract_path != n.tesseract_path || n.images != o.images) {
            // A fixed Tesseract path or language list deserves another try at files whose
            // OCR failed (unchanged files are otherwise never looked at again).
            let requeued = self.catalog.requeue_failed_ocr(n.images)?;
            if requeued > 0 {
                tracing::info!(files = requeued, "OCR settings changed; retrying files whose OCR failed");
                self.invalidate_counts();
            }
        }
        self.index.inner.wake_ocr();
        if indexing_changed(&old, &new) {
            tracing::info!("indexing settings changed; reconciling all roots");
            self.index.rescan_all();
        }
        Ok(new)
    }

    /// Start or stop watchers according to settings and the current roots.
    fn sync_watchers(&self) {
        let s = self.settings();
        let want = self.opts.watch && s.indexing.watch_changes && s.performance.background == BackgroundMode::Automatic;
        let mut w = self.watcher.lock();
        if !want {
            if let Some(ws) = w.take() {
                ws.shutdown();
            }
            self.watch_status.lock().clear();
            return;
        }
        if w.is_none() {
            let index = self.index.clone();
            *w = Some(WatcherService::new(Arc::new(move |changes| index.handle_changes(changes))));
        }
        let ws = w.as_ref().unwrap();
        for r in self.catalog.roots().unwrap_or_default() {
            if ws.is_watching(r.id) {
                continue;
            }
            let status = match ws.watch(r.id, Path::new(&r.path)) {
                Ok(()) => "watching".to_string(),
                Err(e) => {
                    tracing::warn!(root = %r.path, error = %e, "cannot watch folder; relying on periodic rescans");
                    format!("not watched: {e}")
                }
            };
            let _ = self.catalog.set_watch_status(r.id, &status);
            self.watch_status.lock().insert(r.id, status);
        }
    }

    pub fn add_root(&self, path: &str) -> Result<RootInfo> {
        let p = simplify_path(std::fs::canonicalize(path).map_err(|e| Error::InvalidInput(format!("cannot open folder: {e}")))?);
        if !p.is_dir() {
            return Err(Error::InvalidInput("not a folder".into()));
        }
        let ps = path_str(&p);
        if is_under(&ps, &path_str(&self.paths.data_dir)) || filter_form(&ps) == filter_form(&path_str(&self.paths.data_dir)) {
            return Err(Error::InvalidInput("the FastFind data folder cannot be indexed".into()));
        }
        let existing = self.catalog.roots()?;
        for r in &existing {
            if filter_form(&r.path) == filter_form(&ps) {
                return Err(Error::InvalidInput("this folder is already being indexed".into()));
            }
            if is_under(&ps, &r.path) {
                return Err(Error::InvalidInput(format!("already included in {}", r.path)));
            }
        }
        let id = self.catalog.add_root(&ps)?;
        // A new root that contains existing roots absorbs them (their files keep their state).
        for r in existing.iter().filter(|r| is_under(&r.path, &ps)) {
            if let Some(w) = self.watcher.lock().as_ref() {
                w.unwatch(r.id);
            }
            self.catalog.reassign_root(r.id, id)?;
            self.catalog.remove_root(r.id)?;
        }
        self.index.refresh_roots();
        self.sync_watchers();
        self.invalidate_counts();
        self.index.request_scan(id, None, true);
        tracing::info!(root = %ps, "folder added");
        Ok(self.roots()?.into_iter().find(|r| r.id == id).unwrap_or(RootInfo { id, path: ps, ..Default::default() }))
    }

    pub fn remove_root(&self, id: i64) -> Result<()> {
        if let Some(w) = self.watcher.lock().as_ref() {
            w.unwatch(id);
        }
        self.watch_status.lock().remove(&id);
        self.index.delete_root(id);
        self.catalog.remove_root(id)?;
        self.index.refresh_roots();
        self.search.clear_caches();
        self.invalidate_counts();
        tracing::info!(root = id, "folder removed");
        Ok(())
    }

    fn cached_counts(&self) -> Result<(HashMap<Status, u64>, HashMap<i64, u64>)> {
        let mut g = self.counts.lock();
        if let Some((t, a, b)) = g.as_ref() {
            if t.elapsed() < Duration::from_secs(2) {
                return Ok((a.clone(), b.clone()));
            }
        }
        let a = self.catalog.status_counts()?;
        let b = self.catalog.root_file_counts()?;
        *g = Some((Instant::now(), a.clone(), b.clone()));
        Ok((a, b))
    }

    fn invalidate_counts(&self) {
        *self.counts.lock() = None;
    }

    pub fn roots(&self) -> Result<Vec<RootInfo>> {
        let (_, counts) = self.cached_counts()?;
        let statuses = self.watch_status.lock().clone();
        Ok(self
            .catalog
            .roots()?
            .into_iter()
            .map(|r| RootInfo {
                file_count: counts.get(&r.id).copied().unwrap_or(0),
                available: Path::new(&r.path).is_dir(),
                scanning: self.index.is_scanning(r.id),
                watch_status: statuses.get(&r.id).cloned().unwrap_or(r.watch_status),
                last_scan_at: r.last_scan_at,
                id: r.id,
                path: r.path,
            })
            .collect())
    }

    /// Reconcile one root (or all) with the file system now.
    pub fn rescan(&self, root: Option<i64>) {
        match root {
            Some(r) => self.index.request_scan(r, None, true),
            None => self.index.rescan_all(),
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.index.set_paused(paused);
    }

    /// Throw the index away and re-index every root from scratch.
    pub fn rebuild(&self) {
        self.index.clear_all();
        self.search.clear_caches();
    }

    pub fn status(&self) -> Result<IndexStatus> {
        let (counts, _) = self.cached_counts()?;
        let c = |s: Status| counts.get(&s).copied().unwrap_or(0);
        let (index_bytes, text_bytes) = {
            let mut g = self.sizes.lock();
            match *g {
                Some((t, a, b)) if t.elapsed() < Duration::from_secs(5) => (a, b),
                _ => {
                    let a = util::dir_size(&self.paths.index_dir) + self.catalog.file_size();
                    let b = self.texts.file_size();
                    *g = Some((Instant::now(), a, b));
                    (a, b)
                }
            }
        };
        let mut progress = self.index.progress();
        progress.ocr_pending = c(Status::NeedsOcr);
        Ok(IndexStatus {
            files_total: counts.iter().filter(|(s, _)| **s != Status::Ignored).map(|(_, n)| n).sum(),
            indexed: c(Status::Indexed),
            name_only: c(Status::NameOnly),
            skipped: c(Status::Skipped),
            failed: c(Status::Failed),
            encrypted: c(Status::Encrypted),
            needs_ocr: c(Status::NeedsOcr),
            ocr_problem: if self.settings.read().indexing.ocr.enabled { self.index.inner.ocr_problem.read().clone() } else { None },
            index_bytes,
            text_store_bytes: text_bytes,
            last_updated: self.catalog.last_update(),
            progress,
            roots: self.roots()?,
            generation: self.store.generation(),
        })
    }

    pub fn search(&self, req: &SearchRequest, cancel: &CancelToken) -> Result<SearchResponse> {
        let s = self.settings.read().search.clone();
        let r = self.search.search(req, &s, cancel);
        if let Ok(resp) = &r {
            tracing::debug!(query_len = req.query.len(), mode = ?req.mode, ms = resp.elapsed_ms, total = resp.total, "search");
        }
        r
    }

    pub fn snippets(&self, req: &SearchRequest, paths: &[String]) -> Result<Vec<SnippetResult>> {
        self.search.snippets(req, &paths[..paths.len().min(200)])
    }

    pub fn preview(&self, req: &SearchRequest, path: &str) -> Result<Preview> {
        self.search.preview(req, path)
    }

    pub fn problems(&self, status: Option<&str>, query: &str, offset: u64, limit: u64) -> Result<Page<SkippedFile>> {
        let st = match status {
            Some(s) if !s.is_empty() => Some(Status::parse(s).ok_or_else(|| Error::InvalidInput(format!("unknown status {s}")))?),
            _ => None,
        };
        self.catalog.problems(st, query, offset, limit.min(1000))
    }

    /// Only indexed paths may be opened/revealed from the UI (defence in depth against a
    /// compromised web view asking the backend to open arbitrary files).
    pub fn is_indexed(&self, path: &str) -> bool {
        self.search.contains(path)
    }

    /// Remember that the user opened something here (directory gets indexing priority).
    pub fn record_open(&self, path: &str) {
        if let Some(parent) = Path::new(path).parent() {
            let _ = self.catalog.record_dir_hit(&filter_form(&path_str(parent)));
        }
    }

    pub fn supported_extensions(&self) -> Vec<&'static str> {
        self.registry.supported_extensions()
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let mut sys = sysinfo::System::new();
        let rss = sysinfo::get_current_pid()
            .ok()
            .and_then(|pid| {
                sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
                sys.process(pid).map(|p| p.memory())
            })
            .unwrap_or(0);
        let s = self.settings();
        Diagnostics {
            version: env!("CARGO_PKG_VERSION").into(),
            data_dir: path_str(&self.paths.data_dir),
            memory_rss_bytes: rss,
            worker_threads: self.index.worker_count(),
            storage_rotational: self.rotational,
            pdf_engine: crate::parsers::pdf::engine().description().into(),
            ocr_engine: crate::ocr::find_tesseract(&s.indexing.ocr.tesseract_path).map(|p| path_str(&p)),
            index_segments: self.store.num_segments(),
            index_docs: self.store.num_docs(),
            query_cache_entries: self.search.cache_len(),
            query_cache_hits: self.search.hits.load(std::sync::atomic::Ordering::Relaxed),
            query_cache_misses: self.search.misses.load(std::sync::atomic::Ordering::Relaxed),
            log_dir: path_str(&self.paths.log_dir),
        }
    }

    pub fn wait_idle(&self, timeout: Duration) -> bool {
        self.index.wait_idle(timeout)
    }

    /// Check the given OCR settings (not necessarily saved yet): Tesseract found and started,
    /// configured languages installed.
    pub fn check_ocr(&self, ocr: &crate::config::OcrSettings) -> crate::ocr::OcrSetup {
        crate::ocr::probe(ocr)
    }

    pub fn shutdown(&self) {
        if let Some(w) = self.watcher.lock().take() {
            w.shutdown();
        }
        self.index.shutdown();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert_eq!(simplify_path(PathBuf::from(r"\\?\C:\Users\x")), PathBuf::from(r"C:\Users\x"));
        assert!(is_under("/a/b/c", "/a/b"));
        assert!(!is_under("/a/bc", "/a/b"));
        assert!(!is_under("/a/b", "/a/b"));
    }
}
