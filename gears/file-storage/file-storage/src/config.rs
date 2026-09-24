//! Gear configuration for file-storage.
//!
//! Storage backends are loaded from the gear's own platform YAML config section
//! (`cpt-cf-file-storage-fr-backend-config-source`), not a standalone TOML file.
//! M0 pinned the basic knobs; the backend table and data-plane URL are added here.

use std::fmt;

use serde::{Deserialize, Serialize};
use toolkit_utils::SecretString;

/// Upper bound (seconds) accepted for `finalize_token_grace_secs`: 7 days.
///
/// `gear.rs` converts the field to `i64` via a saturating
/// `unwrap_or(i64::MAX)`, so without a ceiling here an oversized (or
/// corrupted/malicious) config value would silently become `i64::MAX`
/// seconds of grace, making the s2s finalize/report-part callbacks' `exp`
/// check a de-facto no-op. `validate()` rejects anything above this well
/// before that conversion runs.
pub const MAX_FINALIZE_TOKEN_GRACE_SECS: u64 = 7 * 24 * 3600;

/// Absolute ceiling accepted for `max_page_size`, independent of whatever an
/// operator configures.
///
/// `max_page_size` is the only practical bound on the batch reads a page of
/// listing results drives (`MetadataRepo::list_for_files`,
/// `VersionRepo::get_manifests`) -- both now chunk themselves against the
/// backend's own bind-parameter budget, so an oversized `max_page_size` no
/// longer risks a hard driver failure, but it still directly inflates a
/// single request's row count, chunk count, response size, and latency with
/// no cap of its own. `validate()` rejects anything above this regardless of
/// what an operator sets, the same way `MAX_FINALIZE_TOKEN_GRACE_SECS`
/// bounds `finalize_token_grace_secs`. `default_max_page_size` (1000) sits
/// exactly at this ceiling, so the shipped default is never itself rejected.
pub const MAX_PAGE_SIZE_CEILING: u64 = 1000;

/// Upper bound (seconds) accepted for `max_url_ttl_secs`: 30 days.
///
/// `max_url_ttl_secs` becomes `Issuer::max_ttl_secs` (`gear.rs`, via the same
/// saturating `i64::try_from(..).unwrap_or(i64::MAX)` conversion as
/// `finalize_token_grace_secs`), which `Issuer::issue` then adds directly to
/// `now.unix_timestamp()` to compute `max_exp`. Without a ceiling here, an
/// oversized value would carry through as `i64::MAX` and overflow that
/// addition; `validate()` rejects it up front instead. 30 days gives an
/// operator room well past the 7-day recommended default without allowing an
/// unbounded value.
pub const MAX_URL_TTL_CEILING: u64 = 30 * 24 * 3600;

/// Upper bound (seconds) accepted for `multipart_session_ttl_secs`: 30 days.
///
/// `gear.rs` converts the field to `i64` via the same saturating
/// `unwrap_or(i64::MAX)` pattern as `finalize_token_grace_secs`, and
/// `MultipartService::initiate_multipart_upload` then adds it directly to
/// `now` to compute the session's `expires_at`. Without a ceiling here, an
/// oversized (or corrupted/malicious) config value would silently become
/// `i64::MAX` seconds, overflowing that addition instead of producing a
/// usable expiry. 30 days comfortably covers even a very large multi-part
/// upload's realistic time budget while the shipped default (24 hours) sits
/// far below it.
pub const MAX_MULTIPART_SESSION_TTL_SECS: u64 = 30 * 24 * 3600;

/// Upper bound (seconds) accepted for `multipart_complete_lease_secs`: 1 day.
///
/// Unlike the TTL/grace knobs above, `gear.rs` already falls back to a safe
/// finite default (`unwrap_or(120)`, not `i64::MAX`) on conversion, so this
/// isn't the same silent-saturation hazard -- but the lease is meant to
/// bound how long one `complete` call may hold the `completing` state before
/// another caller can take it over after a crash (backend-assembly time
/// budget), and nothing otherwise stops an operator from configuring a value
/// that defeats that purpose. 1 day is generous for even a very slow backend
/// assembly while the shipped default (120s) sits far below it.
pub const MAX_MULTIPART_COMPLETE_LEASE_SECS: u64 = 24 * 3600;

