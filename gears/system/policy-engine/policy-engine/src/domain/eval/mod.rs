//! Pure decision logic: which documents apply to a request. How denials
//! combine is `admission_control_sdk::EngineResultBuilder`'s.

pub mod matcher;

pub use matcher::document_applies;
