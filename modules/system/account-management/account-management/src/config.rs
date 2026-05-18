//! Configuration for the Account Management module.
//!
//! Operator-facing knobs consumed by
//! [`crate::domain::tenant::service::TenantService`]. The schema is
//! grouped into sub-sections (matching the YAML layout in
//! `docs/config-example.yaml`) so related knobs travel together and
//! future additions land in the right namespace without renaming.
//!
//! Each section uses `#[serde(default, deny_unknown_fields)]`: any
//! omitted field falls back to its [`Default`] value, and any
//! unknown key surfaces as a loud `init` failure instead of silently
//! ignored configuration.

use serde::Deserialize;

use crate::domain::bootstrap::BootstrapConfig;
use crate::domain::integrity_check::IntegrityCheckConfig;

/// Module configuration for `cyberware-account-management`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountManagementConfig {
    /// Pagination clamps for collection endpoints (currently
    /// `listChildren` only).
    pub listing: ListingConfig,

    /// Tenant-hierarchy depth gating
    /// (DESIGN §3.1 / `algo-depth-threshold-evaluation`).
    pub hierarchy: HierarchyConfig,

    /// Soft-delete + hard-delete pipeline knobs.
    pub retention: RetentionConfig,

    /// Provisioning-row reaper pipeline knobs.
    pub reaper: ReaperConfig,

    /// External `IdP` integration policy.
    pub idp: IdpConfig,

    /// Conversion-request lifecycle knobs (approval TTL, resolved-row
    /// retention window, cleanup tick cadence). Defaults match
    /// `cpt-cf-account-management-adr-conversion-approval` (ADR-0003)
    /// and PRD §5.4.
    pub conversion: ConversionConfig,

    /// Optional platform-bootstrap saga configuration. `None` means no
    /// in-process bootstrap on this platform start (deployment is
    /// expected to bootstrap the root tenant out of band, e.g. CI smoke
    /// tests, multi-region splits, or a unit-test harness). The
    /// surrounding `BootstrapConfig` carries `strict` to control whether
    /// a runtime bootstrap failure is fatal.
    pub bootstrap: Option<BootstrapConfig>,

    /// Periodic hierarchy-integrity check job configuration. Default
    /// is `enabled = true` with a 1-hour cadence; setting `enabled =
    /// false` disables only the in-process loop while leaving the
    /// on-demand `TenantService::check_hierarchy_integrity` SDK
    /// method available to admin tools.
    pub integrity_check: IntegrityCheckConfig,
}

/// Pagination knobs for collection endpoints.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListingConfig {
    /// Hard cap on `$top` for `listChildren` — clamped at the REST
    /// layer before the service call. Matches `OpenAPI`
    /// `Top.maximum` = 200. `validate()` rejects `0` (would empty every
    /// page).
    pub max_top: u32,
}

impl Default for ListingConfig {
    fn default() -> Self {
        Self { max_top: 200 }
    }
}

/// Tenant-hierarchy depth gating.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HierarchyConfig {
    /// Strict-mode reject switch. When `true`, attempts to create a
    /// child tenant at `depth > depth_threshold` are rejected with
    /// `tenant_depth_exceeded`. When `false`, the service emits an
    /// advisory log + metric at the same boundary and proceeds. Both
    /// branches fire at the same threshold per
    /// `algo-depth-threshold-evaluation`.
    pub depth_strict_mode: bool,

    /// Hierarchy depth threshold. Defaults to `10` (DESIGN §3.1 /
    /// PRD). Hard upper bound
    /// [`AccountManagementConfig::MAX_DEPTH_THRESHOLD`] guards the
    /// saga's `parent.depth + 1` arithmetic against silent saturation.
    pub depth_threshold: u32,
}

impl Default for HierarchyConfig {
    fn default() -> Self {
        Self {
            depth_strict_mode: false,
            depth_threshold: 10,
        }
    }
}

