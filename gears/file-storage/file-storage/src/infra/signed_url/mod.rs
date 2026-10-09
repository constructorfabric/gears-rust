//! Signed content URLs.
//!
//! The control plane is the sole minter; it holds an Ed25519 private key and signs short-lived,
//! opaque tokens that authorize exactly one content operation (`op`, file/version, backend
//! path, expiry) against the sidecar. The sidecar holds only the public key and verifies
//! statelessly (no DB lookup).
//!
//! The token is an Ed25519-signed compact token (`base64url(payload).base64url(signature)`),
//! an equivalent of the PASETO `v4.public` from ADR-0004; its format is an internal detail.
//! Signing and verification go through `SignatureProvider` / `SignatureVerifier` (see
//! `provider`), never a crypto crate directly, so the algorithm is replaceable (ADR-0004).

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;

mod provider;
pub use provider::{Ed25519Provider, SignatureProvider, SignatureVerifier};

/// The content operation a token authorizes (checked against the HTTP method by the sidecar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Download (`GET`).
    Get,
    /// Single-part upload (`PUT`).
    Put,
    /// One part of a multipart upload (`PUT`); carries `MultipartClaims`, which the sidecar
    /// enforces before writing any bytes.
    MultipartPart,
}

/// Upload-only content constraints the sidecar enforces while streaming.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadConstraints {
    /// Upper bound on uploaded size (mutually exclusive with `exact_size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size: Option<u64>,
    /// Exact required size (mutually exclusive with `max_size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_size: Option<u64>,
    /// Required content hash, `"<alg>:<hex>"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_hash: Option<String>,
}

/// Claims carried in `op = multipart_part` tokens; the sidecar enforces the plan (part
/// boundaries, exact size) from them before writing a byte.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultipartClaims {
    /// The multipart session that owns this part.
    pub upload_id: Uuid,
    /// 1-based part number (S3 convention; 0 is invalid).
    pub part_number: u32,
    /// Byte offset of this part within the final assembled object.
    pub offset: u64,
    /// Exact byte length the sidecar accepts for this part (`413` otherwise).
    pub size: u64,
    /// The backend's own multipart handle (e.g. an S3 `UploadId`) from
    /// `StorageBackend::initiate_multipart`. Empty means the sidecar uses the offset-object
    /// model instead of `StorageBackend::upload_part`.
    #[serde(default)]
    pub backend_handle: String,
}

/// The signed token's claim set (AND-combined; `exp` is mandatory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    pub op: Op,
    pub file_id: Uuid,
    /// The specific immutable blob: `content_id` for GET, the pending version
    /// for PUT / `multipart_part`.
    pub version_id: Uuid,
    pub backend_id: String,
    pub backend_path: String,
    /// Expiry, unix seconds.
    pub exp: i64,
    #[serde(default, skip_serializing_if = "is_default_constraints")]
    pub upload: UploadConstraints,
    /// Non-empty only when `op = multipart_part`.
    #[serde(default, skip_serializing_if = "is_default_multipart")]
    pub multipart: MultipartClaims,
    /// Correlation id minted at issuance; the sidecar echoes it as `x-request-id` on its
    /// finalize/report-part callback so both planes' logs can be correlated.
    #[serde(default)]
    pub request_id: String,
    /// Stored MIME of the version (`op = get` only). The sidecar has no DB access, so this
    /// is how it emits a real `Content-Type` instead of `application/octet-stream`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    /// Content `ETag` of the (file, version) pair (`op = get` only), the same value as
    /// `DownloadTicket::etag` (`domain::etag::content_etag`); lets the sidecar emit `ETag`
    /// without a DB lookup.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub etag: String,
}

fn is_default_constraints(c: &UploadConstraints) -> bool {
    *c == UploadConstraints::default()
}

fn is_default_multipart(c: &MultipartClaims) -> bool {
    *c == MultipartClaims::default()
}

/// The control-plane signing key (sole minter); the public half goes to the sidecar verifier.
pub struct Issuer {
    provider: Arc<dyn SignatureProvider>,
    /// Maximum lifetime (seconds) of any issued token (`max_url_ttl`).
    max_ttl_secs: i64,
}

