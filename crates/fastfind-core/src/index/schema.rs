//! Tantivy schema.
//!
//! The document store holds only small metadata (path, name, sizes, flags, metadata text), so
//! fetching a page of 100 results decompresses a few KB. Extracted text for snippets lives in
//! the separate [`super::TextStore`].

use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::Index;

use crate::analysis::{FfTokenizer, TOKENIZER_NAME};

/// Bump when the schema or analysis changes; a mismatch triggers an automatic rebuild.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub struct Fields {
    /// Full path; upsert/delete key.
    pub path: Field,
    /// xxh3(path) fast field: deterministic ranking tie-break and text-store key.
    pub pid: Field,
    pub name: Field,
    /// Lowercased file name (raw term + fast) for substring/wildcard name search and sorting.
    pub name_lc: Field,
    /// `filter_form` of the parent directory with trailing '/', for directory prefix filters.
    pub dir: Field,
    /// Tokenised directory path for `path:` queries.
    pub path_t: Field,
    pub ext: Field,
    pub kind: Field,
    pub root: Field,
    pub size: Field,
    /// Unix seconds.
    pub mtime: Field,
    pub content: Field,
    /// Title/author/subject/keywords (indexed + stored).
    pub meta: Field,
    pub flags: Field,
    pub pages: Field,
    /// [`crate::parsers::TextMode`] used to recover text for snippets.
    pub mode: Field,
    /// Stored-only JSON with separate title/author for the preview panel.
    pub info: Field,
}

fn text_opts(stored: bool) -> TextOptions {
    let idx = TextFieldIndexing::default()
        .set_tokenizer(TOKENIZER_NAME)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let o = TextOptions::default().set_indexing_options(idx);
    if stored {
        o.set_stored()
    } else {
        o
    }
}

pub fn build() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let f = Fields {
        path: b.add_text_field("path", STRING | STORED),
        pid: b.add_u64_field("pid", FAST),
        name: b.add_text_field("name", text_opts(true)),
        name_lc: b.add_text_field("name_lc", STRING | FAST),
        dir: b.add_text_field("dir", STRING),
        path_t: b.add_text_field("path_t", text_opts(false)),
        ext: b.add_text_field("ext", STRING | STORED),
        kind: b.add_text_field("kind", STRING | STORED),
        root: b.add_u64_field("root", INDEXED | STORED),
        size: b.add_u64_field("size", INDEXED | STORED | FAST),
        mtime: b.add_i64_field("mtime", INDEXED | STORED | FAST),
        content: b.add_text_field("content", text_opts(false)),
        meta: b.add_text_field("meta", text_opts(true)),
        flags: b.add_u64_field("flags", STORED),
        pages: b.add_u64_field("pages", STORED),
        mode: b.add_u64_field("mode", STORED),
        info: b.add_text_field("info", STORED),
    };
    (b.build(), f)
}

pub fn register_tokenizers(index: &Index) {
    index.tokenizers().register(TOKENIZER_NAME, TextAnalyzer::from(FfTokenizer::default()));
}
