//! Search syntax → AST.
//!
//! ```text
//! invoice                      word (matches word beginnings unless "whole word" is on)
//! "annual report"              exact phrase
//! invoice AND customer         both (AND is implicit between words)
//! invoice OR receipt           either (also `|`)
//! NOT draft, -draft            exclude
//! (a OR b) c                   grouping
//! invoi*                       explicit prefix
//! filename:report  name:*.pdf  file-name contains / glob
//! ext:pdf  ext:docx,xlsx       extension(s)
//! type:spreadsheet             format family (pdf, word, spreadsheet, presentation, text, code, …)
//! path:Projects                directory path contains the word(s)
//! in:"C:\Work\2026"            only below this folder
//! modified:2026-01-01          that day;  >2026-01-01, <=2026-03, 2026-01..2026-03, today, 7d
//! size:>10mb  size:<100kb  size:1mb..5mb
//! ```
//!
//! Operators must be upper-case (`AND`/`OR`/`NOT`) so the words "and"/"or" stay searchable.

use chrono::{Duration as CDuration, Local, NaiveDate, TimeZone};

use crate::error::{Error, Result};
use crate::parsers::Kind;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Term { text: String, prefix: bool },
    Phrase(String),
    Field(FieldFilter),
}

/// `[lo, hi)` range; `None` = unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range<T> {
    pub lo: Option<T>,
    pub hi: Option<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldFilter {
    /// File-name substring or glob (`*`, `?`).
    Name(String),
    Ext(Vec<String>),
    Kind(Vec<String>),
    /// Words that must appear in the directory path.
    Path(String),
    /// Directory restriction (absolute path prefix).
    In(String),
    Modified(Range<i64>),
    Size(Range<u64>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Word(String),
    Quoted(String),
    Field(String, String),
}

fn canonical_field(f: &str) -> Option<&'static str> {
    Some(match f.to_ascii_lowercase().as_str() {
        "filename" | "name" | "file" => "name",
        "ext" | "extension" => "ext",
        "type" | "kind" => "type",
        "path" => "path",
        "in" | "dir" | "folder" => "in",
        "modified" | "date" | "mtime" => "modified",
        "size" => "size",
        _ => return None,
    })
}

