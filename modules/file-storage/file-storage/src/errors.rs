//! Module-level error helpers for the FileStorage module.
//!
//! The runtime `FileStorageError` lives in the SDK crate. This module
//! collects the helpers used by `module.rs` for boot/init failures.

use thiserror::Error;

/// Errors raised during module initialization (config validation, backend
/// roster construction, smoke-tests). Surfaced as `anyhow::Error` to
/// `Module::init`.
#[derive(Debug, Error)]
pub enum InitError {
    #[error("invalid file-storage config: {0}")]
    InvalidConfig(String),

    #[error("backend roster construction failed: {0}")]
    BackendRoster(String),

    #[error("conditional-PUT smoke-test failed for backend {backend} at step {step}: {reason}")]
    SmokeTestFailed {
        backend: String,
        step: &'static str,
        reason: String,
    },
}