/// Retention + hard-delete pipeline knobs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    /// Retention-pipeline tick period in seconds. Must be `> 0`
    /// ([`tokio::time::interval`] panics on zero).
    pub tick_secs: u64,

    /// Default retention window applied at soft-delete time when the
    /// caller does not specify one. `0` disables retention (immediate
    /// hard-delete eligibility).
    pub default_window_secs: u64,

    /// Maximum tenants processed per retention tick. Must be `> 0`
    /// (`LIMIT 0` would scan zero rows forever).
    pub hard_delete_batch_size: usize,

    /// Max parallel hard-delete tasks within one retention tick.
    /// Default `4`. `0` is **rejected by
    /// [`AccountManagementConfig::validate`]** at module init so a
    /// misconfigured deployment fails loud rather than silently
    /// single-flighting; the call site in
    /// `domain::tenant::service::retention` additionally clamps
    /// `.max(1)` as defense-in-depth for tests that bypass `validate`.
    pub hard_delete_concurrency: usize,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            tick_secs: 60,
            // 90 days in seconds.
            default_window_secs: 90 * 86_400,
            hard_delete_batch_size: 64,
            hard_delete_concurrency: 4,
        }
    }
}

/// Provisioning-row reaper pipeline knobs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReaperConfig {
    /// Reaper tick period in seconds. Must be `> 0`
    /// ([`tokio::time::interval`] panics on zero).
    pub tick_secs: u64,

    /// Provisioning-row staleness threshold in seconds — rows older
    /// than this are eligible for reaper compensation. Must be `> 0`
    /// (zero would make every fresh `Provisioning` row instantly
    /// reaper-eligible and trigger premature compensation).
    pub provisioning_timeout_secs: u64,

    /// Maximum provisioning rows processed per reaper tick. Must be
    /// `> 0` (`LIMIT 0` would scan zero rows forever).
    pub batch_size: usize,

    /// Per-tick concurrency for the `IdP` `deprovision_tenant`
    /// classification fan-out. Must be `> 0`. The reaper `IdP` call is
    /// the dominant per-row cost (full provider round-trip,
    /// hundreds of ms typical, multi-second on degraded providers);
    /// fan-out keeps one tick's wall-clock to roughly
    /// `(batch_size / deprovision_concurrency) × IdP_RTT` instead of
    /// `batch_size × IdP_RTT`. The DB-side actions
    /// (`compensate_provisioning_row` / `mark_terminal_provisioning_row` /
    /// `release_claim`) still run sequentially after the classify
    /// fan-out, since they share write paths and serializing them
    /// avoids per-row contention with no meaningful latency cost
    /// (DB writes are 10–100× faster than the `IdP` RTT they
    /// replace).
    pub deprovision_concurrency: usize,
}

impl Default for ReaperConfig {
    fn default() -> Self {
        Self {
            tick_secs: 30,
            provisioning_timeout_secs: 300,
            batch_size: 64,
            deprovision_concurrency: 8,
        }
    }
}

/// Conversion-request lifecycle configuration.
///
/// Owns the three lifecycle windows operators tune for the dual-consent
/// `pending -> {approved, cancelled, rejected, expired}` flow:
/// `approval_ttl_secs`, `resolved_retention_secs`, and the background
/// `cleanup_interval_secs`. Defaults and bounds are pinned in
/// `cpt-cf-account-management-adr-conversion-approval` (ADR-0003) and
/// PRD §5.4 — see [`ConversionConfig::validate`] for the per-field
/// bounds enforced by `AccountManagementModule::init`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConversionConfig {
    /// Approval TTL applied at `request_conversion` time as
    /// `expires_at = now() + approval_ttl`. Default `259_200` (72h)
    /// per ADR-0003. Range `[3600, 2_592_000]` (1h to 30d). Below the
    /// floor the approver-response window becomes unusable; above the
    /// ceiling a pending request can outlive any reasonable resolved-
    /// retention setting and bloat the partial unique index.
    pub approval_ttl_secs: u64,

    /// Resolved-row retention window applied by the soft-delete
    /// reaper as `cutoff = now() - resolved_retention`. Default
    /// `2_592_000` (30d) per ADR-0003. Range `[86_400, 31_536_000]`
    /// (1d to 365d). Below the floor history disappears faster than
    /// typical audit reads; above the ceiling the table grows
    /// unbounded without operator intent. Cross-validated against the
    /// tenant retention window — see [`ConversionConfig::validate`].
    pub resolved_retention_secs: u64,

    /// Cleanup tick cadence for the dedicated conversion reaper that
    /// drives `expire_pending` and `soft_delete_resolved`. Default
    /// `60` (60s) per ADR-0003. Range `[10, 600]` (10s to 10m).
    /// Mirrors the tenant retention pipeline's tick bounds. Distinct
    /// from `retention.tick_secs` so an operator can dial tenant
    /// hard-delete down without delaying conversion expiry alongside.
    pub cleanup_interval_secs: u64,

    /// Per-tick bound on rows the conversion expiry reaper scans.
    /// `LIMIT 0` would scan zero rows forever and is rejected.
    /// Upper bound `MAX_BATCH_SIZE` keeps the SQL `IN(...)` clause
    /// for the candidate-id list well below Postgres's 65 535
    /// prepared-parameter ceiling — see `MAX_BATCH_SIZE` comment.
    pub expire_batch_size: u32,

    /// Per-tick bound on rows the conversion retention sweep
    /// soft-deletes. Same `LIMIT 0` and `MAX_BATCH_SIZE` rejections
    /// as `expire_batch_size`. The retention sweep loads candidate
    /// ids into an `IN(...)` UPDATE filter — staying under the PG
    /// parameter ceiling is the load-bearing reason for the cap.
    pub retention_batch_size: u32,
}

