//! Infrastructure layer (adapters): persistence, storage backends.
//!
//! - `storage` — SeaORM entities + tenant-scoped repositories + migrations
//! - `content` — hashing, magic-byte mime validation, Range parsing
//! - `backend` — pluggable storage-backend abstraction (local-fs, in-memory)
//! - `signed_url` — Ed25519-signed opaque content tokens (issuer + verifier)

pub mod authz;
pub mod backend;
pub mod content;
pub mod quota;
pub mod signed_url;
pub mod storage;
pub mod usage;
