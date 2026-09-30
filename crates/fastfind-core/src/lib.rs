//! FastFind core engine.
//!
//! The crate is UI-agnostic. The Tauri shell (`src-tauri`) and the CLI (`fastfind-cli`) both
//! drive it through [`engine::Engine`].
//!
//! Module map:
//! * [`fs`]      – directory scanning, path filtering, file watching, storage probing
//! * [`parsers`] – pluggable text extraction for every supported document format
//! * [`index`]   – catalog (SQLite), full-text index (Tantivy), text store, indexing pipeline
//! * [`search`]  – query syntax, query planning, execution, matching, snippets
//! * [`preview`] – lazily built match-centred previews
//! * [`ocr`]     – optional Tesseract-based OCR for scanned PDFs and images
//! * [`gen`]     – deterministic sample data for tests and benchmarks

pub mod analysis;
pub mod config;
pub mod engine;
pub mod error;
pub mod fs;
pub mod gen;
pub mod index;
pub mod logging;
pub mod model;
pub mod ocr;
pub mod parsers;
pub mod preview;
pub mod search;
pub mod textsource;
pub mod util;

pub use engine::{Engine, EngineOptions};
pub use error::{Error, Result};
