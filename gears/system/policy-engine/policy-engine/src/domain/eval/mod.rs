//! Pure decision logic: which documents apply and how results combine.

pub mod combiner;
pub mod matcher;

pub use combiner::{CombinedResult, EvaluatedDocument, EvaluatedDocumentKey, combine};
pub use matcher::document_applies;
