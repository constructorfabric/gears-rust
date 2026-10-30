//! Explicit operator configuration. Unknown settings and unsafe lock routes fail boot.
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use toolkit_macros::ExpandVars;

/// Deployment attestation for the toolkit's session advisory-lock connection.
/// Actual session behavior is proven by S1-03, not by this enum.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockRoute {
    Direct,
    SessionPool,
}

/// Required configuration; no implicit enablement/default configuration on the host.
/// Loaded with `config_expanded`, so `${VAR}` placeholders resolve only in fields marked
/// `#[expand_vars]` (the audit minimization key).
#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct OrdersConfig {
    pub lock_route: LockRoute,
    pub idempotency_lease_seconds: u32,
    pub dependency_timeout_ms: u32,
    /// Bounded internal maintenance authority (08 §3.5). Absent means every worker task fails
    /// closed; it is never derived from a caller, PDP fallback or an order's parties.
    #[serde(default)]
    #[expand_vars]
    pub maintenance: Option<MaintenanceConfig>,
    /// Configured Workflow/Subscriptions/Billing principals (D-115). Required: the audit actor
    /// class is derived only from these and the maintenance actor, never defaulted.
    pub service_principals: Vec<ServicePrincipalConfig>,
    /// The Event Broker managed producer (D-200). Required: Orders has no event-less mode.
    pub events: EventsConfig,
    /// Keyed minimization of administrative free text in audit (D-204). Required: there is no
    /// unkeyed fallback.
    #[expand_vars]
    pub audit_minimization: AuditMinimizationConfig,
    /// Draft capture bounds (DESIGN 02 §3.7: static per-gear configuration, not tenant-scoped).
    /// Absent means the declared 200-line baseline.
    #[serde(default)]
    pub capture: CaptureConfig,
    /// The date-policy rows this deployment promotes (02 §4.2, DESIGN 02 §3.7, D-121): the policy
    /// channel. Absent means no promotion: the stored rows stand, and startup still requires a
    /// valid platform default. No Orders endpoint writes these rows.
    #[serde(default)]
    pub date_policy: Option<DatePolicyConfig>,
    /// The gear-local per-(caller, order) edge limiter (Foundation §3.7, D-185, Q-26 fallback).
    /// Absent means the design baseline. The per-caller limit is gateway configuration (the
    /// `rl_orders_caller_write` / `rl_orders_workflow_write` zones), not an Orders setting.
    #[serde(default)]
    pub throttling: ThrottlingConfig,
    /// Gate port budgets and the per-seller commercial identities (S3-09). Absent means the
    /// F03 §3.3 baselines and no configured identity.
    #[serde(default)]
    #[expand_vars]
    pub gate: GateConfig,
}

/// The accepted gate deadline, in milliseconds; the bound of `dependency_timeout_ms` (D-240,
/// unratified like D-227 item 5).
const GATE_DEADLINE_MS_RANGE: std::ops::RangeInclusive<u32> = 1..=60_000;

/// Gate port budgets (F03 §3.3 baselines, D-227 item 5; unratified). Missing = baseline.
#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct GateConfig {
    /// Per-port deadline overrides in milliseconds, keyed by `GatePort` token
    /// (`current-revision`, `resolve`, `sku-read`, …).
    #[serde(default)]
    pub deadline_ms: std::collections::BTreeMap<crate::domain::gate::ports::GatePort, u32>,
    /// The deadline of the one `AuthN` client-credentials exchange per catalog read, in
    /// milliseconds (D-194). Absent = 250, the gate port baseline (F03 §3.3; unratified, D-227
    /// item 5).
    #[serde(default)]
    pub authority_exchange_deadline_ms: Option<u32>,
    /// The deadline of one Pricing acceptance command (`SellabilityV1` `check`, submit phase B,
    /// M2-6), in milliseconds: one per line command, not a gate port (`GatePort::ALL` is
    /// unchanged). Absent = 500.
    #[serde(default)]
    pub acceptance_deadline_ms: Option<u32>,
    /// Per-seller commercial identities for the D-194 service principal (Task 8; Q13).
    #[serde(default)]
    #[expand_vars]
    pub commercial_identities: Vec<CommercialIdentityConfig>,
    /// Bind development stubs of the undelivered gate providers (Task 8b). Requires the
    /// `gate-dev-stubs` cargo feature and `dev_stub_fixture`; never set in production.
    #[serde(default)]
    pub dev_stubs: bool,
    /// The JSON fixture the development stubs answer from; read at init when `dev_stubs` is set.
    #[serde(default)]
    pub dev_stub_fixture: Option<std::path::PathBuf>,
    /// Preview requests per minute per authenticated subject (F03 §4.6 "a rate limit", D-52):
    /// the gear-local [`PerCallerLimiter`](crate::infra::throttle::PerCallerLimiter) in the
    /// Preview service. `1..=600`; absent = 60.
    #[serde(default = "gate_baseline::preview_per_caller_per_minute")]
    pub preview_per_caller_per_minute: u32,
    /// Submit requests per minute per authenticated subject (M2-5). `1..=600`; absent = 10.
    #[serde(default = "gate_baseline::submit_per_caller_per_minute")]
    pub submit_per_caller_per_minute: u32,
    /// The per-port circuit breaker (F03 §3.3, D-233): one per port, shared by every run in
    /// the gear. Absent = enabled at the baselines below.
    #[serde(default)]
    pub breaker: BreakerConfig,
}

