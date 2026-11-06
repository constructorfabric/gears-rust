//! Pluggable signature provider (ADR-0004 "FIPS posture").
//!
//! The token codec MUST call this abstraction and never a crypto crate directly, so the
//! signing primitive (and, in a FIPS deployment, the validated module behind it) is
//! replaceable without touching the codec.
//!
//! `EcdsaP256Provider` uses P-256/SHA-256 through toolkit-security. Private
//! keys are PKCS#8 DER, public keys are uncompressed SEC1 points, and
//! signatures are ASN.1 DER. FIPS builds use the configured native provider.

use std::sync::Arc;

use toolkit_security::crypto::CryptoError;
use toolkit_security::crypto::signature::{EcdsaP256KeyPair, EcdsaP256VerifyingKey};

use crate::domain::error::DomainError;

/// Minting side (control plane): signs payloads and yields a matching verifier.
pub trait SignatureProvider: Send + Sync {
    /// Sign `message`, returning the raw signature bytes.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, DomainError>;
    /// The public-key bytes a peer verifier needs (shared with the sidecar).
    fn public_key(&self) -> Vec<u8>;
    /// A verifier for the tokens this provider mints.
    fn verifier(&self) -> Arc<dyn SignatureVerifier>;
}

/// Verifying side (sidecar): checks a signature against the public key. Holds no
/// secret and can never mint a token.
pub trait SignatureVerifier: Send + Sync {
    /// Verify `signature` over `message`; `Err` on any mismatch.
    fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), DomainError>;
}

/// Default provider: ECDSA P-256/SHA-256 through the toolkit-security crypto facade.
pub struct EcdsaP256Provider {
    key_pair: EcdsaP256KeyPair,
    public_key: Vec<u8>,
}

impl EcdsaP256Provider {
    /// Generate a fresh static keypair (no rotation).
    pub fn generate() -> Result<Self, DomainError> {
        let key_pair = EcdsaP256KeyPair::generate().map_err(|error| {
            tracing::error!(%error, "signed-URL key generation failed");
            DomainError::InternalError
        })?;
        let public_key = key_pair.public_key().to_vec();
        Ok(Self {
            key_pair,
            public_key,
        })
    }

    /// Construct from a PKCS#8 DER P-256 private key, so the keypair is stable across restarts.
    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self, DomainError> {
        let key_pair = EcdsaP256KeyPair::from_pkcs8(bytes).map_err(map_crypto_error)?;
        let public_key = key_pair.public_key().to_vec();
        Ok(Self {
            key_pair,
            public_key,
        })
    }
}

impl SignatureProvider for EcdsaP256Provider {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, DomainError> {
        self.key_pair.sign(message).map_err(|error| {
            tracing::error!(%error, "signed-URL signing failed");
            DomainError::InternalError
        })
    }

    fn public_key(&self) -> Vec<u8> {
        self.public_key.clone()
    }

    fn verifier(&self) -> Arc<dyn SignatureVerifier> {
        Arc::new(EcdsaP256Verifier {
            key: self.key_pair.verifying_key(),
        })
    }
}

/// Verifier for `EcdsaP256Provider`-minted tokens (public key only).
pub struct EcdsaP256Verifier {
    key: EcdsaP256VerifyingKey,
}

impl EcdsaP256Verifier {
    /// Construct from an uncompressed SEC1 P-256 public key (e.g. sidecar config).
    pub fn from_sec1_bytes(public_key: &[u8]) -> Result<Self, DomainError> {
        EcdsaP256VerifyingKey::from_sec1_bytes(public_key)
            .map(|key| Self { key })
            .map_err(map_crypto_error)
    }
}

impl SignatureVerifier for EcdsaP256Verifier {
    fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), DomainError> {
        self.key
            .verify(message, signature)
            .map_err(map_crypto_error)
    }
}

fn map_crypto_error(error: CryptoError) -> DomainError {
    match error {
        CryptoError::InvalidKey | CryptoError::InvalidSignature | CryptoError::MessageTooLong => {
            DomainError::token_invalid(format!("invalid signed-URL key or signature: {error}"))
        }
        _ => {
            tracing::error!(%error, "signed-URL crypto backend failed");
            DomainError::InternalError
        }
    }
}
