//! SHA-256 with an implementation selected by the facade's backend policy.

use rustls::crypto::hash::Context;

use super::backend;

// CNG's length parameters are u32 even when Rust slices use 64-bit lengths.
const MAX_UPDATE_LEN: usize = u32::MAX as usize;

/// A streaming SHA-256 computation. Backend types stay inside the facade.
pub struct Sha256 {
    inner: Box<dyn Context>,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Start hashing.
    /// Call [`super::initialize`] at startup to handle that error explicitly.
    ///
    /// # Panics
    /// Panics if the configured implementation is unavailable.
    #[must_use]
    pub fn new() -> Self {
        Self::with_suite(
            backend::sha256_suite().unwrap_or_else(|error| {
                panic!("application crypto backend is unavailable: {error}")
            }),
        )
    }

    pub(super) fn with_suite(suite: &'static rustls::Tls13CipherSuite) -> Self {
        Self {
            inner: suite.common.hash_provider.start(),
        }
    }

    /// Add bytes, splitting large slices to respect the native CNG ABI.
    pub fn update(&mut self, bytes: impl AsRef<[u8]>) {
        for chunk in bytes.as_ref().chunks(MAX_UPDATE_LEN) {
            self.inner.update(chunk);
        }
    }

    /// Finish hashing and return the exact 32 digest bytes.
    ///
    /// # Panics
    /// Panics if the backend violates the SHA-256 digest-length invariant.
    #[must_use]
    pub fn finalize(self) -> [u8; 32] {
        self.inner
            .finish()
            .as_ref()
            .try_into()
            .unwrap_or_else(|_| panic!("SHA-256 provider returned an invalid digest length"))
    }

    /// Hash bytes without allocating a streaming context for ordinary inputs.
    ///
    /// # Panics
    /// Panics if the configured implementation is unavailable or violates
    /// the SHA-256 digest-length invariant.
    #[must_use]
    pub fn digest(bytes: impl AsRef<[u8]>) -> [u8; 32] {
        let bytes = bytes.as_ref();
        if bytes.len() > MAX_UPDATE_LEN {
            let mut hash = Self::new();
            hash.update(bytes);
            return hash.finalize();
        }
        backend::sha256_suite()
            .unwrap_or_else(|error| panic!("application crypto backend is unavailable: {error}"))
            .common
            .hash_provider
            .hash(bytes)
            .as_ref()
            .try_into()
            .unwrap_or_else(|_| panic!("SHA-256 provider returned an invalid digest length"))
    }
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod tests;