/// The gear-local per-port circuit breaker (F03 §3.3 "0.5 failures over 30 s, open 10 s";
/// D-233, unratified). A deadline or a transient failure counts as a failure, a success or a
/// permanent answer as a success; an open port answers `Unavailable::Outage` without a call.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreakerConfig {
    /// Whether the breaker judges the ports. Absent = true; false admits every call.
    #[serde(default = "gate_baseline::breaker_enabled")]
    pub enabled: bool,
    /// Failures / outcomes in the window at or above which a closed breaker opens; `(0, 1]`, and at least one basis point (0.0001) once rounded.
    /// Absent = 0.5.
    #[serde(default = "gate_baseline::breaker_failure_rate")]
    pub failure_rate: f32,
    /// The sliding window, in seconds; at least 1. Absent = 30.
    #[serde(default = "gate_baseline::breaker_window_seconds")]
    pub window_seconds: u32,
    /// How long an open breaker rejects before one probe call, in seconds; at least 1.
    /// Absent = 10.
    #[serde(default = "gate_baseline::breaker_open_seconds")]
    pub open_seconds: u32,
    /// Outcomes the window must hold before the rate is judged; at least 1. Absent = 10
    /// (unratified, D-233).
    #[serde(default = "gate_baseline::breaker_min_calls")]
    pub min_calls: u32,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            enabled: gate_baseline::breaker_enabled(),
            failure_rate: gate_baseline::breaker_failure_rate(),
            window_seconds: gate_baseline::breaker_window_seconds(),
            open_seconds: gate_baseline::breaker_open_seconds(),
            min_calls: gate_baseline::breaker_min_calls(),
        }
    }
}

impl BreakerConfig {
    /// The breaker policy these settings describe; [`GateConfig::validate`] checks it.
    #[must_use]
    pub fn policy(&self) -> crate::domain::gate::breaker::BreakerPolicy {
        crate::domain::gate::breaker::BreakerPolicy {
            failure_rate: self.failure_rate,
            window: std::time::Duration::from_secs(u64::from(self.window_seconds)),
            open_for: std::time::Duration::from_secs(u64::from(self.open_seconds)),
            min_calls: self.min_calls,
        }
    }
}

mod gate_baseline {
    use crate::domain::gate::breaker::BreakerPolicy;

    pub const fn preview_per_caller_per_minute() -> u32 {
        60
    }
    pub const fn submit_per_caller_per_minute() -> u32 {
        10
    }
    pub const fn breaker_enabled() -> bool {
        true
    }
    pub const fn breaker_failure_rate() -> f32 {
        BreakerPolicy::BASELINE.failure_rate
    }
    pub fn breaker_window_seconds() -> u32 {
        u32::try_from(BreakerPolicy::BASELINE.window.as_secs()).unwrap_or(u32::MAX)
    }
    pub fn breaker_open_seconds() -> u32 {
        u32::try_from(BreakerPolicy::BASELINE.open_for.as_secs()).unwrap_or(u32::MAX)
    }
    pub const fn breaker_min_calls() -> u32 {
        BreakerPolicy::BASELINE.min_calls
    }
}

/// The accepted range of a gate per-caller limit.
const PER_CALLER_PER_MINUTE_RANGE: std::ops::RangeInclusive<u32> = 1..=600;

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            deadline_ms: std::collections::BTreeMap::new(),
            authority_exchange_deadline_ms: None,
            acceptance_deadline_ms: None,
            commercial_identities: Vec::new(),
            dev_stubs: false,
            dev_stub_fixture: None,
            preview_per_caller_per_minute: gate_baseline::preview_per_caller_per_minute(),
            submit_per_caller_per_minute: gate_baseline::submit_per_caller_per_minute(),
            breaker: BreakerConfig::default(),
        }
    }
}

