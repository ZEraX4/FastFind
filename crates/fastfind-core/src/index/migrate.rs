//! Index format upgrades without re-reading every file.
//!
//! A new `SCHEMA_VERSION` (new tokenizer rules or fields) means every document must be indexed
//! again. But the expensive part of indexing (parsing PDFs and Office files, and above all OCR)
//! produced text that is still in the text store. `migrate` builds the new index from the old
//! index's stored fields (pages, metadata, text mode), the catalog (size, date, flags) and the
//! text store, so documents are only re-tokenised:
//!
//! * documents whose text is stored are rebuilt from it. Recognised (OCR) text is reused even
//!   when the stored copy was cut at the store limit: OCR again would cost far more than the
//!   lost tail;
//! * documents without content (name only, skipped, failed, encrypted) are rebuilt from their
//!   metadata alone;
//! * everything else (plain-text files, which are read from disk anyway, re-parsed formats, and
//!   documents whose stored text was cut) is marked changed in the catalog, so the startup scan
//!   reads it again.
//!
//! Crash safety: the new index is built in a separate folder and swapped in by the caller only
//! when complete, so an interrupted upgrade starts again from the untouched old index.

use std::path::Path;
use std::time::Instant;

use tantivy::schema::Value;
use tantivy::{Index, TantivyDocument};

use super::catalog::{Catalog, Status};
use super::service::base_doc;
use super::textstore::TextStore;
use super::{schema, store::IndexStore};
use crate::error::{Error, Result};
use crate::model::flags;
use crate::parsers::registry::kind_for_ext;
use crate::parsers::TextMode;
use crate::util::{extension_of, path_hash};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migrated {
    /// Documents rebuilt from stored data.
    pub rebuilt: u64,
    /// Files left for the next scan to read again.
    pub rescan: u64,
}

/// Build an index of the current schema in `new_dir` from the index in `old_dir`.
/// `stored_cap` is the text store's per-document limit in bytes; `threads` and `writer_budget`
/// size the index writer as for regular indexing.
pub fn migrate(old_dir: &Path, new_dir: &Path, catalog: &Catalog, texts: &TextStore, stored_cap: usize, threads: usize, writer_budget: usize) -> Result<Migrated> {
    let t0 = Instant::now();
    if new_dir.exists() {
        // Left over from an interrupted upgrade.
        std::fs::remove_dir_all(new_dir)?;
    }
    let old = Index::open_in_dir(old_dir)?;
    let old_schema = old.schema();
    let field = |name: &str| old_schema.get_field(name).ok();
    let (Some(old_path), old_mode, old_pages, old_meta, old_info) = (field("path"), field("mode"), field("pages"), field("meta"), field("info")) else {
        return Err(Error::Other("old index has no path field".into()));
    };

    let (new_schema, f) = schema::build();
    let new = IndexStore::create(new_dir, new_schema)?;
    schema::register_tokenizers(&new);
    let threads = threads.clamp(1, 8);
    let mut writer = new.writer_with_num_threads(threads, writer_budget.max(threads * 25_000_000))?;

    let mut out = Migrated::default();
    let mut rescan: Vec<String> = Vec::new();
    let searcher = old.reader()?.searcher();
    for segment in searcher.segment_readers() {
        let store = segment.get_store_reader(64)?;
        for doc in store.iter::<TantivyDocument>(segment.alive_bitset()) {
            let doc = doc?;
            let Some(path) = doc.get_first(old_path).and_then(|v| v.as_str()).map(str::to_string) else { continue };
            // Not in the catalog any more: the scan will not expect it either.
            let Some(row) = catalog.get(&path)? else { continue };
            let u64_of = |fl: Option<tantivy::schema::Field>| fl.and_then(|fl| doc.get_first(fl)).and_then(|v| v.as_u64());
            let text_of = |fl: Option<tantivy::schema::Field>| fl.and_then(|fl| doc.get_first(fl)).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mode = TextMode::from_u64(u64_of(old_mode).unwrap_or(0));
            let has_content = matches!(row.status, Status::Indexed | Status::NeedsOcr);

            let content = if !has_content {
                None
            } else if mode == TextMode::Stored {
                match texts.get(path_hash(&path))? {
                    Some((text, _)) => {
                        let cut = row.flags & flags::TRUNCATED != 0 || text.len() + 16 >= stored_cap;
                        if cut && row.flags & flags::OCR == 0 {
                            rescan.push(path);
                            continue;
                        }
                        Some(text)
                    }
                    None => {
                        rescan.push(path);
                        continue;
                    }
                }
            } else {
                rescan.push(path);
                continue;
            };

            let p = Path::new(&path);
            let pages = u64_of(old_pages).filter(|&n| n > 0).map(|n| n as u32);
            let mut d = base_doc(&f, row.root_id, p, row.size, row.mtime_ns, kind_for_ext(&extension_of(p)), row.flags, pages, mode);
            if let Some(text) = content {
                d.add_text(f.content, &text);
            }
            for (fl, value) in [(f.meta, text_of(old_meta)), (f.info, text_of(old_info))] {
                if !value.is_empty() {
                    d.add_text(fl, &value);
                }
            }
            writer.add_document(d)?;
            out.rebuilt += 1;
        }
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    drop(searcher);
    drop(old);
    catalog.mark_changed(&rescan)?;
    out.rescan = rescan.len() as u64;
    tracing::info!(rebuilt = out.rebuilt, rescan = out.rescan, ms = t0.elapsed().as_millis() as u64, "index upgraded from stored data");
    Ok(out)
}
