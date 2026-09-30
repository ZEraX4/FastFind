//! Cross-platform change notifications (ReadDirectoryChangesW / FSEvents / inotify via
//! `notify`) with debouncing.
//!
//! Raw events are coalesced per path; a path is emitted once it has been quiet for
//! [`QUIET`]. Each emitted path is classified by looking at the file system *now* (exists as a
//! file → upsert, as a directory → scan that subtree, missing → remove), which makes the result
//! robust against the many platform differences in event kinds. Queue overflows and backend
//! errors turn into a targeted rescan of the affected root.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;

const QUIET: Duration = Duration::from_millis(600);
const TICK: Duration = Duration::from_millis(200);
/// Above this many pending paths for one root, a subtree rescan is cheaper than per-file work.
const STORM_THRESHOLD: usize = 20_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Upsert { root: i64, path: PathBuf },
    Remove { root: i64, path: PathBuf },
    Rename { root: i64, from: PathBuf, to: PathBuf },
    ScanDir { root: i64, path: PathBuf },
    Rescan { root: i64 },
}

enum Msg {
    Event(i64, notify::Result<Event>),
    Stop,
}

pub type ChangeHandler = Arc<dyn Fn(Vec<Change>) + Send + Sync>;

pub struct WatcherService {
    tx: Sender<Msg>,
    watchers: Mutex<HashMap<i64, RecommendedWatcher>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl WatcherService {
    pub fn new(handler: ChangeHandler) -> Self {
        let (tx, rx) = unbounded();
        let t = std::thread::Builder::new()
            .name("ff-watch-debounce".into())
            .spawn(move || debounce_loop(rx, handler))
            .expect("spawn watcher thread");
        Self { tx, watchers: Mutex::new(HashMap::new()), thread: Mutex::new(Some(t)) }
    }

    /// Start watching `path` recursively. Errors (e.g. inotify watch limit) are returned so
    /// the caller can fall back to periodic rescans.
    pub fn watch(&self, root: i64, path: &Path) -> Result<(), String> {
        let tx = self.tx.clone();
        let mut w = notify::recommended_watcher(move |res| {
            let _ = tx.send(Msg::Event(root, res));
        })
        .map_err(|e| e.to_string())?;
        w.watch(path, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
        self.watchers.lock().insert(root, w);
        Ok(())
    }

    pub fn unwatch(&self, root: i64) {
        self.watchers.lock().remove(&root);
    }

    pub fn is_watching(&self, root: i64) -> bool {
        self.watchers.lock().contains_key(&root)
    }

    pub fn shutdown(&self) {
        self.watchers.lock().clear();
        let _ = self.tx.send(Msg::Stop);
        if let Some(t) = self.thread.lock().take() {
            let _ = t.join();
        }
    }
}

impl Drop for WatcherService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Default)]
struct Pending {
    paths: HashMap<PathBuf, (i64, Instant)>,
    renames: Vec<(i64, PathBuf, PathBuf, Instant)>,
    /// Windows/macOS report the two halves of a rename separately.
    rename_from: Option<(i64, PathBuf, Instant)>,
    rescans: HashMap<i64, Instant>,
}

fn debounce_loop(rx: Receiver<Msg>, handler: ChangeHandler) {
    let mut p = Pending::default();
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Msg::Stop) => return,
            Ok(Msg::Event(root, res)) => record(&mut p, root, res),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Drain whatever else is queued before flushing.
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Stop => return,
                Msg::Event(root, res) => record(&mut p, root, res),
            }
        }
        let out = flush(&mut p, Instant::now());
        if !out.is_empty() {
            handler(out);
        }
    }
}

fn record(p: &mut Pending, root: i64, res: notify::Result<Event>) {
    let now = Instant::now();
    let ev = match res {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(root, error = %e, "file watcher error; scheduling rescan");
            p.rescans.insert(root, now);
            return;
        }
    };
    if ev.need_rescan() {
        p.rescans.insert(root, now);
        return;
    }
    match ev.kind {
        EventKind::Access(_) => {}
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
            p.renames.push((root, ev.paths[0].clone(), ev.paths[1].clone(), now));
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) if ev.paths.len() == 1 => {
            if let Some((r, old, t)) = p.rename_from.take() {
                p.paths.insert(old, (r, t));
            }
            p.rename_from = Some((root, ev.paths[0].clone(), now));
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) if ev.paths.len() == 1 => match p.rename_from.take() {
            Some((r, from, _)) if r == root => p.renames.push((root, from, ev.paths[0].clone(), now)),
            other => {
                if let Some((r, old, t)) = other {
                    p.paths.insert(old, (r, t));
                }
                p.paths.insert(ev.paths[0].clone(), (root, now));
            }
        },
        _ => {
            for path in ev.paths {
                p.paths.insert(path, (root, now));
            }
        }
    }
}

