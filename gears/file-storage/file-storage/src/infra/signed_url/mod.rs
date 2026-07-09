//! Signed content URLs (`cpt-cf-file-storage-fr-signed-urls`,
//! `cpt-cf-file-storage-component-signed-url-issuer`).
//!
//! The control plane is the sole minter; it holds an Ed25519 private key and
//! signs short-lived, opaque tokens that authorize exactly one content
//! operation against the sidecar. The sidecar holds only the public key and
//! verifies statelessly (no DB lookup).
//!
//! ADR-0004 specifies PASETO `v4.public`; the token here is an equivalent
//! Ed25519-signed compact token (`base64url(payload).base64url(signature)`).
//! Per the FR the token is **opaque** and "the claim-set and crypto may change",
//! so the concrete codec is an internal detail of control + sidecar.
//!
//! Per ADR-0004's FIPS posture the sign/verify primitive sits behind the
//! [`SignatureProvider`] / [`SignatureVerifier`] abstraction (see [`provider`]);
//! this codec calls that abstraction and never a crypto crate directly, so the
//! algorithm and its backing module are replaceable without codec changes.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;

mod provider;
pub use provider::{Ed25519Provider, SignatureProvider, SignatureVerifier};

/// The content operation a token authorizes (bound into the token and checked
/// against the HTTP method by the sidecar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Download (`GET`).
    Get,
    /// Single-part upload (`PUT`).
    Put,
    /// One part of a server-authoritative multipart upload (`PUT` to the sidecar).
    ///
    /// Carries additional multipart-specific claims (`upload_id`, `part_number`,
    /// `offset`, exact `size`) that the sidecar enforces before writing any bytes.
    /// The control plane is the sole minter; the sidecar only verifies (ADR-0004).
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload (FEATURE §4)
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

/// Multipart-part-specific claims carried in `op = multipart_part` tokens.
///
/// The sidecar reads these to enforce the plan (part boundaries, exact size)
/// before writing a single byte — this is the mechanism that closes the
/// per-part abuse vector (FEATURE §4, DESIGN §4.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultipartClaims {
    /// The multipart session that owns this part.
    pub upload_id: Uuid,
    /// 1-based part number (S3 convention; 0 is invalid).
    pub part_number: u32,
    /// Byte offset of this part within the final assembled object.
    pub offset: u64,
    /// **Exact** byte length the sidecar will accept for this part.
    /// The sidecar rejects with `413` if `body.len() ≠ size` (FEATURE §4, point 2).
    pub size: u64,
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
}

fn is_default_constraints(c: &UploadConstraints) -> bool {
    *c == UploadConstraints::default()
}

fn is_default_multipart(c: &MultipartClaims) -> bool {
    *c == MultipartClaims::default()
}

/// The control-plane signing key (sole minter). Delegates the signing primitive
/// to a [`SignatureProvider`]; the public half is shared with the sidecar
/// verifier.
pub struct Issuer {
    provider: Arc<dyn SignatureProvider>,
    /// Maximum lifetime (seconds) any issued token may carry (`max_url_ttl`).
    max_ttl_secs: i64,
}

impl Issuer {
    /// Generate a new static signing key with the default P1 provider
    /// ([`Ed25519Provider`]). P1 uses a single keypair with no rotation (a `kid`
    /// is reserved for P2).
    pub fn generate(max_ttl_secs: i64) -> Result<Self, DomainError> {
        Ok(Self::with_provider(
            Arc::new(Ed25519Provider::generate()?),
            max_ttl_secs,
        ))
    }

    /// Build an issuer from a configured 32-byte Ed25519 seed, so the signing
    /// keypair is stable across restarts (the sidecar's configured public key
    /// keeps verifying issued URLs after a control-plane reboot).
    pub fn from_seed(seed: &[u8], max_ttl_secs: i64) -> Result<Self, DomainError> {
        Ok(Self::with_provider(
            Arc::new(Ed25519Provider::from_seed(seed)?),
            max_ttl_secs,
        ))
    }

    /// Build an issuer over an explicit signature provider. The codec is
    /// algorithm-agnostic, so a FIPS-validated provider can be substituted here
    /// without any other change (ADR-0004).
    #[must_use]
    pub fn with_provider(provider: Arc<dyn SignatureProvider>, max_ttl_secs: i64) -> Self {
        Self {
            provider,
            max_ttl_secs,
        }
    }

    /// The public key (raw bytes) the sidecar must be configured with to verify
    /// URLs this issuer mints.
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
    /// Construct from raw Ed25519 public-key bytes (e.g. shared config). Uses the
    /// default P1 provider's verifier; FIPS deployments construct the matching
    /// provider's verifier instead. Validates the key length up front so a
    /// malformed `FS_SIDECAR_PUBLIC_KEY` fails at startup rather than as a
    /// request-time token error.
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

    /// Verify a token's signature and expiry, returning its claims. The caller
    /// still checks `op` against the HTTP method and enforces upload constraints.
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

        // Expiry is exclusive: a token stops being usable at `exp`, not one
        // second later.
        if now.unix_timestamp() >= claims.exp {
            return Err(DomainError::token_invalid("token expired"));
        }
        Ok(claims)
    }
}

#[cfg(test)]
#[path = "signed_url_tests.rs"]
mod signed_url_tests;
