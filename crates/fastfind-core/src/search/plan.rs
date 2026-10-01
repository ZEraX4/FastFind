//! AST + UI filters → Tantivy query.
//!
//! Every text leaf becomes `content ∨ name^nb ∨ meta^mb` so a file-name hit ranks higher than a
//! body hit, and phrases get an extra boost. Filters are wrapped in constant-score queries so
//! they restrict without changing ranking.

use std::ops::Bound;

use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, Occur, PhrasePrefixQuery, PhraseQuery, Query,
    RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::Term;

use super::query::{FieldFilter, Node, Range};
use crate::analysis::{analyze, words};
use crate::config::SearchSettings;
use crate::error::{Error, Result};
use crate::index::Fields;
use crate::model::SearchFilters;
use crate::util::{filter_form, resolve_dir};

/// Minimum characters before a word is also matched as a prefix (shorter prefixes would expand
/// to huge numbers of terms).
pub const MIN_IMPLICIT_PREFIX: usize = 3;
pub const MIN_EXPLICIT_PREFIX: usize = 2;

pub struct Planner<'a> {
    pub f: &'a Fields,
    pub whole_word: bool,
    pub name_boost: f32,
    pub meta_boost: f32,
    pub phrase_boost: f32,
}

type Q = Box<dyn Query>;

fn term_q(field: Field, tok: &str) -> Q {
    Box::new(TermQuery::new(Term::from_field_text(field, tok), IndexRecordOption::WithFreqs))
}

fn regex_q(field: Field, pattern: &str) -> Result<Q> {
    Ok(Box::new(RegexQuery::from_pattern(pattern, field).map_err(|e| Error::Query(e.to_string()))?))
}

fn filter_q(q: Q) -> Q {
    Box::new(ConstScoreQuery::new(q, 0.0))
}

fn should(qs: Vec<Q>) -> Q {
    if qs.len() == 1 {
        return qs.into_iter().next().unwrap();
    }
    Box::new(BooleanQuery::new(qs.into_iter().map(|q| (Occur::Should, q)).collect()))
}

/// Glob (`*`, `?`) → anchored regex over the whole term; plain text → substring.
pub fn name_pattern(value: &str) -> String {
    let v = value.to_lowercase();
    if v.contains('*') || v.contains('?') {
        let mut out = String::new();
        for c in v.chars() {
            match c {
                '*' => out.push_str(".*"),
                '?' => out.push('.'),
                c => out.push_str(&regex::escape(&c.to_string())),
            }
        }
        out
    } else {
        format!(".*{}.*", regex::escape(&v))
    }
}

impl<'a> Planner<'a> {
    pub fn new(f: &'a Fields, s: &SearchSettings, whole_word: bool) -> Self {
        Self { f, whole_word, name_boost: s.name_boost, meta_boost: s.metadata_boost, phrase_boost: s.phrase_boost }
    }

    fn text_fields(&self) -> [(Field, f32); 3] {
        [(self.f.content, 1.0), (self.f.name, self.name_boost), (self.f.meta, self.meta_boost)]
    }

    /// One word or phrase across content/name/meta.
    pub fn text_leaf(&self, text: &str, is_phrase: bool, explicit_prefix: bool) -> Result<Option<Q>> {
        let toks = analyze(text);
        if toks.is_empty() {
            return Ok(None);
        }
        let last_len = toks.last().map(|t| t.chars().count()).unwrap_or(0);
        let prefix = if is_phrase {
            false
        } else if explicit_prefix {
            last_len >= MIN_EXPLICIT_PREFIX
        } else {
            !self.whole_word && last_len >= MIN_IMPLICIT_PREFIX
        };
        let mut per_field = Vec::new();
        for (field, boost) in self.text_fields() {
            if boost <= 0.0 {
                continue;
            }
            let q: Q = if toks.len() == 1 {
                if prefix {
                    // Exact term too, so exact matches outrank prefix expansions.
                    let exact = term_q(field, &toks[0]);
                    let pre = regex_q(field, &format!("{}.*", regex::escape(&toks[0])))?;
                    Box::new(BooleanQuery::new(vec![(Occur::Should, exact), (Occur::Should, pre)]))
                } else {
                    term_q(field, &toks[0])
                }
            } else {
                let terms: Vec<Term> = toks.iter().map(|t| Term::from_field_text(field, t)).collect();
                let pq: Q = if prefix { Box::new(PhrasePrefixQuery::new(terms)) } else { Box::new(PhraseQuery::new(terms)) };
                Box::new(BoostQuery::new(pq, self.phrase_boost))
            };
            per_field.push(if (boost - 1.0).abs() > f32::EPSILON { Box::new(BoostQuery::new(q, boost)) as Q } else { q });
        }
        Ok(Some(should(per_field)))
    }

