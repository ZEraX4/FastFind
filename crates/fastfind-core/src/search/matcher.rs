//! Finding match spans in text (highlighting, match counts) and verifying candidates that the
//! index can only approximate (case-sensitive, exact-string and regex searches).

use std::collections::VecDeque;

use regex::{Regex, RegexBuilder};

use super::plan::{MIN_EXPLICIT_PREFIX, MIN_IMPLICIT_PREFIX};
use super::query::Node;
use crate::analysis::{normalize, words};
use crate::error::{Error, Result};

/// Finds match spans (byte ranges) in text.
pub trait Highlighter: Send + Sync {
    /// Append up to `limit` spans to `out`, in text order.
    fn find(&self, text: &str, out: &mut Vec<(usize, usize)>, limit: usize);
}

#[derive(Debug, Clone)]
struct Leaf {
    tokens: Vec<String>,
    prefix_last: bool,
}

/// Token-level matcher mirroring the index analysis. Case-insensitive matching compares
/// normalised tokens (lowercase + diacritic folding); case-sensitive compares raw tokens.
pub struct TokenMatcher {
    leaves: Vec<Leaf>,
    case_sensitive: bool,
    max_len: usize,
    /// Boolean structure for verification (leaf ids in DFS order).
    tree: Option<Tree>,
}

#[derive(Debug, Clone)]
enum Tree {
    And(Vec<Tree>),
    Or(Vec<Tree>),
    Not(Box<Tree>),
    Leaf(usize),
    /// Field filters are enforced by the index.
    True,
}

impl TokenMatcher {
    /// Build from a Smart-mode AST.
    pub fn from_ast(ast: &Node, case_sensitive: bool, whole_word: bool) -> Self {
        let mut leaves = Vec::new();
        let tree = build_tree(ast, case_sensitive, whole_word, &mut leaves);
        let max_len = leaves.iter().map(|l| l.tokens.len()).max().unwrap_or(1).max(1);
        Self { leaves, case_sensitive, max_len, tree: Some(tree) }
    }

    fn token_key<'a>(&self, raw: &'a str) -> std::borrow::Cow<'a, str> {
        if self.case_sensitive { std::borrow::Cow::Borrowed(raw) } else { normalize(raw) }
    }

    /// Scan text once; call `hit(leaf, start, end)` for every leaf occurrence. Stops early when
    /// `hit` returns false.
    fn scan(&self, text: &str, mut hit: impl FnMut(usize, usize, usize) -> bool) {
        if self.leaves.is_empty() {
            return;
        }
        let mut window: VecDeque<(String, usize, usize)> = VecDeque::with_capacity(self.max_len);
        for (s, e) in words(text) {
            let key = self.token_key(&text[s..e]).into_owned();
            if window.len() == self.max_len {
                window.pop_front();
            }
            window.push_back((key, s, e));
            for (li, leaf) in self.leaves.iter().enumerate() {
                let n = leaf.tokens.len();
                if n == 0 || n > window.len() {
                    continue;
                }
                let base = window.len() - n;
                let ok = leaf.tokens.iter().enumerate().all(|(i, t)| {
                    let w = &window[base + i].0;
                    if i + 1 == n && leaf.prefix_last { w.starts_with(t.as_str()) } else { w == t }
                });
                if ok && !hit(li, window[base].1, window[window.len() - 1].2) {
                    return;
                }
            }
        }
    }

    pub fn new_found(&self) -> Vec<bool> {
        vec![false; self.leaves.len()]
    }

    /// Mark leaves occurring in `text` (call repeatedly for chunks of a large document).
    /// Returns true once every leaf has been found.
    pub fn mark(&self, text: &str, found: &mut [bool]) -> bool {
        let mut remaining = found.iter().filter(|f| !**f).count();
        if remaining == 0 {
            return true;
        }
        self.scan(text, |li, _, _| {
            if !found[li] {
                found[li] = true;
                remaining -= 1;
            }
            remaining > 0
        });
        remaining == 0
    }

    pub fn eval(&self, found: &[bool]) -> bool {
        match &self.tree {
            Some(t) => eval(t, found),
            None => found.iter().any(|&f| f),
        }
    }

    /// Does `text` (or the file name) satisfy the query's boolean structure?
    pub fn verify(&self, name: &str, text: &str) -> bool {
        let mut found = self.new_found();
        if !self.mark(name, &mut found) {
            self.mark(text, &mut found);
        }
        self.eval(&found)
    }

    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }
}

