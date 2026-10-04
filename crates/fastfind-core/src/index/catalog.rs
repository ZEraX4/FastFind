//! SQLite catalog: roots, per-file indexing state and directory access statistics.

use std::collections::HashMap;
use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::error::Result;
use crate::model::{Page, SkippedFile};
use crate::util::{now_secs, path_hash};

const CATALOG_VERSION: i64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Status {
    /// Content indexed.
    Indexed = 0,
    /// Only the name is indexed (unsupported format).
    NameOnly = 1,
    /// Skipped by policy (too large, excluded type) — name indexed.
    Skipped = 2,
    /// Extraction failed (corrupt, parser error) — name indexed.
    Failed = 3,
    Encrypted = 4,
    /// Scanned document waiting for OCR — name (and any text layer) indexed.
    NeedsOcr = 5,
    /// Not indexed at all (unsupported and "index all file names" disabled). Tracked so
    /// unchanged files are not re-sniffed on every scan.
    Ignored = 6,
}

impl Status {
    pub fn from_i64(v: i64) -> Status {
        match v {
            0 => Status::Indexed,
            1 => Status::NameOnly,
            2 => Status::Skipped,
            3 => Status::Failed,
            4 => Status::Encrypted,
            5 => Status::NeedsOcr,
            _ => Status::Ignored,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Indexed => "indexed",
            Status::NameOnly => "nameOnly",
            Status::Skipped => "skipped",
            Status::Failed => "failed",
            Status::Encrypted => "encrypted",
            Status::NeedsOcr => "needsOcr",
            Status::Ignored => "ignored",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "indexed" => Status::Indexed,
            "nameOnly" => Status::NameOnly,
            "skipped" => Status::Skipped,
            "failed" => Status::Failed,
            "encrypted" => Status::Encrypted,
            "needsOcr" => Status::NeedsOcr,
            "ignored" => Status::Ignored,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct FileRecord {
    pub root_id: i64,
    pub path: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: Option<u64>,
    pub kind: String,
    pub status: Status,
    pub reason: Option<String>,
    pub flags: u64,
}

#[derive(Clone, Debug)]
pub enum CatOp {
    Upsert(FileRecord),
    Delete { path: String },
    /// Delete all files whose path starts with `prefix` + separator.
    DeleteUnder { prefix: String },
    DeleteRoot { root_id: i64 },
    Touch { path: String, size: u64, mtime_ns: i64 },
}

/// Compact per-file state used by the scan differ.
#[derive(Clone, Copy, Debug)]
pub struct Snap {
    pub id: i64,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct RootRow {
    pub id: i64,
    pub path: String,
    pub last_scan_at: Option<i64>,
    pub watch_status: String,
}

#[derive(Clone, Debug)]
pub struct FileRow {
    pub id: i64,
    pub root_id: i64,
    pub path: String,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: Option<u64>,
    pub status: Status,
    pub flags: u64,
}

pub struct Catalog {
    write: Mutex<Connection>,
    read: Mutex<Connection>,
    path: std::path::PathBuf,
}

fn configure(c: &Connection) -> rusqlite::Result<()> {
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.pragma_update(None, "temp_store", "MEMORY")?;
    c.pragma_update(None, "cache_size", -16_000)?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(())
}

fn corrupt_marker(db: &Path) -> std::path::PathBuf {
    db.with_extension("corrupt")
}

/// Is this a "database is damaged" error (as opposed to busy, I/O, constraint…)?
pub fn is_corruption(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

/// Remember that a database is damaged; it is rebuilt at the next start.
pub fn flag_corrupt(db: &Path) {
    tracing::error!(db = %db.display(), "database corruption detected; it will be rebuilt on the next start");
    let _ = std::fs::write(corrupt_marker(db), b"corrupt");
}

/// Range bounds selecting paths strictly below `dir` (either separator style).
fn under_bounds(dir: &str) -> (String, String) {
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    let base = dir.trim_end_matches(['/', '\\']);
    let lo = format!("{base}{sep}");
    let hi = format!("{base}{}", (sep as u8 + 1) as char);
    (lo, hi)
}

impl Catalog {
    /// Open or create. Returns `(catalog, recreated)`; an unreadable database is quarantined.
    pub fn open(path: &Path, quarantine_dir: &Path) -> Result<(Self, bool)> {
        let mut recreated = !path.exists();
        // Cheap start-up check (header, schema, version row). A full `quick_check` would read the
        // whole database — seconds for millions of files — so deeper corruption is caught when
        // SQLite reports it at runtime, which leaves a marker that triggers a rebuild here.
        let marker = corrupt_marker(path);
        let healthy = |p: &Path| -> bool {
            if marker.exists() {
                return false;
            }
            match Connection::open(p) {
                Ok(c) => {
                    let version: rusqlite::Result<i64> = c.query_row("SELECT value FROM meta WHERE key='version'", [], |r| r.get::<_, String>(0)).map(|v| v.parse().unwrap_or(0));
                    version.map(|v| v == CATALOG_VERSION).unwrap_or(false)
                }
                Err(_) => false,
            }
        };
        if path.exists() && !healthy(path) {
            let _ = std::fs::remove_file(&marker);
            tracing::error!(path = %path.display(), "catalog corrupt or outdated; rebuilding");
            super::store::quarantine(path, quarantine_dir)?;
            for ext in ["-wal", "-shm"] {
                let side = path.with_file_name(format!("{}{ext}", path.file_name().unwrap_or_default().to_string_lossy()));
                let _ = std::fs::remove_file(side);
            }
            recreated = true;
        }
        let write = Connection::open(path)?;
        configure(&write)?;
        write.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS roots(
                id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, added_at INTEGER NOT NULL,
                last_scan_at INTEGER, last_scan_ms INTEGER, watch_status TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS files(
                id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL, path TEXT UNIQUE NOT NULL,
                size INTEGER NOT NULL, mtime INTEGER NOT NULL, hash INTEGER, kind TEXT NOT NULL,
                status INTEGER NOT NULL, reason TEXT, flags INTEGER NOT NULL DEFAULT 0,
                indexed_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS files_root ON files(root_id);
             CREATE INDEX IF NOT EXISTS files_status ON files(status);
             CREATE TABLE IF NOT EXISTS dir_hits(dir TEXT PRIMARY KEY, hits INTEGER NOT NULL, last_at INTEGER NOT NULL);",
        )?;
        write.execute("INSERT OR IGNORE INTO meta(key, value) VALUES('version', ?1)", [CATALOG_VERSION.to_string()])?;
        let read = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        configure(&read).ok();
        Ok((Self { write: Mutex::new(write), read: Mutex::new(read), path: path.to_path_buf() }, recreated))
    }

    pub fn db_path(&self) -> &Path {
        &self.path
    }

    // ----- roots ------------------------------------------------------------------------

    pub fn add_root(&self, path: &str) -> Result<i64> {
        let c = self.write.lock();
        c.execute("INSERT OR IGNORE INTO roots(path, added_at) VALUES(?1, ?2)", params![path, now_secs()])?;
        Ok(c.query_row("SELECT id FROM roots WHERE path=?1", [path], |r| r.get(0))?)
    }

    pub fn remove_root(&self, id: i64) -> Result<()> {
        self.write.lock().execute("DELETE FROM roots WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn roots(&self) -> Result<Vec<RootRow>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT id, path, last_scan_at, watch_status FROM roots ORDER BY path")?;
        let rows = st.query_map([], |r| Ok(RootRow { id: r.get(0)?, path: r.get(1)?, last_scan_at: r.get(2)?, watch_status: r.get(3)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Move all files of root `from` to root `to` (used when a new root contains an old one).
    pub fn reassign_root(&self, from: i64, to: i64) -> Result<()> {
        self.write.lock().execute("UPDATE files SET root_id=?2 WHERE root_id=?1", params![from, to])?;
        Ok(())
    }

    pub fn set_root_scanned(&self, id: i64, at: i64, ms: i64) -> Result<()> {
        self.write.lock().execute("UPDATE roots SET last_scan_at=?2, last_scan_ms=?3 WHERE id=?1", params![id, at, ms])?;
        Ok(())
    }

    pub fn set_watch_status(&self, id: i64, status: &str) -> Result<()> {
        self.write.lock().execute("UPDATE roots SET watch_status=?2 WHERE id=?1", params![id, status])?;
        Ok(())
    }

    // ----- files ------------------------------------------------------------------------

    /// Change-detection snapshot of a root (or of the subtree `under`), keyed by path hash.
    pub fn snapshot(&self, root_id: i64, under: Option<&str>) -> Result<HashMap<u64, Snap>> {
        let c = self.read.lock();
        let mut map = HashMap::new();
        let mut push = |r: &rusqlite::Row| -> rusqlite::Result<()> {
            let path: String = r.get(1)?;
            map.insert(path_hash(&path), Snap { id: r.get(0)?, size: r.get::<_, i64>(2)? as u64, mtime_ns: r.get(3)?, hash: r.get::<_, Option<i64>>(4)?.map(|h| h as u64) });
            Ok(())
        };
        match under {
            None => {
                let mut st = c.prepare_cached("SELECT id, path, size, mtime, hash FROM files WHERE root_id=?1")?;
                let mut rows = st.query([root_id])?;
                while let Some(r) = rows.next()? {
                    push(r)?;
                }
            }
            Some(dir) => {
                let (lo, hi) = under_bounds(dir);
                let mut st = c.prepare_cached("SELECT id, path, size, mtime, hash FROM files WHERE path >= ?1 AND path < ?2")?;
                let mut rows = st.query(params![lo, hi])?;
                while let Some(r) = rows.next()? {
                    push(r)?;
                }
            }
        }
        Ok(map)
    }

    pub fn paths_by_ids(&self, ids: &[i64]) -> Result<Vec<(i64, String, u64, i64)>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT path, size, mtime FROM files WHERE id=?1")?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some((p, s, m)) = st.query_row([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))).optional()? {
                out.push((*id, p, s as u64, m));
            }
        }
        Ok(out)
    }

    pub fn get(&self, path: &str) -> Result<Option<FileRow>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT id, root_id, path, size, mtime, hash, status, flags FROM files WHERE path=?1")?;
        Ok(st
            .query_row([path], |r| {
                Ok(FileRow {
                    id: r.get(0)?,
                    root_id: r.get(1)?,
                    path: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                    mtime_ns: r.get(4)?,
                    hash: r.get::<_, Option<i64>>(5)?.map(|h| h as u64),
                    status: Status::from_i64(r.get(6)?),
                    flags: r.get::<_, i64>(7)? as u64,
                })
            })
            .optional()?)
    }

    /// Paths strictly below a directory (for directory removal).
    pub fn paths_under(&self, dir: &str) -> Result<Vec<String>> {
        let (lo, hi) = under_bounds(dir);
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT path FROM files WHERE path >= ?1 AND path < ?2")?;
        let rows = st.query_map(params![lo, hi], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn paths_of_root(&self, root_id: i64) -> Result<Vec<String>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT path FROM files WHERE root_id=?1")?;
        let rows = st.query_map([root_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Apply a batch atomically.
    pub fn apply(&self, ops: &[CatOp]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut c = self.write.lock();
        let tx = c.transaction()?;
        {
            let now = now_secs();
            let mut up = tx.prepare_cached(
                "INSERT INTO files(root_id, path, size, mtime, hash, kind, status, reason, flags, indexed_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(path) DO UPDATE SET root_id=excluded.root_id, size=excluded.size, mtime=excluded.mtime,
                   hash=excluded.hash, kind=excluded.kind, status=excluded.status, reason=excluded.reason,
                   flags=excluded.flags, indexed_at=excluded.indexed_at",
            )?;
            let mut del = tx.prepare_cached("DELETE FROM files WHERE path=?1")?;
            let mut del_under = tx.prepare_cached("DELETE FROM files WHERE path >= ?1 AND path < ?2")?;
            let mut del_root = tx.prepare_cached("DELETE FROM files WHERE root_id=?1")?;
            let mut touch = tx.prepare_cached("UPDATE files SET size=?2, mtime=?3 WHERE path=?1")?;
            for op in ops {
                match op {
                    CatOp::Upsert(f) => {
                        up.execute(params![
                            f.root_id, f.path, f.size as i64, f.mtime_ns, f.hash.map(|h| h as i64), f.kind,
                            f.status as u8 as i64, f.reason, f.flags as i64, now
                        ])?;
                    }
                    CatOp::Delete { path } => {
                        del.execute([path])?;
                    }
                    CatOp::DeleteUnder { prefix } => {
                        let (lo, hi) = under_bounds(prefix);
                        del_under.execute(params![lo, hi])?;
                    }
                    CatOp::DeleteRoot { root_id } => {
                        del_root.execute([root_id])?;
                    }
                    CatOp::Touch { path, size, mtime_ns } => {
                        touch.execute(params![path, *size as i64, mtime_ns])?;
                    }
                }
            }
        }
        tx.execute("INSERT OR REPLACE INTO meta(key, value) VALUES('last_update', ?1)", [now_secs().to_string()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn clear_files(&self) -> Result<()> {
        self.write.lock().execute_batch("DELETE FROM files;")?;
        Ok(())
    }

    // ----- statistics -------------------------------------------------------------------

    pub fn status_counts(&self) -> Result<HashMap<Status, u64>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT status, COUNT(*) FROM files GROUP BY status")?;
        let rows = st.query_map([], |r| Ok((Status::from_i64(r.get(0)?), r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn root_file_counts(&self) -> Result<HashMap<i64, u64>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT root_id, COUNT(*) FROM files WHERE status != 6 GROUP BY root_id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn last_update(&self) -> Option<i64> {
        let c = self.read.lock();
        c.query_row("SELECT value FROM meta WHERE key='last_update'", [], |r| r.get::<_, String>(0))
            .ok()
            .and_then(|v| v.parse().ok())
    }

    /// Files that were not fully indexed, for the "Skipped files" view.
    pub fn problems(&self, status: Option<Status>, query: &str, offset: u64, limit: u64) -> Result<Page<SkippedFile>> {
        let c = self.read.lock();
        let statuses: Vec<i64> = match status {
            Some(s) => vec![s as u8 as i64],
            None => vec![Status::Skipped as i64, Status::Failed as i64, Status::Encrypted as i64, Status::NeedsOcr as i64],
        };
        let list = statuses.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",");
        let like = format!("%{}%", query.replace(['%', '_'], ""));
        let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM files WHERE status IN ({list}) AND path LIKE ?1"), [&like], |r| r.get(0))?;
        let mut st = c.prepare(&format!(
            "SELECT path, status, reason, size, mtime FROM files WHERE status IN ({list}) AND path LIKE ?1 ORDER BY path LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = st.query_map(params![like, limit as i64, offset as i64], |r| {
            Ok(SkippedFile {
                path: r.get(0)?,
                status: Status::from_i64(r.get(1)?).as_str().to_string(),
                reason: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                modified: r.get::<_, i64>(4)? / 1_000_000_000,
            })
        })?;
        Ok(Page { items: rows.collect::<rusqlite::Result<_>>()?, total: total as u64 })
    }

    pub fn pending_ocr(&self, limit: u64) -> Result<Vec<FileRow>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT id, root_id, path, size, mtime, hash, status, flags FROM files WHERE status=5 ORDER BY size LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(FileRow {
                id: r.get(0)?,
                root_id: r.get(1)?,
                path: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mtime_ns: r.get(4)?,
                hash: r.get::<_, Option<i64>>(5)?.map(|h| h as u64),
                status: Status::from_i64(r.get(6)?),
                flags: r.get::<_, i64>(7)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Queue already indexed images for OCR (when "also recognise text in images" is turned
    /// on, unchanged images would otherwise never be looked at again). Returns rows changed.
    pub fn queue_images_for_ocr(&self) -> Result<usize> {
        let n = self.write.lock().execute(
            "UPDATE files SET status=5, reason='image: waiting for OCR' WHERE kind='image' AND status=1",
            [],
        )?;
        Ok(n)
    }

    /// Put files whose OCR failed back in the OCR queue (after the OCR settings change, so a
    /// fixed Tesseract path or language list gets another try). Images only when image OCR is
    /// on. Returns rows changed.
    pub fn requeue_failed_ocr(&self, images: bool) -> Result<usize> {
        self.requeue_failed_ocr_like("OCR failed:%", images)
    }

    /// Like `requeue_failed_ocr`, limited to failures whose reason matches the SQL `LIKE`
    /// pattern (used to retry failures caused by a since-fixed bug).
    pub fn requeue_failed_ocr_like(&self, reason_like: &str, images: bool) -> Result<usize> {
        let n = self.write.lock().execute(
            "UPDATE files SET status=5, reason='waiting for OCR (retry)', flags=flags|?1
             WHERE status=3 AND reason LIKE 'OCR failed:%' AND reason LIKE ?3 AND (kind<>'image' OR ?2)",
            rusqlite::params![crate::model::flags::NEEDS_OCR as i64, images, reason_like],
        )?;
        Ok(n)
    }

    pub fn count_pending_ocr(&self) -> u64 {
        let c = self.read.lock();
        c.query_row("SELECT COUNT(*) FROM files WHERE status=5", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64
    }

    // ----- directory access statistics (indexing priority) --------------------------------

    pub fn record_dir_hit(&self, dir: &str) -> Result<()> {
        self.write.lock().execute(
            "INSERT INTO dir_hits(dir, hits, last_at) VALUES(?1, 1, ?2)
             ON CONFLICT(dir) DO UPDATE SET hits = hits + 1, last_at = excluded.last_at",
            params![dir, now_secs()],
        )?;
        Ok(())
    }

    pub fn hot_dirs(&self, limit: u64) -> Result<Vec<String>> {
        let c = self.read.lock();
        let mut st = c.prepare_cached("SELECT dir FROM dir_hits ORDER BY hits DESC, last_at DESC LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn file_size(&self) -> u64 {
        let c = self.read.lock();
        c.path()
            .map(|p| {
                let base = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                let wal = std::fs::metadata(format!("{p}-wal")).map(|m| m.len()).unwrap_or(0);
                base + wal
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, size: u64) -> FileRecord {
        FileRecord { root_id: 1, path: path.into(), size, mtime_ns: 5, hash: Some(9), kind: "text".into(), status: Status::Indexed, reason: None, flags: 0 }
    }

    #[test]
    fn upsert_snapshot_and_prefix_delete() {
        let dir = tempfile::tempdir().unwrap();
        let (c, created) = Catalog::open(&dir.path().join("c.db"), &dir.path().join("q")).unwrap();
        assert!(created);
        let root = c.add_root("/r").unwrap();
        assert_eq!(root, c.add_root("/r").unwrap());
        c.apply(&[CatOp::Upsert(rec("/r/a/1.txt", 1)), CatOp::Upsert(rec("/r/a/2.txt", 2)), CatOp::Upsert(rec("/r/ab.txt", 3))]).unwrap();
        c.apply(&[CatOp::Upsert(rec("/r/a/1.txt", 10))]).unwrap();
        let snap = c.snapshot(1, None).unwrap();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[&path_hash("/r/a/1.txt")].size, 10);
        assert_eq!(c.snapshot(1, Some("/r/a")).unwrap().len(), 2);
        assert_eq!(c.paths_under("/r/a").unwrap().len(), 2);
        c.apply(&[CatOp::DeleteUnder { prefix: "/r/a".into() }]).unwrap();
        assert_eq!(c.snapshot(1, None).unwrap().len(), 1, "sibling /r/ab.txt must survive");
    }

    #[test]
    fn failed_ocr_is_requeued() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _) = Catalog::open(&dir.path().join("c.db"), &dir.path().join("q")).unwrap();
        c.add_root("/r").unwrap();
        let failed = |path: &str, kind: &str, reason: &str| FileRecord {
            kind: kind.into(),
            status: Status::Failed,
            reason: Some(reason.into()),
            ..rec(path, 1)
        };
        c.apply(&[
            CatOp::Upsert(failed("/r/scan.pdf", "pdf", "OCR failed: tesseract exited with exit code: 1")),
            CatOp::Upsert(failed("/r/photo.png", "image", "OCR failed: OCR timed out")),
            CatOp::Upsert(failed("/r/broken.docx", "word", "invalid zip archive")),
        ])
        .unwrap();
        assert_eq!(c.requeue_failed_ocr(false).unwrap(), 1, "PDF only while image OCR is off");
        assert_eq!(c.count_pending_ocr(), 1);
        let row = c.pending_ocr(5).unwrap().remove(0);
        assert_eq!(row.path, "/r/scan.pdf");
        assert_ne!(row.flags & crate::model::flags::NEEDS_OCR, 0);
        assert_eq!(c.requeue_failed_ocr(true).unwrap(), 1, "then the image");
        assert_eq!(c.count_pending_ocr(), 2, "a parser failure is not an OCR failure");
    }

    #[test]
    fn corrupt_catalog_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.db");
        std::fs::write(&p, b"this is not a sqlite database at all").unwrap();
        let (c, recreated) = Catalog::open(&p, &dir.path().join("q")).unwrap();
        assert!(recreated);
        c.add_root("/x").unwrap();
        assert_eq!(std::fs::read_dir(dir.path().join("q")).unwrap().count(), 1);
    }
}
