//! Composite etag computation.
//!
//! Per ADR-0004 / DESIGN §3.7:
//!     etag = hex(sha256(content_hash || ":" || meta_revision))
//!
//! `content_hash` is the backend-side fingerprint (the S3 `ETag` header for
//! the `s3-compatible` adapter). `meta_revision` is the row's monotonic
//! counter, bumped on every successful UPDATE that mutates metadata or
//! content. Self-healing repair UPDATEs do **not** bump `meta_revision`.

use sha2::{Digest, Sha256};

use file_storage_sdk::Etag;

/// Compute the composite etag for `(content_hash, meta_revision)`.
#[must_use]
pub fn compose(content_hash: &str, meta_revision: i64) -> Etag {
    let mut hasher = Sha256::new();
    hasher.update(content_hash.as_bytes());
    hasher.update(b":");
    hasher.update(meta_revision.to_string().as_bytes());
    hex::encode(hasher.finalize())
}