fn flush(p: &mut Pending, now: Instant) -> Vec<Change> {
    let mut out = Vec::new();
    // Rescans supersede per-path work for the same root.
    let ready_rescans: Vec<i64> = p.rescans.iter().filter(|(_, t)| now - **t >= QUIET).map(|(r, _)| *r).collect();
    for r in &ready_rescans {
        p.rescans.remove(r);
        p.paths.retain(|_, (root, _)| root != r);
        p.renames.retain(|(root, ..)| root != r);
        out.push(Change::Rescan { root: *r });
    }
    // Event storms: collapse into a rescan.
    let mut per_root: HashMap<i64, usize> = HashMap::new();
    for (root, _) in p.paths.values() {
        *per_root.entry(*root).or_default() += 1;
    }
    for (root, n) in per_root {
        if n > STORM_THRESHOLD {
            p.paths.retain(|_, (r, _)| *r != root);
            p.rescans.insert(root, now);
        }
    }
    if let Some((r, from, t)) = p.rename_from.take() {
        if now - t >= QUIET {
            p.paths.insert(from, (r, t));
        } else {
            p.rename_from = Some((r, from, t));
        }
    }
    let mut keep = Vec::new();
    for (root, from, to, t) in p.renames.drain(..) {
        if now - t < QUIET {
            keep.push((root, from, to, t));
            continue;
        }
        match std::fs::metadata(&to) {
            Ok(md) if md.is_file() => out.push(Change::Rename { root, from, to }),
            Ok(md) if md.is_dir() => {
                out.push(Change::Remove { root, path: from });
                out.push(Change::ScanDir { root, path: to });
            }
            _ => out.push(Change::Remove { root, path: from }),
        }
    }
    p.renames = keep;
    let ready: Vec<PathBuf> = p.paths.iter().filter(|(_, (_, t))| now - *t >= QUIET).map(|(k, _)| k.clone()).collect();
    for path in ready {
        let (root, _) = p.paths.remove(&path).unwrap_or((0, now));
        match std::fs::symlink_metadata(&path) {
            Ok(md) if md.is_file() => out.push(Change::Upsert { root, path }),
            Ok(md) if md.is_dir() => out.push(Change::ScanDir { root, path }),
            Ok(_) => {}
            Err(_) => out.push(Change::Remove { root, path }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_classifies_by_current_state() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let gone = dir.path().join("gone.txt");
        let mut p = Pending::default();
        let t0 = Instant::now();
        for path in [&file, &file, &gone] {
            record(&mut p, 1, Ok(Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path.clone())));
        }
        assert!(flush(&mut p, t0).is_empty(), "not quiet yet");
        let mut out = flush(&mut p, t0 + QUIET * 2);
        out.sort_by_key(|c| format!("{c:?}"));
        assert_eq!(out.len(), 2);
        assert!(out.contains(&Change::Upsert { root: 1, path: file.clone() }));
        assert!(out.contains(&Change::Remove { root: 1, path: gone }));
    }

    #[test]
    fn pairs_split_renames_and_rescans_win() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("new.txt");
        std::fs::write(&to, b"x").unwrap();
        let from = dir.path().join("old.txt");
        let mut p = Pending::default();
        record(&mut p, 1, Ok(Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From))).add_path(from.clone())));
        record(&mut p, 1, Ok(Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To))).add_path(to.clone())));
        let out = flush(&mut p, Instant::now() + QUIET * 2);
        assert_eq!(out, vec![Change::Rename { root: 1, from, to }]);

        record(&mut p, 2, Ok(Event::new(EventKind::Create(notify::event::CreateKind::Any)).add_path(dir.path().join("x"))));
        record(&mut p, 2, Err(notify::Error::generic("overflow")));
        let out = flush(&mut p, Instant::now() + QUIET * 2);
        assert_eq!(out, vec![Change::Rescan { root: 2 }]);
    }
}