fn lex(input: &str) -> Vec<Tok> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let read_quoted = |i: &mut usize| -> String {
        // `*i` points after the opening quote. An unterminated quote runs to the end.
        let mut s = String::new();
        while *i < chars.len() && chars[*i] != '"' {
            s.push(chars[*i]);
            *i += 1;
        }
        *i += 1;
        s
    };
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '"' => {
                i += 1;
                out.push(Tok::Quoted(read_quoted(&mut i)));
            }
            '-' if i + 1 < chars.len() && !chars[i + 1].is_whitespace() && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(') => {
                out.push(Tok::Not);
                i += 1;
            }
            '|' => {
                out.push(Tok::Or);
                i += if chars.get(i + 1) == Some(&'|') { 2 } else { 1 };
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                out.push(Tok::And);
                i += 2;
            }
            _ => {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')' && chars[i] != '"' {
                    if chars[i] == ':' {
                        break;
                    }
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if i < chars.len() && chars[i] == ':' && canonical_field(&word).is_some() {
                    i += 1;
                    let value = if i < chars.len() && chars[i] == '"' {
                        i += 1;
                        read_quoted(&mut i)
                    } else {
                        let vs = i;
                        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != ')' {
                            i += 1;
                        }
                        chars[vs..i].iter().collect()
                    };
                    out.push(Tok::Field(canonical_field(&word).unwrap().to_string(), value));
                    continue;
                }
                // Not a field: a word that may contain ':' (e.g. "12:30", "C:\x").
                while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')' && chars[i] != '"' {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.as_str() {
                    "AND" => out.push(Tok::And),
                    "OR" => out.push(Tok::Or),
                    "NOT" => out.push(Tok::Not),
                    _ => out.push(Tok::Word(word)),
                }
            }
        }
    }
    out
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn or(&mut self) -> Result<Option<Node>> {
        let mut parts = Vec::new();
        if let Some(n) = self.and()? {
            parts.push(n);
        }
        while self.peek() == Some(&Tok::Or) {
            self.next();
            if let Some(n) = self.and()? {
                parts.push(n);
            }
        }
        Ok(match parts.len() {
            0 => None,
            1 => parts.pop(),
            _ => Some(Node::Or(parts)),
        })
    }

    fn and(&mut self) -> Result<Option<Node>> {
        let mut parts = Vec::new();
        loop {
            match self.peek() {
                None | Some(Tok::Or) | Some(Tok::RParen) => break,
                Some(Tok::And) => {
                    self.next();
                }
                _ => {
                    if let Some(n) = self.unary()? {
                        parts.push(n);
                    }
                }
            }
        }
        Ok(match parts.len() {
            0 => None,
            1 => parts.pop(),
            _ => Some(Node::And(parts)),
        })
    }

    fn unary(&mut self) -> Result<Option<Node>> {
        if self.peek() == Some(&Tok::Not) {
            self.next();
            return Ok(self.unary()?.map(|n| Node::Not(Box::new(n))));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Option<Node>> {
        match self.next() {
            Some(Tok::LParen) => {
                self.depth += 1;
                if self.depth > 64 {
                    return Err(Error::Query("too many nested parentheses".into()));
                }
                let inner = self.or()?;
                if self.peek() == Some(&Tok::RParen) {
                    self.next();
                }
                self.depth -= 1;
                Ok(inner)
            }
            Some(Tok::Quoted(s)) => Ok((!s.trim().is_empty()).then_some(Node::Phrase(s))),
            Some(Tok::Word(w)) => {
                let prefix = w.ends_with('*') && w.len() > 1;
                let text = w.trim_end_matches('*').to_string();
                Ok((!text.is_empty()).then_some(Node::Term { text, prefix }))
            }
            Some(Tok::Field(f, v)) => Ok(Some(Node::Field(parse_field(&f, &v)?))),
            // Stray `)`, `AND` or `NOT` at the end.
            _ => Ok(None),
        }
    }
}

/// Parse the Smart-mode syntax. `Ok(None)` for an empty query.
pub fn parse(input: &str) -> Result<Option<Node>> {
    if input.len() > 8192 {
        return Err(Error::Query("query too long".into()));
    }
    let mut p = Parser { toks: lex(input), pos: 0, depth: 0 };
    let mut parts = Vec::new();
    while p.pos < p.toks.len() {
        if let Some(n) = p.or()? {
            parts.push(n);
        }
        // Skip an unbalanced ')' and continue.
        if p.peek() == Some(&Tok::RParen) {
            p.next();
        }
    }
    Ok(match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Node::And(parts)),
    })
}

/// For Exact/Regex/Filename modes: pull whitespace-separated `field:value` filters out of the
/// input; the rest (original spacing preserved) is the literal/pattern.
pub fn split_filters(input: &str) -> Result<(String, Vec<FieldFilter>)> {
    let mut filters = Vec::new();
    let mut rest = String::new();
    let mut chars = input.char_indices().peekable();
    let mut token_start = None;
    let bytes = input;
    let mut spans = Vec::new();
    while let Some((i, c)) = chars.next() {
        if c.is_whitespace() {
            if let Some(s) = token_start.take() {
                spans.push((s, i));
            }
        } else if token_start.is_none() {
            token_start = Some(i);
            // A quoted field value may contain spaces: `in:"C:\My Docs"`.
            if let Some(colon) = bytes[i..].find(':') {
                let field = &bytes[i..i + colon];
                if canonical_field(field).is_some() && bytes[i + colon + 1..].starts_with('"') {
                    let vstart = i + colon + 2;
                    let vend = bytes[vstart..].find('"').map(|p| vstart + p + 1).unwrap_or(bytes.len());
                    spans.push((i, vend));
                    token_start = None;
                    while chars.peek().map(|(j, _)| *j < vend).unwrap_or(false) {
                        chars.next();
                    }
                }
            }
        }
    }
    if let Some(s) = token_start {
        spans.push((s, input.len()));
    }
    let mut last = 0;
    for (s, e) in spans {
        let tok = &input[s..e];
        let is_field = tok.find(':').map(|c| canonical_field(&tok[..c]).is_some() && c + 1 < tok.len()).unwrap_or(false);
        if is_field {
            let c = tok.find(':').unwrap();
            let v = tok[c + 1..].trim_matches('"');
            filters.push(parse_field(canonical_field(&tok[..c]).unwrap(), v)?);
            rest.push_str(&input[last..s]);
            last = e;
        }
    }
    rest.push_str(&input[last..]);
    Ok((rest.trim().to_string(), filters))
}

