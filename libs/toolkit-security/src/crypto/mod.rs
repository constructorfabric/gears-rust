//! Application cryptography through the platform's selected implementation.
//!
//! With `fips`, Linux uses AWS-LC FIPS, macOS uses corecrypto and Windows
//! uses CNG. A missing runtime witness is an error, never a fallback. These
//! witnesses identify implementation routing; deployment must still satisfy
//! the module certificate's operational environment and security policy.

pub mod aead;
mod backend;
mod ecdsa;
pub mod hash;
pub mod mac;
pub mod rng;
mod rsa;
pub mod signature;
pub mod sigv4;

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
    /// A signature or AEAD key could not be generated or imported, or an AWS
    /// Signature Version 4 secret access key is empty.
    #[error("invalid key")]
    InvalidKey,
    /// An AWS Signature Version 4 credential scope has a date that is not
    /// `YYYYMMDD`, or a region or service that is empty, longer than 64 bytes
    /// or not ASCII letters, digits, `-`, `_` and `.`.
    #[error("invalid SigV4 credential scope")]
    InvalidCredentialScope,
    /// The selected backend could not generate a signing key.
    #[error("native signing key generation failed")]
    KeyGenerationFailed,
    /// The selected backend could not produce a signature.
    #[error("native signing operation failed")]
    SigningFailed,
    /// The selected backend could not seal a message.
    #[error("native sealing operation failed")]
    SealingFailed,
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

/// Decode an unencrypted PKCS#8 PEM document (`-----BEGIN PRIVATE KEY-----`)
/// into a buffer that is erased on every path and hand the DER to `import`.
///
/// Surrounding whitespace is trimmed; otherwise RFC 7468 strict (64-column
/// base64 lines). Any other label, SEC1/PKCS#1 and encrypted keys included,
/// is `InvalidKey`.
fn with_pkcs8_pem<T>(
    pem: &str,
    import: impl FnOnce(&[u8]) -> Result<T, CryptoError>,
) -> Result<T, CryptoError> {
    let pem = pem.trim().as_bytes();
    let mut buffer = zeroize::Zeroizing::new(vec![0; pem.len()]);
    let (label, der) =
        pem_rfc7468::decode(pem, &mut buffer).map_err(|_| CryptoError::InvalidKey)?;
    if label != "PRIVATE KEY" {
        return Err(CryptoError::InvalidKey);
    }
    import(der)
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
