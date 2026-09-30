//! Data transfer types shared with the UI (serialised as camelCase JSON) and the CLI.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
#[serde(rename_all = "camelCase")]
pub enum SearchMode {
    #[default]
    Smart,
    Exact,
    Regex,
    Filename,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
#[serde(rename_all = "camelCase")]
pub enum SortOrder {
    #[default]
    Relevance,
    Modified,
    Size,
    Name,
}

/// Structured filters coming from the UI filter panel (the query syntax offers the same
/// filters inline; both are combined with AND).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchFilters {
    /// Extensions without dot, lowercase.
    pub exts: Vec<String>,
    /// Kind families (`pdf`, `word`, `spreadsheet`, ...).
    pub kinds: Vec<String>,
    /// Restrict to these directories (and below).
    pub dirs: Vec<String>,
    /// Unix seconds, inclusive.
    pub modified_after: Option<i64>,
    /// Unix seconds, exclusive.
    pub modified_before: Option<i64>,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchRequest {
    pub query: String,
    pub mode: SearchMode,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub filters: SearchFilters,
    pub sort: SortOrder,
    pub offset: u32,
    pub limit: u32,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::Smart,
            case_sensitive: false,
            whole_word: false,
            filters: SearchFilters::default(),
            sort: SortOrder::Relevance,
            offset: 0,
            limit: 100,
        }
    }
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self { query: query.into(), ..Default::default() }
    }

    /// Everything that determines matching (not paging) — key for matcher and session caches.
    pub fn match_key(&self) -> MatchKey {
        MatchKey {
            query: self.query.clone(),
            mode: self.mode,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MatchKey {
    pub query: String,
    pub mode: SearchMode,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

/// Per-document flags stored in the index and catalog.
pub mod flags {
    /// Only the first part of the document's text was indexed (size cap reached).
    pub const TRUNCATED: u64 = 1;
    /// Scanned PDF / image without a text layer: OCR required to search its content.
    pub const NEEDS_OCR: u64 = 2;
    /// Text came from OCR.
    pub const OCR: u64 = 4;
    /// Only the file name is indexed (unsupported type, skipped or failed).
    pub const NAME_ONLY: u64 = 8;
    pub const ENCRYPTED: u64 = 16;
    pub const FAILED: u64 = 32;
    /// Page numbers are approximate (DOCX: derived from Word's last rendered page breaks).
    pub const APPROX_PAGES: u64 = 64;
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ResultItem {
    pub path: String,
    pub name: String,
    pub dir: String,
    pub ext: String,
    pub kind: String,
    pub size: u64,
    /// Unix seconds.
    pub modified: i64,
    pub score: f32,
    pub flags: u64,
    pub pages: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Strategy {
    /// Answered purely from the inverted index.
    Indexed,
    /// Index candidates verified against text (case-sensitive / exact string).
    Verified,
    /// Regex scan over (prefiltered) candidates.
    Scan,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub items: Vec<ResultItem>,
    pub total: u64,
    /// True when `total` is a lower bound (verified/scan strategies stop early).
    pub total_is_lower_bound: bool,
    pub offset: u32,
    pub elapsed_ms: f64,
    pub strategy: Strategy,
    /// Search stopped because the time budget was exhausted; more results may exist.
    pub partial: bool,
    pub warnings: Vec<String>,
    /// Index generation the results were computed against.
    pub generation: u64,
}

/// A highlighted excerpt. `highlights` are `[start, end)` offsets in UTF-16 code units so the
/// UI can slice JavaScript strings directly.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub text: String,
    pub highlights: Vec<[u32; 2]>,
    pub location: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SnippetResult {
    pub path: String,
    pub snippets: Vec<Snippet>,
    pub match_count: u32,
    /// Counting stopped at the cap; the real count is higher.
    pub match_count_capped: bool,
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSection {
    pub text: String,
    pub highlights: Vec<[u32; 2]>,
    pub location: Option<String>,
    /// Global index of the first highlight in this section (for "match 3 of 12").
    pub first_match: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub modified: i64,
    pub flags: u64,
    pub pages: Option<u32>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub total_matches: u32,
    pub matches_capped: bool,
    pub sections: Vec<PreviewSection>,
    /// Beginning of the document when there are no content matches (e.g. filename hit).
    pub head: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RootInfo {
    pub id: i64,
    pub path: String,
    pub file_count: u64,
    pub last_scan_at: Option<i64>,
    pub watch_status: String,
    pub scanning: bool,
    pub available: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub active: bool,
    pub scanning: bool,
    pub paused: bool,
    pub discovered: u64,
    pub queued: u64,
    pub processed: u64,
    pub bytes_processed: u64,
    pub files_per_sec: f64,
    pub percent: Option<f32>,
    pub current_path: Option<String>,
    pub ocr_pending: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub files_total: u64,
    pub indexed: u64,
    pub name_only: u64,
    pub skipped: u64,
    pub failed: u64,
    pub encrypted: u64,
    pub needs_ocr: u64,
    pub index_bytes: u64,
    pub text_store_bytes: u64,
    pub last_updated: Option<i64>,
    pub progress: IndexProgress,
    pub roots: Vec<RootInfo>,
    pub generation: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFile {
    pub path: String,
    pub status: String,
    pub reason: Option<String>,
    pub size: u64,
    pub modified: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub version: String,
    pub data_dir: String,
    pub memory_rss_bytes: u64,
    pub worker_threads: usize,
    pub storage_rotational: Option<bool>,
    pub pdf_engine: String,
    pub ocr_engine: Option<String>,
    pub index_segments: usize,
    pub index_docs: u64,
    pub query_cache_entries: usize,
    pub query_cache_hits: u64,
    pub query_cache_misses: u64,
    pub log_dir: String,
}
