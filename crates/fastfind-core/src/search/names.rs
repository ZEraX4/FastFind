//! File-name matching (`filename:`, Filename mode, the name side of the regex prefilter)
//! without walking the term dictionary.
//!
//! A substring regex such as `.*report.*` over `name_lc` visits every distinct name in the
//! term dictionary and unions the posting lists of all matches: about 45 ms at 300,000 files
//! and growing linearly. Instead, each segment's distinct names (the `name_lc` fast column's
//! dictionary, in term-ordinal order) are kept in one buffer. A substring is found with one
//! SIMD `memmem` pass over that buffer, a glob with a regex per distinct name, and matching
//! ordinals become documents through a doc → ordinal table.
//!
//! The per-segment tables are built on the first name query and cached by segment ID
//! (segments never change); `prune` drops the tables of segments that merges removed.
//! Memory: the distinct names plus 8 bytes per document.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use regex::bytes::Regex;
use tantivy::query::{BitSetDocSet, ConstScorer, EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::index::SegmentId;
use tantivy::{DocId, DocSet, Score, Searcher, SegmentReader, TantivyError};
use tantivy_common::BitSet;

use crate::error::{Error, Result};

/// The fast field holding lowercased file names.
const FIELD: &str = "name_lc";

/// What a name must match.
#[derive(Clone)]
enum Pattern {
    /// Lowercased substring.
    Contains(String),
    /// Whole-name regex (from a glob).
    Whole(Regex),
}

/// Documents whose (lowercased) file name contains `value`, or matches it as a glob when it
/// has `*` or `?`. Constant score.
#[derive(Clone)]
pub struct NameQuery {
    pattern: Pattern,
}

impl NameQuery {
    pub fn new(value: &str) -> Result<NameQuery> {
        let v = value.to_lowercase();
        let pattern = if v.contains('*') || v.contains('?') {
            let mut re = String::from("^(?s:");
            for c in v.chars() {
                match c {
                    '*' => re.push_str(".*"),
                    '?' => re.push('.'),
                    c => re.push_str(&regex::escape(&c.to_string())),
                }
            }
            re.push_str(")$");
            Pattern::Whole(Regex::new(&re).map_err(|e| Error::Query(e.to_string()))?)
        } else {
            Pattern::Contains(v)
        };
        Ok(NameQuery { pattern })
    }
}

impl fmt::Debug for NameQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.pattern {
            Pattern::Contains(s) => write!(f, "NameQuery(contains {s:?})"),
            Pattern::Whole(r) => write!(f, "NameQuery(glob {:?})", r.as_str()),
        }
    }
}

impl Query for NameQuery {
    fn weight(&self, _: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(NameWeight { pattern: self.pattern.clone() }))
    }
}

struct NameWeight {
    pattern: Pattern,
}

impl NameWeight {
    fn docs(&self, reader: &SegmentReader) -> tantivy::Result<BitSet> {
        let max_doc = reader.max_doc();
        let mut docs = BitSet::with_max_value(max_doc);
        let names = segment_names(reader)?;
        let ords = names.matching(&self.pattern);
        if ords.len() == 0 {
            return Ok(docs);
        }
        for (doc, &ord) in names.doc_ords.iter().enumerate() {
            if ord != NO_NAME && ords.contains(ord) {
                docs.insert(doc as DocId);
            }
        }
        Ok(docs)
    }
}

impl Weight for NameWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(ConstScorer::new(BitSetDocSet::from(self.docs(reader)?), boost)))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut s = self.scorer(reader, 1.0)?;
        if s.seek(doc) != doc {
            return Err(TantivyError::InvalidArgument(format!("document {doc} does not match")));
        }
        Ok(Explanation::new("NameQuery", 1.0))
    }
}

const NO_NAME: u32 = u32::MAX;

/// One segment's distinct names and which one each document has.
struct SegmentNames {
    /// Distinct names in ordinal order, each followed by a 0 byte (never part of a file name).
    buf: Vec<u8>,
    /// Start of each name in `buf`; one extra entry at the end.
    starts: Vec<u32>,
    /// Name ordinal per document (`NO_NAME` for none).
    doc_ords: Vec<u32>,
}

impl SegmentNames {
    fn build(reader: &SegmentReader) -> tantivy::Result<SegmentNames> {
        let max_doc = reader.max_doc();
        let Some(col) = reader.fast_fields().str(FIELD)? else {
            return Ok(SegmentNames { buf: Vec::new(), starts: vec![0], doc_ords: vec![NO_NAME; max_doc as usize] });
        };
        let mut buf = Vec::new();
        let mut starts = Vec::with_capacity(col.num_terms() + 1);
        let mut terms = col.dictionary().stream()?;
        while terms.advance() {
            starts.push(buf.len() as u32);
            buf.extend_from_slice(terms.key());
            buf.push(0);
        }
        starts.push(buf.len() as u32);
        let ords = col.ords();
        let doc_ords = (0..max_doc).map(|d| ords.first(d).map_or(NO_NAME, |o| o as u32)).collect();
        Ok(SegmentNames { buf, starts, doc_ords })
    }

