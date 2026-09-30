//! Parallel directory traversal.
//!
//! * One `read_dir` per directory; the entry's file type and (on Windows) its metadata come
//!   from the same `FindNextFile` record, so no extra system calls per file.
//! * Excluded directories are pruned before descent.
//! * Work is a shared priority queue of directories: "hot" directories (recently opened from
//!   search results) first, then depth-first (bounded memory, good locality).
//! * Symbolic links / junctions are skipped unless following is enabled; when following, a set
//!   of canonical directory paths prevents loops.
//! * A directory that cannot be read is reported in `failed_dirs` so the caller never deletes
//!   index entries below it just because it was temporarily unreadable.

use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use super::PathFilter;
use crate::util::{filter_form, system_time_ns, CancelToken};

const MAX_DEPTH: u32 = 512;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
}

#[derive(Clone)]
pub struct ScanOptions {
    pub filter: Arc<PathFilter>,
    pub threads: usize,
    pub follow_symlinks: bool,
    /// `filter_form` of directories to visit first.
    pub hot_dirs: Arc<HashSet<String>>,
}

#[derive(Debug, Default, Clone)]
pub struct ScanSummary {
    pub dirs: u64,
    pub files: u64,
    pub errors: u64,
    pub failed_dirs: Vec<PathBuf>,
    pub cancelled: bool,
}

struct DirTask {
    hot: bool,
    depth: u32,
    seq: u64,
    path: PathBuf,
}

impl PartialEq for DirTask {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == CmpOrdering::Equal
    }
}
impl Eq for DirTask {}
impl PartialOrd for DirTask {
    fn partial_cmp(&self, o: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(o))
    }
}
impl Ord for DirTask {
    fn cmp(&self, o: &Self) -> CmpOrdering {
        (self.hot, self.depth, self.seq).cmp(&(o.hot, o.depth, o.seq))
    }
}

struct Shared {
    queue: BinaryHeap<DirTask>,
    active: usize,
    seq: u64,
    summary: ScanSummary,
}

#[cfg(windows)]
fn attrs(md: &fs::Metadata) -> (bool, bool, bool) {
    use std::os::windows::fs::MetadataExt;
    let a = md.file_attributes();
    // (hidden, system, reparse point)
    (a & 0x2 != 0, a & 0x4 != 0, a & 0x400 != 0)
}

#[cfg(not(windows))]
fn attrs(_md: &fs::Metadata) -> (bool, bool, bool) {
    (false, false, false)
}

/// Scan `root`, calling `on_file` for every file that passes the filter. `on_file` may be
/// called concurrently from several threads.
pub fn scan<F>(root: &Path, opts: &ScanOptions, cancel: &CancelToken, on_file: F) -> ScanSummary
where
    F: Fn(FileEntry) + Sync,
{
    let shared = Mutex::new(Shared {
        queue: BinaryHeap::new(),
        active: 0,
        seq: 0,
        summary: ScanSummary::default(),
    });
    let cv = Condvar::new();
    let visited: Mutex<HashSet<PathBuf>> = Mutex::new(HashSet::new());
    if opts.follow_symlinks {
        if let Ok(c) = fs::canonicalize(root) {
            visited.lock().insert(c);
        }
    }
    shared.lock().queue.push(DirTask { hot: false, depth: 0, seq: 0, path: root.to_path_buf() });
    let threads = opts.threads.clamp(1, 64);

    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| worker(&shared, &cv, &visited, opts, cancel, &on_file));
        }
    });
    let mut summary = std::mem::take(&mut shared.lock().summary);
    summary.cancelled = cancel.is_cancelled();
    summary
}

