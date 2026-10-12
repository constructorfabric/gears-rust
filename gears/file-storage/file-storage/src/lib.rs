//! File Storage Gear — control plane.
//!
//! Owns metadata, authorization, versioning, conditional-request semantics, and
//! the issuance of signed content URLs. It carries **no** file content — bytes
//! move over signed URLs against the sidecar (ADR-0003).

pub use file_storage_sdk::{FileStorageClientV1, FileStorageError};

pub mod gear;
pub use gear::FileStorageGear;

// Re-exported for schema-level migration tests (see `tests/migration_test.rs`).
pub use infra::storage::migrations::Migrator;

// Not a stable surface (exposed for in-crate tests) — consume the SDK.
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