    fn count(&self) -> usize {
        self.starts.len() - 1
    }

    fn name(&self, ord: usize) -> &[u8] {
        &self.buf[self.starts[ord] as usize..self.starts[ord + 1] as usize - 1]
    }

    /// Ordinals of the names matching `pattern`.
    fn matching(&self, pattern: &Pattern) -> BitSet {
        let mut ords = BitSet::with_max_value(self.count() as u32);
        match pattern {
            Pattern::Contains(needle) if needle.is_empty() => (0..self.count() as u32).for_each(|o| ords.insert(o)),
            Pattern::Contains(needle) => {
                let finder = memchr::memmem::Finder::new(needle.as_bytes());
                let mut from = 0;
                // One pass over all names; after a hit, continue at the next name.
                while let Some(i) = finder.find(&self.buf[from..]) {
                    let at = from + i;
                    let ord = self.starts.partition_point(|&s| s as usize <= at) - 1;
                    ords.insert(ord as u32);
                    from = self.starts[ord + 1] as usize;
                    if from >= self.buf.len() {
                        break;
                    }
                }
            }
            Pattern::Whole(re) => {
                for ord in 0..self.count() {
                    if re.is_match(self.name(ord)) {
                        ords.insert(ord as u32);
                    }
                }
            }
        }
        ords
    }
}

fn cache() -> &'static Mutex<HashMap<SegmentId, Arc<SegmentNames>>> {
    static CACHE: OnceLock<Mutex<HashMap<SegmentId, Arc<SegmentNames>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn segment_names(reader: &SegmentReader) -> tantivy::Result<Arc<SegmentNames>> {
    let id = reader.segment_id();
    if let Some(n) = cache().lock().get(&id) {
        return Ok(n.clone());
    }
    // Built outside the lock: other segments can be served meanwhile. Two queries racing on a
    // new segment both build it once; the second insert just replaces an identical table.
    let names = Arc::new(SegmentNames::build(reader)?);
    cache().lock().insert(id, names.clone());
    Ok(names)
}

/// Drop cached tables of segments that `searcher` no longer has (after merges). Cheap: only
/// does work when the index generation changed since the last call.
pub fn prune(searcher: &Searcher) {
    static LAST: AtomicU64 = AtomicU64::new(u64::MAX);
    let generation = searcher.generation().generation_id();
    if LAST.swap(generation, Ordering::Relaxed) == generation {
        return;
    }
    let live: HashSet<SegmentId> = searcher.segment_readers().iter().map(|r| r.segment_id()).collect();
    cache().lock().retain(|id, _| live.contains(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> SegmentNames {
        let mut buf = Vec::new();
        let mut starts = Vec::new();
        for n in list {
            starts.push(buf.len() as u32);
            buf.extend_from_slice(n.as_bytes());
            buf.push(0);
        }
        starts.push(buf.len() as u32);
        SegmentNames { buf, starts, doc_ords: Vec::new() }
    }

    fn hits(n: &SegmentNames, q: &str) -> Vec<String> {
        let set = n.matching(&NameQuery::new(q).unwrap().pattern);
        (0..n.count()).filter(|&o| set.contains(o as u32)).map(|o| String::from_utf8_lossy(n.name(o)).into_owned()).collect()
    }

    #[test]
    fn substrings_and_globs() {
        let n = names(&["annual report.pdf", "geçiş izni.jpg", "report.docx", "reports-2026.xlsx", "summary.txt"]);
        assert_eq!(hits(&n, "Report"), ["annual report.pdf", "report.docx", "reports-2026.xlsx"]);
        assert_eq!(hits(&n, "geçiş"), ["geçiş izni.jpg"]);
        assert_eq!(hits(&n, "*.pdf"), ["annual report.pdf"]);
        assert_eq!(hits(&n, "report.*"), ["report.docx"]);
        assert_eq!(hits(&n, "report?-*"), ["reports-2026.xlsx"]);
        assert_eq!(hits(&n, "y.txt"), ["summary.txt"], "a match at the end of the buffer");
        assert_eq!(hits(&n, "pdfgeç"), Vec::<String>::new(), "matches never span two names");
        assert_eq!(hits(&n, "").len(), 5);
    }

    #[test]
    fn glob_metacharacters_are_literal() {
        let n = names(&["a+b (1).txt", "ab1.txt"]);
        assert_eq!(hits(&n, "a+b (?).txt"), ["a+b (1).txt"]);
        assert_eq!(hits(&n, "(1)"), ["a+b (1).txt"]);
    }
}