fn leaf_for(text: &str, is_phrase: bool, explicit_prefix: bool, case_sensitive: bool, whole_word: bool) -> Leaf {
    let tokens: Vec<String> = words(text)
        .map(|(s, e)| if case_sensitive { text[s..e].to_string() } else { normalize(&text[s..e]).into_owned() })
        .collect();
    let last = tokens.last().map(|t| t.chars().count()).unwrap_or(0);
    let prefix_last = !is_phrase && if explicit_prefix { last >= MIN_EXPLICIT_PREFIX } else { !whole_word && last >= MIN_IMPLICIT_PREFIX };
    Leaf { tokens, prefix_last }
}

fn build_tree(n: &Node, cs: bool, ww: bool, leaves: &mut Vec<Leaf>) -> Tree {
    match n {
        Node::And(v) => Tree::And(v.iter().map(|c| build_tree(c, cs, ww, leaves)).collect()),
        Node::Or(v) => Tree::Or(v.iter().map(|c| build_tree(c, cs, ww, leaves)).collect()),
        Node::Not(c) => Tree::Not(Box::new(build_tree(c, cs, ww, leaves))),
        Node::Field(_) => Tree::True,
        Node::Term { text, prefix } => {
            let l = leaf_for(text, false, *prefix, cs, ww);
            if l.tokens.is_empty() {
                return Tree::True;
            }
            leaves.push(l);
            Tree::Leaf(leaves.len() - 1)
        }
        Node::Phrase(p) => {
            let l = leaf_for(p, true, false, cs, ww);
            if l.tokens.is_empty() {
                return Tree::True;
            }
            leaves.push(l);
            Tree::Leaf(leaves.len() - 1)
        }
    }
}

fn eval(t: &Tree, found: &[bool]) -> bool {
    match t {
        Tree::And(v) => v.iter().all(|c| eval(c, found)),
        Tree::Or(v) => v.iter().any(|c| eval(c, found)),
        Tree::Not(c) => !eval(c, found),
        Tree::Leaf(i) => found[*i],
        Tree::True => true,
    }
}

/// Highlight only leaves that are not negated.
pub struct PositiveTokenHighlighter(TokenMatcher);

impl PositiveTokenHighlighter {
    pub fn new(ast: &Node, case_sensitive: bool, whole_word: bool) -> Self {
        let mut pos = Vec::new();
        super::query::positive_leaves(ast, &mut pos);
        let mut leaves = Vec::new();
        for n in &pos {
            build_tree(n, case_sensitive, whole_word, &mut leaves);
        }
        let max_len = leaves.iter().map(|l| l.tokens.len()).max().unwrap_or(1).max(1);
        Self(TokenMatcher { leaves, case_sensitive, max_len, tree: None })
    }
}

impl Highlighter for PositiveTokenHighlighter {
    fn find(&self, text: &str, out: &mut Vec<(usize, usize)>, limit: usize) {
        let start_len = out.len();
        let mut last_end = 0usize;
        self.0.scan(text, |_, s, e| {
            // Overlapping hits of different leaves: keep the first.
            if s >= last_end || out.len() == start_len {
                out.push((s, e));
                last_end = e;
            }
            out.len() - start_len < limit
        });
    }
}

/// Regex-based matcher for Exact and Regex modes.
pub struct RegexMatcher {
    pub re: Regex,
}

/// Linear-time regex (Rust's engine never backtracks) with compile-size limits, so hostile or
/// accidental patterns cannot hang a search.
pub fn build_regex(pattern: &str, case_sensitive: bool) -> Result<Regex> {
    if pattern.len() > 2000 {
        return Err(Error::Query("regular expression too long (max 2000 characters)".into()));
    }
    RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .multi_line(true)
        .size_limit(8 << 20)
        .dfa_size_limit(16 << 20)
        .nest_limit(64)
        .build()
        .map_err(|e| Error::Query(format!("invalid regular expression: {e}")))
}

/// Exact-string pattern: whitespace runs match any whitespace (text extracted from documents
/// often has line breaks where the user typed spaces); `whole_word` anchors at word boundaries.
pub fn exact_pattern(literal: &str, whole_word: bool) -> String {
    let parts: Vec<String> = literal.split_whitespace().map(regex::escape).collect();
    let mut p = parts.join(r"\s+");
    if whole_word {
        if literal.trim_start().starts_with(|c: char| c.is_alphanumeric()) {
            p = format!(r"\b{p}");
        }
        if literal.trim_end().ends_with(|c: char| c.is_alphanumeric()) {
            p.push_str(r"\b");
        }
    }
    p
}