    pub fn node(&self, n: &Node) -> Result<Option<Q>> {
        match n {
            Node::Term { text, prefix } => self.text_leaf(text, false, *prefix),
            Node::Phrase(p) => self.text_leaf(p, true, false),
            Node::Field(f) => Ok(Some(filter_q(self.field(f)?))),
            Node::Not(inner) => Ok(self.node(inner)?.map(|q| {
                Box::new(BooleanQuery::new(vec![(Occur::Must, Box::new(AllQuery) as Q), (Occur::MustNot, q)])) as Q
            })),
            Node::And(children) => {
                let mut clauses: Vec<(Occur, Q)> = Vec::new();
                for c in children {
                    match c {
                        Node::Not(inner) => {
                            if let Some(q) = self.node(inner)? {
                                clauses.push((Occur::MustNot, q));
                            }
                        }
                        other => {
                            if let Some(q) = self.node(other)? {
                                clauses.push((Occur::Must, q));
                            }
                        }
                    }
                }
                if clauses.is_empty() {
                    return Ok(None);
                }
                if !clauses.iter().any(|(o, _)| *o == Occur::Must) {
                    clauses.push((Occur::Must, Box::new(AllQuery)));
                }
                Ok(Some(Box::new(BooleanQuery::new(clauses))))
            }
            Node::Or(children) => {
                let mut qs = Vec::new();
                for c in children {
                    if let Some(q) = self.node(c)? {
                        qs.push(q);
                    }
                }
                Ok((!qs.is_empty()).then(|| should(qs)))
            }
        }
    }

    pub fn field(&self, f: &FieldFilter) -> Result<Q> {
        let fl = self.f;
        Ok(match f {
            FieldFilter::Name(v) => regex_q(fl.name_lc, &name_pattern(v))?,
            FieldFilter::Ext(exts) => should(exts.iter().map(|e| term_q_raw(fl.ext, e)).collect()),
            FieldFilter::Kind(kinds) => should(kinds.iter().map(|k| term_q_raw(fl.kind, k)).collect()),
            FieldFilter::Path(v) => {
                let prefix = v.ends_with('*');
                let toks = analyze(v.trim_end_matches('*'));
                match toks.len() {
                    0 => Box::new(AllQuery),
                    1 if prefix => regex_q(fl.path_t, &format!("{}.*", regex::escape(&toks[0])))?,
                    1 => term_q(fl.path_t, &toks[0]),
                    _ => Box::new(PhraseQuery::new(toks.iter().map(|t| Term::from_field_text(fl.path_t, t)).collect())),
                }
            }
            FieldFilter::In(dir) => dir_query(fl, dir)?,
            FieldFilter::Modified(r) => range_i64(fl.mtime, *r),
            FieldFilter::Size(r) => range_u64(fl.size, *r),
        })
    }

    /// Structured filters from the UI.
    pub fn ui_filters(&self, sf: &SearchFilters) -> Result<Vec<Q>> {
        let mut out: Vec<Q> = Vec::new();
        if !sf.exts.is_empty() {
            let exts: Vec<String> = sf.exts.iter().map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
            if !exts.is_empty() {
                out.push(self.field(&FieldFilter::Ext(exts))?);
            }
        }
        if !sf.kinds.is_empty() {
            out.push(self.field(&FieldFilter::Kind(sf.kinds.clone()))?);
        }
        if !sf.dirs.is_empty() {
            let mut qs = Vec::new();
            for d in &sf.dirs {
                qs.push(dir_query(self.f, d)?);
            }
            out.push(should(qs));
        }
        if sf.modified_after.is_some() || sf.modified_before.is_some() {
            out.push(range_i64(self.f.mtime, Range { lo: sf.modified_after, hi: sf.modified_before }));
        }
        if sf.size_min.is_some() || sf.size_max.is_some() {
            out.push(range_u64(self.f.size, Range { lo: sf.size_min, hi: sf.size_max.map(|m| m.saturating_add(1)) }));
        }
        Ok(out.into_iter().map(filter_q).collect())
    }

