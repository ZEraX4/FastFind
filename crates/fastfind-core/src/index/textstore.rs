//! Extracted-text cache for binary formats (PDF, Office, ODF, EPUB, RTF).
//!
//! One zstd-compressed blob per document keyed by `xxh3(path)`:
//! `[u32 LE text length][UTF-8 text][location-anchor JSON]`. Compression happens in the parser
//! workers (parallel); the single writer only inserts bytes.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::error::{Error, Result};
use crate::parsers::{locmap, Loc};

pub struct TextStore {
    write: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    path: std::path::PathBuf,
}

pub enum TextOp {
    Put { key: u64, blob: Vec<u8> },
    Delete { key: u64 },
}

pub fn encode(text: &str, locs: &[Loc]) -> Vec<u8> {
    let loc_bytes = locmap::encode(locs);
    let mut raw = Vec::with_capacity(4 + text.len() + loc_bytes.len());
    raw.extend_from_slice(&(text.len() as u32).to_le_bytes());
    raw.extend_from_slice(text.as_bytes());
    raw.extend_from_slice(&loc_bytes);
    zstd::bulk::compress(&raw, 3).unwrap_or_default()
}

pub fn decode(blob: &[u8]) -> Option<(String, Vec<Loc>)> {
    // Decompressed size is bounded: stored text is capped by settings (≤ 64 MB) plus anchors.
    let raw = zstd::bulk::decompress(blob, 80 << 20).ok()?;
    let n = u32::from_le_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let text = String::from_utf8(raw.get(4..4 + n)?.to_vec()).ok()?;
    let locs = locmap::decode(raw.get(4 + n..).unwrap_or(&[]));
    Some((text, locs))
}

impl TextStore {
    pub fn open(path: &Path, quarantine_dir: &Path) -> Result<Self> {
        let open = |p: &Path| -> rusqlite::Result<Connection> {
            let c = Connection::open(p)?;
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.pragma_update(None, "synchronous", "NORMAL")?;
            c.busy_timeout(std::time::Duration::from_secs(10))?;
            c.execute_batch("CREATE TABLE IF NOT EXISTS texts(key INTEGER PRIMARY KEY, data BLOB NOT NULL);")?;
            // Cheap check only (see Catalog::open); runtime corruption leaves a marker.
            c.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))?;
            Ok(c)
        };
        let marker = path.with_extension("corrupt");
        if marker.exists() {
            let _ = std::fs::remove_file(&marker);
            if path.exists() {
                super::store::quarantine(path, quarantine_dir)?;
            }
        }
        let write = match open(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "text store unreadable; rebuilding (snippets regenerate as files are re-indexed)");
                super::store::quarantine(path, quarantine_dir)?;
                open(path)?
            }
        };
        Ok(Self { write: Mutex::new(write), readers: Mutex::new(Vec::new()), path: path.to_path_buf() })
    }

    fn reader(&self) -> Result<Connection> {
        if let Some(c) = self.readers.lock().pop() {
            return Ok(c);
        }
        let c = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(c)
    }

    /// Fetch and decode. Safe to call from many threads concurrently (connection pool).
    pub fn get(&self, key: u64) -> Result<Option<(String, Vec<Loc>)>> {
        let c = self.reader()?;
        let blob: Option<Vec<u8>> = c
            .prepare_cached("SELECT data FROM texts WHERE key=?1")?
            .query_row([key as i64], |r| r.get(0))
            .optional()?;
        {
            let mut pool = self.readers.lock();
            if pool.len() < 16 {
                pool.push(c);
            }
        }
        match blob {
            None => Ok(None),
            Some(b) => decode(&b).map(Some).ok_or_else(|| Error::Other("corrupt text blob".into())),
        }
    }

    pub fn apply(&self, ops: &[TextOp]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut c = self.write.lock();
        let tx = c.transaction()?;
        {
            let mut put = tx.prepare_cached("INSERT OR REPLACE INTO texts(key, data) VALUES(?1, ?2)")?;
            let mut del = tx.prepare_cached("DELETE FROM texts WHERE key=?1")?;
            for op in ops {
                match op {
                    TextOp::Put { key, blob } => {
                        put.execute(params![*key as i64, blob])?;
                    }
                    TextOp::Delete { key } => {
                        del.execute([*key as i64])?;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        self.write.lock().execute_batch("DELETE FROM texts; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    pub fn db_path(&self) -> &Path {
        &self.path
    }

    pub fn file_size(&self) -> u64 {
        let base = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let wal = std::fs::metadata(format!("{}-wal", self.path.display())).map(|m| m.len()).unwrap_or(0);
        base + wal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsers::LocKind;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = TextStore::open(&dir.path().join("t.db"), &dir.path().join("q")).unwrap();
        let locs = vec![Loc { offset: 0, kind: LocKind::Page(1) }];
        s.apply(&[TextOp::Put { key: 7, blob: encode("héllo wörld", &locs) }]).unwrap();
        let (t, l) = s.get(7).unwrap().unwrap();
        assert_eq!(t, "héllo wörld");
        assert_eq!(l, locs);
        s.apply(&[TextOp::Delete { key: 7 }]).unwrap();
        assert!(s.get(7).unwrap().is_none());
    }
}
