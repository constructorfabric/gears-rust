use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::{SigningError, TokenIssuerError};
use crate::models::{
    MintCapabilityRequest, MintGrantRequest, PublicKeyVersion, SignatureResult, SigningKeyRef,
};

/// Signing port implemented by the backing plugin (e.g. `OpenBao` Transit).
///
/// Keys are platform-scoped; tenant context is carried implicitly via the
/// [`SecurityContext`].
#[async_trait]
pub trait SigningClientV1: Send + Sync {
    /// Signs `signing_input` with the named key and returns the signature bytes
    /// together with the key version used.
    async fn sign(
        &self,
        ctx: &SecurityContext,
        key: &SigningKeyRef,
        signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError>;

    /// Returns all current public key versions for the named signing key.
    async fn public_keys(
        &self,
        ctx: &SecurityContext,
        key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError>;
}

/// Consumer-facing API trait for minting capability and grant tokens.
#[async_trait]
pub trait TokenIssuerClientV1: Send + Sync {
    /// Mints a short-lived capability JWT and returns it as a compact serialized
    /// string.
    async fn mint_capability(
        &self,
        ctx: &SecurityContext,
        req: MintCapabilityRequest,
    ) -> Result<String, TokenIssuerError>;

    /// Mints a short-lived data-plane grant JWT (`grant+jwt`) and returns it as a
    /// compact serialized string, together with its absolute expiry (Unix
    /// seconds). The caller identity (`sub`, `subject_tenant`) comes from `ctx`;
    /// the resolved resource identity, operations, and clamped TTL come from
    /// `req`.
    async fn mint_grant(
        &self,
        ctx: &SecurityContext,
        req: MintGrantRequest,
    ) -> Result<GrantToken, TokenIssuerError>;
}

/// A minted grant token plus its absolute expiry (Unix seconds), so the caller
/// can populate the issuance response's `expires_at` without re-decoding the JWT.
#[derive(Debug, Clone)]
pub struct GrantToken {
    /// The compact-serialized `grant+jwt`.
    pub token: String,
    /// Absolute expiry (`exp`, Unix seconds).
    pub expires_at: i64,
}