    /// Filename mode: every whitespace-separated word must occur in the file name
    /// (substring or glob); exact name-token hits rank first.
    pub fn filename_query(&self, text: &str) -> Result<Option<Q>> {
        let mut clauses: Vec<(Occur, Q)> = Vec::new();
        for w in text.split_whitespace() {
            clauses.push((Occur::Must, regex_q(self.f.name_lc, &name_pattern(w))?));
            for t in analyze(w.trim_matches(|c| c == '*' || c == '?')) {
                clauses.push((Occur::Should, term_q(self.f.name, &t)));
            }
        }
        Ok((!clauses.is_empty()).then(|| Box::new(BooleanQuery::new(clauses)) as Q))
    }

    /// Candidate superset for an exact literal: the phrase of its tokens (the verifier then
    /// checks punctuation, case and whole-word exactly). `None` = no tokens → must scan.
    pub fn literal_candidates(&self, literal: &str) -> Result<Option<Q>> {
        self.text_leaf(literal, true, false).map(|q| q.map(|q| q as Q))
    }

    /// Cheap index prefilter for a regex: documents whose content contains *every* literal the
    /// regex requires (or whose name contains the longest one). `None` = no usable literal →
    /// scan all candidates.
    pub fn regex_prefilter(&self, pattern: &str) -> Option<Q> {
        let lits = required_literals(pattern);
        let mut content: Vec<(Occur, Q)> = Vec::new();
        for lit in &lits {
            content.extend(self.literal_clauses(lit));
        }
        if content.is_empty() {
            return None;
        }
        let longest = lits.iter().max_by_key(|l| l.chars().filter(|c| c.is_alphanumeric()).count())?;
        let name = regex_q(self.f.name_lc, &format!(".*{}.*", regex::escape(&longest.to_lowercase()))).ok()?;
        Some(Box::new(BooleanQuery::new(vec![
            (Occur::Should, Box::new(BooleanQuery::new(content)) as Q),
            (Occur::Should, name),
        ])))
    }

    /// Index clauses guaranteeing that `lit` can occur in the content.
    fn literal_clauses(&self, lit: &str) -> Vec<(Occur, Q)> {
        let spans: Vec<(usize, usize)> = words(lit).collect();
        let mut out: Vec<(Occur, Q)> = Vec::new();
        let n = spans.len();
        for (i, (s, e)) in spans.iter().enumerate() {
            let t = crate::analysis::normalize(&lit[*s..*e]).into_owned();
            let esc = regex::escape(&t);
            // A token touching the literal's edge may be part of a longer word in the text.
            let open_start = i == 0 && *s == 0;
            let open_end = i + 1 == n && *e == lit.len();
            let q: Option<Q> = match (open_start, open_end) {
                (false, false) => Some(term_q(self.f.content, &t)),
                _ if t.chars().count() < 3 => None,
                (true, true) => regex_q(self.f.content, &format!(".*{esc}.*")).ok(),
                (true, false) => regex_q(self.f.content, &format!(".*{esc}")).ok(),
                (false, true) => regex_q(self.f.content, &format!("{esc}.*")).ok(),
            };
            if let Some(q) = q {
                out.push((Occur::Must, q));
            }
        }
        out
    }
}

fn term_q_raw(field: Field, v: &str) -> Q {
    Box::new(TermQuery::new(Term::from_field_text(field, v), IndexRecordOption::Basic))
}

fn dir_query(f: &Fields, dir: &str) -> Result<Q> {
    let mut d = filter_form(&resolve_dir(dir.trim()));
    if d.is_empty() {
        return Ok(Box::new(AllQuery));
    }
    if !d.ends_with('/') {
        d.push('/');
    }
    regex_q(f.dir, &format!("{}.*", regex::escape(&d)))
}

