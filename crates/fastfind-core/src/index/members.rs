//! Members: search results that live inside a file (the e-mails of an Outlook archive).
//!
//! A member is a Tantivy document (plus stored text) whose path is the container's path, the
//! platform separator and `#<id>`, e.g. `C:\Mail\2025.pst\#2097188`. No real file can have
//! such a path (its parent is a file), so the key never collides with a file's.
//!
//! Members have no catalog rows: the catalog keeps tracking only real files, so scans, rename
//! detection and the file counts are unchanged. The container's own row is written after its
//! members, so an interrupted run re-reads the whole container on the next scan. Removing,
//! renaming or re-indexing a container deletes its members by a path range query.

use std::ops::Bound;
use std::path::MAIN_SEPARATOR;

use tantivy::query::RangeQuery;
use tantivy::schema::Value;
use tantivy::{TantivyDocument, Term};

use super::schema::Fields;
use super::store::IndexStore;
use crate::util::extension_of;

/// Can this file have members? (By name: it decides how a path is treated everywhere.)
pub fn is_container(path: &str) -> bool {
    extension_of(std::path::Path::new(path)) == "pst"
}

pub fn member_path(container: &str, id: u32) -> String {
    format!("{container}{MAIN_SEPARATOR}#{id}")
}

/// (container, id) when `path` names a member.
pub fn split(path: &str) -> Option<(&str, u32)> {
    let (container, id) = path.rsplit_once(MAIN_SEPARATOR)?;
    let id = id.strip_prefix('#')?.parse().ok()?;
    is_container(container).then_some((container, id))
}

/// Matches every member of `container` ("…pst\#" up to, not including, "…pst\$").
pub fn range(f: &Fields, container: &str) -> RangeQuery {
    let lo = format!("{container}{MAIN_SEPARATOR}#");
    let hi = format!("{container}{MAIN_SEPARATOR}$");
    RangeQuery::new(Bound::Included(Term::from_field_text(f.path, &lo)), Bound::Excluded(Term::from_field_text(f.path, &hi)))
}

/// Paths of the committed members of `container` (to delete their stored text).
pub fn committed(store: &IndexStore, container: &str) -> Vec<String> {
    let f = store.fields;
    let searcher = store.searcher();
    let Ok(addrs) = searcher.search(&range(&f, container), &tantivy::collector::DocSetCollector) else { return Vec::new() };
    addrs
        .into_iter()
        .filter_map(|a| searcher.doc::<TantivyDocument>(a).ok())
        .filter_map(|d| d.get_first(f.path).and_then(|v| v.as_str()).map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_paths_round_trip() {
        let c = format!("{s}mail{s}2025.PST", s = MAIN_SEPARATOR);
        let m = member_path(&c, 2097188);
        assert_eq!(split(&m), Some((c.as_str(), 2097188)));
        assert_eq!(split(&c), None);
        assert_eq!(split(&format!("{s}mail{s}notes.txt{s}#12", s = MAIN_SEPARATOR)), None);
        assert_eq!(split(&format!("{c}{MAIN_SEPARATOR}#x")), None);
        assert!(is_container(&c));
    }
}