impl Issuer {
    /// Generate a new static `Ed25519Provider` signing key (single keypair, no rotation).
    pub fn generate(max_ttl_secs: i64) -> Result<Self, DomainError> {
        Ok(Self::with_provider(
            Arc::new(Ed25519Provider::generate()?),
            max_ttl_secs,
        ))
    }

    /// Build an issuer from a 32-byte Ed25519 seed, so the keypair is stable across restarts.
    pub fn from_seed(seed: &[u8], max_ttl_secs: i64) -> Result<Self, DomainError> {
        Ok(Self::with_provider(
            Arc::new(Ed25519Provider::from_seed(seed)?),
            max_ttl_secs,
        ))
    }

    /// Build an issuer over an explicit provider (e.g. a FIPS-validated one, ADR-0004).
    #[must_use]
    pub fn with_provider(provider: Arc<dyn SignatureProvider>, max_ttl_secs: i64) -> Self {
        Self {
            provider,
            max_ttl_secs,
        }
    }

    /// The raw public key the sidecar needs to verify URLs this issuer mints.
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.provider.public_key()
    }

    /// The verifier the sidecar uses (public key only).
    #[must_use]
    pub fn verifier(&self) -> Verifier {
        Verifier {
            verifier: self.provider.verifier(),
        }
    }

    /// Mint a token for `claims`, clamping its lifetime to `max_ttl`.
    pub fn issue(&self, mut claims: Claims, now: OffsetDateTime) -> Result<String, DomainError> {
        let max_exp = now.unix_timestamp() + self.max_ttl_secs;
        if claims.exp > max_exp {
            claims.exp = max_exp;
        }
        let payload = serde_json::to_vec(&claims)
            .map_err(|e| DomainError::token_invalid(format!("serialize claims: {e}")))?;
        let sig = self.provider.sign(&payload);
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(&sig)
        ))
    }
}

/// The sidecar's verifier: public key only, stateless verification.
#[derive(Clone)]
pub struct Verifier {
    verifier: Arc<dyn SignatureVerifier>,
}

impl Verifier {
    /// Construct from raw Ed25519 public-key bytes. The key length is validated up front so a
    /// malformed `FS_SIDECAR_PUBLIC_KEY` fails at startup, not as a request-time error.
    pub fn from_public_key(public_key: Vec<u8>) -> Result<Self, DomainError> {
        const ED25519_PUBLIC_KEY_LEN: usize = 32;
        if public_key.len() != ED25519_PUBLIC_KEY_LEN {
            return Err(DomainError::token_invalid(format!(
                "invalid Ed25519 public key length: expected {ED25519_PUBLIC_KEY_LEN} bytes, got {}",
                public_key.len()
            )));
        }
        Ok(Self {
            verifier: Arc::new(provider::Ed25519Verifier::new(public_key)),
        })
    }

    /// Construct over an explicit verifier (matches a non-default provider).
    #[must_use]
    pub fn with_verifier(verifier: Arc<dyn SignatureVerifier>) -> Self {
        Self { verifier }
    }

    /// Verify a token's signature and expiry, returning its claims. The caller still checks
    /// `op` against the HTTP method and enforces upload constraints.
    pub fn verify(&self, token: &str, now: OffsetDateTime) -> Result<Claims, DomainError> {
        let (payload_b64, sig_b64) = token
            .split_once('.')
            .ok_or_else(|| DomainError::token_invalid("malformed token"))?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| DomainError::token_invalid("bad payload encoding"))?;
        let sig = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| DomainError::token_invalid("bad signature encoding"))?;

        self.verifier.verify(&payload, &sig)?;

        let claims: Claims = serde_json::from_slice(&payload)
            .map_err(|_| DomainError::token_invalid("bad claims"))?;

        // Expiry is exclusive: unusable from `exp` on.
        if now.unix_timestamp() >= claims.exp {
            return Err(DomainError::token_invalid("token expired"));
        }
        Ok(claims)
    }
}

#[cfg(test)]
#[path = "signed_url_tests.rs"]
mod signed_url_tests;
