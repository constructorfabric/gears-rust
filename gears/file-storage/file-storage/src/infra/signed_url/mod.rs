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
use time::{Duration, OffsetDateTime};
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

    /// A single-key verifier bound to this issuer's current public key.
    ///
    /// The sidecar never calls this -- it builds its own `Verifier` directly
    /// from `FS_SIDECAR_PUBLIC_KEY`/`FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` (see
    /// `bin/sidecar.rs`). This is instead the control plane's OWN default
    /// verifier for the s2s finalize/report-part callbacks
    /// (`FileService::verifier`, `domain::service::mod`), i.e. what the
    /// control plane itself still accepts, not what it hands anyone else.
    /// `FileService::with_previous_signing_public_keys` extends it with
    /// `FileStorageConfig::previous_signing_public_keys` so a
    /// `signing_key_seed` rotation doesn't reject an in-flight upload's
    /// callback the moment the control plane restarts on the new seed --
    /// see `docs/operations.md`'s `signing_key_seed` -> Rotation procedure.
    #[must_use]
    pub fn verifier(&self) -> Verifier {
        Verifier::with_verifier(self.provider.verifier())
    }

    /// Mint a token for `claims`, clamping its lifetime to `max_ttl`.
    pub fn issue(&self, mut claims: Claims, now: OffsetDateTime) -> Result<String, DomainError> {
        // `checked_add`, not a plain `+`: `FileStorageConfig::validate()`
        // already bounds `max_url_ttl_secs` at `MAX_URL_TTL_CEILING` (30
        // days), so this should never actually overflow `i64` -- but a
        // config invariant living in a different module is exactly the kind
        // of thing that can silently drift, and the fallout of an unchecked
        // overflow here is a wraparound (or a debug-build panic), not a
        // wrong-but-recoverable value. Defense in depth.
        let max_exp = now
            .unix_timestamp()
            .checked_add(self.max_ttl_secs)
            .ok_or_else(|| {
                DomainError::database(
                    "max_url_ttl_secs overflowed computing the token's max expiry",
                )
            })?;
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
///
/// The same type, and the same [`dedupe_public_keys`]/[`parse_public_key_list`]
/// helpers, back the control plane's OWN verifier for the s2s finalize/
/// report-part callbacks (`FileService::verifier`,
/// `FileService::with_previous_signing_public_keys`,
/// `FileStorageConfig::previous_signing_public_keys`) — a second, independent
/// place a `signing_key_seed` rotation needs multi-key acceptance, since the
/// sidecar's own multi-key set only widens what the *sidecar* accepts for
/// PUT/GET/part-upload requests, not what the control plane itself accepts
/// on those two callback routes.
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
        self.verify_with_grace(token, now, Duration::ZERO)
    }

    /// Verify a token exactly like [`Self::verify`], except a token whose
    /// `exp` has already passed is still accepted as long as `now` is no
    /// more than `grace` past it (`now < exp + grace`).
    ///
    /// This is for the **server-to-server finalize/report-part callbacks
    /// only**, never for a request that starts a content operation (PUT/GET/
    /// a part upload). Those callbacks carry the very same signed PUT token
    /// the sidecar already checked with strict `verify` at the start of the
    /// upload; the sidecar then deliberately does not re-check it for the
    /// rest of the stream, so a slow-but-live upload can legitimately take
    /// longer than the token's TTL and only reach finalize afterwards, with
    /// the bytes already durably written. Rejecting that finalize call would
    /// strand durable data behind a callback that can never succeed. The
    /// grace window does not weaken what is being verified: the signature,
    /// `op`, and the token's binding to `(file_id, version_id)` are checked
    /// exactly as before -- only the `exp` deadline gets slack, and only by
    /// as much as the caller (`FileStorageConfig::finalize_token_grace_secs`)
    /// configures. `grace = Duration::ZERO` is exactly [`Self::verify`]'s
    /// behaviour.
    pub fn verify_with_grace(
        &self,
        token: &str,
        now: OffsetDateTime,
        grace: Duration,
    ) -> Result<Claims, DomainError> {
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

        // Expiry is exclusive: a token stops being usable at `exp` (plus the
        // grace window, if any), not one second later. `grace` is clamped to
        // whole seconds and saturates rather than overflowing `i64`, which
        // matches `exp`'s own unit and cannot practically be reached by any
        // configured grace value.
        let expires_at = claims.exp.saturating_add(grace.whole_seconds());
        if now.unix_timestamp() >= expires_at {
            return Err(DomainError::token_invalid("token expired"));
        }
        Ok(claims)
    }
}

