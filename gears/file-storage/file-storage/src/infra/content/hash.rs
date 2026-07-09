//! Content hashing. P1 is locked to SHA-256 (ADR-0002); the hash backs version
//! identity checks, the `expected_hash` upload constraint, and the opaque `ETag`.
//!
//! This is the **single** SHA-256 call site in the gear: it is on the DE0708
//! FIPS-hasher allow-list (see `SECURITY.md §9`), so all `sha2` usage is
//! confined here and reviewable in one place. Content addressing/integrity is
//! the non-signature use the allow-list covers; the signed-URL signing
//! primitive lives behind its own provider abstraction (ADR-0004).

use sha2::{Digest, Sha256};

/// The P1 hash algorithm label stored on every version row.
pub const ALGORITHM: &str = "SHA-256";

/// Compute the SHA-256 digest of `bytes` (32 raw bytes).
#[must_use]
pub fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

/// Compute the SHA-256 digest as a lowercase hex string.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha256(bytes))
}

/// Compute the SHA-256 digest over a sequence of byte slices, hashed in order.
/// Used to derive the opaque content `ETag` from a domain tag plus identifiers
/// without allocating a concatenated buffer.
#[must_use]
pub fn sha256_parts(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

/// A streaming SHA-256 accumulator for chunked uploads.
#[derive(Default)]
pub struct Hasher {
    inner: Sha256,
    len: u64,
}

impl Hasher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes.
    pub fn update(&mut self, chunk: &[u8]) {
        self.inner.update(chunk);
        self.len += chunk.len() as u64;
    }

    /// Total number of bytes fed so far.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether no bytes have been fed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Finalize into the raw 32-byte digest.
    #[must_use]
    pub fn finalize(self) -> Vec<u8> {
        self.inner.finalize().to_vec()
    }
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod hash_tests;
