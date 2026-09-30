use std::io;

/// Engine-level error. Parser failures use [`crate::parsers::ParseError`] and never escape
/// the indexing pipeline; they are recorded per file in the catalog instead.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("index error: {0}")]
    Index(#[from] tantivy::TantivyError),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("invalid query: {0}")]
    Query(String),
    #[error("invalid settings: {0}")]
    Settings(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("another FastFind instance is already using this index")]
    AlreadyRunning,
    #[error("search cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Settings(e.to_string())
    }
}

impl From<tantivy::query::QueryParserError> for Error {
    fn from(e: tantivy::query::QueryParserError) -> Self {
        Error::Query(e.to_string())
    }
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