/// Decode one base64url-encoded (`URL_SAFE_NO_PAD`) Ed25519 public key.
///
/// Shared by the sidecar's `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` parsing
/// ([`parse_public_key_list`]) and the control plane's
/// `FileStorageConfig::previous_signing_public_keys` (`config::validate`,
/// `gear.rs`) so a malformed entry is decoded — and reported — identically
/// regardless of which side is parsing it. `index` is the entry's 0-based
/// position in whatever list the caller is decoding, named in the error so a
/// misconfiguration is easy to locate; this function does no length check —
/// callers get that for free from [`Verifier::from_public_keys`], which every
/// caller of this function goes on to call with the decoded bytes.
pub fn decode_public_key_entry(raw: &str, index: usize) -> Result<Vec<u8>, DomainError> {
    URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|e| DomainError::token_invalid(format!("invalid public key entry #{index}: {e}")))
}

/// Ceiling on how many entries either side's "previous keys" list may carry —
/// `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` (parsed here by [`parse_public_key_list`])
/// and `FileStorageConfig::previous_signing_public_keys` (`config::validate`).
///
/// Both lists back a [`Verifier`], which tries every configured key in turn on
/// every verification (see [`Verifier`]'s doc comment on why there's no `kid`
/// claim to skip straight to the right one) — so an unbounded list is an
/// unbounded per-request cost, not just a config-hygiene concern. The set only
/// ever needs to span keys retained during an in-progress `signing_key_seed`
/// rotation window (this module's doc comment on [`Verifier`] already
/// documents the set as "primary + at most a couple of retained previous
/// keys"); 8 leaves generous headroom over that for a rotation that overlaps
/// with a second, unrelated rotation, or one left stale for a while, without
/// letting the list grow large enough for the linear scan to matter.
pub const MAX_PREVIOUS_SIGNING_PUBLIC_KEYS: usize = 8;

/// Parse a comma-separated list of base64url-encoded Ed25519 public keys —
/// the `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` wire format (`bin/sidecar.rs`'s
/// module doc comment).
///
/// Each element is trimmed; an empty element (e.g. a stray trailing comma)
/// is silently skipped rather than rejected — unlike a genuinely malformed
/// key, it carries no ambiguity about operator intent. A key that fails to
/// decode fails the whole parse, via [`decode_public_key_entry`]. Does **not**
/// enforce [`MAX_PREVIOUS_SIGNING_PUBLIC_KEYS`] itself — the sidecar's
/// `build_config` checks the parsed length, mirroring `config::validate`'s
/// check on `FileStorageConfig::previous_signing_public_keys`.
pub fn parse_public_key_list(raw: &str) -> Result<Vec<Vec<u8>>, DomainError> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, s)| decode_public_key_entry(s, i))
        .collect()
}

/// De-duplicate an ordered public-key set: `primary` always leads, followed
/// by `previous` in order, with any repeat — of `primary`, or of an entry
/// already kept from `previous` — dropped. Order is preserved among the
/// survivors. Returns `(deduped_keys, dropped_count)`.
///
/// Shared by the sidecar's `FS_SIDECAR_PUBLIC_KEY`/
/// `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` wiring (`bin/sidecar.rs::main`) and the
/// control plane's `signing_key_seed`/`previous_signing_public_keys` wiring
/// (`FileService::with_previous_signing_public_keys`) — both accept a small
/// ordered key set for the same reason (see this module's doc comment on
/// [`Verifier`]).
///
/// A duplicate key is a harmless no-op here: [`Verifier::verify`] already
/// tries each key in the set in order and stops at the first match, so a
/// repeated key only costs one wasted comparison in the rare case where
/// every other key fails to verify — never a correctness issue. That is why
/// this silently drops duplicates (leaving a startup warning to the call
/// site) instead of rejecting them as a configuration error.
#[must_use]
pub fn dedupe_public_keys(primary: Vec<u8>, previous: Vec<Vec<u8>>) -> (Vec<Vec<u8>>, usize) {
    let mut deduped = Vec::with_capacity(previous.len() + 1);
    deduped.push(primary);
    let mut dropped = 0_usize;
    for key in previous {
        if deduped.contains(&key) {
            dropped += 1;
        } else {
            deduped.push(key);
        }
    }
    (deduped, dropped)
}

#[cfg(test)]
#[path = "signed_url_tests.rs"]
mod signed_url_tests;
