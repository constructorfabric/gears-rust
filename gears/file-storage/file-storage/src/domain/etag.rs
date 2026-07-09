//! Pure ETag formula for the file-storage control plane.
//!
//! The content ETag is a deterministic opaque token derived from `(file_id,
//! content_id)` via a SHA-256 over a domain-separation prefix (not a secret key —
//! it is a fingerprint, not a MAC). It is opaque by design — it never encodes the
//! raw content hash that backs the version row — and is defined once here so every
//! call site (service, DTO, handler) reads from the same source of truth.

// Domain terms (ETag, If-Match) appear in comments below.
#![allow(clippy::doc_markdown)]

use uuid::Uuid;

use file_storage_sdk::File;

use crate::infra::content::hash;

/// Derive the opaque content ETag from a `(file_id, content_id)` pair.
///
/// The ETag is quoted (`"<hex>"`) per RFC 9110 §8.8.3. The 16-byte prefix of the
/// SHA-256 digest gives 128 bits of collision resistance — sufficient for an
/// optimistic-concurrency token (DESIGN §3.1, §4.2).
#[must_use]
pub fn content_etag(file_id: Uuid, content_id: Uuid) -> String {
    let digest = hash::sha256_parts(&[b"fs-etag-v1", file_id.as_bytes(), content_id.as_bytes()]);
    format!("\"{}\"", hex::encode(&digest[..16]))
}

/// Return the current content ETag for `file`, or `None` if no content is bound
/// yet (`file.content_id` is `None`).
#[must_use]
pub fn etag_for(file: &File) -> Option<String> {
    file.content_id.map(|cid| content_etag(file.file_id, cid))
}