impl Default for ConversionConfig {
    fn default() -> Self {
        // 72h = 259_200s, 30d = 2_592_000s, 60s tick — pinned in
        // ADR-0003 §1 (Decision: Configurable lifecycle windows).
        Self {
            approval_ttl_secs: 72 * 60 * 60,
            resolved_retention_secs: 30 * 24 * 60 * 60,
            cleanup_interval_secs: 60,
            expire_batch_size: 256,
            retention_batch_size: 256,
        }
    }
}

impl ConversionConfig {
    /// Lower / upper bounds pinned in DESIGN §3.2 (`ConversionService`
    /// configuration bounds).
    pub(crate) const MIN_APPROVAL_TTL_SECS: u64 = 60 * 60; // 1h
    pub(crate) const MAX_APPROVAL_TTL_SECS: u64 = 30 * 24 * 60 * 60; // 30d
    pub(crate) const MIN_RESOLVED_RETENTION_SECS: u64 = 24 * 60 * 60; // 1d
    pub(crate) const MAX_RESOLVED_RETENTION_SECS: u64 = 365 * 24 * 60 * 60; // 365d
    pub(crate) const MIN_CLEANUP_INTERVAL_SECS: u64 = 10;
    pub(crate) const MAX_CLEANUP_INTERVAL_SECS: u64 = 10 * 60; // 10m
    /// Upper bound on per-tick batch sizes (`expire_batch_size` /
    /// `retention_batch_size`). The retention sweep lowers candidate
    /// ids into an `IN(...)` `UPDATE` filter — Postgres caps prepared
    /// statements at 65 535 parameters, so the cap stays well below
    /// that ceiling with headroom for other parameters in the
    /// statement (`status` / `deleted_at` / scope clauses). The
    /// production default of 256 is more than sufficient for typical
    /// pending / resolved-row volumes; operators tuning higher can
    /// raise it up to this ceiling.
    pub(crate) const MAX_BATCH_SIZE: u32 = 4_096;

    /// Validate per-field bounds. Cross-field constraints
    /// (`resolved_retention <= retention.default_window_secs` so
    /// resolved-conversion history cannot outlive the tenant row
    /// it cascades from) are evaluated by
    /// [`AccountManagementConfig::validate`] which has both sub-
    /// sections in scope.
    ///
    /// # Errors
    ///
    /// Returns a human-readable string naming each invalid field.
    pub fn validate(&self) -> Result<(), String> {
        let mut bad: Vec<String> = Vec::new();
        if !(Self::MIN_APPROVAL_TTL_SECS..=Self::MAX_APPROVAL_TTL_SECS)
            .contains(&self.approval_ttl_secs)
        {
            bad.push(format!(
                "conversion.approval_ttl_secs (must be in [{}, {}]; got {})",
                Self::MIN_APPROVAL_TTL_SECS,
                Self::MAX_APPROVAL_TTL_SECS,
                self.approval_ttl_secs,
            ));
        }
        if !(Self::MIN_RESOLVED_RETENTION_SECS..=Self::MAX_RESOLVED_RETENTION_SECS)
            .contains(&self.resolved_retention_secs)
        {
            bad.push(format!(
                "conversion.resolved_retention_secs (must be in [{}, {}]; got {})",
                Self::MIN_RESOLVED_RETENTION_SECS,
                Self::MAX_RESOLVED_RETENTION_SECS,
                self.resolved_retention_secs,
            ));
        }
        if !(Self::MIN_CLEANUP_INTERVAL_SECS..=Self::MAX_CLEANUP_INTERVAL_SECS)
            .contains(&self.cleanup_interval_secs)
        {
            bad.push(format!(
                "conversion.cleanup_interval_secs (must be in [{}, {}]; got {})",
                Self::MIN_CLEANUP_INTERVAL_SECS,
                Self::MAX_CLEANUP_INTERVAL_SECS,
                self.cleanup_interval_secs,
            ));
        }
        if self.expire_batch_size == 0 || self.expire_batch_size > Self::MAX_BATCH_SIZE {
            bad.push(format!(
                "conversion.expire_batch_size (must be in [1, {}]; got {}; zero would scan no \
                 rows forever and values above the cap risk PG IN(...) prepared-parameter \
                 ceiling)",
                Self::MAX_BATCH_SIZE,
                self.expire_batch_size,
            ));
        }
        if self.retention_batch_size == 0 || self.retention_batch_size > Self::MAX_BATCH_SIZE {
            bad.push(format!(
                "conversion.retention_batch_size (must be in [1, {}]; got {}; zero would scan \
                 no rows forever and values above the cap risk PG IN(...) prepared-parameter \
                 ceiling)",
                Self::MAX_BATCH_SIZE,
                self.retention_batch_size,
            ));
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(bad.join(", "))
        }
    }
}