fn range_i64(field: Field, r: Range<i64>) -> Q {
    let lo = r.lo.map(|v| Bound::Included(Term::from_field_i64(field, v))).unwrap_or(Bound::Unbounded);
    let hi = r.hi.map(|v| Bound::Excluded(Term::from_field_i64(field, v))).unwrap_or(Bound::Unbounded);
    if matches!((&lo, &hi), (Bound::Unbounded, Bound::Unbounded)) {
        return Box::new(AllQuery);
    }
    if let (Some(a), Some(b)) = (r.lo, r.hi) {
        if a >= b {
            return Box::new(EmptyQuery);
        }
    }
    Box::new(RangeQuery::new(lo, hi))
}

fn range_u64(field: Field, r: Range<u64>) -> Q {
    let lo = r.lo.map(|v| Bound::Included(Term::from_field_u64(field, v))).unwrap_or(Bound::Unbounded);
    let hi = r.hi.map(|v| Bound::Excluded(Term::from_field_u64(field, v))).unwrap_or(Bound::Unbounded);
    if matches!((&lo, &hi), (Bound::Unbounded, Bound::Unbounded)) {
        return Box::new(AllQuery);
    }
    if let (Some(a), Some(b)) = (r.lo, r.hi) {
        if a >= b {
            return Box::new(EmptyQuery);
        }
    }
    Box::new(RangeQuery::new(lo, hi))
}

/// Literal strings that every match of `pattern` must contain (conservative: gives up on
/// alternations, inline case-insensitivity and anything it does not understand).
pub fn required_literals(pattern: &str) -> Vec<String> {
    use regex_syntax::hir::{Hir, HirKind};
    let Ok(hir) = regex_syntax::ParserBuilder::new().build().parse(pattern) else { return vec![] };
    fn walk(h: &Hir, out: &mut Vec<String>) {
        match h.kind() {
            HirKind::Literal(l) => out.push(String::from_utf8_lossy(&l.0).into_owned()),
            HirKind::Capture(c) => walk(&c.sub, out),
            HirKind::Repetition(r) if r.min >= 1 => walk(&r.sub, out),
            HirKind::Concat(subs) => {
                let mut cur = String::new();
                for s in subs {
                    if let HirKind::Literal(l) = s.kind() {
                        cur.push_str(&String::from_utf8_lossy(&l.0));
                    } else {
                        if !cur.is_empty() {
                            out.push(std::mem::take(&mut cur));
                        }
                        walk(s, out);
                    }
                }
                if !cur.is_empty() {
                    out.push(cur);
                }
            }
            _ => {}
        }
    }
    let mut lits = Vec::new();
    walk(&hir, &mut lits);
    lits.retain(|l| l.chars().any(|c| c.is_alphanumeric()));
    lits
}

/// The most informative required literal (most alphanumeric characters).
pub fn longest_required_literal(pattern: &str) -> Option<String> {
    required_literals(pattern).into_iter().rev().max_by_key(|l| l.chars().filter(|c| c.is_alphanumeric()).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_extraction() {
        assert_eq!(longest_required_literal("invoice-[0-9]{4}").as_deref(), Some("invoice-"));
        assert_eq!(longest_required_literal(r"\d+ total amount").as_deref(), Some(" total amount"));
        assert_eq!(required_literals("zeppel[a-z]+ total"), vec!["zeppel".to_string(), " total".to_string()]);
        assert_eq!(longest_required_literal("zeppel[a-z]+ total").as_deref(), Some("zeppel"));
        assert_eq!(longest_required_literal("(foo|bar)baz"), Some("baz".into()));
        assert_eq!(longest_required_literal("foo|bar"), None);
        assert_eq!(longest_required_literal("[a-z]+"), None);
        assert_eq!(longest_required_literal("(abc)+x?"), Some("abc".into()));
    }

    #[test]
    fn globs() {
        assert_eq!(name_pattern("*.PDF"), r".*\.pdf");
        assert_eq!(name_pattern("report"), ".*report.*");
        assert_eq!(name_pattern("a?c"), "a.c");
    }
}
