//! Gear configuration for file-storage.
//!
//! Storage backends are loaded from static config at startup.

use std::fmt;

use serde::{Deserialize, Serialize};
use toolkit_utils::SecretString;

/// Upper bound (seconds) for `finalize_token_grace_secs`: 7 days.
///
/// `gear.rs` converts to `i64` saturating; an oversized value would make the `exp` check a no-op.
pub const MAX_FINALIZE_TOKEN_GRACE_SECS: u64 = 7 * 24 * 3600;

/// Upper bound (seconds) for `multipart_session_ttl_secs`: 30 days (added to `now`; must not overflow).
pub const MAX_MULTIPART_SESSION_TTL_SECS: u64 = 30 * 24 * 3600;

/// Upper bound (seconds) for `multipart_complete_lease_secs`: 1 day (a huge lease lets a crashed
/// completer block others).
pub const MAX_MULTIPART_COMPLETE_LEASE_SECS: u64 = 24 * 3600;

/// Absolute ceiling accepted for `max_page_size` (the platform-wide cursor-pagination cap);
/// `validate()` rejects anything above it.
pub(crate) const MAX_PAGE_SIZE_CEILING: u64 = 200;

/// Configuration for the `file-storage` gear.
///
/// `Debug` is implemented manually so the `signing_key_pkcs8` private key is never
/// printed (a config dump must not leak the URL-signing key).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct FileStorageConfig {
    /// Default signed-URL TTL in seconds (default 15 minutes), kept short to bound the
    /// stale-permission window. Never exceeds `max_url_ttl_secs`.
    #[serde(default = "default_default_url_ttl_secs")]
    pub default_url_ttl_secs: u64,

    /// Hard ceiling on signed-URL TTL in seconds (default 7 days).
    #[serde(default = "default_max_url_ttl_secs")]
    pub max_url_ttl_secs: u64,

    /// Grace (seconds) on the signed PUT token's `exp` when re-checked on finalize/report-part: the
    /// sidecar checks `exp` only at PUT start, so a slow upload can arrive expired. `0` disables.
    /// Default 3600, capped by `validate()`.
    #[serde(default = "default_finalize_token_grace_secs")]
    pub finalize_token_grace_secs: u64,

    /// Lifetime (seconds) of a multipart session, independent of `default_url_ttl_secs` so large
    /// uploads are not capped by the URL TTL. Default 86400.
    #[serde(default = "default_multipart_session_ttl_secs")]
    pub multipart_session_ttl_secs: u64,

    /// Lease (seconds) a multipart `complete` holds on `completing` before takeover after a crash.
    /// Default 120.
    #[serde(default = "default_multipart_complete_lease_secs")]
    pub multipart_complete_lease_secs: u64,

    /// Public base URL of the data-plane sidecar that signed URLs point at.
    #[serde(default = "default_sidecar_base_url")]
    pub sidecar_base_url: String,

    /// Default page size for `GET /files` listing.
    #[serde(default = "default_page_size")]
    pub default_page_size: u64,

    /// Maximum page size a caller may request.
    #[serde(default = "default_max_page_size")]
    pub max_page_size: u64,

    /// Local filesystem root for the default `local-fs` backend.
    #[serde(default = "default_storage_root")]
    pub storage_root: String,

    /// Base64url-encoded PKCS#8 DER ECDSA P-256 private key for the URL-signing key, making the key
    /// stable across restarts. When absent an ephemeral key is generated at boot (dev
    /// only: signed URLs do not survive a restart and the sidecar needs reconfiguring).
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub signing_key_pkcs8: Option<SecretString>,

    /// When `true` (default), gear init fails if `signing_key_pkcs8` is absent, since
    /// replicas would otherwise each mint a different key. Set `false` for dev/test.
    #[serde(default = "default_require_signing_key")]
    pub require_signing_key: bool,

    /// Seconds an idempotency key is retained (default 86400); after that a retry with
    /// the same key is a fresh request.
    #[serde(default = "default_idempotency_ttl_secs")]
    pub idempotency_ttl_secs: u64,

    /// Registers an extra non-durable `memory` backend (dev/test only; loses content on
    /// restart). Default `false`.
    #[serde(default)]
    pub enable_in_memory_backend: bool,

    /// S3-compatible backends to register alongside `local-fs`, each keyed by its `id`.
    #[serde(default)]
    pub s3_backends: Vec<S3BackendConfig>,

    /// Backend id that new uploads write to (`BackendRegistry::default_backend`).
    /// `None` keeps `local-fs`. Must name a registered backend (`local-fs`, `memory` if
    /// enabled, or an `s3_backends` entry); an unknown id fails gear init.
    #[serde(default)]
    pub default_backend_id: Option<String>,

    /// **Required** (enforced by `validate()`). Shared secret the sidecar sends in the
    /// `x-fs-internal-token` header on the finalize/report-part callbacks, on top of the
    /// signed upload token; set the same value as `FS_SIDECAR_INTERNAL_TOKEN`. The
    /// control plane trusts the size and SHA-256 reported on that callback. Interim until
    /// `toolkit-security::internal_auth` can replace it (ADR-0003). Redacted in `Debug`.
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub finalize_internal_secret: Option<SecretString>,
}

