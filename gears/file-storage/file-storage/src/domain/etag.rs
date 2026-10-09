//! Pure ETag formula, defined once for every call site.
//!
//! The content ETag is an opaque token derived from `(file_id, content_id)` via SHA-256
//! with a domain-separation prefix (a fingerprint, not a MAC). It never encodes the raw
//! content hash.

// Domain terms (ETag, If-Match) appear in comments below.
#![allow(clippy::doc_markdown)]

use uuid::Uuid;

use file_storage_sdk::File;

use crate::infra::content::hash;

/// Derive the opaque content ETag from a `(file_id, content_id)` pair.
///
/// Quoted (`"<hex>"`) per RFC 9110 §8.8.3; the 16-byte digest prefix is enough for an
/// optimistic-concurrency token.
#[must_use]
pub fn content_etag(file_id: Uuid, content_id: Uuid) -> String {
    let digest = hash::sha256_parts(&[b"fs-etag-v1", file_id.as_bytes(), content_id.as_bytes()]);
    format!("\"{}\"", hex::encode(&digest[..16]))
}

/// Current content ETag for `file`, or `None` if no content is bound yet.
#[must_use]
pub fn etag_for(file: &File) -> Option<String> {
    file.content_id.map(|cid| content_etag(file.file_id, cid))
}