fn parse_field(field: &str, value: &str) -> Result<FieldFilter> {
    let v = value.trim();
    if v.is_empty() {
        return Err(Error::Query(format!("{field}: needs a value")));
    }
    Ok(match field {
        "name" => FieldFilter::Name(v.to_string()),
        "ext" => FieldFilter::Ext(
            v.split([',', '|', ';'])
                .map(|e| e.trim().trim_start_matches('*').trim_start_matches('.').to_lowercase())
                .filter(|e| !e.is_empty())
                .collect(),
        ),
        "type" => {
            let mut kinds = Vec::new();
            for k in v.split([',', '|']) {
                match Kind::parse(k.trim()) {
                    Some(k) => kinds.push(k.as_str().to_string()),
                    None => return Err(Error::Query(format!(
                        "unknown type '{k}' (use pdf, word, spreadsheet, presentation, text, code, data, web, ebook, image, other)"
                    ))),
                }
            }
            FieldFilter::Kind(kinds)
        }
        "path" => FieldFilter::Path(v.to_string()),
        "in" => FieldFilter::In(v.to_string()),
        "modified" => FieldFilter::Modified(parse_date_range(v, Local::now().timestamp())?),
        "size" => FieldFilter::Size(parse_size_range(v)?),
        _ => return Err(Error::Query(format!("unknown field {field}"))),
    })
}

fn split_op(v: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = v.strip_prefix(op) {
            return (op, rest.trim());
        }
    }
    ("", v)
}

fn local_ts(d: NaiveDate) -> i64 {
    Local
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|t| t.timestamp())
        .unwrap_or_else(|| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
}

