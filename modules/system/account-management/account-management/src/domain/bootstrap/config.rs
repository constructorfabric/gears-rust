//! Configuration for the [`crate::domain::bootstrap::service::BootstrapService`].
//!
//! The platform-bootstrap FEATURE (see
//! `modules/system/account-management/docs/features/feature-platform-bootstrap.md`)
//! requires the operator to declare the root-tenant identity AND the
//! IdP-wait backoff envelope at deployment time. Defaults match
//! FEATURE §3 `algo-platform-bootstrap-idp-wait-with-backoff`:
//! `idp_retry_backoff_initial = 2s`, `idp_retry_backoff_max = 30s`,
//! `idp_retry_timeout = 5min`. The envelope bounds the saga retry
//! loop on `IdpUnavailable` raised during `provision_tenant`. The
//! bootstrap saga itself is gated by
//! [`BootstrapConfig::strict`] — `true` makes a bootstrap failure
//! lifecycle-fatal during module `init`, while `false` logs the
//! failure and lets the module proceed (useful for dev or multi-region
//! splits where the root tenant is bootstrapped out of band).
//!
//! `BootstrapConfig` is deliberately separate from
//! [`crate::config::AccountManagementConfig`] so deployments that bootstrap
//! externally (multi-region splash-page / CI smoke tests / unit tests)
//! can leave the slot `None` without polluting the rest of the module
//! configuration with optional fields.

use std::time::Duration;

use modkit_macros::domain_model;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

/// Operational upper bound on `idp_wait_timeout` (1 hour).
///
/// Caps `idp_wait_timeout` so that:
///
/// 1. `Instant::now() + idp_wait_timeout` cannot overflow the
///    platform's `Instant` representation (any value past the
///    `Instant::checked_add` ceiling is operationally meaningless --
///    the bootstrap saga is a sub-second-to-minutes operation, not a
///    multi-day wait), and
/// 2. `i64::try_from(secs * 2)` for the FEATURE-§3 stuck-threshold
///    (`2 × idp_wait_timeout.as_secs()`) cannot saturate to `i64::MAX`,
///    which would silently disable the stuck-row branch in
///    `step_loop_classify` for the entire platform.
///
/// Tightened from the previous 24h ceiling because no operationally
/// meaningful bootstrap path waits longer than minutes — a wait
/// pushing past an hour means something else is wrong (`IdP` down,
/// types-registry stalled, network partition) and the platform
/// should fail loud rather than spin silently. 1h still keeps the
/// `secs * 2 -> i64` cast trivially in range.
pub const MAX_IDP_WAIT_TIMEOUT: Duration = Duration::from_hours(1);

