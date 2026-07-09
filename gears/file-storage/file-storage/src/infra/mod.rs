//! Infrastructure layer (adapters): persistence, storage backends.
//!
//! - `storage` — SeaORM entities + tenant-scoped repositories + migrations
//! - `content` — hashing, magic-byte mime validation, Range parsing
//! - `backend` — pluggable storage-backend abstraction (local-fs, in-memory)
//! - `signed_url` — Ed25519-signed opaque content tokens (issuer + verifier)
//! - `external_clients` — optional external-service adapters (quota, usage reporting)

pub mod authz;
pub mod backend;
pub mod content;
pub mod external_clients;
pub mod signed_url;
pub mod storage;