/// External `IdP` integration policy.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdpConfig {
    /// When `true`, module init fails closed if no
    /// `IdpPluginClient` is registered in `ClientHub`.
    /// When `false` (default), AM falls back to the no-op
    /// `NoopIdpProvider`, in which case `create_tenant` returns
    /// [`crate::domain::error::DomainError::UnsupportedOperation`] at
    /// runtime if the saga reaches the `IdP` step. Production
    /// deployments that need `IdP` integration MUST set this to `true`
    /// so the missing-plugin condition surfaces as a clean init
    /// failure instead of a runtime error on every create. The
    /// default is `false` so dev / test deployments without an `IdP`
    /// plugin keep booting without changing existing config.
    pub required: bool,
}

impl AccountManagementConfig {
    /// Upper bound on `hierarchy.depth_threshold` so that the
    /// `algo-depth-threshold-evaluation` `parent.depth + 1` arithmetic
    /// in `create_tenant` cannot land on `u32::MAX` and either silently
    /// saturate (via `saturating_add`) or overflow if the
    /// implementation ever switches to checked arithmetic. The 1 M cap
    /// is far past any realistic hierarchy (the design default is 10).
    pub(crate) const MAX_DEPTH_THRESHOLD: u32 = 1_000_000;

    /// Reject configurations that would panic the lifecycle tasks or
    /// produce undefined runtime behavior. Called by the module's
    /// `init` lifecycle hook before `serve` spawns the retention +
    /// reaper background tasks.
    ///
    /// Hard panic gates:
    ///
    /// * `retention.tick_secs == 0` / `reaper.tick_secs == 0` —
    ///   [`tokio::time::interval`] panics on zero.
    /// * `reaper.provisioning_timeout_secs == 0` — would make every
    ///   fresh `Provisioning` row instantly reaper-eligible.
    /// * `retention.hard_delete_batch_size == 0` /
    ///   `reaper.batch_size == 0` — the SQL `LIMIT` clamp evaluates to
    ///   zero and the pipeline ticks scan zero rows forever.
    /// * `retention.hard_delete_concurrency == 0` — would degrade to
    ///   single-flight processing of every batch with no observable
    ///   error. Although the retention call site clamps with
    ///   `.max(1)`, the misconfig is still rejected here so it
    ///   surfaces as an `init` failure instead of a silent rewrite.
    /// * `listing.max_top == 0` — every `listChildren` call returns
    ///   an empty page regardless of the requested `$top`.
    /// * `hierarchy.depth_threshold > MAX_DEPTH_THRESHOLD` — guards
    ///   the saga's `parent.depth + 1` arithmetic against silent
    ///   saturation.
    ///
    /// # Errors
    ///
    /// Returns a human-readable string naming each invalid field.
    /// Callers map this into [`crate::domain::error::DomainError::Internal`]
    /// (a fatal `init` failure).
    pub fn validate(&self) -> Result<(), String> {
        let mut bad: Vec<&'static str> = Vec::new();
        if self.retention.tick_secs == 0 {
            bad.push("retention.tick_secs (must be > 0; tokio::time::interval panics on zero)");
        }
        if self.reaper.tick_secs == 0 {
            bad.push("reaper.tick_secs (must be > 0; tokio::time::interval panics on zero)");
        }
        if self.reaper.provisioning_timeout_secs == 0 {
            bad.push(
                "reaper.provisioning_timeout_secs (must be > 0; zero would make every fresh provisioning row instantly reaper-eligible and trigger premature compensation)",
            );
        }
        if self.retention.hard_delete_batch_size == 0 {
            bad.push(
                "retention.hard_delete_batch_size (must be > 0; zero would scan no rows forever)",
            );
        }
        if self.reaper.batch_size == 0 {
            bad.push("reaper.batch_size (must be > 0; zero would scan no rows forever)");
        }
        if self.retention.hard_delete_concurrency == 0 {
            bad.push("retention.hard_delete_concurrency (must be > 0; zero is normalised to 1 at the call site but rejected here so the misconfig is observable)");
        }
        if self.reaper.deprovision_concurrency == 0 {
            bad.push("reaper.deprovision_concurrency (must be > 0; zero is normalised to 1 at the call site but rejected here so the misconfig is observable)");
        }
        if self.listing.max_top == 0 {
            bad.push("listing.max_top (must be > 0; zero would empty every listChildren response)");
        }
        if self.hierarchy.depth_threshold > Self::MAX_DEPTH_THRESHOLD {
            bad.push(
                "hierarchy.depth_threshold (must be <= MAX_DEPTH_THRESHOLD; protects saga depth arithmetic)",
            );
        }
        // Conversion sub-section: validate eagerly so a bad
        // approval_ttl / resolved_retention / cleanup_interval / batch
        // surfaces at init time. Cross-field guard
        // `resolved_retention <= retention.default_window_secs` keeps
        // resolved-conversion history from outliving the tenant
        // hard-delete cascade — see DESIGN §3.2 cross-validation
        // requirement and ADR-0003 §1.
        //
        // Edge case: `retention.default_window_secs == 0` means
        // "immediate hard-delete eligibility" for tenants. The
        // FK `conversion_requests.tenant_id REFERENCES tenants(id)
        // ON DELETE CASCADE` already wipes resolved-request rows
        // the moment the tenant is hard-deleted, so a shorter tenant
        // window does NOT let conversion history outlive its tenant.
        // The cross-check therefore only matters when tenant retention
        // is actually enabled (`> 0`); skipping it on `0` keeps
        // deployments with disabled tenant retention from being unable
        // to satisfy the `MIN_RESOLVED_RETENTION_SECS` floor (1 day).
        let conversion_err = self.conversion.validate().err();
        if self.retention.default_window_secs > 0
            && self.conversion.resolved_retention_secs > self.retention.default_window_secs
        {
            bad.push(
                "conversion.resolved_retention_secs (must be <= retention.default_window_secs \
                 when tenant retention is enabled; conversion_requests.tenant_id is ON DELETE \
                 CASCADE so resolved-request history is reclaimed alongside the tenant)",
            );
        }
        // Integrity-check sub-section: validate eagerly so a bad
        // interval / jitter / initial_delay surfaces here rather than
        // panicking inside the spawned loop on `tokio::time::sleep`.
        let integrity_err = self.integrity_check.validate().err();
        // Bootstrap sub-section is NOT validated here. The bootstrap
        // saga's `BootstrapConfig::strict` field is the
        // operator-facing knob that selects whether a malformed
        // `[bootstrap]` block is init-fatal or warn-and-skip:
        // `AccountManagementModule::init` runs `boot_cfg.validate()`
        // explicitly and routes the result via `strict`. Folding
        // bootstrap validation into the global config check would
        // make `strict = false` (best-effort posture for dev / CI /
        // multi-region splits where the root tenant is bootstrapped
        // out of band) unreachable — a malformed block would abort
        // init before the strict-vs-non-strict branch in `init`
        // could see the error. See `module.rs::Module::init`.
        if bad.is_empty() && conversion_err.is_none() && integrity_err.is_none() {
            Ok(())
        } else {
            let mut parts: Vec<String> = bad.into_iter().map(str::to_owned).collect();
            if let Some(err) = conversion_err {
                parts.push(err);
            }
            if let Some(err) = integrity_err {
                parts.push(err);
            }
            Err(format!(
                "account-management configuration is invalid: {}",
                parts.join(", ")
            ))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod tests;
