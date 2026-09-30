//! Which directories and files are visited, and which files get their content indexed.

use std::collections::HashSet;
use std::path::{Component, Path};

use crate::config::IndexingSettings;
use crate::util::filter_form;

#[derive(Debug, Clone)]
pub struct PathFilter {
    excluded_names: HashSet<String>,
    excluded_globs: Vec<String>,
    excluded_paths: Vec<String>,
    pub include_hidden: bool,
    pub include_system: bool,
    pub max_file_size: u64,
    pub max_pdf_size: u64,
    included_exts: HashSet<String>,
    excluded_exts: HashSet<String>,
    pub index_all_filenames: bool,
    pub detect_text: bool,
    /// The application's own data directory is never indexed (avoids feedback loops when a
    /// user adds their home folder).
    data_dir: String,
}

fn name_key(name: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        name.to_lowercase()
    } else {
        name.to_string()
    }
}

/// Minimal glob: `*` matches any run, `?` one character.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl PathFilter {
    pub fn from_settings(s: &IndexingSettings, data_dir: &Path) -> Self {
        let mut excluded_names = HashSet::new();
        let mut excluded_globs = Vec::new();
        let mut excluded_paths = Vec::new();
        for d in &s.excluded_dirs {
            let d = d.trim();
            if d.contains('/') || d.contains('\\') {
                excluded_paths.push(filter_form(d.trim_end_matches(['/', '\\'])));
            } else if d.contains('*') || d.contains('?') {
                excluded_globs.push(name_key(d));
            } else {
                excluded_names.insert(name_key(d));
            }
        }
        Self {
            excluded_names,
            excluded_globs,
            excluded_paths,
            include_hidden: s.index_hidden,
            include_system: s.index_system,
            max_file_size: s.max_file_size_mb.saturating_mul(1 << 20),
            max_pdf_size: s.max_pdf_size_mb.saturating_mul(1 << 20),
            included_exts: s.included_extensions.iter().cloned().collect(),
            excluded_exts: s.excluded_extensions.iter().cloned().collect(),
            index_all_filenames: s.index_all_filenames,
            detect_text: s.detect_text_files,
            data_dir: filter_form(&data_dir.to_string_lossy()),
        }
    }

    fn name_excluded(&self, name: &str) -> bool {
        let k = name_key(name);
        self.excluded_names.contains(&k) || self.excluded_globs.iter().any(|g| glob_match(g, &k))
    }

    fn path_prefix_excluded(&self, path: &Path) -> bool {
        let f = filter_form(&path.to_string_lossy());
        let under = |prefix: &str| f == prefix || (f.starts_with(prefix) && f[prefix.len()..].starts_with('/'));
        (!self.data_dir.is_empty() && under(&self.data_dir)) || self.excluded_paths.iter().any(|p| under(p))
    }

    pub fn is_hidden_name(name: &str) -> bool {
        name.starts_with('.') && name != "." && name != ".."
    }

    /// Should the scanner descend into this directory?
    pub fn dir_allowed(&self, name: &str, path: &Path, hidden_attr: bool, system_attr: bool) -> bool {
        if self.name_excluded(name) {
            return false;
        }
        if !self.include_hidden && (hidden_attr || Self::is_hidden_name(name)) {
            return false;
        }
        if !self.include_system && system_attr {
            return false;
        }
        !self.path_prefix_excluded(path)
    }

    /// Should this file appear in the index at all (name or content)?
    pub fn file_allowed(&self, name: &str, hidden_attr: bool, system_attr: bool) -> bool {
        if !self.include_hidden && (hidden_attr || Self::is_hidden_name(name)) {
            return false;
        }
        if !self.include_system && system_attr {
            return false;
        }
        true
    }

    /// Content indexing permitted for this extension by the include/exclude lists?
    pub fn content_allowed(&self, ext: &str) -> bool {
        if self.excluded_exts.contains(ext) {
            return false;
        }
        self.included_exts.is_empty() || self.included_exts.contains(ext)
    }

    /// Full check for a path reported by the file watcher (no directory walk context): every
    /// component below `root` must pass the directory rules.
    pub fn path_allowed(&self, root: &Path, path: &Path) -> bool {
        if self.path_prefix_excluded(path) {
            return false;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let comps: Vec<_> = rel.components().collect();
        for (i, c) in comps.iter().enumerate() {
            if let Component::Normal(n) = c {
                let n = n.to_string_lossy();
                let last = i + 1 == comps.len();
                if !last && self.name_excluded(&n) {
                    return false;
                }
                if !self.include_hidden && Self::is_hidden_name(&n) {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob_match("*.tmp", "a.tmp"));
        assert!(glob_match("cache-?", "cache-1"));
        assert!(!glob_match("cache-?", "cache-12"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
    }

    #[test]
    fn exclusions() {
        let mut s = IndexingSettings::default();
        s.excluded_dirs.push("*.bak".into());
        s.excluded_dirs.push("/data/secret".into());
        s.excluded_extensions = vec!["log".into()];
        let f = PathFilter::from_settings(&s, Path::new("/home/u/.local/share/fastfind"));
        assert!(!f.dir_allowed("node_modules", Path::new("/p/node_modules"), false, false));
        assert!(!f.dir_allowed("old.bak", Path::new("/p/old.bak"), false, false));
        assert!(!f.dir_allowed(".hidden", Path::new("/p/.hidden"), false, false));
        assert!(!f.dir_allowed("secret", Path::new("/data/secret"), false, false));
        assert!(f.dir_allowed("secrets", Path::new("/data/secrets"), false, false));
        assert!(f.dir_allowed("src", Path::new("/p/src"), false, false));
        assert!(!f.content_allowed("log"));
        assert!(f.content_allowed("txt"));
        assert!(!f.path_allowed(Path::new("/p"), Path::new("/p/.git/config")));
        assert!(!f.path_allowed(Path::new("/p"), Path::new("/p/a/node_modules/x.js")));
        assert!(f.path_allowed(Path::new("/p"), Path::new("/p/a/b.txt")));
        assert!(!f.path_allowed(Path::new("/home/u"), Path::new("/home/u/.local/share/fastfind/index/x")));
    }
}
