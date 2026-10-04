//! Persistent user settings (`settings.json`) and application data paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::SearchMode;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub enum CpuPreference {
    /// Few workers, background I/O priority. Best while working on battery.
    Low,
    #[default]
    Balanced,
    /// Use (almost) all cores.
    High,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundMode {
    /// Index changes as soon as they are detected.
    #[default]
    Automatic,
    /// Watch for changes but only index when the user triggers "Update now".
    Manual,
    /// No indexing at all until resumed.
    Paused,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchSettings {
    pub default_mode: SearchMode,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Results per page requested by the UI (virtual list loads further pages lazily).
    pub page_size: u32,
    /// Hard cap on results a single query may page through.
    pub result_limit: u32,
    /// Ranking weight of a file-name hit relative to a content hit.
    pub name_boost: f32,
    /// Ranking weight of document metadata (title/author/subject).
    pub metadata_boost: f32,
    /// Extra weight for phrase matches.
    pub phrase_boost: f32,
    /// Time budget for verified (case-sensitive/exact) searches before returning partial results.
    pub verify_budget_ms: u64,
    /// Time budget for regex scans per page.
    pub regex_budget_ms: u64,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            default_mode: SearchMode::Smart,
            case_sensitive: false,
            whole_word: false,
            page_size: 100,
            result_limit: 10_000,
            name_boost: 3.0,
            metadata_boost: 1.5,
            phrase_boost: 2.0,
            verify_budget_ms: 3_000,
            regex_budget_ms: 10_000,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct OcrSettings {
    pub enabled: bool,
    /// Explicit path to the `tesseract` executable; auto-detected when empty.
    pub tesseract_path: String,
    /// Tesseract language codes, e.g. `eng` or `eng+deu`.
    pub languages: String,
    /// Also OCR image files (png, jpg, tiff, bmp).
    pub images: bool,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self { enabled: false, tesseract_path: String::new(), languages: "eng".into(), images: false }
    }
}

pub const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".git", ".hg", ".svn", "node_modules", "target", "bin", "obj", "build", "dist", ".cache",
    ".vscode", ".idea", "__pycache__", ".venv", "venv", ".gradle", ".next", ".nuxt",
    "$RECYCLE.BIN", "System Volume Information", ".Trash", ".Trashes", ".Spotlight-V100",
    ".fseventsd", "lost+found",
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct IndexingSettings {
    /// 0 = automatic (derived from CPU preference, core count and storage type).
    pub worker_count: u32,
    /// Files above this size are indexed by name only.
    pub max_file_size_mb: u64,
    /// PDFs above this size are indexed by name only (PDF parsing needs RAM ∝ size).
    pub max_pdf_size_mb: u64,
    /// Maximum extracted characters indexed per file (MB of UTF-8 text).
    pub max_indexed_text_mb: u64,
    /// Maximum extracted text kept for snippets/preview per binary document (KB).
    pub stored_text_kb: u64,
    /// Only these extensions are content-indexed (empty = all supported).
    pub included_extensions: Vec<String>,
    /// Never content-index these extensions.
    pub excluded_extensions: Vec<String>,
    /// Directory names or glob patterns (`*.tmp`) or absolute paths to skip.
    pub excluded_dirs: Vec<String>,
    pub follow_symlinks: bool,
    pub index_hidden: bool,
    pub index_system: bool,
    /// Index names of files whose content is not supported (enables filename search over them).
    pub index_all_filenames: bool,
    /// Sniff files with unknown extensions and index them if they look like text.
    pub detect_text_files: bool,
    /// Compare content hashes when mtime changed but size did not (avoids re-parsing touched files).
    pub content_hash: bool,
    pub watch_changes: bool,
    /// Periodic reconcile scan interval (minutes, 0 = never). Catches changes watchers miss
    /// (network shares, overflowed event queues).
    pub rescan_interval_min: u32,
    /// Per-file parse time limit in seconds.
    pub parse_timeout_secs: u64,
    pub ocr: OcrSettings,
}

impl Default for IndexingSettings {
    fn default() -> Self {
        Self {
            worker_count: 0,
            max_file_size_mb: 1024,
            max_pdf_size_mb: 512,
            max_indexed_text_mb: 16,
            stored_text_kb: 1024,
            included_extensions: vec![],
            excluded_extensions: vec![],
            excluded_dirs: DEFAULT_EXCLUDED_DIRS.iter().map(|s| s.to_string()).collect(),
            follow_symlinks: false,
            index_hidden: false,
            index_system: false,
            index_all_filenames: true,
            detect_text_files: true,
            content_hash: true,
            watch_changes: true,
            rescan_interval_min: 60,
            parse_timeout_secs: 120,
            ocr: OcrSettings::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PerformanceSettings {
    pub cpu: CpuPreference,
    /// Memory budget (MB) shared by the index writer buffer and query/snippet caches.
    pub memory_cache_mb: u32,
    pub background: BackgroundMode,
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self { cpu: CpuPreference::Balanced, memory_cache_mb: 256, background: BackgroundMode::Automatic }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AppearanceSettings {
    pub theme: Theme,
    /// UI scale factor (0.8 – 1.6).
    pub font_scale: f32,
    pub high_contrast: bool,
    pub show_preview: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self { theme: Theme::System, font_scale: 1.0, high_contrast: false, show_preview: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateSettings {
    /// Daily check for a new version. `None` until the user has been asked: FastFind makes no
    /// network request without consent.
    pub check_automatically: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub search: SearchSettings,
    pub indexing: IndexingSettings,
    pub performance: PerformanceSettings,
    pub appearance: AppearanceSettings,
    pub updates: UpdateSettings,
}

impl Settings {
    pub fn validate(&mut self) -> Result<()> {
        let s = &mut self.search;
        s.page_size = s.page_size.clamp(10, 500);
        s.result_limit = s.result_limit.clamp(100, 1_000_000);
        s.name_boost = s.name_boost.clamp(0.0, 20.0);
        s.metadata_boost = s.metadata_boost.clamp(0.0, 20.0);
        s.phrase_boost = s.phrase_boost.clamp(1.0, 20.0);
        s.verify_budget_ms = s.verify_budget_ms.clamp(200, 60_000);
        s.regex_budget_ms = s.regex_budget_ms.clamp(500, 120_000);
        let i = &mut self.indexing;
        i.worker_count = i.worker_count.min(128);
        i.max_file_size_mb = i.max_file_size_mb.clamp(1, 64 * 1024);
        i.max_pdf_size_mb = i.max_pdf_size_mb.clamp(1, 16 * 1024);
        i.max_indexed_text_mb = i.max_indexed_text_mb.clamp(1, 512);
        i.stored_text_kb = i.stored_text_kb.clamp(16, 64 * 1024);
        i.parse_timeout_secs = i.parse_timeout_secs.clamp(5, 3600);
        for e in i.included_extensions.iter_mut().chain(i.excluded_extensions.iter_mut()) {
            *e = e.trim().trim_start_matches('.').to_lowercase();
        }
        i.included_extensions.retain(|e| !e.is_empty());
        i.excluded_extensions.retain(|e| !e.is_empty());
        i.excluded_dirs.iter_mut().for_each(|d| *d = d.trim().to_string());
        i.excluded_dirs.retain(|d| !d.is_empty());
        if i.ocr.languages.trim().is_empty() {
            i.ocr.languages = "eng".into();
        }
        if !i.ocr.languages.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '_') {
            return Err(Error::Settings("OCR languages may only contain letters, digits, '_' and '+'".into()));
        }
        self.performance.memory_cache_mb = self.performance.memory_cache_mb.clamp(64, 8192);
        self.appearance.font_scale = self.appearance.font_scale.clamp(0.8, 1.6);
        Ok(())
    }

    pub fn load(path: &Path) -> Settings {
        let mut s = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).map_err(|e| {
                tracing::warn!(error = %e, "settings.json is invalid; using defaults");
                e
            }).ok())
            .unwrap_or_default();
        if s.validate().is_err() {
            s = Settings::default();
        }
        s
    }

    /// Atomic save: write to a temp file in the same directory, then rename over the original.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Resolved application data locations.
#[derive(Clone, Debug)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub index_dir: PathBuf,
    pub catalog_db: PathBuf,
    pub text_db: PathBuf,
    pub settings_file: PathBuf,
    pub log_dir: PathBuf,
    pub quarantine_dir: PathBuf,
}

impl AppPaths {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            index_dir: data_dir.join("index"),
            catalog_db: data_dir.join("catalog.db"),
            text_db: data_dir.join("text.db"),
            settings_file: data_dir.join("settings.json"),
            log_dir: data_dir.join("logs"),
            quarantine_dir: data_dir.join("quarantine"),
            data_dir,
        }
    }

    /// Platform default: `%LOCALAPPDATA%\FastFind`, `~/Library/Application Support/FastFind`,
    /// `$XDG_DATA_HOME/fastfind`. Overridable with `FASTFIND_DATA_DIR`.
    pub fn default_location() -> Self {
        if let Ok(p) = std::env::var("FASTFIND_DATA_DIR") {
            if !p.is_empty() {
                return Self::new(PathBuf::from(p));
            }
        }
        let dir = directories::ProjectDirs::from("app", "FastFind", "FastFind")
            .map(|d| d.data_local_dir().to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("FastFind"));
        Self::new(dir)
    }

    pub fn ensure(&self) -> Result<()> {
        for d in [&self.data_dir, &self.log_dir] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.search.case_sensitive = true;
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
        // Unknown / missing fields fall back to defaults instead of failing.
        std::fs::write(&p, r#"{"search":{"wholeWord":true},"futureField":1}"#).unwrap();
        let l = Settings::load(&p);
        assert!(l.search.whole_word);
        assert_eq!(l.indexing, IndexingSettings::default());
        // Settings from 1.0.0 have no `updates` section: the user has not been asked yet.
        assert_eq!(l.updates.check_automatically, None);
        std::fs::write(&p, r#"{"updates":{"checkAutomatically":false}}"#).unwrap();
        assert_eq!(Settings::load(&p).updates.check_automatically, Some(false));
    }

    #[test]
    fn validate_clamps_and_normalises() {
        let mut s = Settings::default();
        s.indexing.excluded_extensions = vec![" .TMP ".into(), "".into()];
        s.search.page_size = 1;
        s.validate().unwrap();
        assert_eq!(s.indexing.excluded_extensions, vec!["tmp"]);
        assert_eq!(s.search.page_size, 10);
        s.indexing.ocr.languages = "eng; rm -rf".into();
        assert!(s.validate().is_err());
    }
}
