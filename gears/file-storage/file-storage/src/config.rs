//! Gear configuration for file-storage.
//!
//! In P1 storage backends are loaded from static TOML at startup
//! (`cpt-cf-file-storage-fr-backend-config-source`). M0 pinned the basic knobs;
//! the backend table and data-plane URL are added here.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Configuration for the `file-storage` gear.
///
/// `Debug` is implemented manually so the `signing_key_seed` private key is never
/// printed (a config dump must not leak the URL-signing key).
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileStorageConfig {
    /// Default URL TTL (seconds) applied to every signed URL the control plane
    /// mints, kept short to bound the stale-permission window (DESIGN §4.5,
    /// recommended minutes). Callers may justify more, never beyond
    /// `max_url_ttl_secs`. See `cpt-cf-file-storage-fr-signed-urls`.
    #[serde(default = "default_default_url_ttl_secs")]
    pub default_url_ttl_secs: u64,

    /// Hard ceiling on URL TTL (seconds) the control plane will sign; recommended
    /// 7 days. The control plane refuses to mint beyond this.
    /// See `cpt-cf-file-storage-fr-signed-urls`.
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

    /// Local filesystem root for the default `local-fs` backend (P1 static).
    #[serde(default = "default_storage_root")]
    pub storage_root: String,

    /// Base64url-encoded 32-byte Ed25519 seed for the URL-signing key. When set,
    /// the signing keypair (and the public key the sidecar verifies against) is
    /// **stable across restarts**. When absent, an ephemeral key is generated at
    /// boot — fine for local dev, but signed URLs do not survive a restart and
    /// the sidecar must be reconfigured. Configure this in any real deployment.
    #[serde(default)]
    pub signing_key_seed: Option<String>,

    /// Window (seconds) for which an idempotency key is retained.
    /// After this window, a retry with the same key is treated as a fresh request.
    /// Default: 86400 (24 hours).
    ///
    /// @cpt-cf-file-storage-fr-upload-idempotency
    #[serde(default = "default_idempotency_ttl_secs")]
    pub idempotency_ttl_secs: u64,

    /// Grace period (seconds) before a pending version or abandoned multipart
    /// session is eligible for orphan reconciliation.
    /// Default: 3600 (1 hour).
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    #[serde(default = "default_orphan_grace_secs")]
    pub orphan_grace_secs: u64,

    /// How often (seconds) the background cleanup sweep fires.
    /// Default: 3600 (1 hour).
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-cf-file-storage-fr-retention-policies
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,

    /// When `true`, the background cleanup sweep is started at gear init.
    /// **Must be `false` by default** so integration tests are deterministic.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-cf-file-storage-fr-retention-policies
    #[serde(default = "default_enable_background_sweep")]
    pub enable_background_sweep: bool,
}

impl FileStorageConfig {
    /// Validates cross-field invariants that `serde` cannot express.
    ///
    /// Called at gear init (see `gear.rs`) before the config is used to wire
    /// anything up, so a misconfiguration fails fast with a clear message
    /// rather than manifesting as runtime misbehaviour.
    pub fn validate(&self) -> anyhow::Result<()> {
        // A zero sweep interval with the sweep enabled turns the background
        // loop (`sleep(Duration::from_secs(0))`) into a tight spin that pegs
        // the runtime and floods the logs. Reject it up front.
        if self.enable_background_sweep && self.sweep_interval_secs == 0 {
            anyhow::bail!(
                "invalid file-storage config: sweep_interval_secs must be > 0 when \
                 enable_background_sweep is true"
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
            .field("orphan_grace_secs", &self.orphan_grace_secs)
            .field("sweep_interval_secs", &self.sweep_interval_secs)
            .field("enable_background_sweep", &self.enable_background_sweep)
            // Never print the signing key — only whether one is configured.
            .field(
                "signing_key_seed",
                &self.signing_key_seed.as_ref().map(|_| "<redacted>"),
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
            idempotency_ttl_secs: default_idempotency_ttl_secs(),
            orphan_grace_secs: default_orphan_grace_secs(),
            sweep_interval_secs: default_sweep_interval_secs(),
            enable_background_sweep: default_enable_background_sweep(),
        }
    }
}

fn default_default_url_ttl_secs() -> u64 {
    // 15 minutes: the short default issuance TTL (DESIGN §4.5) that bounds the
    // stale-permission window for every minted URL.
    15 * 60
}

fn default_max_url_ttl_secs() -> u64 {
    // 7 days, the recommended maximum from the signed-URL FR.
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

fn default_orphan_grace_secs() -> u64 {
    3600 // 1 hour
}

fn default_sweep_interval_secs() -> u64 {
    3600 // 1 hour
}

fn default_enable_background_sweep() -> bool {
    false // must be false so tests are deterministic
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