/// `[start, end)` of an absolute date expression (year, month or day), or of a relative one.
fn date_span(v: &str, now: i64) -> Result<(i64, i64)> {
    let bad = || Error::Query(format!("cannot understand date '{v}' (use YYYY, YYYY-MM, YYYY-MM-DD, today, yesterday or 7d/4w/6m/1y)"));
    let today = Local.timestamp_opt(now, 0).single().ok_or_else(bad)?.date_naive();
    match v.to_ascii_lowercase().as_str() {
        "today" => return Ok((local_ts(today), local_ts(today + CDuration::days(1)))),
        "yesterday" => return Ok((local_ts(today - CDuration::days(1)), local_ts(today))),
        _ => {}
    }
    let parts: Vec<&str> = v.split(['-', '/', '.']).collect();
    let num = |s: &str| s.parse::<u32>().map_err(|_| bad());
    match parts.as_slice() {
        [y] if y.len() == 4 => {
            let y = num(y)? as i32;
            let a = NaiveDate::from_ymd_opt(y, 1, 1).ok_or_else(bad)?;
            let b = NaiveDate::from_ymd_opt(y + 1, 1, 1).ok_or_else(bad)?;
            Ok((local_ts(a), local_ts(b)))
        }
        [y, m] if y.len() == 4 => {
            let (y, m) = (num(y)? as i32, num(m)?);
            let a = NaiveDate::from_ymd_opt(y, m, 1).ok_or_else(bad)?;
            let b = if m == 12 { NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { NaiveDate::from_ymd_opt(y, m + 1, 1) }.ok_or_else(bad)?;
            Ok((local_ts(a), local_ts(b)))
        }
        [y, m, d] if y.len() == 4 => {
            let a = NaiveDate::from_ymd_opt(num(y)? as i32, num(m)?, num(d)?).ok_or_else(bad)?;
            Ok((local_ts(a), local_ts(a + CDuration::days(1))))
        }
        _ => Err(bad()),
    }
}

/// Relative age like `7d`, `2w`, `6m`, `1y` → seconds.
fn relative_secs(v: &str) -> Option<i64> {
    let v = v.to_ascii_lowercase();
    let (n, unit) = v.split_at(v.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = n.parse().ok()?;
    let day = 86_400;
    Some(n * match unit {
        "h" => 3600,
        "d" => day,
        "w" => 7 * day,
        "m" => 30 * day,
        "y" => 365 * day,
        _ => return None,
    })
}

pub fn parse_date_range(v: &str, now: i64) -> Result<Range<i64>> {
    if let Some((a, b)) = v.split_once("..") {
        let lo = if a.is_empty() { None } else { Some(date_span(a, now)?.0) };
        let hi = if b.is_empty() { None } else { Some(date_span(b, now)?.1) };
        return Ok(Range { lo, hi });
    }
    let (op, rest) = split_op(v);
    if let Some(secs) = relative_secs(rest) {
        // "7d" / "<7d" = within the last 7 days; ">7d" = older than that.
        return Ok(match op {
            ">" | ">=" => Range { lo: None, hi: Some(now - secs) },
            _ => Range { lo: Some(now - secs), hi: None },
        });
    }
    let (start, end) = date_span(rest, now)?;
    Ok(match op {
        ">" => Range { lo: Some(end), hi: None },
        ">=" => Range { lo: Some(start), hi: None },
        "<" => Range { lo: None, hi: Some(start) },
        "<=" => Range { lo: None, hi: Some(end) },
        _ => Range { lo: Some(start), hi: Some(end) },
    })
}

pub fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_lowercase();
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let (n, unit) = v.split_at(split);
    let n: f64 = n.parse().ok()?;
    let mul: f64 = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((n * mul) as u64)
}

pub fn parse_size_range(v: &str) -> Result<Range<u64>> {
    let bad = || Error::Query(format!("cannot understand size '{v}' (use e.g. >10mb, <500kb, 1mb..5mb)"));
    if let Some((a, b)) = v.split_once("..") {
        let lo = if a.is_empty() { None } else { Some(parse_size(a).ok_or_else(bad)?) };
        let hi = if b.is_empty() { None } else { Some(parse_size(b).ok_or_else(bad)?.saturating_add(1)) };
        return Ok(Range { lo, hi });
    }
    let (op, rest) = split_op(v);
    let n = parse_size(rest).ok_or_else(bad)?;
    Ok(match op {
        ">" => Range { lo: Some(n.saturating_add(1)), hi: None },
        "<" => Range { lo: None, hi: Some(n) },
        "<=" => Range { lo: None, hi: Some(n.saturating_add(1)) },
        "=" => Range { lo: Some(n), hi: Some(n.saturating_add(1)) },
        // Plain `size:10mb` reads as "at least 10 MB".
        _ => Range { lo: Some(n), hi: None },
    })
}

/// Positive text leaves (terms/phrases not under NOT) — used for highlighting.
pub fn positive_leaves(n: &Node, out: &mut Vec<Node>) {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().for_each(|c| positive_leaves(c, out)),
        Node::Not(_) | Node::Field(_) => {}
        leaf => out.push(leaf.clone()),
    }
}

