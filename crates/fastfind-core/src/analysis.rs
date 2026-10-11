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
//! * normalisation = lowercase + diacritic folding (`Café` → `cafe`, harakat removed), plus
//!   Arabic letter folding: alef forms → ا, ة → ه, ى/ی → ي, ک → ك, tatweel removed, and
//!   Arabic-Indic / Persian digits → 0-9;
//! * Arabic words with a definite article (`المدرسة`, `والمدرسة`, `للمدرسة`…) are indexed in
//!   both forms at the same position, and queries use the form without the article
//!   ([`strip_article`], [`query_form`]).
//!
//! Changing any rule changes what existing indexes contain: bump
//! [`crate::index::schema::SCHEMA_VERSION`] in the same commit. Existing indexes are then
//! re-tokenised from stored text on the next start ([`crate::index::migrate`]); only files
//! without stored text are read again.

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
            // Most combining marks (Arabic harakat, decomposed accents) count as alphabetic and
            // so continue a word, but one cannot start a word: alone it would normalise to "".
            if !is_word(first) || is_combining_mark(first) {
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
                        if let Some(f) = fold_char(d) {
                            out.push(f);
                        }
                    }
                }
            }
        }
    }
    Cow::Owned(out)
}

/// Letter folds applied after decomposition (which already turns أ إ آ into ا and drops
/// harakat). `None` removes the character.
#[inline]
fn fold_char(c: char) -> Option<char> {
    Some(match c {
        '\u{0640}' => return None,                  // tatweel (kashida)
        '\u{0671}' => '\u{0627}',                   // alef wasla → alef
        '\u{0629}' => '\u{0647}',                   // teh marbuta → heh
        '\u{0649}' | '\u{06CC}' => '\u{064A}',      // alef maksura, Persian yeh → yeh
        '\u{06A9}' => '\u{0643}',                   // keheh → kaf
        '\u{0660}'..='\u{0669}' => char::from(b'0' + (c as u32 - 0x0660) as u8), // Arabic-Indic digits
        '\u{06F0}'..='\u{06F9}' => char::from(b'0' + (c as u32 - 0x06F0) as u8), // Persian digits
        _ => c,
    })
}

/// Arabic definite article and its common attached forms. Longest first.
const ARABIC_ARTICLES: [&str; 6] = ["وال", "بال", "كال", "فال", "لل", "ال"];

/// The word without a leading Arabic definite article (`المدرسه` → `مدرسه`), if it has one and
/// at least three letters remain, so short words such as `الله` or `الام` are left alone.
/// Takes a normalised token.
///
/// The index stores both forms at the same position, and queries use the stripped form for
/// whole words. So `مدرسة` finds `المدرسة`, `المدرسة` finds `مدرسة`, and a partly typed
/// `المد` still prefix-matches the full form.
pub fn strip_article(word: &str) -> Option<&str> {
    if !word.starts_with(['و', 'ب', 'ك', 'ف', 'ل', 'ا']) {
        return None;
    }
    for prefix in ARABIC_ARTICLES {
        if let Some(rest) = word.strip_prefix(prefix) {
            return (rest.chars().count() >= 3).then_some(rest);
        }
    }
    None
}

/// Normalised token as used in queries: without the Arabic article when it has one.
pub fn query_form(token: &str) -> String {
    let n = normalize(token);
    match strip_article(&n) {
        Some(rest) => rest.to_string(),
        None => n.into_owned(),
    }
}

/// Normalised tokens of a string (used to analyse query terms).
pub fn analyze(text: &str) -> Vec<String> {
    words(text).map(|(s, e)| query_form(&text[s..e])).collect()
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
    /// Second form of the current word (without the Arabic article), emitted at the same
    /// position.
    pending: Option<String>,
}

impl Tokenizer for FfTokenizer {
    type TokenStream<'a> = FfTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> FfTokenStream<'a> {
        self.token.reset();
        FfTokenStream { text, words: words(text), token: &mut self.token, pending: None }
    }
}

impl TokenStream for FfTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if let Some(alt) = self.pending.take() {
            // Same position and offsets as the full form, so phrases match either.
            self.token.text = alt;
            return true;
        }
        match self.words.next() {
            Some((s, e)) => {
                self.token.text.clear();
                self.token.text.push_str(&normalize(&self.text[s..e]));
                self.pending = strip_article(&self.token.text).map(str::to_string);
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

    fn norm_all(s: &str) -> Vec<String> {
        analyze(s)
    }

    #[test]
    fn arabic_diacritics_stay_inside_words() {
        // Harakat are combining marks: they must not split a word into letters.
        assert_eq!(toks("كَتَبَ الطَّالِبُ"), ["كَتَبَ", "الطَّالِبُ"]);
        assert_eq!(norm_all("كَتَبَ"), ["كتب"]);
        // Decomposed Latin (e.g. macOS file names) behaves the same way.
        assert_eq!(norm_all("Cafe\u{301} bar"), ["cafe", "bar"]);
        // A stray mark after a separator is not a word.
        assert!(norm_all("\u{64E} ").is_empty());
    }

    #[test]
    fn arabic_letter_variants_fold() {
        // Alef with hamza or madda, alef wasla → alef.
        for w in ["أحمد", "إحمد", "آحمد", "ٱحمد"] {
            assert_eq!(normalize(w), "احمد", "{w}");
        }
        assert_eq!(normalize("مدرسة"), "مدرسه"); // teh marbuta → heh
        assert_eq!(normalize("مستشفى"), "مستشفي"); // alef maksura → yeh
        assert_eq!(normalize("كتـــاب"), "كتاب"); // tatweel removed
        assert_eq!(normalize("ی"), "ي"); // Persian yeh
        assert_eq!(normalize("ک"), "ك"); // keheh
        assert_eq!(normalize("٢٠٢٦"), "2026"); // Arabic-Indic digits
        assert_eq!(normalize("۲۰۲۶"), "2026"); // Persian digits
    }

    #[test]
    fn arabic_definite_article_is_removed_in_queries() {
        assert_eq!(normalize("المدرسة"), "المدرسه", "normalize itself keeps the article");
        for w in ["المدرسة", "والمدرسة", "بالمدرسة", "للمدرسة", "مدرسة"] {
            assert_eq!(query_form(w), "مدرسه", "{w}");
        }
        // Too short to be article + word: kept (الله must not become له).
        assert_eq!(query_form("الله"), "الله");
        assert_eq!(query_form("الأم"), "الام");
        // A word that merely starts with و / ب / ل is untouched.
        assert_eq!(query_form("ولد"), "ولد");
        assert_eq!(query_form("بيت"), "بيت");
        // A partly typed word keeps the article, to prefix-match the full form.
        assert_eq!(query_form("المد"), "المد");
    }

    #[test]
    fn index_stores_both_forms_at_one_position() {
        let mut t = FfTokenizer::default();
        let mut s = t.token_stream("في المدرسة الكبيرة");
        let mut out = Vec::new();
        while s.advance() {
            out.push((s.token().text.clone(), s.token().position));
        }
        let expect: Vec<(String, usize)> = [("في", 0), ("المدرسه", 1), ("مدرسه", 1), ("الكبيره", 2), ("كبيره", 2)]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b))
            .collect();
        assert_eq!(out, expect);
    }

    #[test]
    fn turkish_letters_fold() {
        assert_eq!(normalize("İstanbul"), "istanbul");
        assert_eq!(normalize("ISPARTA"), "isparta");
        assert_eq!(normalize("ılık"), "ilik");
        assert_eq!(normalize("Geçiş"), "gecis");
        assert_eq!(normalize("ŞÜĞÖ"), "sugo");
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
