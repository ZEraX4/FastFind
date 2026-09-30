//! Query syntax, planning, execution, matching and snippets.

pub mod engine;
pub mod matcher;
pub mod plan;
pub mod query;
pub mod snippet;

pub use engine::SearchService;
