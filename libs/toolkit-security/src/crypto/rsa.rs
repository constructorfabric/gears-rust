//! RSA verification through the same provider as TLS. PKCS#1 is used only
//! to serialize public integers; it does not perform cryptographic operations.

use pkcs1::der::Encode;
use rustls::SignatureScheme;
use rustls::pki_types::alg_id;

use super::{CryptoError, backend};

/// RSA signature algorithms supported by the platform's RSA JWKS provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RsaAlgorithm {
    /// RSASSA-PKCS1-v1_5 with SHA-256.
    Pkcs1Sha256,
    /// RSASSA-PKCS1-v1_5 with SHA-384.
    Pkcs1Sha384,
    /// RSASSA-PKCS1-v1_5 with SHA-512.
    Pkcs1Sha512,
    /// RSASSA-PSS with SHA-256 and a digest-length salt.
    PssSha256,
    /// RSASSA-PSS with SHA-384 and a digest-length salt.
    PssSha384,
    /// RSASSA-PSS with SHA-512 and a digest-length salt.
    PssSha512,
}

impl RsaAlgorithm {
    fn signature_id(self) -> alg_id::AlgorithmIdentifier {
        match self {
            Self::Pkcs1Sha256 => alg_id::RSA_PKCS1_SHA256,
            Self::Pkcs1Sha384 => alg_id::RSA_PKCS1_SHA384,
            Self::Pkcs1Sha512 => alg_id::RSA_PKCS1_SHA512,
            Self::PssSha256 => alg_id::RSA_PSS_SHA256,
            Self::PssSha384 => alg_id::RSA_PSS_SHA384,
            Self::PssSha512 => alg_id::RSA_PSS_SHA512,
        }
    }

    fn scheme(self) -> SignatureScheme {
        match self {
            Self::Pkcs1Sha256 => SignatureScheme::RSA_PKCS1_SHA256,
            Self::Pkcs1Sha384 => SignatureScheme::RSA_PKCS1_SHA384,
            Self::Pkcs1Sha512 => SignatureScheme::RSA_PKCS1_SHA512,
            Self::PssSha256 => SignatureScheme::RSA_PSS_SHA256,
            Self::PssSha384 => SignatureScheme::RSA_PSS_SHA384,
            Self::PssSha512 => SignatureScheme::RSA_PSS_SHA512,
        }
    }
}

/// Verify an RSA signature over the original message (without prehashing).
///
/// `n` and `e` are unsigned big-endian integers without leading zero bytes,
/// as in a JWK. Moduli must be odd and 2048–8192 bits. Exponents must be odd,
/// at least 3 and at most 33 bits. The selected algorithm is never substituted.
///
/// # Errors
/// Returns an error for invalid key/signature bytes, an oversized message,
/// unavailable backend or unsupported algorithm. No fallback is attempted.
pub fn verify_rsa_components(
    algorithm: RsaAlgorithm,
    n: &[u8],
    e: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    verify_with_provider(backend::provider()?, algorithm, n, e, message, signature)
}

// The public entry point validates the production provider before reaching here.
pub(super) fn verify_with_provider(
    provider: &rustls::crypto::CryptoProvider,
    algorithm: RsaAlgorithm,
    n: &[u8],
    e: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    validate_components(n, e)?;
    if message.len() > u32::MAX as usize {
        return Err(CryptoError::MessageTooLong);
    }
    if signature.len() != n.len() {
        return Err(CryptoError::InvalidSignature);
    }
    let public_key = pkcs1::RsaPublicKey {
        modulus: pkcs1::UintRef::new(n).map_err(|_| CryptoError::InvalidKey)?,
        public_exponent: pkcs1::UintRef::new(e).map_err(|_| CryptoError::InvalidKey)?,
    }
    .to_der()
    .map_err(|_| CryptoError::InvalidKey)?;
    let algorithms = provider
        .signature_verification_algorithms
        .mapping
        .iter()
        .find(|(scheme, _)| *scheme == algorithm.scheme())
        .map(|(_, algorithms)| *algorithms)
        .ok_or(CryptoError::UnsupportedAlgorithm)?;
    if algorithms.iter().any(|verifier| {
        verifier.public_key_alg_id() == alg_id::RSA_ENCRYPTION
            && verifier.signature_alg_id() == algorithm.signature_id()
            && verifier
                .verify_signature(&public_key, message, signature)
                .is_ok()
    }) {
        Ok(())
    } else {
        Err(CryptoError::InvalidSignature)
    }
}

fn validate_components(n: &[u8], e: &[u8]) -> Result<(), CryptoError> {
    // Check canonical encodings before UintRef normalizes leading zeros.
    if n.is_empty() || e.is_empty() || n[0] == 0 || e[0] == 0 {
        return Err(CryptoError::InvalidKey);
    }
    if n.len() > 1024 || e.len() > 5 {
        return Err(CryptoError::InvalidKey);
    }
    let modulus_bits = n.len() * 8 - n[0].leading_zeros() as usize;
    let exponent = e
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    if !(2048..=8192).contains(&modulus_bits)
        || n[n.len() - 1] & 1 == 0
        || exponent < 3
        || exponent & 1 == 0
        || exponent >= (1u64 << 33)
    {
        return Err(CryptoError::InvalidKey);
    }
    Ok(())
}

#[cfg(test)]
#[path = "rsa_tests.rs"]
mod tests;
