//! File Storage Gear — control plane.
//!
//! Owns metadata, authorization, versioning, conditional-request semantics, and
//! the issuance of signed content URLs. It carries **no** file content — bytes
//! move over signed URLs against the sidecar (ADR-0003).
//!
//! This is the M0 scaffold: the gear registers, wires the DB capability with the
//! P1 migration, and exposes an (empty) REST surface. The domain/infra/api
//! layers are filled in milestones M1+.

// === PUBLIC API (from SDK) ===
pub use file_storage_sdk::{FileStorageClientV1, FileStorageError};

// === GEAR ENTRY POINT ===
pub mod gear;
pub use gear::FileStorageGear;

// Re-exported for schema-level migration tests (see `tests/migration_test.rs`).
pub use infra::storage::migrations::Migrator;

// === INTERNAL MODULES ===
// Exposed only for in-crate tests; not a stable surface — consume the SDK.
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
