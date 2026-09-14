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
    /// The backend's own multipart handle (e.g. an S3 `UploadId`), as
    /// returned by `StorageBackend::initiate_multipart` at plan-mint time.
    ///
    /// Empty for backends that don't support native multipart at all (never
    /// reached in practice: `initiate_multipart_upload` rejects such a
    /// backend before minting any per-part token) — the sidecar uses an
    /// empty value as the signal to fall back to the local-fs-style
    /// offset-object model instead of calling `StorageBackend::upload_part`.
    /// `#[serde(default)]` keeps verification tolerant of a token minted
    /// before this field existed.
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
    /// Opaque correlation id minted at issuance time (P2 1.8 remediation).
    ///
    /// Carried end-to-end through the signed token so the sidecar can echo it
    /// back as the `x-request-id` header on its finalize/report-part callback
    /// to the control plane, letting both planes' logs be correlated by the
    /// same id even though the callback arrives on a disconnected HTTP
    /// request from the one that issued the token. `#[serde(default)]` keeps
    /// verification tolerant of a token minted before this field existed.
    #[serde(default)]
    pub request_id: String,
    /// Stored MIME of the version (`op = get` tokens only; P2 1.11).
    ///
    /// The sidecar has no DB access, so this is the only way it can emit a
    /// real `Content-Type` on a download response instead of a generic
    /// `application/octet-stream` fallback. `#[serde(default)]` keeps
    /// verification tolerant of tokens minted before this field existed
    /// (old sidecars ignore the new field; new sidecars tolerate old tokens
    /// by falling back).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    /// Opaque content `ETag` of the (file, version) pair (`op = get` tokens
    /// only; P2 1.11), the same value returned in `DownloadTicket::etag` —
    /// one source of truth (`domain::etag::content_etag`).
    ///
    /// Lets the sidecar emit a real `ETag` header without a DB lookup.
    /// `#[serde(default)]` keeps verification tolerant of tokens minted
    /// before this field existed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub etag: String,
    /// Upload-flow redesign: instructs the control plane's **finalize**
    /// callback handler to also bind the finalized version as the file's
    /// current content, under a `content_id IS NULL` CAS (`op = put` tokens
    /// only). Minted **exclusively** by `POST /files` for a brand-new file
    /// with `bind: "auto"` — never for `POST /files/{id}/versions` (replacing
    /// existing content must go through the JWT-authorized `bind`/`complete`
    /// paths; see DESIGN §3.6's delegated-authorization amendment). The
    /// sidecar itself never reads this claim. `#[serde(default)]` keeps
    /// verification tolerant of tokens minted before this field existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bind_on_finalize: bool,
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
        Verifier::with_verifier(self.provider.verifier())
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

/// The sidecar's verifier: a small **ordered** set of public keys, stateless
/// verification.
///
/// This exists so a `signing_key_seed` rotation on the control plane never
/// needs an outage or invalidates already-issued signed URLs: the sidecar is
/// rolled out first with the new key added to its set (see
/// `docs/operations.md`'s `signing_key_seed` → **Rotation** section), so
/// tokens signed by either the old or the new key verify throughout the
/// rollout window. There is deliberately no `kid` claim — the set is small
/// (primary + at most a couple of retained previous keys), so trying each
/// verifier in turn is cheap, and it avoids a claim that would otherwise let
/// a token name which key to check.
#[derive(Clone)]
pub struct Verifier {
    /// Verifiers to try, in order. `verifiers[0]` is the current primary key
    /// (the one the control plane signs new tokens with); anything after it
    /// exists only to keep accepting tokens minted by an older key during a
    /// rotation window. Never empty — [`Self::from_public_keys`] returns an
    /// error and [`Self::with_verifiers`] panics on an empty set, since a `Verifier`
    /// that can accept no key at all is a misconfiguration, not a valid
    /// "accept nothing" state.
    verifiers: Vec<Arc<dyn SignatureVerifier>>,
}

impl Verifier {
    /// Construct from a single raw Ed25519 public-key (e.g. shared config).
    /// Thin single-key wrapper over [`Self::from_public_keys`], kept so
    /// existing single-key call sites don't need to change.
    pub fn from_public_key(public_key: Vec<u8>) -> Result<Self, DomainError> {
        Self::from_public_keys(vec![public_key])
    }

    /// Construct from an ordered list of raw Ed25519 public-key bytes (e.g.
    /// `FS_SIDECAR_PUBLIC_KEY` followed by `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`).
    /// Uses the default P1 provider's verifier for each key; FIPS deployments
    /// construct the matching provider's verifiers instead via
    /// [`Self::with_verifiers`]. Validates every key's length up front so a
    /// malformed key fails at startup rather than as a request-time token
    /// error, and rejects an empty list for the same reason (see
    /// [`Self::verifiers`](Self)'s doc comment).
    pub fn from_public_keys(public_keys: Vec<Vec<u8>>) -> Result<Self, DomainError> {
        const ED25519_PUBLIC_KEY_LEN: usize = 32;
        if public_keys.is_empty() {
            return Err(DomainError::token_invalid(
                "at least one public key is required to construct a Verifier",
            ));
        }
        let verifiers = public_keys
            .into_iter()
            .map(|key| {
                if key.len() != ED25519_PUBLIC_KEY_LEN {
                    return Err(DomainError::token_invalid(format!(
                        "invalid Ed25519 public key length: expected {ED25519_PUBLIC_KEY_LEN} bytes, got {}",
                        key.len()
                    )));
                }
                Ok(Arc::new(provider::Ed25519Verifier::new(key)) as Arc<dyn SignatureVerifier>)
            })
            .collect::<Result<Vec<_>, DomainError>>()?;
        Ok(Self { verifiers })
    }

    /// Construct over a single explicit verifier (matches a non-default
    /// provider). Thin single-verifier wrapper over [`Self::with_verifiers`].
    #[must_use]
    pub fn with_verifier(verifier: Arc<dyn SignatureVerifier>) -> Self {
        Self::with_verifiers(vec![verifier])
    }

    /// Construct over an ordered list of explicit verifiers — primary first,
    /// then any retained for a rotation window. See [`Self::from_public_keys`]
    /// for the try-in-order verification contract [`Self::verify`] applies.
    ///
    /// # Panics
    ///
    /// Panics if `verifiers` is empty — a `Verifier` that can accept no key
    /// at all is a programming error, not a valid "accept nothing" state.
    #[must_use]
    pub fn with_verifiers(verifiers: Vec<Arc<dyn SignatureVerifier>>) -> Self {
        assert!(
            !verifiers.is_empty(),
            "Verifier::with_verifiers requires at least one SignatureVerifier"
        );
        Self { verifiers }
    }

    /// Verify a token's signature and expiry, returning its claims. The caller
    /// still checks `op` against the HTTP method and enforces upload constraints.
    ///
    /// Token parsing (splitting and base64-decoding the payload/signature) is
    /// done exactly once, regardless of how many keys are configured. The
    /// signature is then checked against each configured verifier in order
    /// (primary first, see [`Self::verifiers`](Self)'s doc comment),
    /// returning as soon as one accepts. When none accept, the error is
    /// exactly the same "signature verification failed" a single-key
    /// `Verifier` would return — a caller can never tell from the response
    /// how many keys were tried or which (if any) came close.
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

        let signature_ok = self
            .verifiers
            .iter()
            .any(|verifier| verifier.verify(&payload, &sig).is_ok());
        if !signature_ok {
            return Err(DomainError::token_invalid("signature verification failed"));
        }

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