/// One seller's commercial identity: client credentials the gate presents as the D-194 service
/// principal. `client_secret` is a secret reference (`"${VAR}"`) resolved at load time; `Debug`
/// redacts it.
#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct CommercialIdentityConfig {
    /// The seller tenant this identity acts for; unique across the list.
    pub seller_tenant_id: uuid::Uuid,
    /// The client identifier.
    pub client_id: String,
    /// The client secret reference.
    #[expand_vars]
    pub client_secret: SecretString,
    /// Requested scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl GateConfig {
    /// Reject a deadline (port, authority exchange or acceptance) outside `1..=60000` ms, a
    /// duplicated seller identity, an empty or unresolved (`${`) `client_secret`, a blank
    /// `client_id` or scope, a per-caller limit outside `1..=600`, an out-of-range `gate.breaker`
    /// value, and
    /// `dev_stubs` without the `gate-dev-stubs` feature or without a fixture path.
    ///
    /// # Errors
    /// A sanitized operator error naming the offending key or tenant (never a secret).
    pub fn validate(&self) -> anyhow::Result<()> {
        for (port, ms) in &self.deadline_ms {
            anyhow::ensure!(
                GATE_DEADLINE_MS_RANGE.contains(ms),
                "bss-orders: gate.deadline_ms.{} must be 1..=60000",
                port.token()
            );
        }
        for (key, value) in [
            (
                "authority_exchange_deadline_ms",
                self.authority_exchange_deadline_ms,
            ),
            ("acceptance_deadline_ms", self.acceptance_deadline_ms),
        ] {
            anyhow::ensure!(
                value.is_none_or(|ms| GATE_DEADLINE_MS_RANGE.contains(&ms)),
                "bss-orders: gate.{key} must be 1..=60000"
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for identity in &self.commercial_identities {
            anyhow::ensure!(
                seen.insert(identity.seller_tenant_id),
                "bss-orders: gate.commercial_identities: duplicate seller_tenant_id {}",
                identity.seller_tenant_id
            );
            let secret = identity.client_secret.expose_secret();
            anyhow::ensure!(
                !secret.is_empty() && !secret.contains("${"),
                "bss-orders: gate.commercial_identities[{}]: client_secret is empty or an unresolved secret reference",
                identity.seller_tenant_id
            );
            anyhow::ensure!(
                !identity.client_id.trim().is_empty(),
                "bss-orders: gate.commercial_identities[{}]: client_id must not be blank",
                identity.seller_tenant_id
            );
            anyhow::ensure!(
                identity.scopes.iter().all(|s| !s.trim().is_empty()),
                "bss-orders: gate.commercial_identities[{}]: a scope is blank",
                identity.seller_tenant_id
            );
        }
        for (key, value) in [
            (
                "preview_per_caller_per_minute",
                self.preview_per_caller_per_minute,
            ),
            (
                "submit_per_caller_per_minute",
                self.submit_per_caller_per_minute,
            ),
        ] {
            anyhow::ensure!(
                PER_CALLER_PER_MINUTE_RANGE.contains(&value),
                "bss-orders: gate.{key} must be 1..=600"
            );
        }
        // The domain error names the key (`gate.breaker.failure_rate`, …).
        self.breaker
            .policy()
            .validate()
            .map_err(|e| anyhow::anyhow!("bss-orders: {e}"))?;
        #[cfg(not(feature = "gate-dev-stubs"))]
        anyhow::ensure!(
            !self.dev_stubs,
            "bss-orders: gate.dev_stubs requires the gate-dev-stubs feature"
        );
        #[cfg(feature = "gate-dev-stubs")]
        anyhow::ensure!(
            !self.dev_stubs || self.dev_stub_fixture.is_some(),
            "bss-orders: gate.dev_stubs requires gate.dev_stub_fixture"
        );
        Ok(())
    }

    /// The one development-delivery value: the fixture the stubs answer from, present only when
    /// the `gate-dev-stubs` feature, `dev_stubs = true` and `dev_stub_fixture` all hold (the
    /// documented double opt-in). `validate` is the only place that refuses the partial states.
    #[must_use]
    pub fn dev_delivery_fixture(&self) -> Option<&std::path::Path> {
        (cfg!(feature = "gate-dev-stubs") && self.dev_stubs)
            .then_some(self.dev_stub_fixture.as_deref())
            .flatten()
    }

    /// The Preview limiter's settings (validated by [`Self::validate`]).
    ///
    /// # Errors
    /// A limit outside `1..=600`.
    pub fn preview_limit(&self) -> anyhow::Result<crate::infra::throttle::CallerLimitSettings> {
        caller_limit(
            "preview_per_caller_per_minute",
            self.preview_per_caller_per_minute,
        )
    }

    /// The submit limiter's settings (M2-5; validated by [`Self::validate`]).
    ///
    /// # Errors
    /// A limit outside `1..=600`.
    pub fn submit_limit(&self) -> anyhow::Result<crate::infra::throttle::CallerLimitSettings> {
        caller_limit(
            "submit_per_caller_per_minute",
            self.submit_per_caller_per_minute,
        )
    }
}

fn caller_limit(
    key: &str,
    per_minute: u32,
) -> anyhow::Result<crate::infra::throttle::CallerLimitSettings> {
    let positive = std::num::NonZeroU32::new(per_minute)
        .filter(|n| PER_CALLER_PER_MINUTE_RANGE.contains(&n.get()))
        .ok_or_else(|| anyhow::anyhow!("bss-orders: gate.{key} must be 1..=600"))?;
    Ok(crate::infra::throttle::CallerLimitSettings {
        per_minute: positive,
        max_keys: crate::infra::throttle::PER_CALLER_MAX_KEYS_BASELINE,
    })
}

/// The per-(caller, order) request limit: 20 per minute is the declared baseline and the
/// maximum (a deployment may tighten it, never widen it), and `per_order_max_keys` bounds the
/// limiter's key store, at most its 100000 baseline (D-221).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThrottlingConfig {
    #[serde(default = "throttling_baseline::per_order_per_minute")]
    pub per_order_per_minute: u32,
    #[serde(default = "throttling_baseline::per_order_max_keys")]
    pub per_order_max_keys: u64,
}
mod throttling_baseline {
    pub const fn per_order_per_minute() -> u32 {
        crate::infra::throttle::PER_ORDER_PER_MINUTE_BASELINE
    }
    pub const fn per_order_max_keys() -> u64 {
        crate::infra::throttle::PER_ORDER_MAX_KEYS_BASELINE
    }
}
impl Default for ThrottlingConfig {
    fn default() -> Self {
        Self {
            per_order_per_minute: throttling_baseline::per_order_per_minute(),
            per_order_max_keys: throttling_baseline::per_order_max_keys(),
        }
    }
}