impl Highlighter for RegexMatcher {
    fn find(&self, text: &str, out: &mut Vec<(usize, usize)>, limit: usize) {
        for m in self.re.find_iter(text).take(limit) {
            if m.start() < m.end() {
                out.push((m.start(), m.end()));
            }
        }
    }
}

/// Candidate verification strategy.
pub enum Verifier {
    Tokens(TokenMatcher),
    Regex(Regex),
}

impl Verifier {
    pub fn verify(&self, name: &str, text: &str) -> bool {
        match self {
            Verifier::Tokens(t) => t.verify(name, text),
            Verifier::Regex(r) => r.is_match(name) || r.is_match(text),
        }
    }

    /// Verify a document delivered in chunks; `keep_going` is polled between chunks so a
    /// search can be cancelled or hit its time budget mid-file.
    pub fn verify_chunks(&self, name: &str, mut each_chunk: impl FnMut(&mut dyn FnMut(&str) -> bool), keep_going: &dyn Fn() -> bool) -> bool {
        match self {
            Verifier::Regex(r) => {
                if r.is_match(name) {
                    return true;
                }
                let mut hit = false;
                each_chunk(&mut |c| {
                    hit = r.is_match(c);
                    !hit && keep_going()
                });
                hit
            }
            Verifier::Tokens(t) => {
                let mut found = t.new_found();
                if !t.mark(name, &mut found) {
                    each_chunk(&mut |c| !t.mark(c, &mut found) && keep_going());
                }
                t.eval(&found)
            }
        }
    }
}

/// Matches nothing (filename-only queries and filter-only queries): snippets fall back to
/// the beginning of the document.
pub struct NoHighlight;

impl Highlighter for NoHighlight {
    fn find(&self, _: &str, _: &mut Vec<(usize, usize)>, _: usize) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::query::parse;

    fn spans(h: &dyn Highlighter, text: &str) -> Vec<String> {
        let mut v = Vec::new();
        h.find(text, &mut v, 100);
        v.into_iter().map(|(s, e)| text[s..e].to_string()).collect()
    }

    #[test]
    fn highlights_terms_phrases_and_prefixes() {
        let ast = parse("\"annual report\" invoic -draft").unwrap().unwrap();
        let h = PositiveTokenHighlighter::new(&ast, false, false);
        let text = "The Annual  Report lists INVOICES; draft invoice.";
        assert_eq!(spans(&h, text), ["Annual  Report", "INVOICES", "invoice"]);
        // Whole word: no prefix expansion.
        let h = PositiveTokenHighlighter::new(&parse("invoic").unwrap().unwrap(), false, true);
        assert!(spans(&h, text).is_empty());
        // Accent folding.
        let h = PositiveTokenHighlighter::new(&parse("cafe").unwrap().unwrap(), false, true);
        assert_eq!(spans(&h, "Un café noir"), ["café"]);
    }

    #[test]
    fn case_sensitive_verification_with_boolean_logic() {
        let ast = parse("Rust AND (tokio OR async) -Java").unwrap().unwrap();
        let v = TokenMatcher::from_ast(&ast, true, true);
        assert!(v.verify("x", "Rust with tokio"));
        assert!(!v.verify("x", "rust with tokio"), "case must match");
        assert!(!v.verify("x", "Rust tokio and Java"));
        assert!(v.verify("Rust notes.txt", "about async"), "name hits count");
    }

    #[test]
    fn exact_and_regex() {
        let re = build_regex(&exact_pattern("foo.bar()", false), false).unwrap();
        assert!(re.is_match("call FOO.BAR() now"));
        assert!(!re.is_match("fooXbar()"));
        let re = build_regex(&exact_pattern("annual report", true), true).unwrap();
        assert!(re.is_match("the annual\nreport"));
        assert!(!re.is_match("semiannual report"));
        assert!(build_regex("(a", false).is_err());
        // Catastrophic-backtracking classics are linear here.
        let re = build_regex("(a+)+$", false).unwrap();
        let s = "a".repeat(50_000) + "!";
        let t = std::time::Instant::now();
        assert!(!re.is_match(&s));
        assert!(t.elapsed().as_secs() < 2);
    }
}
