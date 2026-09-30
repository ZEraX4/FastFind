//! Text analysis shared by the index, the query planner and the highlighter.
//!
//! Using one tokenizer everywhere guarantees that what is highlighted is exactly what matched.
//!
//! Rules:
//! * a token is a maximal run of alphanumeric characters (`_`, `-`, `.` and all punctuation
//!   split tokens, so `my_var`, `invoice-2024` and `a.b` are searchable by their parts);
//! * CJK ideographs and kana are emitted one character per token (phrase queries then match
//!   character sequences, as Lucene's StandardTokenizer does);
//! * tokens longer than [`MAX_TOKEN_CHARS`] are dropped (hashes, base64 blobs);
//! * normalisation = lowercase + diacritic folding (`Café` → `cafe`).

use std::borrow::Cow;

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

pub const TOKENIZER_NAME: &str = "ff";
pub const MAX_TOKEN_CHARS: usize = 64;

#[inline]
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x31F0..=0x31FF // Katakana phonetic extensions
        | 0x3400..=0x4DBF // CJK Ext A
        | 0x4E00..=0x9FFF // CJK Unified
        | 0xF900..=0xFAFF // CJK Compatibility
        | 0x20000..=0x2FA1F)
}

#[inline]
fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

/// Iterator over `(byte_start, byte_end)` of tokens in `text`.
pub struct Words<'a> {
    text: &'a str,
    pos: usize,
}

pub fn words(text: &str) -> Words<'_> {
    Words { text, pos: 0 }
}

impl<'a> Iterator for Words<'a> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        let bytes = self.text.as_bytes();
        loop {
            // Skip separators quickly on the ASCII fast path.
            while self.pos < bytes.len() && bytes[self.pos] < 0x80 && !bytes[self.pos].is_ascii_alphanumeric() {
                self.pos += 1;
            }
            if self.pos >= bytes.len() {
                return None;
            }
            let rest = &self.text[self.pos..];
            let mut chars = rest.char_indices();
            let (_, first) = chars.next()?;
            if !is_word(first) {
                self.pos += first.len_utf8();
                continue;
            }
            let start = self.pos;
            if is_cjk(first) {
                self.pos += first.len_utf8();
                return Some((start, self.pos));
            }
            let mut end = start + first.len_utf8();
            let mut count = 1usize;
            for (i, c) in chars {
                if !is_word(c) || is_cjk(c) {
                    break;
                }
                end = start + i + c.len_utf8();
                count += 1;
            }
            self.pos = end;
            if count > MAX_TOKEN_CHARS {
                continue;
            }
            return Some((start, end));
        }
    }
}

/// Lowercase + fold diacritics. Allocation-free for tokens that are already lowercase ASCII.
pub fn normalize(token: &str) -> Cow<'_, str> {
    if token.is_ascii() {
        if token.bytes().any(|b| b.is_ascii_uppercase()) {
            return Cow::Owned(token.to_ascii_lowercase());
        }
        return Cow::Borrowed(token);
    }
    let mut out = String::with_capacity(token.len());
    for c in token.chars().flat_map(|c| c.to_lowercase()) {
        match c {
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ø' => out.push('o'),
            'ł' => out.push('l'),
            'đ' | 'ð' => out.push('d'),
            'þ' => out.push_str("th"),
            'ı' => out.push('i'),
            _ => {
                for d in c.nfd() {
                    if !is_combining_mark(d) {
                        out.push(d);
                    }
                }
            }
        }
    }
    Cow::Owned(out)
}

/// Normalised tokens of a string (used to analyse query terms).
pub fn analyze(text: &str) -> Vec<String> {
    words(text).map(|(s, e)| normalize(&text[s..e]).into_owned()).collect()
}

/// Tantivy adapter.
#[derive(Clone, Default)]
pub struct FfTokenizer {
    token: Token,
}

pub struct FfTokenStream<'a> {
    text: &'a str,
    words: Words<'a>,
    token: &'a mut Token,
}

impl Tokenizer for FfTokenizer {
    type TokenStream<'a> = FfTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> FfTokenStream<'a> {
        self.token.reset();
        FfTokenStream { text, words: words(text), token: &mut self.token }
    }
}

impl TokenStream for FfTokenStream<'_> {
    fn advance(&mut self) -> bool {
        match self.words.next() {
            Some((s, e)) => {
                self.token.text.clear();
                self.token.text.push_str(&normalize(&self.text[s..e]));
                self.token.offset_from = s;
                self.token.offset_to = e;
                self.token.position = self.token.position.wrapping_add(1);
                self.token.position_length = 1;
                true
            }
            None => false,
        }
    }

    fn token(&self) -> &Token {
        self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<&str> {
        words(s).map(|(a, b)| &s[a..b]).collect()
    }

    #[test]
    fn splits_on_punctuation() {
        assert_eq!(toks("invoice-2024, my_var a.b"), ["invoice", "2024", "my", "var", "a", "b"]);
    }

    #[test]
    fn cjk_single_chars() {
        assert_eq!(toks("東京abc"), ["東", "京", "abc"]);
    }

    #[test]
    fn folds_case_and_accents() {
        assert_eq!(normalize("Café"), "cafe");
        assert_eq!(normalize("STRASSE"), "strasse");
        assert_eq!(normalize("Straße"), "strasse");
        assert_eq!(normalize("ÆØ"), "aeo");
        assert!(matches!(normalize("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn drops_overlong_tokens() {
        let long = "a".repeat(MAX_TOKEN_CHARS + 1);
        let s = format!("x {long} y");
        assert_eq!(toks(&s), ["x", "y"]);
    }

    #[test]
    fn tantivy_stream_positions() {
        let mut t = FfTokenizer::default();
        let mut s = t.token_stream("Hello Wörld");
        assert!(s.advance());
        assert_eq!(s.token().text, "hello");
        assert_eq!(s.token().position, 0);
        assert!(s.advance());
        assert_eq!(s.token().text, "world");
        assert_eq!(s.token().position, 1);
        assert!(!s.advance());
    }
}