/// One S3-compatible backend entry (`FileStorageConfig::s3_backends`).
///
/// `Debug` is implemented manually so `secret_access_key` is never printed.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3BackendConfig {
    /// Backend id; must be unique across the whole registry (`BackendRegistry::new`).
    pub id: String,

    /// S3-compatible endpoint, e.g. `http://127.0.0.1:9000` for `MinIO`. `None` means real
    /// AWS S3 (`https://s3.{region}.amazonaws.com`).
    #[serde(default)]
    pub endpoint: Option<String>,

    /// Region used for `SigV4` signing (e.g. `us-east-1` for most `MinIO` setups).
    pub region: String,

    /// Target bucket name.
    pub bucket: String,

    /// Access key id. `None` reads `AWS_ACCESS_KEY_ID` from the environment.
    #[serde(default)]
    pub access_key_id: Option<String>,

    /// Secret access key. `None` reads `AWS_SECRET_ACCESS_KEY` from the environment.
    /// Redacted in `Debug`.
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub secret_access_key: Option<SecretString>,

    /// Path-style addressing (default `true`, as most non-AWS endpoints require it).
    /// Currently accepted but not forwarded: `S3Backend::new` always uses
    /// `UrlStyle::Path`, which is also valid on AWS S3.
    #[serde(default = "default_path_style")]
    pub path_style: bool,
}

impl fmt::Debug for S3BackendConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3BackendConfig")
            .field("id", &self.id)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            // Never print the secret — only whether one is configured.
            .field(
                "secret_access_key",
                &self.secret_access_key.as_ref().map(|_| "<redacted>"),
            )
            .field("path_style", &self.path_style)
            .finish()
    }
}

fn default_path_style() -> bool {
    true // most non-AWS S3-compatible endpoints (MinIO, s3s-fs) require it
}

impl FileStorageConfig {
    /// Validates cross-field invariants that `serde` cannot express; called at gear
    /// init so a misconfiguration fails fast.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.require_signing_key && self.signing_key_pkcs8.is_none() {
            anyhow::bail!(
                "invalid file-storage config: signing_key_pkcs8 is required (set \
                 require_signing_key: false to allow an ephemeral per-boot key in dev)"
            );
        }
        // finalize trusts the size/hash the sidecar reports, so the secret is mandatory.
        if self
            .finalize_internal_secret
            .as_ref()
            .is_none_or(|s| s.expose().is_empty())
        {
            anyhow::bail!(
                "invalid file-storage config: finalize_internal_secret is required (set the \
                 same value as the sidecar's FS_SIDECAR_INTERNAL_TOKEN)"
            );
        }
        // Bounded: `gear.rs` saturates the `i64` conversion (would make `exp` a no-op).
        if self.finalize_token_grace_secs > MAX_FINALIZE_TOKEN_GRACE_SECS {
            anyhow::bail!(
                "invalid file-storage config: finalize_token_grace_secs ({}) must not exceed \
                 MAX_FINALIZE_TOKEN_GRACE_SECS ({})",
                self.finalize_token_grace_secs,
                MAX_FINALIZE_TOKEN_GRACE_SECS
            );
        }
        // A zero page size would make every listing return an empty page with no cursor.
        if self.default_page_size == 0 || self.max_page_size == 0 {
            anyhow::bail!(
                "invalid file-storage config: default_page_size ({}) and max_page_size ({}) \
                 must be at least 1",
                self.default_page_size,
                self.max_page_size
            );
        }
        // The default page size must not exceed the max.
        if self.default_page_size > self.max_page_size {
            anyhow::bail!(
                "invalid file-storage config: default_page_size ({}) must not exceed \
                 max_page_size ({})",
                self.default_page_size,
                self.max_page_size
            );
        }
        // Cap `max_page_size` at the ceiling.
        if self.max_page_size > MAX_PAGE_SIZE_CEILING {
            anyhow::bail!(
                "invalid file-storage config: max_page_size ({}) must not exceed \
                 MAX_PAGE_SIZE_CEILING ({})",
                self.max_page_size,
                MAX_PAGE_SIZE_CEILING
            );
        }
        // Zero would mint a 1-second session (`.max(1)` downstream).
        if self.multipart_session_ttl_secs == 0 {
            anyhow::bail!(
                "invalid file-storage config: multipart_session_ttl_secs must be > 0 (a \
                 zero-second session lifetime would expire before any upload could complete)"
            );
        }
        // The session must outlive the per-part URLs.
        if self.multipart_session_ttl_secs < self.default_url_ttl_secs {
            anyhow::bail!(
                "invalid file-storage config: multipart_session_ttl_secs ({}) must be >= \
                 default_url_ttl_secs ({}) -- otherwise a signed upload URL minted at initiate \
                 time could remain valid past the multipart session's own expiry",
                self.multipart_session_ttl_secs,
                self.default_url_ttl_secs
            );
        }
        // Bounded: added to `now` for `expires_at`.
        if self.multipart_session_ttl_secs > MAX_MULTIPART_SESSION_TTL_SECS {
            anyhow::bail!(
                "invalid file-storage config: multipart_session_ttl_secs ({}) must not exceed \
                 MAX_MULTIPART_SESSION_TTL_SECS ({})",
                self.multipart_session_ttl_secs,
                MAX_MULTIPART_SESSION_TTL_SECS
            );
        }
        // Zero would defeat crash recovery.
        if self.multipart_complete_lease_secs == 0 {
            anyhow::bail!(
                "invalid file-storage config: multipart_complete_lease_secs must be > 0 (a \
                 zero-second lease would let another caller take it over almost immediately)"
            );
        }
        // Bounded: an oversized lease lets a crashed completer block others.
        if self.multipart_complete_lease_secs > MAX_MULTIPART_COMPLETE_LEASE_SECS {
            anyhow::bail!(
                "invalid file-storage config: multipart_complete_lease_secs ({}) must not exceed \
                 MAX_MULTIPART_COMPLETE_LEASE_SECS ({})",
                self.multipart_complete_lease_secs,
                MAX_MULTIPART_COMPLETE_LEASE_SECS
            );
        }
        Ok(())
    }
}

