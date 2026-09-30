//! Query execution.
//!
//! Three strategies:
//! * **Indexed** – answered entirely by Tantivy (Smart, Filename). Top-k with offset, exact
//!   total from a `Count` collector, deterministic ranking (score, then path hash).
//! * **Verified** – Tantivy yields a candidate superset in rank order; candidates are checked
//!   against their text in parallel batches until the page is full (case-sensitive, Exact).
//! * **Scan** – regex over (prefiltered) candidates with a time budget (Regex mode).
//!
//! Verified/scan progress is kept in a session per query so paging continues where it left
//! off instead of starting over.

use std::cmp::Reverse;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lru::LruCache;
use parking_lot::Mutex;
use rayon::prelude::*;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::{DocAddress, DocId, Order, Score, Searcher, SegmentReader, TantivyDocument, Term};

use super::matcher::{build_regex, exact_pattern, Highlighter, NoHighlight, PositiveTokenHighlighter, RegexMatcher, TokenMatcher, Verifier};
use super::plan::Planner;
use super::query::{self, FieldFilter, Node};
use super::snippet::{self, SnippetOptions, STREAM_SCAN_BYTES};
use crate::config::SearchSettings;
use crate::error::{Error, Result};
use crate::index::{Fields, IndexStore, TextStore};
use crate::model::{MatchKey, Preview, ResultItem, SearchMode, SearchRequest, SearchResponse, SnippetResult, SortOrder, Strategy};
use crate::parsers::{ParserRegistry, TextMode};
use crate::textsource::TextSource;
use crate::util::CancelToken;

const VERIFY_BATCH: usize = 256;

/// (page of hits, verified total so far, candidates exhausted, stopped by time budget)
type VerifiedPage = (Vec<(f32, DocAddress)>, u64, bool, bool);

/// A query compiled once per (query, mode, case, whole-word).
pub struct Compiled {
    pub strategy: Strategy,
    ast: Option<Node>,
    /// Literal (Exact), pattern (Regex) or name words (Filename).
    text: String,
    filters: Vec<FieldFilter>,
    pub highlighter: Arc<dyn Highlighter>,
    verifier: Option<Arc<Verifier>>,
    mode: SearchMode,
    whole_word: bool,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PageKey {
    req: SearchRequest,
    generation: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    req: SearchRequest,
    generation: u64,
}

#[derive(Default)]
struct Session {
    verified: Vec<(f32, DocAddress)>,
    next_candidate: usize,
    exhausted: bool,
}

/// Minimal stored fields needed to verify/snippet a document.
struct DocLite {
    path: String,
    name: String,
    mode: TextMode,
}

pub struct SearchService {
    store: Arc<IndexStore>,
    texts: Arc<TextStore>,
    registry: Arc<ParserRegistry>,
    compiled: Mutex<LruCache<MatchKey, Arc<Compiled>>>,
    pages: Mutex<LruCache<PageKey, SearchResponse>>,
    sessions: Mutex<LruCache<SessionKey, Arc<Mutex<Session>>>>,
    snippets: Mutex<LruCache<(MatchKey, String, u64), SnippetResult>>,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n.max(1)).unwrap()
}

