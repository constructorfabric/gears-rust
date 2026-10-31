//! Application cryptography through the platform's selected implementation.
//!
//! With `fips`, Linux uses AWS-LC FIPS, macOS uses corecrypto and Windows
//! uses CNG. A missing runtime witness is an error, never a fallback. These
//! witnesses identify implementation routing; deployment must still satisfy
//! the module certificate's operational environment and security policy.

mod backend;
mod ecdsa;
pub mod hash;
pub mod mac;
pub mod rng;
mod rsa;
pub mod signature;

/// Failure to obtain the configured cryptographic implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CryptoError {
    /// The configured provider does not report the required FIPS witness.
    #[error("configured crypto provider does not report the required FIPS witness")]
    FipsUnavailable,
    /// A required application primitive is absent from the provider.
    #[error("configured crypto provider does not provide SHA-256")]
    Sha256Unavailable,
    /// A FIPS HMAC generation key is shorter than 112 bits.
    #[error("FIPS HMAC keys must contain at least 14 bytes")]
    HmacKeyTooShort,
    /// The message exceeds the portable backend's u32 byte limit.
    #[error("message exceeds the portable crypto backend length limit")]
    MessageTooLong,
    /// Authentication tag verification failed.
    #[error("invalid authentication tag")]
    InvalidTag,
    /// The selected CSPRNG failed to fill the caller's buffer.
    #[error("cryptographic random number generation failed")]
    RandomFailed,
    /// The native FIPS provider does not provide the requested algorithm.
    #[error("algorithm is unavailable under the configured crypto policy")]
    UnsupportedAlgorithm,
    /// A signature key could not be generated or imported.
    #[error("invalid signature key")]
    InvalidKey,
    /// The selected backend could not generate a signing key.
    #[error("native signing key generation failed")]
    KeyGenerationFailed,
    /// The selected backend could not produce a signature.
    #[error("native signing operation failed")]
    SigningFailed,
    /// A signature did not verify.
    #[error("invalid signature")]
    InvalidSignature,
}

/// Validate the application crypto backend before accepting requests.
///
/// This does not install a global TLS provider. Standalone SDKs use the same
/// policy without depending on server bootstrap or another library's globals.
///
/// # Errors
/// Returns an error if SHA-256 is absent or the FIPS witness is unavailable.
pub fn initialize() -> Result<(), CryptoError> {
    backend::sha256_suite().map(|_| ())
}

/// Compare bytes without content-dependent early exit. Lengths are public.
#[must_use]
pub fn constant_time_eq(expected: &[u8], provided: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    expected.ct_eq(provided).into()
}

#[cfg(all(test, feature = "fips", feature = "jwt", target_os = "windows"))]
#[path = "windows_tests.rs"]
mod windows_tests;