impl fmt::Debug for FileStorageConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStorageConfig")
            .field("default_url_ttl_secs", &self.default_url_ttl_secs)
            .field("max_url_ttl_secs", &self.max_url_ttl_secs)
            .field("finalize_token_grace_secs", &self.finalize_token_grace_secs)
            .field(
                "multipart_session_ttl_secs",
                &self.multipart_session_ttl_secs,
            )
            .field(
                "multipart_complete_lease_secs",
                &self.multipart_complete_lease_secs,
            )
            .field("sidecar_base_url", &self.sidecar_base_url)
            .field("default_page_size", &self.default_page_size)
            .field("max_page_size", &self.max_page_size)
            .field("storage_root", &self.storage_root)
            .field("idempotency_ttl_secs", &self.idempotency_ttl_secs)
            .field("enable_in_memory_backend", &self.enable_in_memory_backend)
            // Never print the signing key — only whether one is configured.
            .field(
                "signing_key_pkcs8",
                &self.signing_key_pkcs8.as_ref().map(|_| "<redacted>"),
            )
            .field("require_signing_key", &self.require_signing_key)
            // Safe: `S3BackendConfig` has its own redacting `Debug`.
            .field("s3_backends", &self.s3_backends)
            .field("default_backend_id", &self.default_backend_id)
            // Never print the shared secret — only whether one is configured.
            .field(
                "finalize_internal_secret",
                &self.finalize_internal_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Default for FileStorageConfig {
    fn default() -> Self {
        Self {
            default_url_ttl_secs: default_default_url_ttl_secs(),
            max_url_ttl_secs: default_max_url_ttl_secs(),
            finalize_token_grace_secs: default_finalize_token_grace_secs(),
            multipart_session_ttl_secs: default_multipart_session_ttl_secs(),
            multipart_complete_lease_secs: default_multipart_complete_lease_secs(),
            sidecar_base_url: default_sidecar_base_url(),
            default_page_size: default_page_size(),
            max_page_size: default_max_page_size(),
            storage_root: default_storage_root(),
            signing_key_pkcs8: None,
            require_signing_key: default_require_signing_key(),
            idempotency_ttl_secs: default_idempotency_ttl_secs(),
            enable_in_memory_backend: false,
            s3_backends: Vec::new(),
            default_backend_id: None,
            finalize_internal_secret: None,
        }
    }
}

fn default_default_url_ttl_secs() -> u64 {
    // 15 minutes.
    15 * 60
}

fn default_max_url_ttl_secs() -> u64 {
    // 7 days.
    7 * 24 * 60 * 60
}

fn default_finalize_token_grace_secs() -> u64 {
    3600 // 1 hour: see FileStorageConfig::finalize_token_grace_secs
}

fn default_multipart_session_ttl_secs() -> u64 {
    86400 // 24 hours
}

fn default_multipart_complete_lease_secs() -> u64 {
    120 // backend-assembly budget; see FileStorageConfig::multipart_complete_lease_secs
}

fn default_sidecar_base_url() -> String {
    "http://localhost:8087".to_owned()
}

fn default_page_size() -> u64 {
    25 // platform-wide cursor-pagination default (guidelines/DNA/REST/QUERYING.md)
}

fn default_max_page_size() -> u64 {
    200 // platform-wide cursor-pagination cap; see MAX_PAGE_SIZE_CEILING
}

fn default_storage_root() -> String {
    "./.file-storage-data".to_owned()
}

fn default_idempotency_ttl_secs() -> u64 {
    86400 // 24 hours
}

fn default_require_signing_key() -> bool {
    true // secure-by-default: no private key configured must not silently accept an ephemeral key
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