impl SearchService {
    pub fn new(store: Arc<IndexStore>, texts: Arc<TextStore>, registry: Arc<ParserRegistry>, cache_mb: u32) -> Self {
        // Rough sizing: a cached page is ~30 KB, a snippet ~0.5 KB.
        let pages = (cache_mb as usize * 4).clamp(64, 4096);
        Self {
            store,
            texts,
            registry,
            compiled: Mutex::new(LruCache::new(nz(256))),
            pages: Mutex::new(LruCache::new(nz(pages))),
            sessions: Mutex::new(LruCache::new(nz(32))),
            snippets: Mutex::new(LruCache::new(nz(pages * 20))),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn cache_len(&self) -> usize {
        self.pages.lock().len()
    }

    pub fn clear_caches(&self) {
        self.pages.lock().clear();
        self.sessions.lock().clear();
        self.snippets.lock().clear();
    }

    fn fields(&self) -> &Fields {
        &self.store.fields
    }

    pub fn compile(&self, key: &MatchKey) -> Result<Arc<Compiled>> {
        if let Some(c) = self.compiled.lock().get(key) {
            return Ok(c.clone());
        }
        let cs = key.case_sensitive;
        let ww = key.whole_word;
        let c = match key.mode {
            SearchMode::Smart => {
                let ast = query::parse(&key.query)?;
                let (highlighter, verifier): (Arc<dyn Highlighter>, Option<Arc<Verifier>>) = match &ast {
                    Some(a) if query::has_text(a) => (
                        Arc::new(PositiveTokenHighlighter::new(a, cs, ww)),
                        cs.then(|| Arc::new(Verifier::Tokens(TokenMatcher::from_ast(a, true, ww)))),
                    ),
                    _ => (Arc::new(NoHighlight), None),
                };
                Compiled {
                    strategy: if verifier.is_some() { Strategy::Verified } else { Strategy::Indexed },
                    ast,
                    text: String::new(),
                    filters: vec![],
                    highlighter,
                    verifier,
                    mode: key.mode,
                    whole_word: ww,
                }
            }
            SearchMode::Exact => {
                let (lit, filters) = query::split_filters(&key.query)?;
                if lit.is_empty() {
                    Compiled { strategy: Strategy::Indexed, ast: None, text: lit, filters, highlighter: Arc::new(NoHighlight), verifier: None, mode: key.mode, whole_word: ww }
                } else {
                    let re = build_regex(&exact_pattern(&lit, ww), cs)?;
                    Compiled {
                        strategy: Strategy::Verified,
                        ast: None,
                        text: lit,
                        filters,
                        highlighter: Arc::new(RegexMatcher { re: re.clone() }),
                        verifier: Some(Arc::new(Verifier::Regex(re))),
                        mode: key.mode,
                        whole_word: ww,
                    }
                }
            }
            SearchMode::Regex => {
                let (pat, filters) = query::split_filters(&key.query)?;
                if pat.is_empty() {
                    Compiled { strategy: Strategy::Indexed, ast: None, text: pat, filters, highlighter: Arc::new(NoHighlight), verifier: None, mode: key.mode, whole_word: ww }
                } else {
                    let pattern = if ww { format!(r"\b(?:{pat})\b") } else { pat.clone() };
                    let re = build_regex(&pattern, cs)?;
                    Compiled {
                        strategy: Strategy::Scan,
                        ast: None,
                        text: pat,
                        filters,
                        highlighter: Arc::new(RegexMatcher { re: re.clone() }),
                        verifier: Some(Arc::new(Verifier::Regex(re))),
                        mode: key.mode,
                        whole_word: ww,
                    }
                }
            }
            SearchMode::Filename => {
                let (text, filters) = query::split_filters(&key.query)?;
                Compiled { strategy: Strategy::Indexed, ast: None, text, filters, highlighter: Arc::new(NoHighlight), verifier: None, mode: key.mode, whole_word: ww }
            }
        };
        let c = Arc::new(c);
        self.compiled.lock().put(key.clone(), c.clone());
        Ok(c)
    }

    fn build_query(&self, c: &Compiled, req: &SearchRequest, s: &SearchSettings) -> Result<Option<Box<dyn Query>>> {
        let planner = Planner::new(self.fields(), s, c.whole_word);
        let user: Option<Box<dyn Query>> = match c.mode {
            SearchMode::Smart => match &c.ast {
                Some(a) => planner.node(a)?,
                None => None,
            },
            SearchMode::Exact if !c.text.is_empty() => Some(planner.literal_candidates(&c.text)?.unwrap_or_else(|| Box::new(AllQuery))),
            SearchMode::Regex if !c.text.is_empty() => Some(planner.regex_prefilter(&c.text).unwrap_or_else(|| Box::new(AllQuery))),
            SearchMode::Filename if !c.text.is_empty() => planner.filename_query(&c.text)?,
            _ => None,
        };
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for f in &c.filters {
            clauses.push((Occur::Must, Box::new(tantivy::query::ConstScoreQuery::new(planner.field(f)?, 0.0))));
        }
        for q in planner.ui_filters(&req.filters)? {
            clauses.push((Occur::Must, q));
        }
        match user {
            Some(q) if clauses.is_empty() => Ok(Some(q)),
            Some(q) => {
                clauses.push((Occur::Must, q));
                Ok(Some(Box::new(BooleanQuery::new(clauses))))
            }
            None if clauses.is_empty() => Ok(None),
            None => {
                clauses.push((Occur::Must, Box::new(AllQuery)));
                Ok(Some(Box::new(BooleanQuery::new(clauses))))
            }
        }
    }

    /// Top documents for a page. Relevance ties are broken by path hash, so identical queries
    /// always return identical orderings.
    fn collect(&self, searcher: &Searcher, q: &dyn Query, sort: SortOrder, offset: usize, limit: usize, count: bool) -> Result<(u64, Vec<(f32, DocAddress)>)> {
        let top = TopDocs::with_limit(limit.max(1)).and_offset(offset);
        macro_rules! run {
            ($c:expr, $map:expr) => {{
                if count {
                    let (n, docs) = searcher.search(q, &(Count, $c))?;
                    (n as u64, docs.into_iter().map($map).collect())
                } else {
                    let docs = searcher.search(q, &$c)?;
                    (0u64, docs.into_iter().map($map).collect())
                }
            }};
        }
        Ok(match sort {
            SortOrder::Relevance => {
                let c = top.tweak_score(move |seg: &SegmentReader| {
                    let col = seg.fast_fields().u64("pid").ok();
                    move |doc: DocId, score: Score| (score, Reverse(col.as_ref().and_then(|c| c.first(doc)).unwrap_or(0)))
                });
                run!(c, |((s, _), a): ((f32, Reverse<u64>), DocAddress)| (s, a))
            }
            SortOrder::Modified => run!(top.order_by_fast_field::<i64>("mtime", Order::Desc), |(_, a): (Option<i64>, DocAddress)| (0.0, a)),
            SortOrder::Size => run!(top.order_by_u64_field("size", Order::Desc), |(_, a): (Option<u64>, DocAddress)| (0.0, a)),
            SortOrder::Name => run!(top.order_by_string_fast_field("name_lc", Order::Asc), |(_, a): (Option<String>, DocAddress)| (0.0, a)),
        })
    }

    fn item(&self, doc: &TantivyDocument, score: f32) -> ResultItem {
        let f = self.fields();
        let s = |fl| doc.get_first(fl).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let u = |fl| doc.get_first(fl).and_then(|v| v.as_u64()).unwrap_or(0);
        let path = s(f.path);
        let dir = std::path::Path::new(&path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let pages = u(f.pages);
        ResultItem {
            name: s(f.name),
            dir,
            ext: s(f.ext),
            kind: s(f.kind),
            size: u(f.size),
            modified: doc.get_first(f.mtime).and_then(|v| v.as_i64()).unwrap_or(0),
            score,
            flags: u(f.flags),
            pages: (pages > 0).then_some(pages as u32),
            path,
        }
    }

    fn lite(&self, doc: &TantivyDocument) -> DocLite {
        let f = self.fields();
        DocLite {
            path: doc.get_first(f.path).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name: doc.get_first(f.name).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            mode: TextMode::from_u64(doc.get_first(f.mode).and_then(|v| v.as_u64()).unwrap_or(0)),
        }
    }

    fn find_doc(&self, searcher: &Searcher, path: &str) -> Option<TantivyDocument> {
        let q = TermQuery::new(Term::from_field_text(self.fields().path, path), IndexRecordOption::Basic);
        let hits = searcher.search(&q, &TopDocs::with_limit(1).order_by_score()).ok()?;
        let (_, addr) = hits.first()?;
        searcher.doc(*addr).ok()
    }

    pub fn search(&self, req: &SearchRequest, s: &SearchSettings, cancel: &CancelToken) -> Result<SearchResponse> {
        let t0 = Instant::now();
        let searcher = self.store.searcher();
        let generation = searcher.generation().generation_id();
        let key = PageKey { req: req.clone(), generation };
        if let Some(mut hit) = self.pages.lock().get(&key).cloned() {
            self.hits.fetch_add(1, Ordering::Relaxed);
            hit.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
            return Ok(hit);
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let compiled = self.compile(&req.match_key())?;
        let limit = req.limit.clamp(1, 500) as usize;
        let offset = req.offset as usize;
        let empty = |strategy: Strategy| SearchResponse {
            items: vec![],
            total: 0,
            total_is_lower_bound: false,
            offset: req.offset,
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            strategy,
            partial: false,
            warnings: vec![],
            generation: self.store.generation(),
        };
        let Some(q) = self.build_query(&compiled, req, s)? else { return Ok(empty(compiled.strategy.clone())) };
        if offset >= s.result_limit as usize {
            return Ok(empty(compiled.strategy.clone()));
        }
        let mut warnings = Vec::new();
        let (items, total, lower_bound, partial) = match (&compiled.strategy, &compiled.verifier) {
            (Strategy::Indexed, _) | (_, None) => {
                let (total, hits) = self.collect(&searcher, q.as_ref(), req.sort, offset, limit, true)?;
                let items = hits.into_iter().filter_map(|(sc, a)| searcher.doc::<TantivyDocument>(a).ok().map(|d| self.item(&d, sc))).collect();
                (items, total, false, false)
            }
            (strategy, Some(verifier)) => {
                let budget = Duration::from_millis(if matches!(strategy, Strategy::Scan) { s.regex_budget_ms } else { s.verify_budget_ms });
                if matches!(strategy, Strategy::Scan) && planner_scans_everything(&compiled) {
                    warnings.push("This regular expression has no fixed text to narrow the search, so every document is scanned. Add filters (ext:, in:) or a literal word to speed it up.".into());
                }
                let (hits, total, exhausted, partial) = self.verified_page(&searcher, generation, req, q.as_ref(), verifier, offset, limit, budget, cancel)?;
                let items = hits.into_iter().filter_map(|(sc, a)| searcher.doc::<TantivyDocument>(a).ok().map(|d| self.item(&d, sc))).collect();
                (items, total, !exhausted, partial)
            }
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let resp = SearchResponse {
            items,
            total: total.min(s.result_limit as u64),
            total_is_lower_bound: lower_bound || total > s.result_limit as u64,
            offset: req.offset,
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            strategy: compiled.strategy.clone(),
            partial,
            warnings,
            generation: self.store.generation(),
        };
        if !partial {
            self.pages.lock().put(key, resp.clone());
        }
        Ok(resp)
    }

    #[allow(clippy::too_many_arguments)]
    fn verified_page(
        &self,
        searcher: &Searcher,
        generation: u64,
        req: &SearchRequest,
        q: &dyn Query,
        verifier: &Verifier,
        offset: usize,
        limit: usize,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<VerifiedPage> {
        let mut base = req.clone();
        base.offset = 0;
        base.limit = 0;
        let skey = SessionKey { req: base, generation };
        let session = self.sessions.lock().get_or_insert(skey, || Arc::new(Mutex::new(Session::default()))).clone();
        let mut sess = session.lock();
        let deadline = Instant::now() + budget;
        let need = offset + limit + 1; // +1 tells us whether there is a next page
        let mut partial = false;
        while sess.verified.len() < need && !sess.exhausted {
            if cancel.is_cancelled() || Instant::now() > deadline {
                partial = !cancel.is_cancelled();
                break;
            }
            let (_, batch) = self.collect(searcher, q, req.sort, sess.next_candidate, VERIFY_BATCH, false)?;
            if batch.len() < VERIFY_BATCH {
                sess.exhausted = true;
            }
            sess.next_candidate += batch.len();
            let docs: Vec<(f32, DocAddress, DocLite)> = batch
                .into_iter()
                .filter_map(|(sc, a)| searcher.doc::<TantivyDocument>(a).ok().map(|d| (sc, a, self.lite(&d))))
                .collect();
            let keep_going = || !cancel.is_cancelled() && Instant::now() <= deadline;
            let verdicts: Vec<bool> = docs
                .par_iter()
                .map(|(_, _, d)| {
                    if !keep_going() {
                        return false;
                    }
                    let src = TextSource::load(&d.path, d.mode, &self.texts, &self.registry);
                    verifier.verify_chunks(&d.name, |each| {
                        src.for_each_chunk(STREAM_SCAN_BYTES, |c, _| if each(c) { ControlFlow::Continue(()) } else { ControlFlow::Break(()) });
                    }, &keep_going)
                })
                .collect();
            if !keep_going() && !sess.exhausted {
                // The batch was cut short: re-verify it next time instead of dropping matches.
                sess.next_candidate -= docs.len();
                partial = !cancel.is_cancelled();
                break;
            }
            for ((sc, a, _), ok) in docs.into_iter().zip(verdicts) {
                if ok {
                    sess.verified.push((sc, a));
                }
            }
        }
        let total = sess.verified.len() as u64;
        let page = sess.verified.iter().skip(offset).take(limit).copied().collect();
        Ok((page, total, sess.exhausted, partial))
    }

    /// Snippets and match counts for the given result paths (computed in parallel, cached).
    pub fn snippets(&self, req: &SearchRequest, paths: &[String]) -> Result<Vec<SnippetResult>> {
        let key = req.match_key();
        let compiled = self.compile(&key)?;
        let searcher = self.store.searcher();
        let generation = searcher.generation().generation_id();
        let opts = SnippetOptions::default();
        Ok(paths
            .par_iter()
            .map(|p| {
                let ck = (key.clone(), p.clone(), generation);
                if let Some(r) = self.snippets.lock().get(&ck) {
                    return r.clone();
                }
                let r = match self.find_doc(&searcher, p) {
                    Some(doc) => {
                        let d = self.lite(&doc);
                        let src = TextSource::load(&d.path, d.mode, &self.texts, &self.registry);
                        snippet::snippets(p, &src, compiled.highlighter.as_ref(), &opts)
                    }
                    None => SnippetResult { path: p.clone(), note: Some("not in index".into()), ..Default::default() },
                };
                self.snippets.lock().put(ck, r.clone());
                r
            })
            .collect())
    }

    pub fn preview(&self, req: &SearchRequest, path: &str) -> Result<Preview> {
        let compiled = self.compile(&req.match_key())?;
        let searcher = self.store.searcher();
        let doc = self.find_doc(&searcher, path).ok_or_else(|| Error::InvalidInput("file is not in the index".into()))?;
        let item = self.item(&doc, 0.0);
        let d = self.lite(&doc);
        let info: serde_json::Value = doc
            .get_first(self.fields().info)
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let mut p = Preview {
            path: item.path.clone(),
            name: item.name.clone(),
            kind: item.kind.clone(),
            size: item.size,
            modified: item.modified,
            flags: item.flags,
            pages: item.pages,
            title: info.get("title").and_then(|v| v.as_str()).map(String::from),
            author: info.get("author").and_then(|v| v.as_str()).map(String::from),
            ..Default::default()
        };
        crate::preview::flag_notes(&mut p);
        if item.flags & crate::model::flags::NAME_ONLY == 0 || item.flags & crate::model::flags::OCR != 0 {
            let src = TextSource::load(&d.path, d.mode, &self.texts, &self.registry);
            crate::preview::build(&mut p, &src, compiled.highlighter.as_ref());
        }
        Ok(p)
    }

    /// Is this path in the index? (Used to restrict "open file" to indexed files.)
    pub fn contains(&self, path: &str) -> bool {
        self.find_doc(&self.store.searcher(), path).is_some()
    }
}

fn planner_scans_everything(c: &Compiled) -> bool {
    c.mode == SearchMode::Regex && super::plan::longest_required_literal(&c.text).map(|l| l.chars().filter(|c| c.is_alphanumeric()).count() < 3).unwrap_or(true)
}