/// The promoted date policy: the platform default, resource-tenant overrides and overrides
/// retired by this promotion. Every switch is explicit; none defaults.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatePolicyConfig {
    pub platform_default: DatePolicySwitches,
    #[serde(default)]
    pub overrides: Vec<DatePolicyOverrideConfig>,
    /// Resource tenants whose override row this promotion removes, restoring the default.
    #[serde(default)]
    pub retired_overrides: Vec<uuid::Uuid>,
}

/// Whether a service-activation / acceptance-due date must be authored rather than defaulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatePolicySwitches {
    pub service_activation_required: bool,
    pub acceptance_due_required: bool,
}

/// One resource tenant's override row.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatePolicyOverrideConfig {
    pub resource_tenant_id: uuid::Uuid,
    pub service_activation_required: bool,
    pub acceptance_due_required: bool,
}

/// The declared line cap. The baseline is the design's 200 lines: a capped basket's catalog
/// resolution stays inside the 250 ms port deadline, and the 64 KiB event bound was proven at 200
/// lines (S2-08), so a deployment may lower it but never raise it.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    pub line_cap: u16,
}
impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            line_cap: crate::domain::capture::LINE_CAP_BASELINE,
        }
    }
}

/// The active key for `display_label`/`internal_notes` audit values (D-204).
///
/// `key` is a secret reference: deployments write `"${ENV_VAR}"` and the platform resolves it
/// at load time, so the secret is never committed. `Debug` redacts it and no error message
/// carries it. Rotation installs a new `key_id` with a new key; existing rows keep the ID
/// they were written with, and verification never needs any key.
#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct AuditMinimizationConfig {
    pub key_id: String,
    #[expand_vars]
    pub key: SecretString,
}

/// The Orders producer principal.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsConfig {
    /// Service principal presented to Event Broker; its grants must allow producing the Orders
    /// event family under platform-root tenancy (08 §4.3) and nothing confers them implicitly.
    pub producer_subject_id: uuid::Uuid,
    pub producer_tenant_id: uuid::Uuid,
}

