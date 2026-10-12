//! Content hashing (SHA-256, ADR-0002): version identity checks, the `expected_hash` upload
//! constraint and the opaque `ETag`.
//!
//! This is an integrity/identity hash, not a signature or key-derivation primitive, and is
//! excluded from the FIPS claim (ADR-0006); the `expected_hash` check also stays on SHA-256.
//! Content hashing has two modes (both SHA-256): whole-object, implemented here, and the
//! multipart offset-manifest composite (see `hash_mode`).
//!
//! This is the only SHA-256 call site in the gear (the DE0708 FIPS-hasher allow-list); the
//! signed-URL signing primitive lives behind its own provider abstraction (ADR-0004).

use sha2::{Digest, Sha256};

/// The hash algorithm label stored on every version row.
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

/// SHA-256 over a sequence of byte slices, hashed in order (no concatenated buffer);
/// used to derive the opaque content `ETag`.
#[must_use]
pub fn sha256_parts(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

/// Convert a SHA-256 digest `Vec` into a fixed-size array.
///
/// # Panics
/// Panics if `digest` is not exactly 32 bytes (an internal invariant; SHA-256 always yields 32).
#[must_use]
pub fn digest_to_array(digest: Vec<u8>) -> [u8; 32] {
    digest
        .try_into()
        .unwrap_or_else(|v: Vec<u8>| panic!("SHA-256 digest must be 32 bytes, got {}", v.len()))
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
