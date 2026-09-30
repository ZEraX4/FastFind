//! Persistent storage and the indexing pipeline.
//!
//! * [`catalog`] – SQLite metadata: roots, per-file indexing state (size, mtime, hash, status,
//!   error), directory access statistics. Source of truth for incremental indexing.
//! * [`store`] – Tantivy full-text index (inverted index, positions, fast fields).
//! * [`textstore`] – zstd-compressed extracted text for binary formats (snippets/preview).
//! * [`service`] – scanner → change detection → bounded lanes → parser workers → single
//!   writer, plus file watching, OCR scheduling and progress reporting.
//!
//! Commit ordering (crash safety): Tantivy commit → text store commit → catalog commit. Every
//! write is an idempotent upsert keyed by path, so a crash in between only causes a few files
//! to be re-indexed on the next start.

pub mod catalog;
pub mod schema;
pub mod service;
pub mod store;
pub mod textstore;

pub use catalog::{Catalog, CatOp, FileRecord, Status};
pub use schema::Fields;
pub use service::IndexService;
pub use store::IndexStore;
pub use textstore::TextStore;