/// One authenticated service principal, matched on `(subject_id, tenant_id)`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServicePrincipalConfig {
    pub role: crate::domain::audit::ServiceRole,
    pub subject_id: uuid::Uuid,
    pub tenant_id: uuid::Uuid,
}

/// The closed set of lifecycle-owned maintenance work (08 §3.5) an operator can allowlist in
/// [`MaintenanceConfig::tasks`]. Nothing else may use it; the maintenance authority grants exactly
/// the configured tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceTask {
    /// Per-state TTL expiry.
    StateExpiry,
    /// Draft auto-void.
    DraftAutoVoid,
    /// Idempotency-window cleanup and unresolved-execution discovery.
    IdempotencyCleanup,
    /// Bounded purge of the retention stores.
    RetentionPurge,
    /// Read-only audit chain and checkpoint verification.
    AuditVerification,
    /// Audit checkpoint capture.
    AuditCheckpoint,
}

/// Configured service actor, allowlisted worker tasks, their class connections and cadences.
#[derive(Debug, Clone, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceConfig {
    pub actor_subject_id: uuid::Uuid,
    pub actor_tenant_id: uuid::Uuid,
    pub tasks: Vec<MaintenanceTask>,
    /// One restricted login connection per privilege class the allowlisted tasks need
    /// (migration 08 roles: discovery, maintenance, retention, verifier, checkpoint). Each is a
    /// secret DSN reference (`"${VAR}"`) resolved at load time; a task whose class connection is
    /// absent fails validation, and startup attests every connection's privileges.
    #[serde(default)]
    #[expand_vars]
    pub connections: MaintenanceConnectionsConfig,
    /// Worker cadences and batch bounds; absent means the design baselines.
    #[serde(default)]
    pub schedule: MaintenanceScheduleConfig,
}

/// The class connections (secret DSN references).
#[derive(Clone, Default, Deserialize, ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceConnectionsConfig {
    #[serde(default)]
    #[expand_vars]
    pub discovery: Option<SecretString>,
    #[serde(default)]
    #[expand_vars]
    pub maintenance: Option<SecretString>,
    #[serde(default)]
    #[expand_vars]
    pub retention: Option<SecretString>,
    #[serde(default)]
    #[expand_vars]
    pub verifier: Option<SecretString>,
    #[serde(default)]
    #[expand_vars]
    pub checkpoint: Option<SecretString>,
}
impl std::fmt::Debug for MaintenanceConnectionsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // DSNs carry credentials: only presence is printed.
        f.debug_struct("MaintenanceConnectionsConfig")
            .field("discovery", &self.discovery.is_some())
            .field("maintenance", &self.maintenance.is_some())
            .field("retention", &self.retention.is_some())
            .field("verifier", &self.verifier.is_some())
            .field("checkpoint", &self.checkpoint.is_some())
            .finish()
    }
}
impl MaintenanceConnectionsConfig {
    fn get(&self, class: crate::infra::workers::RoleClass) -> Option<&SecretString> {
        use crate::infra::workers::RoleClass;
        match class {
            RoleClass::Discovery => self.discovery.as_ref(),
            RoleClass::Maintenance => self.maintenance.as_ref(),
            RoleClass::Retention => self.retention.as_ref(),
            RoleClass::Verifier => self.verifier.as_ref(),
            RoleClass::Checkpoint => self.checkpoint.as_ref(),
        }
    }
}

