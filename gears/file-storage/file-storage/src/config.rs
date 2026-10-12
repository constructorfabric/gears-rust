//! Gear configuration for file-storage.
//!
//! Storage backends are loaded from static config at startup.

use std::fmt;

use serde::{Deserialize, Serialize};
use toolkit_utils::SecretString;

/// Configuration for the `file-storage` gear.
///
/// `Debug` is implemented manually so the `signing_key_seed` private key is never
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

    /// Base64url-encoded 32-byte Ed25519 seed for the URL-signing key, making the key
    /// stable across restarts. When absent an ephemeral key is generated at boot (dev
    /// only: signed URLs do not survive a restart and the sidecar needs reconfiguring).
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub signing_key_seed: Option<SecretString>,

    /// When `true` (default), gear init fails if `signing_key_seed` is absent, since
    /// replicas would otherwise each mint a different key. Set `false` for dev/test.
    #[serde(default = "default_require_signing_key_seed")]
    pub require_signing_key_seed: bool,

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
        if self.require_signing_key_seed && self.signing_key_seed.is_none() {
            anyhow::bail!(
                "invalid file-storage config: signing_key_seed is required (set \
                 require_signing_key_seed: false to allow an ephemeral per-boot key in dev)"
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
        Ok(())
    }
}

impl fmt::Debug for FileStorageConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStorageConfig")
            .field("default_url_ttl_secs", &self.default_url_ttl_secs)
            .field("max_url_ttl_secs", &self.max_url_ttl_secs)
            .field("sidecar_base_url", &self.sidecar_base_url)
            .field("default_page_size", &self.default_page_size)
            .field("max_page_size", &self.max_page_size)
            .field("storage_root", &self.storage_root)
            .field("idempotency_ttl_secs", &self.idempotency_ttl_secs)
            .field("enable_in_memory_backend", &self.enable_in_memory_backend)
            // Never print the signing key — only whether one is configured.
            .field(
                "signing_key_seed",
                &self.signing_key_seed.as_ref().map(|_| "<redacted>"),
            )
            .field("require_signing_key_seed", &self.require_signing_key_seed)
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
            sidecar_base_url: default_sidecar_base_url(),
            default_page_size: default_page_size(),
            max_page_size: default_max_page_size(),
            storage_root: default_storage_root(),
            signing_key_seed: None,
            require_signing_key_seed: default_require_signing_key_seed(),
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

fn default_sidecar_base_url() -> String {
    "http://localhost:8087".to_owned()
}

fn default_page_size() -> u64 {
    50
}

fn default_max_page_size() -> u64 {
    1000
}

fn default_storage_root() -> String {
    "./.file-storage-data".to_owned()
}

fn default_idempotency_ttl_secs() -> u64 {
    86400 // 24 hours
}

fn default_require_signing_key_seed() -> bool {
    true // secure-by-default: no seed configured must not silently accept an ephemeral key
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
