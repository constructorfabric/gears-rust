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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_is_deterministic_for_same_inputs() {
        let a = compose("abc123", 7);
        let b = compose("abc123", 7);
        assert_eq!(a, b);
    }

    #[test]
    fn compose_differs_for_different_content() {
        assert_ne!(compose("aaa", 1), compose("bbb", 1));
    }

    #[test]
    fn compose_differs_for_different_revision() {
        assert_ne!(compose("aaa", 1), compose("aaa", 2));
    }

    #[test]
    fn compose_returns_hex_sha256_64_chars() {
        let e = compose("anything", 0);
        assert_eq!(e.len(), 64, "sha256 hex is 64 chars: got {e}");
        assert!(e.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn compose_handles_empty_content_hash() {
        // Sentinel state for fresh `pending_upload` rows.
        let e = compose("", 0);
        assert_eq!(e.len(), 64);
    }
}