fn worker<F: Fn(FileEntry) + Sync>(
    shared: &Mutex<Shared>,
    cv: &Condvar,
    visited: &Mutex<HashSet<PathBuf>>,
    opts: &ScanOptions,
    cancel: &CancelToken,
    on_file: &F,
) {
    loop {
        let task = {
            let mut g = shared.lock();
            loop {
                if cancel.is_cancelled() {
                    g.queue.clear();
                }
                if let Some(t) = g.queue.pop() {
                    g.active += 1;
                    break t;
                }
                if g.active == 0 {
                    cv.notify_all();
                    return;
                }
                cv.wait(&mut g);
            }
        };
        let mut subdirs = Vec::new();
        let mut files = 0u64;
        let mut errors = 0u64;
        let mut failed = None;
        match fs::read_dir(&task.path) {
            Ok(rd) => {
                for entry in rd {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let Ok(entry) = entry else {
                        errors += 1;
                        continue;
                    };
                    let Ok(ft) = entry.file_type() else {
                        errors += 1;
                        continue;
                    };
                    let name_os = entry.file_name();
                    let name = name_os.to_string_lossy();
                    let path = entry.path();
                    if ft.is_symlink() {
                        if !opts.follow_symlinks {
                            continue;
                        }
                        // Follow: stat the target (broken links simply fail and are skipped).
                        match fs::metadata(&path) {
                            Ok(md) if md.is_dir() => {
                                if task.depth < MAX_DEPTH && opts.filter.dir_allowed(&name, &path, false, false) {
                                    if let Ok(real) = fs::canonicalize(&path) {
                                        if visited.lock().insert(real) {
                                            subdirs.push(path);
                                        }
                                    }
                                }
                            }
                            Ok(md) if md.is_file()
                                && opts.filter.file_allowed(&name, false, false) => {
                                    files += 1;
                                    on_file(FileEntry { path, size: md.len(), mtime_ns: md.modified().map(system_time_ns).unwrap_or(0) });
                                }
                            _ => {}
                        }
                        continue;
                    }
                    if ft.is_dir() {
                        let (hidden, system, reparse) = entry.metadata().map(|m| attrs(&m)).unwrap_or_default();
                        if reparse && !opts.follow_symlinks {
                            continue;
                        }
                        if task.depth < MAX_DEPTH && opts.filter.dir_allowed(&name, &path, hidden, system) {
                            if opts.follow_symlinks {
                                if let Ok(real) = fs::canonicalize(&path) {
                                    if !visited.lock().insert(real) {
                                        continue;
                                    }
                                }
                            }
                            subdirs.push(path);
                        }
                    } else if ft.is_file() {
                        let Ok(md) = entry.metadata() else {
                            // Vanished between listing and stat, or permission denied.
                            errors += 1;
                            continue;
                        };
                        let (hidden, system, _) = attrs(&md);
                        if !opts.filter.file_allowed(&name, hidden, system) {
                            continue;
                        }
                        files += 1;
                        on_file(FileEntry { path, size: md.len(), mtime_ns: md.modified().map(system_time_ns).unwrap_or(0) });
                    }
                    // Sockets, FIFOs, devices: never indexed.
                }
            }
            Err(e) => {
                tracing::debug!(dir = %task.path.display(), error = %e, "cannot read directory");
                errors += 1;
                failed = Some(task.path.clone());
            }
        }
        let mut g = shared.lock();
        g.active -= 1;
        g.summary.dirs += 1;
        g.summary.files += files;
        g.summary.errors += errors;
        if let Some(f) = failed {
            g.summary.failed_dirs.push(f);
        }
        for d in subdirs {
            g.seq += 1;
            let hot = !opts.hot_dirs.is_empty() && opts.hot_dirs.contains(&filter_form(&d.to_string_lossy()));
            let seq = g.seq;
            g.queue.push(DirTask { hot: hot || task.hot, depth: task.depth + 1, seq, path: d });
        }
        cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IndexingSettings;

    #[test]
    fn scans_prunes_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for p in ["a/b/c", "a/node_modules/x", "d", ".hidden"] {
            fs::create_dir_all(r.join(p)).unwrap();
        }
        for f in ["a/1.txt", "a/b/2.txt", "a/b/c/3.txt", "a/node_modules/x/4.js", "d/5.md", ".hidden/6.txt", ".dotfile"] {
            fs::write(r.join(f), b"x").unwrap();
        }
        let filter = Arc::new(PathFilter::from_settings(&IndexingSettings::default(), Path::new("/nonexistent")));
        let opts = ScanOptions { filter, threads: 4, follow_symlinks: false, hot_dirs: Arc::new(HashSet::new()) };
        let found = Mutex::new(Vec::new());
        let summary = scan(r, &opts, &CancelToken::new(), |e| found.lock().push(e.path));
        let mut names: Vec<String> = found.into_inner().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["1.txt", "2.txt", "3.txt", "5.md"]);
        assert_eq!(summary.files, 4);
        assert!(summary.failed_dirs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("a")).unwrap();
        fs::write(r.join("a/f.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(r, r.join("a/loop")).unwrap();
        let filter = Arc::new(PathFilter::from_settings(&IndexingSettings::default(), Path::new("/nonexistent")));
        let opts = ScanOptions { filter, threads: 2, follow_symlinks: true, hot_dirs: Arc::new(HashSet::new()) };
        let n = std::sync::atomic::AtomicUsize::new(0);
        scan(r, &opts, &CancelToken::new(), |_| {
            n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        assert_eq!(n.into_inner(), 1);
    }
}