/// Does the query contain any content leaf (vs. filters only)?
pub fn has_text(n: &Node) -> bool {
    match n {
        Node::And(v) | Node::Or(v) => v.iter().any(has_text),
        Node::Not(c) => has_text(c),
        Node::Field(_) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Node {
        Node::Term { text: s.into(), prefix: false }
    }

    #[test]
    fn precedence_and_implicit_and() {
        assert_eq!(parse("a b").unwrap(), Some(Node::And(vec![t("a"), t("b")])));
        assert_eq!(
            parse("a OR b c").unwrap(),
            Some(Node::Or(vec![t("a"), Node::And(vec![t("b"), t("c")])]))
        );
        assert_eq!(
            parse("(a OR b) AND -c").unwrap(),
            Some(Node::And(vec![Node::Or(vec![t("a"), t("b")]), Node::Not(Box::new(t("c")))]))
        );
        // Lower-case "and"/"or" are ordinary words.
        assert_eq!(parse("rock and roll").unwrap(), Some(Node::And(vec![t("rock"), t("and"), t("roll")])));
    }

    #[test]
    fn phrases_prefix_and_fields() {
        assert_eq!(parse("\"annual report\"").unwrap(), Some(Node::Phrase("annual report".into())));
        assert_eq!(parse("invoi*").unwrap(), Some(Node::Term { text: "invoi".into(), prefix: true }));
        assert_eq!(parse("ext:pdf,DOCX").unwrap(), Some(Node::Field(FieldFilter::Ext(vec!["pdf".into(), "docx".into()]))));
        assert_eq!(parse("filename:report").unwrap(), Some(Node::Field(FieldFilter::Name("report".into()))));
        assert_eq!(parse("in:\"C:\\My Docs\" x").unwrap(), Some(Node::And(vec![Node::Field(FieldFilter::In("C:\\My Docs".into())), t("x")])));
        // Unknown "field" stays a word (times, URLs, drive letters).
        assert_eq!(parse("12:30").unwrap(), Some(t("12:30")));
        assert!(parse("type:banana").is_err());
    }

    #[test]
    fn robust_to_garbage() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("   ").unwrap(), None);
        assert!(parse("((((a").unwrap().is_some());
        assert!(parse("a ) b").unwrap().is_some());
        assert_eq!(parse("\"unterminated phrase").unwrap(), Some(Node::Phrase("unterminated phrase".into())));
        assert!(parse(&"(".repeat(100)).is_err());
        assert_eq!(parse("OR AND NOT").unwrap(), None);
    }

    #[test]
    fn dates() {
        let now = local_ts(NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()) + 3600;
        let day = |y, m, d| local_ts(NaiveDate::from_ymd_opt(y, m, d).unwrap());
        assert_eq!(parse_date_range("2026-01-01", now).unwrap(), Range { lo: Some(day(2026, 1, 1)), hi: Some(day(2026, 1, 2)) });
        assert_eq!(parse_date_range(">2026-01", now).unwrap(), Range { lo: Some(day(2026, 2, 1)), hi: None });
        assert_eq!(parse_date_range("<=2025", now).unwrap(), Range { lo: None, hi: Some(day(2026, 1, 1)) });
        assert_eq!(parse_date_range("2026-01..2026-03", now).unwrap(), Range { lo: Some(day(2026, 1, 1)), hi: Some(day(2026, 4, 1)) });
        assert_eq!(parse_date_range("7d", now).unwrap(), Range { lo: Some(now - 7 * 86400), hi: None });
        assert_eq!(parse_date_range("today", now).unwrap().lo, Some(day(2026, 9, 24)));
        assert!(parse_date_range("2026-13-01", now).is_err());
        assert!(parse_date_range("soon", now).is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("10mb"), Some(10 << 20));
        assert_eq!(parse_size("1.5k"), Some(1536));
        assert_eq!(parse_size_range(">10mb").unwrap(), Range { lo: Some((10 << 20) + 1), hi: None });
        assert_eq!(parse_size_range("<1kb").unwrap(), Range { lo: None, hi: Some(1024) });
        assert_eq!(parse_size_range("1mb..2mb").unwrap(), Range { lo: Some(1 << 20), hi: Some((2 << 20) + 1) });
        assert!(parse_size_range("big").is_err());
    }

    #[test]
    fn split_filters_for_literal_modes() {
        let (rest, f) = split_filters("invoice-[0-9]{4} ext:pdf in:\"C:\\A B\"").unwrap();
        assert_eq!(rest, "invoice-[0-9]{4}");
        assert_eq!(f.len(), 2);
        let (rest, f) = split_filters("  foo  bar ").unwrap();
        assert_eq!(rest, "foo  bar");
        assert!(f.is_empty());
    }
}
