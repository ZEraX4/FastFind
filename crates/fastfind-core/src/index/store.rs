//! Tantivy index lifecycle: open, version check, quarantine-and-rebuild on corruption, single
//! writer, manually reloaded reader with a generation counter.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tantivy::directory::MmapDirectory;
use tantivy::store::Compressor;
use tantivy::{Index, IndexReader, IndexSettings, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, TantivyError};

use super::schema::{self, Fields, SCHEMA_VERSION};
use crate::error::{Error, Result};

const VERSION_FILE: &str = "fastfind-schema-version";

pub struct IndexStore {
    pub index: Index,
    pub fields: Fields,
    reader: IndexReader,
    writer: Mutex<IndexWriter<TantivyDocument>>,
    generation: AtomicU64,
}

/// Move a damaged store out of the way (kept for diagnosis, never deleted automatically).
pub fn quarantine(path: &Path, quarantine_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(quarantine_dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "store".into());
    let dest = quarantine_dir.join(format!("{name}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    std::fs::rename(path, &dest)?;
    Ok(dest)
}

impl IndexStore {
    /// Open (or create) the index. Returns `(store, rebuilt)` where `rebuilt` means the index
    /// is new or was recreated, so the catalog must forget what was indexed.
    pub fn open(dir: &Path, quarantine_dir: &Path, writer_budget: usize, writer_threads: usize) -> Result<(Self, bool)> {
        let (schema, fields) = schema::build();
        let version_ok = std::fs::read_to_string(dir.join(VERSION_FILE))
            .map(|v| v.trim() == SCHEMA_VERSION.to_string())
            .unwrap_or(false);
        let mut rebuilt = false;
        let index = if dir.join("meta.json").exists() && version_ok {
            match Index::open_in_dir(dir) {
                Ok(i) if i.schema() == schema => i,
                Ok(_) | Err(_) => {
                    tracing::error!(dir = %dir.display(), "index unreadable or schema mismatch; rebuilding");
                    quarantine(dir, quarantine_dir)?;
                    rebuilt = true;
                    Self::create(dir, schema)?
                }
            }
        } else {
            if dir.exists() {
                tracing::warn!(dir = %dir.display(), "index version mismatch or incomplete; rebuilding");
                quarantine(dir, quarantine_dir)?;
            }
            rebuilt = true;
            Self::create(dir, schema)?
        };
        schema::register_tokenizers(&index);
        let threads = writer_threads.clamp(1, 8);
        // Tantivy requires at least 15 MB per indexing thread.
        let budget = writer_budget.max(threads * 20_000_000);
        let writer = index.writer_with_num_threads(threads, budget).map_err(|e| match e {
            TantivyError::LockFailure(..) => Error::AlreadyRunning,
            other => Error::Index(other),
        })?;
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        Ok((Self { index, fields, reader, writer: Mutex::new(writer), generation: AtomicU64::new(1) }, rebuilt))
    }

    fn create(dir: &Path, schema: tantivy::schema::Schema) -> Result<Index> {
        std::fs::create_dir_all(dir)?;
        let settings = IndexSettings { docstore_compression: Compressor::Lz4, ..Default::default() };
        let index = Index::builder().schema(schema).settings(settings).open_or_create(MmapDirectory::open(dir).map_err(|e| Error::Other(e.to_string()))?)?;
        std::fs::write(dir.join(VERSION_FILE), SCHEMA_VERSION.to_string())?;
        Ok(index)
    }

    pub fn searcher(&self) -> Searcher {
        self.reader.searcher()
    }

    /// Incremented each time committed changes become visible to searches.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn with_writer<R>(&self, f: impl FnOnce(&mut IndexWriter<TantivyDocument>) -> R) -> R {
        f(&mut self.writer.lock())
    }

    /// Commit pending writes and make them searchable.
    pub fn commit(&self) -> Result<()> {
        self.writer.lock().commit()?;
        self.reader.reload()?;
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Discard everything (used by "Rebuild index").
    pub fn clear(&self) -> Result<()> {
        let mut w = self.writer.lock();
        w.delete_all_documents()?;
        w.commit()?;
        drop(w);
        self.reader.reload()?;
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub fn num_docs(&self) -> u64 {
        self.searcher().num_docs()
    }

    pub fn num_segments(&self) -> usize {
        self.searcher().segment_readers().len()
    }

    /// Block until background merges finish (clean shutdown / benchmarks).
    pub fn wait_merging(self) {
        let w = self.writer.into_inner();
        let _ = w.wait_merging_threads();
    }
}
