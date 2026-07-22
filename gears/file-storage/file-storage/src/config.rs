//! Gear configuration for file-storage.
//!
//! In P1 storage backends are loaded from static TOML at startup
//! (`cpt-cf-file-storage-fr-backend-config-source`). M0 pinned the basic knobs;
//! the backend table and data-plane URL are added here.

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
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub signing_key_seed: Option<SecretString>,

    /// When `true` (the default), gear init fails fast if `signing_key_seed`
    /// is absent instead of silently minting an ephemeral per-boot key. A
    /// multi-replica deployment that forgets to set the seed would otherwise
    /// mint a different signing key per replica, breaking signed URLs across
    /// requests routed to a different replica. Set `false` to explicitly opt
    /// into the ephemeral-key dev/test behaviour.
    #[serde(default = "default_require_signing_key_seed")]
    pub require_signing_key_seed: bool,

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
    /// **Defaults to `true`** — any deployment that doesn't say otherwise
    /// gets orphan/retention sweeping on out of the box. Test/dev harnesses
    /// that construct a `FileStorageConfig` directly (not via YAML) and need
    /// deterministic behavior must explicitly set this to `false`.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-cf-file-storage-fr-retention-policies
    #[serde(default = "default_enable_background_sweep")]
    pub enable_background_sweep: bool,

    /// When `true`, an additional non-durable `memory` backend is registered
    /// alongside the default `local-fs` backend. **Must be `false` by
    /// default** — the in-memory backend loses all content on restart, so it
    /// must be an explicit dev/test opt-in rather than always present.
    ///
    /// @cpt-cf-file-storage-fr-backend-config-source
    #[serde(default)]
    pub enable_in_memory_backend: bool,

    /// Zero or more S3-compatible backends to register alongside `local-fs`
    /// (and `memory` if enabled). Each entry becomes one `S3Backend` in the
    /// registry, keyed by its own `id`. Empty by default — a deployment opts
    /// in explicitly.
    ///
    /// @cpt-cf-file-storage-adr-s3-client-selection
    #[serde(default)]
    pub s3_backends: Vec<S3BackendConfig>,

    /// Backend id `build_backend_registry` designates as the registry's
    /// default (the backend new `create`/`initiate_multipart` calls write
    /// to — see `BackendRegistry::default_backend`). `None` (the default)
    /// keeps `local-fs` as the default, preserving today's behavior for every
    /// deployment that doesn't set this. Set this to one of `s3_backends`'
    /// configured ids to make that S3 backend the default instead — e.g. the
    /// S3 e2e suite (`testing/e2e/gears/file_storage/lifecycle_s3/`) sets
    /// this so `POST /files` and `POST /files/{id}/multipart` mint upload
    /// URLs whose `claims.backend_id` names the S3 test-double backend,
    /// exercising Stage 5's per-request sidecar dispatch end-to-end. The
    /// configured id must be one of the registry's backends (`local-fs`,
    /// `memory` if enabled, or an `s3_backends` entry) — `build_backend_registry`
    /// surfaces an unknown id as a fail-fast gear-init error via
    /// `BackendRegistry::new`'s own validation, never a panic.
    #[serde(default)]
    pub default_backend_id: Option<String>,

    /// Interim gear-local shared secret (P2 0.1 remaining) the s2s
    /// finalize/report-part callback routes additionally require, on top of
    /// the signed upload token, via the `x-fs-internal-token` request
    /// header. `None` (the default) preserves today's token-only trust
    /// model. This is a stop-gap until the platform's
    /// `toolkit-security::internal_auth` profiles are deployable in this
    /// gear — see `docs/ADR/0003-…-sidecar-data-plane.md`'s trust-model
    /// section — at which point the comparator should be swapped for
    /// `InternalAuthenticator`. Never printed by `Debug`.
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub finalize_internal_secret: Option<SecretString>,

    /// When `true`, gear init fails fast if `finalize_internal_secret` is
    /// absent instead of silently accepting the token-only trust model for
    /// the finalize/report-part callbacks. Mirrors `require_signing_key_seed`
    /// (`config.rs`). Defaults to `false` so existing deployments — and any
    /// sidecar not yet redeployed with `FS_SIDECAR_INTERNAL_TOKEN` — keep
    /// working; flip to `true` only after every sidecar talking to this
    /// control plane has been redeployed with the matching env var (see the
    /// migration-path note in the ADR).
    #[serde(default)]
    pub require_finalize_internal_secret: bool,
}