/// Worker cadences and batch bounds. Baselines: cleanup every 60 s over 500 rows (01 §3.1),
/// daily purge in batches of 5,000 (01 §3.5), daily checkpoints and a 500-order verification
/// slice per namespace per minute (D-100), expiry/auto-void sweeps every 60 s over 500 rows.
/// A deployment may tighten any of them but never exceed the stated bounds.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceScheduleConfig {
    #[serde(default = "baseline::cleanup_interval_seconds")]
    pub cleanup_interval_seconds: u32,
    #[serde(default = "baseline::cleanup_batch")]
    pub cleanup_batch: u32,
    #[serde(default = "baseline::retention_interval_seconds")]
    pub retention_interval_seconds: u32,
    #[serde(default = "baseline::retention_batch")]
    pub retention_batch: u32,
    #[serde(default = "baseline::retention_batches_per_pass")]
    pub retention_batches_per_pass: u32,
    #[serde(default = "baseline::checkpoint_interval_seconds")]
    pub checkpoint_interval_seconds: u32,
    #[serde(default = "baseline::verification_interval_seconds")]
    pub verification_interval_seconds: u32,
    #[serde(default = "baseline::verification_orders_per_pass")]
    pub verification_orders_per_pass: u32,
    #[serde(default = "baseline::sweep_interval_seconds")]
    pub sweep_interval_seconds: u32,
    #[serde(default = "baseline::sweep_batch")]
    pub sweep_batch: u32,
}
mod baseline {
    pub const fn cleanup_interval_seconds() -> u32 {
        60
    }
    pub const fn cleanup_batch() -> u32 {
        500
    }
    pub const fn retention_interval_seconds() -> u32 {
        86_400
    }
    pub const fn retention_batch() -> u32 {
        5000
    }
    pub const fn retention_batches_per_pass() -> u32 {
        24
    }
    pub const fn checkpoint_interval_seconds() -> u32 {
        86_400
    }
    pub const fn verification_interval_seconds() -> u32 {
        60
    }
    pub const fn verification_orders_per_pass() -> u32 {
        500
    }
    pub const fn sweep_interval_seconds() -> u32 {
        60
    }
    pub const fn sweep_batch() -> u32 {
        500
    }
}
impl Default for MaintenanceScheduleConfig {
    fn default() -> Self {
        Self {
            cleanup_interval_seconds: baseline::cleanup_interval_seconds(),
            cleanup_batch: baseline::cleanup_batch(),
            retention_interval_seconds: baseline::retention_interval_seconds(),
            retention_batch: baseline::retention_batch(),
            retention_batches_per_pass: baseline::retention_batches_per_pass(),
            checkpoint_interval_seconds: baseline::checkpoint_interval_seconds(),
            verification_interval_seconds: baseline::verification_interval_seconds(),
            verification_orders_per_pass: baseline::verification_orders_per_pass(),
            sweep_interval_seconds: baseline::sweep_interval_seconds(),
            sweep_batch: baseline::sweep_batch(),
        }
    }
}
impl OrdersConfig {
    /// Reject zero or unreasonable execution/dependency bounds.
    ///
    /// # Errors
    /// Returns a sanitized operator error for an invalid duration.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.idempotency_lease()?;
        anyhow::ensure!(
            (1..=60_000).contains(&self.dependency_timeout_ms),
            "bss-orders: dependency timeout must be 1..60000 milliseconds"
        );
        if let Some(maintenance) = &self.maintenance {
            anyhow::ensure!(
                !maintenance.actor_subject_id.is_nil() && !maintenance.actor_tenant_id.is_nil(),
                "bss-orders: maintenance actor must be a configured service identity"
            );
            anyhow::ensure!(
                !maintenance.tasks.is_empty(),
                "bss-orders: maintenance requires at least one allowlisted task"
            );
            // F01 retention purge: refusals are always audited, so scheduling workers without
            // the purge would let refused audit rows grow unbounded. No maintenance block at
            // all stays the finding-185 warning (no worker scheduled).
            anyhow::ensure!(
                maintenance.tasks.contains(&MaintenanceTask::RetentionPurge),
                "bss-orders: maintenance tasks must include retention_purge (refusals are always audited)"
            );
            self.maintenance_connections()?;
            self.worker_settings()?;
        }
        self.actor_identities()?;
        self.producer_settings()?;
        self.admin_text_key()?;
        self.capture_settings()?;
        self.date_policy_plan()?;
        self.throttle_settings()?;
        self.gate.validate()?;
        Ok(())
    }

    /// The validated edge-limiter settings.
    ///
    /// # Errors
    /// A per-order limit outside `1..=20`, or a key bound outside `1..=100000` (the baseline is
    /// the maximum, D-221).
    pub(crate) fn throttle_settings(
        &self,
    ) -> anyhow::Result<crate::infra::throttle::ThrottleSettings> {
        use crate::infra::throttle::{
            PER_ORDER_MAX_KEYS_BASELINE, PER_ORDER_PER_MINUTE_BASELINE, ThrottleSettings,
        };
        let per_minute = self.throttling.per_order_per_minute;
        anyhow::ensure!(
            (1..=PER_ORDER_PER_MINUTE_BASELINE).contains(&per_minute),
            "bss-orders: throttling.per_order_per_minute must be 1..{PER_ORDER_PER_MINUTE_BASELINE} (the declared baseline is the maximum)"
        );
        let per_order_per_minute = std::num::NonZeroU32::new(per_minute).ok_or_else(|| {
            anyhow::anyhow!("bss-orders: throttling.per_order_per_minute must be positive")
        })?;
        // The key bound is the only memory bound on caller-keyed limiter state, so it is
        // capped like every other setting: a deployment may tighten it, never widen it (D-221).
        anyhow::ensure!(
            (1..=PER_ORDER_MAX_KEYS_BASELINE).contains(&self.throttling.per_order_max_keys),
            "bss-orders: throttling.per_order_max_keys must be 1..{PER_ORDER_MAX_KEYS_BASELINE} (the declared baseline is the maximum)"
        );
        Ok(ThrottleSettings {
            per_order_per_minute,
            per_order_max_keys: self.throttling.per_order_max_keys,
        })
    }

    /// The validated date-policy promotion, if this deployment carries one.
    ///
    /// # Errors
    /// A nil tenant, a duplicated tenant, a tenant both overridden and retired, or more rows
    /// than one promotion may carry.
    pub(crate) fn date_policy_plan(
        &self,
    ) -> anyhow::Result<Option<crate::infra::dates::DatePolicyPlan>> {
        self.date_policy
            .as_ref()
            .map(crate::infra::dates::DatePolicyPlan::from_config)
            .transpose()
    }

    /// The validated capture bounds.
    ///
    /// # Errors
    /// A line cap outside `1..=200`.
    pub(crate) fn capture_settings(
        &self,
    ) -> anyhow::Result<crate::infra::capture::CaptureSettings> {
        let cap = self.capture.line_cap;
        anyhow::ensure!(
            (1..=crate::domain::capture::LINE_CAP_BASELINE).contains(&cap),
            "bss-orders: capture.line_cap must be 1..200 (the declared baseline is the maximum)"
        );
        Ok(crate::infra::capture::CaptureSettings {
            line_cap: usize::from(cap),
        })
    }

    /// The keyed administrative-text minimizer (D-204).
    ///
    /// # Errors
    /// A malformed key ID or a key outside 32..=1024 bytes. The message never carries the key.
    pub(crate) fn admin_text_key(&self) -> anyhow::Result<crate::domain::audit::AdminTextKey> {
        let config = &self.audit_minimization;
        let key = config.key.expose_secret();
        // A placeholder that reached validation unexpanded is a reference, not a key.
        anyhow::ensure!(
            !key.contains("${"),
            "bss-orders: audit_minimization: key is an unresolved secret reference"
        );
        crate::domain::audit::AdminTextKey::new(&config.key_id, key.as_bytes())
            .map_err(|e| anyhow::anyhow!("bss-orders: audit_minimization: {e}"))
    }

    /// The validated producer principal.
    ///
    /// # Errors
    /// A nil producer identity, or one shared with another configured principal.
    pub(crate) fn producer_settings(
        &self,
    ) -> anyhow::Result<crate::infra::broker::ProducerSettings> {
        let events = &self.events;
        anyhow::ensure!(
            !events.producer_subject_id.is_nil() && !events.producer_tenant_id.is_nil(),
            "bss-orders: events producer must be a configured service identity"
        );
        let shared = self
            .service_principals
            .iter()
            .any(|p| p.subject_id == events.producer_subject_id)
            || self
                .maintenance
                .as_ref()
                .is_some_and(|m| m.actor_subject_id == events.producer_subject_id);
        anyhow::ensure!(
            !shared,
            "bss-orders: the events producer identity must be distinct from service and maintenance principals"
        );
        Ok(crate::infra::broker::ProducerSettings {
            subject_id: events.producer_subject_id,
            tenant_id: events.producer_tenant_id,
        })
    }

    /// Audit actor-class identities (D-115): system = the configured maintenance actor,
    /// service = the configured principals, user = every other authenticated subject.
    ///
    /// # Errors
    /// Missing Workflow principal, nil/duplicate principals, or overlap with the system actor.
    pub(crate) fn actor_identities(&self) -> anyhow::Result<crate::domain::audit::ActorIdentities> {
        use crate::domain::audit::{ActorIdentities, Principal};
        let system = self.maintenance.as_ref().map(|m| Principal {
            subject_id: m.actor_subject_id,
            subject_tenant_id: m.actor_tenant_id,
        });
        ActorIdentities::new(
            system,
            self.service_principals.iter().map(|p| {
                (
                    p.role,
                    Principal {
                        subject_id: p.subject_id,
                        subject_tenant_id: p.tenant_id,
                    },
                )
            }),
        )
        .map_err(|e| anyhow::anyhow!("bss-orders: service_principals: {e}"))
    }

    /// The explicit positive finite registry lease (Foundation §3.6); no implicit default.
    ///
    /// # Errors
    /// Zero or more than one day.
    pub(crate) fn idempotency_lease(
        &self,
    ) -> anyhow::Result<crate::domain::idempotency::LeaseDuration> {
        crate::domain::idempotency::LeaseDuration::from_seconds(self.idempotency_lease_seconds)
            .map_err(|_| anyhow::anyhow!("bss-orders: idempotency lease must be 1..86400 seconds"))
    }

    /// The class connections the allowlisted tasks need, keyed by class.
    ///
    /// # Errors
    /// A task whose class connection is absent, or a connection reference left unresolved.
    pub(crate) fn maintenance_connections(
        &self,
    ) -> anyhow::Result<std::collections::BTreeMap<crate::infra::workers::RoleClass, SecretString>>
    {
        use crate::infra::workers::RoleClass;
        let mut out = std::collections::BTreeMap::new();
        let Some(maintenance) = &self.maintenance else {
            return Ok(out);
        };
        for task in &maintenance.tasks {
            let class = RoleClass::for_task(*task);
            let Some(dsn) = maintenance.connections.get(class) else {
                anyhow::bail!(
                    "bss-orders: maintenance.connections.{} is required for task {task:?} (restricted `{}` login)",
                    class.config_key(),
                    class.role_name()
                );
            };
            anyhow::ensure!(
                !dsn.expose_secret().contains("${"),
                "bss-orders: maintenance.connections.{} is an unresolved secret reference",
                class.config_key()
            );
            anyhow::ensure!(
                dsn.expose_secret().starts_with("postgres://")
                    || dsn.expose_secret().starts_with("postgresql://"),
                "bss-orders: maintenance.connections.{} must be a PostgreSQL DSN",
                class.config_key()
            );
            out.insert(class, dsn.clone());
        }
        Ok(out)
    }

    /// The validated worker cadences and batch bounds.
    ///
    /// # Errors
    /// A zero value or one beyond its design bound.
    pub(crate) fn worker_settings(&self) -> anyhow::Result<crate::infra::workers::WorkerSettings> {
        use std::time::Duration;
        let s = self
            .maintenance
            .as_ref()
            .map(|m| m.schedule)
            .unwrap_or_default();
        let check = |name: &str, value: u32, min: u32, max: u32| -> anyhow::Result<u32> {
            anyhow::ensure!(
                (min..=max).contains(&value),
                "bss-orders: maintenance.schedule.{name} must be {min}..{max}"
            );
            Ok(value)
        };
        Ok(crate::infra::workers::WorkerSettings {
            cleanup_interval: Duration::from_secs(u64::from(check(
                "cleanup_interval_seconds",
                s.cleanup_interval_seconds,
                1,
                3600,
            )?)),
            cleanup_batch: u64::from(check("cleanup_batch", s.cleanup_batch, 1, 500)?),
            retention_interval: Duration::from_secs(u64::from(check(
                "retention_interval_seconds",
                s.retention_interval_seconds,
                60,
                86_400,
            )?)),
            retention_batch: u64::from(check("retention_batch", s.retention_batch, 1, 5000)?),
            retention_batches_per_pass: check(
                "retention_batches_per_pass",
                s.retention_batches_per_pass,
                1,
                10_000,
            )?,
            checkpoint_interval: Duration::from_secs(u64::from(check(
                "checkpoint_interval_seconds",
                s.checkpoint_interval_seconds,
                60,
                86_400,
            )?)),
            verification_interval: Duration::from_secs(u64::from(check(
                "verification_interval_seconds",
                s.verification_interval_seconds,
                1,
                3600,
            )?)),
            verification_orders_per_pass: u64::from(check(
                "verification_orders_per_pass",
                s.verification_orders_per_pass,
                1,
                500,
            )?),
            sweep_interval: Duration::from_secs(u64::from(check(
                "sweep_interval_seconds",
                s.sweep_interval_seconds,
                1,
                3600,
            )?)),
            sweep_batch: u64::from(check("sweep_batch", s.sweep_batch, 1, 5000)?),
        })
    }

    /// The bounded worker capability; `None` when no maintenance identity is configured.
    pub(crate) fn maintenance_authority(
        &self,
    ) -> Option<crate::infra::maintenance::MaintenanceAuthority> {
        let config = self.maintenance.as_ref()?;
        let actor = crate::infra::maintenance::ServiceActor::configured(
            config.actor_subject_id,
            config.actor_tenant_id,
        )?;
        Some(crate::infra::maintenance::MaintenanceAuthority::configured(
            actor,
            config.tasks.iter().copied(),
        ))
    }
}
