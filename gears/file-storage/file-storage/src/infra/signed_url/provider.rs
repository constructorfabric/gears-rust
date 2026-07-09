//! Pluggable signature provider (ADR-0004 "FIPS posture").
//!
//! The token codec ([`super::Issuer`] / [`super::Verifier`]) MUST call this
//! abstraction and **never** a crypto crate directly, so the signing primitive
//! — and, in a FIPS deployment, the validated module backing it — is
//! replaceable without touching the codec, claim-set, or the rest of the
//! design. This is the binding constraint from ADR-0004: no dependency may
//! hard-wire a non-FIPS algorithm at the crate boundary.
//!
//! P1 ships [`Ed25519Provider`] (Ed25519 via `ring`). Ed25519 is FIPS 186-5
//! approved; a FIPS deployment swaps this impl for one backed by a validated
//! module (e.g. `rustls-corecrypto-provider`, or an ECDSA P-256 alternative) by
//! implementing the same traits — the token stays opaque and the codec unchanged.

use std::sync::Arc;

use ring::rand::SystemRandom;
use ring::signature::{self, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

use crate::domain::error::DomainError;

/// Minting side (control plane): signs payloads and yields a matching verifier.
pub trait SignatureProvider: Send + Sync {
    /// Sign `message`, returning the raw signature bytes.
    fn sign(&self, message: &[u8]) -> Vec<u8>;
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

/// Default P1 provider: Ed25519 via `ring`. Behind the trait so the algorithm
/// and backing module are replaceable (ADR-0004).
pub struct Ed25519Provider {
    key_pair: Ed25519KeyPair,
    public_key: Vec<u8>,
}

impl Ed25519Provider {
    /// Generate a fresh static keypair (P1 uses one keypair, no rotation).
    pub fn generate() -> Result<Self, DomainError> {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| DomainError::token_invalid("failed to generate signing key"))?;
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|_| DomainError::token_invalid("failed to load signing key"))?;
        let public_key = key_pair.public_key().as_ref().to_vec();
        Ok(Self {
            key_pair,
            public_key,
        })
    }

    /// Construct from a configured 32-byte Ed25519 seed, so the keypair (and
    /// therefore the public key the sidecar verifies against) is **stable across
    /// restarts** rather than regenerated on every boot.
    pub fn from_seed(seed: &[u8]) -> Result<Self, DomainError> {
        let key_pair = Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| {
            DomainError::token_invalid("invalid Ed25519 signing seed (expected 32 bytes)")
        })?;
        let public_key = key_pair.public_key().as_ref().to_vec();
        Ok(Self {
            key_pair,
            public_key,
        })
    }
}

impl SignatureProvider for Ed25519Provider {
    fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.key_pair.sign(message).as_ref().to_vec()
    }

    fn public_key(&self) -> Vec<u8> {
        self.public_key.clone()
    }

    fn verifier(&self) -> Arc<dyn SignatureVerifier> {
        Arc::new(Ed25519Verifier::new(self.public_key.clone()))
    }
}

/// Verifier for [`Ed25519Provider`]-minted tokens (public key only).
pub struct Ed25519Verifier {
    public_key: Vec<u8>,
}

impl Ed25519Verifier {
    /// Construct from raw Ed25519 public-key bytes (e.g. sidecar config).
    #[must_use]
    pub fn new(public_key: Vec<u8>) -> Self {
        Self { public_key }
    }
}

impl SignatureVerifier for Ed25519Verifier {
    fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), DomainError> {
        UnparsedPublicKey::new(&signature::ED25519, &self.public_key)
            .verify(message, signature)
            .map_err(|_| DomainError::token_invalid("signature verification failed"))
    }
}