/// One S3-compatible backend entry (`FileStorageConfig::s3_backends`).
///
/// `Debug` is implemented manually so `secret_access_key` is never printed (a
/// config dump must not leak the credential), mirroring
/// `FileStorageConfig`'s own manual `Debug` impl for `signing_key_seed`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3BackendConfig {
    /// Backend id this entry registers under (must be unique across the
    /// whole registry, including `local-fs`/`memory` — enforced by
    /// `BackendRegistry::new`).
    pub id: String,

    /// S3-compatible HTTP(S) endpoint, e.g. `http://127.0.0.1:9000` for
    /// `MinIO`/`s3s-fs`. `None` means real AWS S3 — the endpoint is derived
    /// from `region` (`https://s3.{region}.amazonaws.com`).
    #[serde(default)]
    pub endpoint: Option<String>,

    /// AWS region (or the region the S3-compatible endpoint expects for
    /// `SigV4` signing, e.g. `us-east-1` for most `MinIO`/`s3s-fs` setups).
    pub region: String,

    /// Target bucket name.
    pub bucket: String,

    /// Access key id. `None` resolves `AWS_ACCESS_KEY_ID` from the process
    /// environment at construction time instead of a static config value.
    #[serde(default)]
    pub access_key_id: Option<String>,

    /// Secret access key. `None` resolves `AWS_SECRET_ACCESS_KEY` from the
    /// process environment at construction time instead of a static config
    /// value. Never printed by `Debug` — see the struct-level doc comment.
    #[serde(
        default,
        serialize_with = "toolkit_utils::secret_string::serialize_option_exposed"
    )]
    pub secret_access_key: Option<SecretString>,

    /// `true` for path-style addressing (`MinIO`/`s3s-fs`-style endpoints),
    /// `false` for virtual-hosted-style real S3. Defaults to `true` since
    /// most non-AWS S3-compatible endpoints require it.
    ///
    /// NOTE: `S3Backend::new` (Stage 1) always builds its `rusty_s3::Bucket`
    /// with `UrlStyle::Path` regardless of this flag — path-style addressing
    /// is also valid against real AWS S3, just not the modern default. This
    /// field is accepted and round-tripped today as a forward-compatible
    /// knob; wiring it through to `S3Backend` (adding a virtual-hosted-style
    /// option) is deferred to a later stage, not part of this config-wiring
    /// stage.
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
        // A missing signing_key_seed makes gear init mint an ephemeral per-boot
        // key; in a multi-replica deployment each replica would get a
        // different key, breaking signed URLs across replicas. Require an
        // explicit opt-out for this to be acceptable (e.g. local dev/test).
        if self.require_signing_key_seed && self.signing_key_seed.is_none() {
            anyhow::bail!(
                "invalid file-storage config: signing_key_seed is required (set \
                 require_signing_key_seed: false to allow an ephemeral per-boot key in dev)"
            );
        }
        // A missing finalize_internal_secret with the flag set would silently
        // fall back to the token-only trust model for the s2s finalize/
        // report-part callbacks — require an explicit opt-out (P2 0.1
        // remaining).
        if self.require_finalize_internal_secret && self.finalize_internal_secret.is_none() {
            anyhow::bail!(
                "invalid file-storage config: finalize_internal_secret is required (set \
                 require_finalize_internal_secret: false to allow the token-only trust model)"
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
            .field("enable_in_memory_backend", &self.enable_in_memory_backend)
            // Never print the signing key — only whether one is configured.
            .field(
                "signing_key_seed",
                &self.signing_key_seed.as_ref().map(|_| "<redacted>"),
            )
            .field("require_signing_key_seed", &self.require_signing_key_seed)
            // Safe to print directly: `S3BackendConfig` has its own redacting
            // `Debug` impl that substitutes `secret_access_key`'s value —
            // without that, this line would leak the secret through
            // `FileStorageConfig`'s output even though this struct never
            // touches the field itself.
            .field("s3_backends", &self.s3_backends)
            .field("default_backend_id", &self.default_backend_id)
            // Never print the shared secret — only whether one is configured.
            .field(
                "finalize_internal_secret",
                &self.finalize_internal_secret.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "require_finalize_internal_secret",
                &self.require_finalize_internal_secret,
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
            orphan_grace_secs: default_orphan_grace_secs(),
            sweep_interval_secs: default_sweep_interval_secs(),
            enable_background_sweep: default_enable_background_sweep(),
            enable_in_memory_backend: false,
            s3_backends: Vec::new(),
            default_backend_id: None,
            finalize_internal_secret: None,
            require_finalize_internal_secret: false,
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
    true // on by default; test/dev harnesses building a config directly must opt out explicitly for determinism
}

fn default_require_signing_key_seed() -> bool {
    true // secure-by-default: no seed configured must not silently accept an ephemeral key
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
