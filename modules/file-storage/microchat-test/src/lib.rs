//! Test-only microchat module — a real consumer of `cf-file-storage-sdk`
//! that exercises the full Rust stack (Service → SeaOrmRepo → S3Backend
//! → s3s-fs over loopback) under integration tests.
//!
//! This crate is **not** the production `mini-chat` module. It has
//! `publish = false`, no REST surface, and is never linked into a
//! binary — it is consumed exclusively from `cf-file-storage`'s
//! `tests/microchat_*` integration tests.
//!
//! See `modules/file-storage/docs/unit-microchat.md` for the full
//! design and test plan.

#![allow(dead_code)] // skeleton — bodies land in P2/P4

pub mod entity;
pub mod error;
pub mod migration;
pub mod repo;
pub mod service;
pub mod validators;

pub use error::MicrochatError;
pub use migration::{Migrator, MicrochatMigrationName};
pub use repo::{Attachment, AttachmentStatus, MicrochatRepo};
pub use service::{AttachHandle, Microchat, MicrochatLimits};
pub use validators::{MIME_ALLOWLIST, validate_filename, validate_mime};
