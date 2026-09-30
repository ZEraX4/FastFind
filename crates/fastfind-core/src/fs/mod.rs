//! File-system layer: filtering, parallel scanning, change watching and storage probing.

pub mod filter;
pub mod scanner;
pub mod storage;
pub mod watcher;

pub use filter::PathFilter;
pub use scanner::{scan, FileEntry, ScanOptions, ScanSummary};
