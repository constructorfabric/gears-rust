//! Cryptographic randomness from the configured implementation, not retry jitter.

use rustls::crypto::SecureRandom;

use super::{CryptoError, backend};

/// Fill the caller's buffer with cryptographically secure random bytes.
///
/// # Errors
/// Returns an error if the backend is unavailable or its RNG fails. On failure,
/// part of the buffer may have been written; callers must discard the result.
pub fn fill(bytes: &mut [u8]) -> Result<(), CryptoError> {
    fill_with(backend::provider()?.secure_random, bytes)
}

pub(super) fn fill_with(random: &dyn SecureRandom, bytes: &mut [u8]) -> Result<(), CryptoError> {
    for chunk in bytes.chunks_mut(u32::MAX as usize) {
        random.fill(chunk).map_err(|_| CryptoError::RandomFailed)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "rng_tests.rs"]
mod tests;