/// Upper bound (seconds) accepted for `orphan_grace_secs`: 30 days.
///
/// `domain::cleanup::CleanupEngine::run_sweep` subtracts it from `now` (via
/// the same `i64::try_from(..).unwrap_or(3600)` pattern, already a safe
/// finite fallback rather than `i64::MAX`), so this isn't an overflow hazard
/// the way the addition sites above are -- but it otherwise has no ceiling of
/// its own, and the recommended `max_url_ttl_secs` (7 days) already implies
/// operators may reasonably want to raise `orphan_grace_secs` to match (see
/// the `max_url_ttl_secs`-vs-`orphan_grace_secs` warning below). 30 days
/// leaves room for that while still rejecting an unbounded value.
pub const MAX_ORPHAN_GRACE_SECS: u64 = 30 * 24 * 3600;

/// Upper bound (seconds) accepted for `idempotency_ttl_secs`: 30 days.
///
/// `FileService::create_file` adds it to `now` (via the same
/// `i64::try_from(..).unwrap_or(86400)` pattern, already a safe finite
/// fallback) to compute the stored idempotency record's `expires_at`. Not an
/// overflow hazard given that fallback, but otherwise has no ceiling of its
/// own; 30 days is well past any realistic retry window while the shipped
/// default (24 hours) sits far below it.
pub const MAX_IDEMPOTENCY_TTL_SECS: u64 = 30 * 24 * 3600;

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
    /// 7 days. `Issuer::issue` silently clamps `exp` down to `now + max_url_ttl_secs`
    /// when a caller-requested TTL would exceed it, rather than refusing to mint.
    /// See `cpt-cf-file-storage-fr-signed-urls`.
    #[serde(default = "default_max_url_ttl_secs")]
    pub max_url_ttl_secs: u64,

    /// Grace period (seconds) applied to the signed PUT token's `exp` when it
    /// is re-checked on the server-to-server finalize and report-part
    /// callbacks, on top of `verify`'s strict check. The sidecar checks the
    /// token once at the start of a PUT and deliberately does not re-check
    /// it for the rest of the stream (`FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS`
    /// bounds only inter-chunk idle time, not the upload's total duration);
    /// it then forwards that same token to the control plane's finalize
    /// callback once the upload completes. A slow-but-live upload that
    /// takes longer than the token's TTL (`default_url_ttl_secs`, 15
    /// minutes by default) can therefore reach finalize with an already-
    /// expired token even though every byte was legitimately written to the
    /// backend. This grace absorbs exactly that gap: it applies ONLY to the
    /// `exp` check on the s2s finalize/report-part callbacks, never to the
    /// token check the sidecar performs at the start of a PUT/GET request or
    /// a part upload. `0` disables it, restoring the previous strict
    /// behaviour where `exp` is enforced with no slack. Default: 3600 (1
    /// hour). Capped at `MAX_FINALIZE_TOKEN_GRACE_SECS` (7 days) by
    /// `validate()` -- gear.rs's `i64::try_from(..).unwrap_or(i64::MAX)`
    /// conversion would otherwise silently saturate an oversized value to
    /// `i64::MAX` seconds of grace, making the `exp` check a de-facto no-op.
    #[serde(default = "default_finalize_token_grace_secs")]
    pub finalize_token_grace_secs: u64,

    /// Lifetime (seconds) of a multipart upload *session* -- i.e. how long
    /// `MultipartUploadSession::expires_at` is set to at initiate time.
    /// **Deliberately independent of `default_url_ttl_secs`**: before this
    /// field existed, the session shared `default_url_ttl_secs` (a short
    /// bound meant for individual signed URLs, DESIGN §4.5), which capped
    /// every multi-GB multipart upload's total time budget at 15 minutes by
    /// default -- self-defeating for the large-upload use case multipart
    /// exists for, and for the introspect/resume flow (`cpt-cf-file-storage-
    /// flow-multipart-introspect`), whose resume-token expiry is capped at
    /// the session's own `expires_at` and so could never extend a session
    /// past that same short window. Default: 86400 (24 hours). Per-part
    /// signed-URL TTLs are unaffected and continue to use
    /// `default_url_ttl_secs`.
    #[serde(default = "default_multipart_session_ttl_secs")]
    pub multipart_session_ttl_secs: u64,

    /// Completion-lease duration (seconds) for multipart `complete`
    /// (upload-flow redesign): how long one `complete` may hold the
    /// `completing` state before another caller can take the lease over
    /// after a crash. Sized to the backend-assembly time budget; the lease
    /// is held WITHOUT any open DB transaction. Default: 120.
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
    #[serde(default = "default_idempotency_ttl_secs")]
    pub idempotency_ttl_secs: u64,

    /// Grace period (seconds) before a pending version or abandoned multipart
    /// session is eligible for orphan reconciliation.
    /// Default: 3600 (1 hour).
    #[serde(default = "default_orphan_grace_secs")]
    pub orphan_grace_secs: u64,

    /// How often (seconds) the background cleanup sweep fires.
    /// Default: 3600 (1 hour).
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,

    /// When `true`, the background cleanup sweep is started at gear init.
    /// **Defaults to `true`** — any deployment that doesn't say otherwise
    /// gets orphan/retention sweeping on out of the box. Test/dev harnesses
    /// that construct a `FileStorageConfig` directly (not via YAML) and need
    /// deterministic behavior must explicitly set this to `false`.
    #[serde(default = "default_enable_background_sweep")]
    pub enable_background_sweep: bool,

    /// When `true`, an additional non-durable `memory` backend is registered
    /// alongside the default `local-fs` backend. **Must be `false` by
    /// default** — the in-memory backend loses all content on restart, so it
    /// must be an explicit dev/test opt-in rather than always present.
    #[serde(default)]
    pub enable_in_memory_backend: bool,

    /// Zero or more S3-compatible backends to register alongside `local-fs`
    /// (and `memory` if enabled). Each entry becomes one `S3Backend` in the
    /// registry, keyed by its own `id`. Empty by default — a deployment opts
    /// in explicitly.
    #[serde(default)]
    pub s3_backends: Vec<S3BackendConfig>,

    /// Backend id `build_backend_registry` designates as the registry's
    /// default (the backend new `create`/`initiate_multipart` calls write
    /// to — see `BackendRegistry::default_backend`). `None` (the default)
    /// keeps `local-fs` as the default, preserving today's behavior for every
    /// deployment that doesn't set this. Set this to one of `s3_backends`'
    /// configured ids to make that S3 backend the default instead — e.g. the
    /// S3 e2e suite (`testing/e2e/suites/file_storage/lifecycle_s3/`) sets
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

    /// Public keys of previously-active `signing_key_seed`s that the s2s
    /// finalize/report-part callback routes still accept, on top of the
    /// current seed's key (`FileService::verifier`,
    /// `Verifier::verify_with_grace`). Each entry is a base64url-encoded
    /// (`URL_SAFE_NO_PAD`) raw Ed25519 public key — the same wire format as
    /// one element of the sidecar's `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` list.
    /// Empty by default: absent an in-progress `signing_key_seed` rotation
    /// there is no previous key to accept.
    ///
    /// This exists because the sidecar's own multi-key
    /// `FS_SIDECAR_PUBLIC_KEY`/`FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` mechanism
    /// only widens what the **sidecar** accepts for the PUT/GET/part-upload
    /// requests it verifies itself; it does nothing for the control plane's
    /// OWN verification of the sidecar's finalize/report-part callbacks,
    /// which — absent this field — only ever accepted the single key
    /// `signing_key_seed` currently derives. Without it, restarting the
    /// control plane on a new seed (`signing_key_seed`'s **Rotation**
    /// procedure, step 3, below) fails the callback for every upload started
    /// before the restart, even though its signed URL (issued by the OLD
    /// seed, and still being honoured by the sidecar fleet per step 2) is
    /// otherwise perfectly valid.
    ///
    /// `validate()` fails gear init on a malformed entry (bad base64, or the
    /// wrong decoded length); harmless duplicates (of the current key, or
    /// within this list) are silently deduped with a startup warning once
    /// the current key is actually known — see
    /// `FileService::with_previous_signing_public_keys` and
    /// `infra::signed_url::dedupe_public_keys`.
    #[serde(default)]
    pub previous_signing_public_keys: Vec<String>,
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
        // `gear.rs` converts `finalize_token_grace_secs` to `i64` via a
        // saturating `unwrap_or(i64::MAX)`; without an upper bound here an
        // oversized value would silently become `i64::MAX` seconds of grace,
        // making the s2s finalize/report-part callbacks' `exp` check a
        // de-facto no-op. Reject it up front instead.
        if self.finalize_token_grace_secs > MAX_FINALIZE_TOKEN_GRACE_SECS {
            anyhow::bail!(
                "invalid file-storage config: finalize_token_grace_secs ({}) must not exceed \
                 MAX_FINALIZE_TOKEN_GRACE_SECS ({})",
                self.finalize_token_grace_secs,
                MAX_FINALIZE_TOKEN_GRACE_SECS
            );
        }
        // `max_url_ttl_secs` otherwise has no ceiling of its own -- `gear.rs`
        // converts it to `i64` via the same saturating `unwrap_or(i64::MAX)`
        // pattern as `finalize_token_grace_secs`, and `Issuer::issue` then
        // adds it directly to `now.unix_timestamp()`. Reject it up front, the
        // same way MAX_FINALIZE_TOKEN_GRACE_SECS bounds
        // finalize_token_grace_secs above.
        if self.max_url_ttl_secs > MAX_URL_TTL_CEILING {
            anyhow::bail!(
                "invalid file-storage config: max_url_ttl_secs ({}) must not exceed \
                 MAX_URL_TTL_CEILING ({})",
                self.max_url_ttl_secs,
                MAX_URL_TTL_CEILING
            );
        }
        // `MultipartService` applies `url_ttl_secs.max(1)` (per-part URLs,
        // resume-URL caps) as defense-in-depth against a zero TTL reaching
        // `checked_add`; unlike `finalize_token_grace_secs`, `0` has no
        // documented "disabled" meaning here; it would just silently mint
        // every signed URL with a 1-second TTL. Reject it up front instead of
        // letting that substitution paper over a misconfiguration.
        if self.default_url_ttl_secs == 0 {
            anyhow::bail!(
                "invalid file-storage config: default_url_ttl_secs must be > 0 (a zero-second \
                 signed URL TTL would expire before any client could plausibly use it)"
            );
        }
        // `default_url_ttl_secs` is what every mint uses absent a caller
        // override, so it must itself respect the ceiling the control plane
        // is supposed to enforce -- otherwise the very first signed URL
        // minted with no override already violates `max_url_ttl_secs`.
        if self.default_url_ttl_secs > self.max_url_ttl_secs {
            anyhow::bail!(
                "invalid file-storage config: default_url_ttl_secs ({}) must not exceed \
                 max_url_ttl_secs ({})",
                self.default_url_ttl_secs,
                self.max_url_ttl_secs
            );
        }
        // Same reasoning for listing: `default_page_size` is what `GET /files`
        // uses absent a caller-supplied `limit`, so it must not itself exceed
        // the cap the config claims to enforce on every page.
        if self.default_page_size > self.max_page_size {
            anyhow::bail!(
                "invalid file-storage config: default_page_size ({}) must not exceed \
                 max_page_size ({})",
                self.default_page_size,
                self.max_page_size
            );
        }
        // `max_page_size` otherwise has no ceiling of its own -- an operator
        // could configure it arbitrarily large, directly inflating the row
        // count, chunk count, response size, and latency of every listing
        // request. Reject it up front, the same way MAX_FINALIZE_TOKEN_GRACE_SECS
        // bounds finalize_token_grace_secs above.
        if self.max_page_size > MAX_PAGE_SIZE_CEILING {
            anyhow::bail!(
                "invalid file-storage config: max_page_size ({}) must not exceed \
                 MAX_PAGE_SIZE_CEILING ({})",
                self.max_page_size,
                MAX_PAGE_SIZE_CEILING
            );
        }
        // The live-multipart-session guard (retention-cleanup.md §"Live-Multipart-
        // Session Guard") reasons that a long-running upload can legitimately keep
        // its backing pending version alive past `orphan_grace_secs`, for exactly
        // as long as the signed URL driving it remains valid. That reasoning
        // applies verbatim to single-part PUT URLs too, but nothing else enforces
        // it for them (there is no session row to guard on). `default_url_ttl_secs`
        // exceeding `orphan_grace_secs` is a direct self-contradiction -- every
        // upload minted with the default TTL would risk the sweep deleting its
        // still-pending version out from under a still-valid in-flight PUT -- so
        // it is rejected outright.
        if self.default_url_ttl_secs > self.orphan_grace_secs {
            anyhow::bail!(
                "invalid file-storage config: default_url_ttl_secs ({}) must not exceed \
                 orphan_grace_secs ({}) -- otherwise every signed PUT URL minted at the \
                 default TTL risks the orphan-reconciliation sweep deleting its still-pending \
                 version while the URL is still valid",
                self.default_url_ttl_secs,
                self.orphan_grace_secs
            );
        }
        // `orphan_grace_secs` otherwise has no ceiling of its own -- reject it
        // up front, the same way MAX_FINALIZE_TOKEN_GRACE_SECS bounds
        // finalize_token_grace_secs above.
        if self.orphan_grace_secs > MAX_ORPHAN_GRACE_SECS {
            anyhow::bail!(
                "invalid file-storage config: orphan_grace_secs ({}) must not exceed \
                 MAX_ORPHAN_GRACE_SECS ({})",
                self.orphan_grace_secs,
                MAX_ORPHAN_GRACE_SECS
            );
        }
        // `MultipartService` applies `session_ttl_secs.max(1)` as defense-in-
        // depth against a zero TTL reaching `checked_add`; `0` has no
        // documented "disabled" meaning for a session lifetime -- it would
        // just silently mint a 1-second session, breaking every multipart
        // upload. Reject it up front, the same way `default_url_ttl_secs`
        // is rejected above.
        if self.multipart_session_ttl_secs == 0 {
            anyhow::bail!(
                "invalid file-storage config: multipart_session_ttl_secs must be > 0 (a \
                 zero-second session lifetime would expire before any upload could complete)"
            );
        }
        // A multipart session must outlive (or at least match) the per-part
        // signed URLs minted at initiate time -- otherwise the very first
        // batch of upload URLs would carry an `exp` beyond the session's own
        // `expires_at`, and `complete_multipart_upload`'s defense-in-depth
        // expiry check (or a client uploading right up to the URL's `exp`)
        // could reject an upload whose signed URLs were technically still
        // valid. This mirrors the `default_url_ttl_secs`-vs-`orphan_grace_secs`
        // self-contradiction check above.
        if self.multipart_session_ttl_secs < self.default_url_ttl_secs {
            anyhow::bail!(
                "invalid file-storage config: multipart_session_ttl_secs ({}) must be >= \
                 default_url_ttl_secs ({}) -- otherwise a signed upload URL minted at initiate \
                 time could remain valid past the multipart session's own expiry",
                self.multipart_session_ttl_secs,
                self.default_url_ttl_secs
            );
        }
        // `gear.rs` converts `multipart_session_ttl_secs` to `i64` via the
        // same saturating `unwrap_or(i64::MAX)` pattern as
        // `finalize_token_grace_secs`, and
        // `MultipartService::initiate_multipart_upload` then adds it directly
        // to `now` to compute the session's `expires_at`. Without a ceiling
        // here an oversized (or corrupted/malicious) config value would
        // silently become `i64::MAX` seconds and overflow that addition.
        // Reject it up front instead.
        if self.multipart_session_ttl_secs > MAX_MULTIPART_SESSION_TTL_SECS {
            anyhow::bail!(
                "invalid file-storage config: multipart_session_ttl_secs ({}) must not exceed \
                 MAX_MULTIPART_SESSION_TTL_SECS ({})",
                self.multipart_session_ttl_secs,
                MAX_MULTIPART_SESSION_TTL_SECS
            );
        }
        // `MultipartService` applies `complete_lease_secs.max(1)` as defense-
        // in-depth against a zero TTL reaching `checked_add`; `0` has no
        // documented "disabled" meaning for the lease -- it would just
        // silently grant a 1-second lease, letting a second caller take over
        // the lease almost immediately and defeating the crash-recovery
        // purpose it exists for. Reject it up front, the same way the other
        // `*_ttl_secs`/lease knobs above are.
        if self.multipart_complete_lease_secs == 0 {
            anyhow::bail!(
                "invalid file-storage config: multipart_complete_lease_secs must be > 0 (a \
                 zero-second lease would let another caller take it over almost immediately)"
            );
        }
        // `multipart_complete_lease_secs` otherwise has no ceiling of its own
        // -- it bounds how long one `complete` call may hold the `completing`
        // state before another caller can take it over after a crash
        // (backend-assembly time budget), and an oversized value would defeat
        // that purpose by letting a stuck/crashed completer block every other
        // caller far longer than any real assembly could take.
        if self.multipart_complete_lease_secs > MAX_MULTIPART_COMPLETE_LEASE_SECS {
            anyhow::bail!(
                "invalid file-storage config: multipart_complete_lease_secs ({}) must not exceed \
                 MAX_MULTIPART_COMPLETE_LEASE_SECS ({})",
                self.multipart_complete_lease_secs,
                MAX_MULTIPART_COMPLETE_LEASE_SECS
            );
        }
        // `idempotency_ttl_secs` otherwise has no ceiling of its own --
        // `FileService::create_file` adds it directly to `now` to compute the
        // stored idempotency record's `expires_at`.
        if self.idempotency_ttl_secs > MAX_IDEMPOTENCY_TTL_SECS {
            anyhow::bail!(
                "invalid file-storage config: idempotency_ttl_secs ({}) must not exceed \
                 MAX_IDEMPOTENCY_TTL_SECS ({})",
                self.idempotency_ttl_secs,
                MAX_IDEMPOTENCY_TTL_SECS
            );
        }
        // `max_url_ttl_secs` is deliberately NOT hard-bailed on the same
        // condition: the recommended defaults are `max_url_ttl_secs = 7 days`
        // and `orphan_grace_secs = 1 hour`, so a bail here would reject every
        // default deployment. A caller that justifies a longer-lived
        // single-part URL (up to `max_url_ttl_secs`, via a per-call TTL
        // override) only risks the same race if it did not also raise
        // `orphan_grace_secs` accordingly -- surfaced as a warning so an
        // operator notices, rather than a hard failure that would break the
        // defaults.
        if self.max_url_ttl_secs > self.orphan_grace_secs {
            tracing::warn!(
                max_url_ttl_secs = self.max_url_ttl_secs,
                orphan_grace_secs = self.orphan_grace_secs,
                "file-storage: max_url_ttl_secs exceeds orphan_grace_secs -- a single-part PUT \
                 URL justified up to the configured maximum TTL can outlive orphan_grace_secs, \
                 so the orphan-reconciliation sweep may delete its still-pending version while \
                 an in-flight PUT against that URL is still valid. Raise orphan_grace_secs to \
                 at least max_url_ttl_secs to close this window."
            );
        }
        // Each entry must be a validly-formed (base64url, 32-byte) Ed25519
        // public key -- fail gear init on a malformed one rather than
        // surfacing it lazily at the first finalize/report-part callback
        // that happens to try it. The dedupe-against-the-actual-current-key
        // step (harmless duplicates -> warn, not reject) happens in
        // `gear.rs`/`FileService::with_previous_signing_public_keys`, once
        // the current key is actually derived from `signing_key_seed` --
        // unlike the sidecar's `FS_SIDECAR_PUBLIC_KEY`, this config never
        // carries the primary key as a literal value for `validate()` to
        // compare against.
        if !self.previous_signing_public_keys.is_empty() {
            let decoded = self
                .previous_signing_public_keys
                .iter()
                .enumerate()
                .map(|(i, k)| crate::infra::signed_url::decode_public_key_entry(k, i))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    anyhow::anyhow!(
                        "invalid file-storage config: previous_signing_public_keys: {e}"
                    )
                })?;
            crate::infra::signed_url::Verifier::from_public_keys(decoded).map_err(|e| {
                anyhow::anyhow!("invalid file-storage config: previous_signing_public_keys: {e}")
            })?;
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
            // An Ed25519 public key is not secret -- safe to print in full,
            // unlike `signing_key_seed`/`finalize_internal_secret` above.
            .field(
                "previous_signing_public_keys",
                &self.previous_signing_public_keys,
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
            previous_signing_public_keys: Vec::new(),
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

fn default_finalize_token_grace_secs() -> u64 {
    3600 // 1 hour: see FileStorageConfig::finalize_token_grace_secs
}

fn default_multipart_complete_lease_secs() -> u64 {
    120 // backend-assembly budget; see FileStorageConfig::multipart_complete_lease_secs
}

fn default_multipart_session_ttl_secs() -> u64 {
    86400 // 24 hours: a real time budget for a multi-GB upload, independent
    // of the short default_url_ttl_secs used for individual signed URLs.
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