/// Bootstrap-feature configuration.
///
/// Duration fields carry their unit in the field name: `_secs` is seconds.
/// UUIDs are deployment-stable — changing `root_id` between platform
/// restarts breaks the `fr-bootstrap-idempotency` contract.
#[domain_model]
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BootstrapConfig {
    /// Deterministic UUID for the platform root tenant. The bootstrap
    /// service reads this id back via
    /// [`crate::domain::tenant::repo::TenantRepo::find_by_id`] on every
    /// platform start to classify the bootstrap state — picking a
    /// fresh UUID per restart silently breaks idempotency.
    pub root_id: Uuid,

    /// Human-readable display name for the root tenant. Forwarded into
    /// the `tenants.name` column verbatim.
    pub root_name: String,

    /// Chained GTS tenant-type identifier (e.g.
    /// `gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~`) forwarded
    /// to the `IdP` plugin in
    /// [`account_management_sdk::IdpProvisionTenantRequest::tenant_type`].
    /// `serde::Deserialize` lifts the configured string into the typed
    /// wrapper at config-load time so downstream consumers do not
    /// re-parse on every saga step. The `tenants.tenant_type_uuid`
    /// foreign-key value is derived from this GTS id at saga time
    /// via the same V5-UUID algorithm `create_tenant` uses, so
    /// operators only configure the canonical type identifier.
    pub root_tenant_type: gts::GtsSchemaId,

    /// Opaque deployment-supplied metadata forwarded to the `IdP` plugin
    /// without interpretation. AM does **not** validate the shape of
    /// this blob — that contract is owned by the `IdP` plugin.
    pub root_tenant_metadata: Option<Value>,

    /// Total time the bootstrap saga is allowed to spend waiting for
    /// `IdP` availability (FEATURE §3 `idp_retry_timeout`, default 300s).
    /// Used as the deadline for the saga retry loop on
    /// `IdpUnavailable` raised during step 2 (`provision_tenant`).
    ///
    /// Bounded by [`MAX_IDP_WAIT_TIMEOUT`] in
    /// [`BootstrapConfig::validate`] so neither
    /// `Instant::now() + idp_wait_timeout` nor
    /// `i64::try_from(idp_wait_timeout.as_secs() * 2)` (used for the
    /// FEATURE-§3 stuck threshold) can overflow on a misconfiguration.
    ///
    /// Wire shape is a humantime-style duration string (e.g.
    /// `"5m"`, `"300s"`, `"1h"`). The strongly typed in-memory
    /// `Duration` replaces the prior `u64` seconds field so call-
    /// sites do not need to wrap with `Duration::from_secs(...)`.
    #[serde(with = "modkit_utils::humantime_serde")]
    pub idp_wait_timeout: Duration,

    /// Initial sleep between `IdP`-availability retries (FEATURE §3
    /// `idp_retry_backoff_initial`, default 2s). Wire shape is a
    /// humantime-style duration string (e.g. `"2s"`).
    #[serde(with = "modkit_utils::humantime_serde")]
    pub idp_retry_backoff_initial: Duration,

    /// Cap on the doubled backoff (FEATURE §3 `idp_retry_backoff_max`,
    /// default 30s). Wire shape is a humantime-style duration string
    /// (e.g. `"30s"`).
    #[serde(with = "modkit_utils::humantime_serde")]
    pub idp_retry_backoff_max: Duration,

    /// Strict-mode flag. When `true`, a bootstrap failure aborts module
    /// `init` (lifecycle-fatal). When `false`, the failure is logged
    /// and the module proceeds — useful for dev / multi-region splits
    /// where the root tenant is bootstrapped out of band.
    pub strict: bool,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            // Deterministic placeholder root id; deployments **MUST**
            // override this with their canonical platform-root UUID.
            // The default exists only so `serde(default)` round-trips
            // an empty TOML table without panicking — the production
            // wiring path requires an explicit value.
            root_id: Uuid::nil(),
            root_name: "platform-root".to_owned(),
            root_tenant_type: gts::GtsSchemaId::new(""),
            root_tenant_metadata: None,
            idp_wait_timeout: Duration::from_mins(5),
            idp_retry_backoff_initial: Duration::from_secs(2),
            idp_retry_backoff_max: Duration::from_secs(30),
            strict: false,
        }
    }
}

impl BootstrapConfig {
    /// Reject deployments whose required identifiers were never set.
    ///
    /// `serde(default)` lets the operator omit any field, so an empty
    /// `[bootstrap]` TOML table deserialises to a config with
    /// `root_id = Uuid::nil()` and `root_tenant_type = ""`. With
    /// `strict = true` the saga would then insert a nil-id root,
    /// breaking the `fr-bootstrap-idempotency` contract on the next
    /// platform start (see `feature-platform-bootstrap.md` lines
    /// 23-25 — UUIDs are "deployment-stable; changing it between
    /// platform restarts breaks the `fr-bootstrap-idempotency`
    /// contract"). This validator is invoked by the module-level
    /// wiring before constructing `BootstrapService` so the failure
    /// surfaces during `init` rather than at the first DB write.
    ///
    /// # Errors
    ///
    /// Returns a human-readable string naming each missing /
    /// nil-valued field. Callers map this into
    /// [`crate::domain::error::DomainError::Internal`] (strict-mode
    /// init failure).
    pub fn validate(&self) -> Result<(), String> {
        let mut missing: Vec<&'static str> = Vec::new();
        if self.root_id.is_nil() {
            missing.push("root_id");
        }
        if self.root_tenant_type.as_ref().trim().is_empty() {
            missing.push("root_tenant_type");
        }
        if self.root_name.trim().is_empty() {
            missing.push("root_name");
        }
        if self.idp_wait_timeout.is_zero() {
            missing.push("idp_wait_timeout (must be > 0)");
        }
        // Cap at `MAX_IDP_WAIT_TIMEOUT` so the deadline math
        // (`Instant::now() + idp_wait_timeout` in
        // `BootstrapService::run`) and the stuck-threshold cast
        // (`i64::try_from(idp_wait_timeout.as_secs() * 2)`) are both
        // safe by construction. See `MAX_IDP_WAIT_TIMEOUT` for rationale.
        if self.idp_wait_timeout > MAX_IDP_WAIT_TIMEOUT {
            missing.push("idp_wait_timeout (must be <= 1h)");
        }
        if self.idp_retry_backoff_initial.is_zero() {
            missing.push("idp_retry_backoff_initial (must be > 0)");
        }
        if self.idp_retry_backoff_max < self.idp_retry_backoff_initial {
            missing.push("idp_retry_backoff_max (must be >= initial)");
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "bootstrap configuration is missing or invalid: {}",
                missing.join(", ")
            ))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
