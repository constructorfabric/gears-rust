//! Configuration for `[quota-enforcement]`. Read once at `Gear::init`.

use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::time::Duration;

use gts::GtsTypeId;
use serde::Deserialize;

use crate::domain::catalog::CatalogConfig;
use crate::domain::policies::schemas::SnapshotLimits;
use crate::domain::policies::{EvaluationLimits, PolicyLimits, PolicyRuntimeLimits};
use crate::domain::quotas::{GaugeTiming, QuotaLimits};

/// Gear configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuotaEnforcementConfig {
    /// Vendor of the active storage plugin. Exactly one plugin is active per
    /// deployment (DESIGN, "Single storage plugin per deployment").
    pub storage_vendor: String,
    /// Timing of the sweeper elections on the platform `cluster` gear.
    pub election: ElectionTimingConfig,
    /// Budget, in seconds, for a sweep body to stop after leadership loss or
    /// shutdown. A body that overruns the budget is aborted.
    pub sweeper_stop_timeout_secs: u64,
    /// Operational metrics.
    pub metrics: MetricsConfig,
    /// The owner projections configured for evaluation.
    pub catalog: CatalogSection,
    /// Bounds of the Quota lifecycle surface.
    pub quotas: QuotasSection,
    /// Bounds of the resolution-policy surface.
    pub policies: PoliciesSection,
    /// Timing of the lifecycle-gauge refresh.
    pub gauges: GaugesSection,
    /// Bounds of the consumption hot path.
    pub operations: OperationsSection,
    /// Timing of the retention sweeper.
    pub retention: RetentionSection,
}

impl Default for QuotaEnforcementConfig {
    fn default() -> Self {
        Self {
            storage_vendor: "constructorfabric".to_owned(),
            election: ElectionTimingConfig::default(),
            sweeper_stop_timeout_secs: 10,
            metrics: MetricsConfig::default(),
            catalog: CatalogSection::default(),
            quotas: QuotasSection::default(),
            policies: PoliciesSection::default(),
            gauges: GaugesSection::default(),
            operations: OperationsSection::default(),
            retention: RetentionSection::default(),
        }
    }
}

impl QuotaEnforcementConfig {
    /// Reject a configuration the gear cannot start with.
    ///
    /// # Errors
    ///
    /// Returns an error when the vendor is blank, a timing value is zero, or
    /// the metrics prefix is not a valid instrument-name prefix.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.storage_vendor.trim().is_empty() {
            anyhow::bail!(
                "[quota-enforcement].storage_vendor must not be empty or whitespace-only"
            );
        }
        self.election.validate()?;
        if self.sweeper_stop_timeout_secs == 0 {
            anyhow::bail!("[quota-enforcement].sweeper_stop_timeout_secs must be at least 1");
        }
        self.metrics.validate()?;
        self.catalog.validate()?;
        self.quotas.validate()?;
        self.policies.validate()?;
        self.gauges.validate()?;
        self.operations.validate()?;
        self.retention.validate()
    }

    /// Budget for a sweep body to stop after leadership loss or shutdown.
    #[must_use]
    pub const fn sweeper_stop_timeout(&self) -> Duration {
        Duration::from_secs(self.sweeper_stop_timeout_secs)
    }
}

/// Timing of one sweeper election (`[quota-enforcement.election]`).
///
/// The defaults are the cluster gear's defaults. A shorter TTL gives a faster
/// takeover after a crash at the cost of more renewal traffic; a larger
/// missed-renewal budget tolerates more backend jitter before leadership counts
/// as lost.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ElectionTimingConfig {
    /// Time-to-live of a leadership claim, in seconds. A crashed leader is
    /// replaced within this window plus observation lag.
    pub ttl_secs: u64,
    /// Consecutive renewal failures tolerated before leadership counts as
    /// lost. The cluster gear renews every `ttl / (max_missed_renewals + 1)`.
    pub max_missed_renewals: u8,
}

impl Default for ElectionTimingConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 30,
            max_missed_renewals: 2,
        }
    }
}

impl ElectionTimingConfig {
    /// Reject timing values the cluster gear cannot run an election with.
    ///
    /// # Errors
    ///
    /// Returns an error when the TTL or the missed-renewal budget is zero.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.ttl_secs == 0 {
            anyhow::bail!("[quota-enforcement.election].ttl_secs must be at least 1");
        }
        if self.max_missed_renewals == 0 {
            anyhow::bail!("[quota-enforcement.election].max_missed_renewals must be at least 1");
        }
        Ok(())
    }

    /// Time-to-live of a leadership claim.
    #[must_use]
    pub const fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl_secs)
    }
}

/// Operational-metrics configuration for `[quota-enforcement.metrics]`.
///
/// The PRD section 5.16 catalogue names instruments without a namespace
/// (`denial_total`, ...). The prefix is empty by default so the rendered
/// names match the catalogue verbatim. Operators may set one.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    /// Optional instrument-name prefix.
    pub prefix: String,
}

impl MetricsConfig {
    /// Full instrument name for a catalogue name.
    #[must_use]
    pub fn instrument_name(&self, catalogue_name: &str) -> String {
        let prefix = self.prefix.trim();
        if prefix.is_empty() {
            catalogue_name.to_owned()
        } else {
            format!("{prefix}_{catalogue_name}")
        }
    }

    /// Reject a prefix that is not a valid instrument-name prefix
    /// (`[A-Za-z_][A-Za-z0-9_]*`). Empty is valid.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid prefix.
    pub fn validate(&self) -> anyhow::Result<()> {
        let prefix = self.prefix.trim();
        if prefix.is_empty() {
            return Ok(());
        }
        let mut chars = prefix.chars();
        let valid = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            anyhow::bail!(
                "[quota-enforcement.metrics].prefix must match [A-Za-z_][A-Za-z0-9_]* (got {:?})",
                self.prefix
            );
        }
        Ok(())
    }
}

/// The evaluation catalogue (`[quota-enforcement.catalog]`): the concrete owner
/// projections this deployment resolves at bootstrap (ADR-0007). Request and
/// constraint contracts are discovered from the registry, not configured.
/// Empty lists boot an empty catalogue.
// @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-config
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CatalogSection {
    /// GTS type ids of concrete subject projections derived from
    /// `gts.cf.core.qe.subj.v1~`.
    pub subject_projections: Vec<String>,
    /// GTS type ids of concrete resource projections derived from
    /// `gts.cf.core.qe.res.v1~`.
    pub resource_projections: Vec<String>,
}
// @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-config

impl CatalogSection {
    /// Reject ids that are not GTS type ids and duplicate entries.
    ///
    /// # Errors
    ///
    /// Returns an error naming the field and the offending entry.
    pub fn validate(&self) -> anyhow::Result<()> {
        Self::validate_list("subject_projections", &self.subject_projections)?;
        Self::validate_list("resource_projections", &self.resource_projections)
    }

    fn validate_list(field: &str, ids: &[String]) -> anyhow::Result<()> {
        for (index, id) in ids.iter().enumerate() {
            GtsTypeId::try_new(id).map_err(|e| {
                anyhow::anyhow!(
                    "[quota-enforcement.catalog].{field}[{index}] is not a GTS type id: {e}"
                )
            })?;
            if ids[..index].contains(id) {
                anyhow::bail!("[quota-enforcement.catalog].{field} lists {id} twice");
            }
        }
        Ok(())
    }

    /// The domain view of the section.
    ///
    /// # Errors
    ///
    /// Returns an error when an id does not parse; `validate` reports the same
    /// condition with the field name.
    pub fn to_domain(&self) -> anyhow::Result<CatalogConfig> {
        let parse = |ids: &[String]| -> anyhow::Result<Vec<GtsTypeId>> {
            ids.iter()
                .map(|id| GtsTypeId::try_new(id).map_err(|e| anyhow::anyhow!("{id}: {e}")))
                .collect()
        };
        Ok(CatalogConfig {
            subject_projections: parse(&self.subject_projections)?,
            resource_projections: parse(&self.resource_projections)?,
        })
    }
}

/// Bounds of the Quota lifecycle surface (`[quota-enforcement.quotas]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct QuotasSection {
    /// Largest canonical-JSON size of a Quota's `metadata` object, in bytes
    /// (PRD section 5.2, default 4 KiB).
    pub metadata_max_bytes: usize,
    /// Entries the metric classification cache holds before evicting the
    /// least recently used one.
    pub metric_cache_entries: usize,
    /// Age after which a cached classification is refreshed from the
    /// registry, in seconds.
    pub metric_cache_ttl_secs: u64,
    /// Additional age during which a cached classification may still be
    /// served to a write when the registry does not answer, in seconds.
    /// Beyond `ttl + grace` nothing is served; the write fails closed.
    pub metric_cache_stale_grace_secs: u64,
    /// Largest page a list request may ask for.
    pub list_max_limit: u32,
    /// Largest number of explicit ids one list request may name.
    pub list_max_ids: usize,
}

impl Default for QuotasSection {
    fn default() -> Self {
        Self {
            metadata_max_bytes: 4096,
            metric_cache_entries: 256,
            metric_cache_ttl_secs: 60,
            metric_cache_stale_grace_secs: 300,
            list_max_limit: 500,
            list_max_ids: 100,
        }
    }
}

impl QuotasSection {
    /// Smallest metadata size limit: an empty object serialized.
    pub const MIN_METADATA_BYTES: usize = 2;
    /// Largest metadata size limit.
    pub const MAX_METADATA_BYTES: usize = 1_048_576;

    /// Reject bounds the gear cannot serve with.
    ///
    /// # Errors
    ///
    /// Returns an error naming the field that is out of its range.
    pub fn validate(&self) -> anyhow::Result<()> {
        if !(Self::MIN_METADATA_BYTES..=Self::MAX_METADATA_BYTES).contains(&self.metadata_max_bytes)
        {
            anyhow::bail!(
                "[quota-enforcement.quotas].metadata_max_bytes must be within {}..={}",
                Self::MIN_METADATA_BYTES,
                Self::MAX_METADATA_BYTES
            );
        }
        if self.metric_cache_entries == 0 {
            anyhow::bail!("[quota-enforcement.quotas].metric_cache_entries must be at least 1");
        }
        if self.metric_cache_ttl_secs == 0 {
            anyhow::bail!("[quota-enforcement.quotas].metric_cache_ttl_secs must be at least 1");
        }
        if self.list_max_limit == 0 {
            anyhow::bail!("[quota-enforcement.quotas].list_max_limit must be at least 1");
        }
        if self.list_max_ids == 0 {
            anyhow::bail!("[quota-enforcement.quotas].list_max_ids must be at least 1");
        }
        Ok(())
    }

    /// The domain view of the request bounds.
    #[must_use]
    pub const fn to_limits(&self) -> QuotaLimits {
        QuotaLimits {
            metadata_max_bytes: self.metadata_max_bytes,
            list_max_limit: self.list_max_limit,
            list_max_ids: self.list_max_ids,
        }
    }

    /// Age after which a cached classification is refreshed.
    #[must_use]
    pub const fn metric_cache_ttl(&self) -> Duration {
        Duration::from_secs(self.metric_cache_ttl_secs)
    }

    /// Additional age during which a stale classification may serve a write.
    #[must_use]
    pub const fn metric_cache_stale_grace(&self) -> Duration {
        Duration::from_secs(self.metric_cache_stale_grace_secs)
    }
}

/// Bounds of the resolution-policy surface (`[quota-enforcement.policies]`).
///
/// `evaluation_timeout_upper_ms` is also a database lock-hold knob: an engine
/// evaluates while the evaluation transaction holds Quota rows, so the clamp
/// bounds evaluation's contribution to that hold. It does not bound
/// compilation, lock acquisition, or the transaction as a whole.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PoliciesSection {
    /// Ceiling a policy's requested `timeout_ms` is clamped to at each
    /// evaluation (feature default 5 ms). Applied when the budget is built,
    /// never persisted, so lowering it takes effect on the next evaluation
    /// after the process loads the new value.
    pub evaluation_timeout_upper_ms: u64,
    /// Operations an engine may charge in one evaluation before it reports
    /// `CostExceeded`.
    pub evaluation_cost_limit: u64,
    /// Largest serialized `engine_config` accepted at create or update.
    pub config_max_bytes: usize,
    /// Largest comment or description, in bytes.
    pub comment_max_bytes: usize,
    /// Largest page a version-history request may ask for.
    pub list_max_limit: u32,
    /// Compiled artifacts kept in process; the oldest is evicted past this.
    pub artifact_cache_entries: usize,
    /// Attempts to prepare a missing artifact and retry the evaluation
    /// transaction before failing with a canonical error.
    pub preparation_max_attempts: u32,
    /// Artifact compilations that may run at once. Compilation is unbounded
    /// CPU work off the transaction path; this caps what a cold cache can
    /// spend at once.
    pub preparation_max_concurrency: usize,
    /// Largest persisted schema snapshot, in bytes. `most-restrictive-wins`
    /// persists an empty one; only `cel` carries a closure.
    pub snapshot_max_bytes: usize,
    /// Distinct resolved schemas one snapshot may hold.
    pub snapshot_max_schemas: usize,
    /// Deepest JSON nesting a resolved schema may reach.
    pub snapshot_max_depth: usize,
}

impl Default for PoliciesSection {
    fn default() -> Self {
        Self {
            evaluation_timeout_upper_ms: 5,
            evaluation_cost_limit: 10_000,
            config_max_bytes: 16_384,
            comment_max_bytes: 1_024,
            list_max_limit: 50,
            artifact_cache_entries: 512,
            preparation_max_attempts: 3,
            preparation_max_concurrency: 4,
            snapshot_max_bytes: 65_536,
            snapshot_max_schemas: 32,
            snapshot_max_depth: 32,
        }
    }
}

impl PoliciesSection {
    /// Longest evaluation clamp an operator may configure. Evaluation runs
    /// under database row locks; a full second is already generous.
    pub const MAX_EVALUATION_TIMEOUT_MS: u64 = 1_000;
    /// Largest `engine_config` any deployment may accept.
    pub const MAX_CONFIG_BYTES: usize = 1_048_576;
    /// Largest schema snapshot any deployment may persist per version.
    pub const MAX_SNAPSHOT_BYTES: usize = 4_194_304;

    /// Reject bounds the gear cannot serve with.
    ///
    /// # Errors
    ///
    /// Returns an error naming the field that is out of its range.
    pub fn validate(&self) -> anyhow::Result<()> {
        self.to_limits().map(|_| ())
    }

    /// The domain view of the bounds. Validation and conversion are one step,
    /// so a zero cannot reach a `NonZero` field by any other path.
    ///
    /// # Errors
    ///
    /// Returns an error naming the field that is zero or above its ceiling.
    pub fn to_limits(&self) -> anyhow::Result<PolicyRuntimeLimits> {
        fn at_least_one_u64(field: &str, value: u64) -> anyhow::Result<NonZeroU64> {
            NonZeroU64::new(value).ok_or_else(|| {
                anyhow::anyhow!("[quota-enforcement.policies].{field} must be at least 1")
            })
        }
        fn at_least_one_u32(field: &str, value: u32) -> anyhow::Result<NonZeroU32> {
            NonZeroU32::new(value).ok_or_else(|| {
                anyhow::anyhow!("[quota-enforcement.policies].{field} must be at least 1")
            })
        }
        fn at_least_one_usize(field: &str, value: usize) -> anyhow::Result<NonZeroUsize> {
            NonZeroUsize::new(value).ok_or_else(|| {
                anyhow::anyhow!("[quota-enforcement.policies].{field} must be at least 1")
            })
        }

        let upper_timeout_ms = at_least_one_u64(
            "evaluation_timeout_upper_ms",
            self.evaluation_timeout_upper_ms,
        )?;
        if upper_timeout_ms.get() > Self::MAX_EVALUATION_TIMEOUT_MS {
            anyhow::bail!(
                "[quota-enforcement.policies].evaluation_timeout_upper_ms must be at most {}",
                Self::MAX_EVALUATION_TIMEOUT_MS
            );
        }
        at_least_one_usize("config_max_bytes", self.config_max_bytes)?;
        if self.config_max_bytes > Self::MAX_CONFIG_BYTES {
            anyhow::bail!(
                "[quota-enforcement.policies].config_max_bytes must be at most {}",
                Self::MAX_CONFIG_BYTES
            );
        }
        at_least_one_usize("comment_max_bytes", self.comment_max_bytes)?;
        at_least_one_u32("list_max_limit", self.list_max_limit)?;
        at_least_one_usize("snapshot_max_bytes", self.snapshot_max_bytes)?;
        if self.snapshot_max_bytes > Self::MAX_SNAPSHOT_BYTES {
            anyhow::bail!(
                "[quota-enforcement.policies].snapshot_max_bytes must be at most {}",
                Self::MAX_SNAPSHOT_BYTES
            );
        }
        at_least_one_usize("snapshot_max_schemas", self.snapshot_max_schemas)?;
        at_least_one_usize("snapshot_max_depth", self.snapshot_max_depth)?;

        Ok(PolicyRuntimeLimits {
            evaluation: EvaluationLimits {
                upper_timeout_ms,
                cost_limit: at_least_one_u64("evaluation_cost_limit", self.evaluation_cost_limit)?,
            },
            authoring: PolicyLimits {
                config_bytes: self.config_max_bytes,
                comment_bytes: self.comment_max_bytes,
                list_limit: self.list_max_limit,
            },
            snapshot: SnapshotLimits {
                bytes: self.snapshot_max_bytes,
                schemas: self.snapshot_max_schemas,
                depth: self.snapshot_max_depth,
            },
            artifact_cache_entries: at_least_one_usize(
                "artifact_cache_entries",
                self.artifact_cache_entries,
            )?,
            preparation_max_attempts: at_least_one_u32(
                "preparation_max_attempts",
                self.preparation_max_attempts,
            )?,
            preparation_max_concurrency: at_least_one_usize(
                "preparation_max_concurrency",
                self.preparation_max_concurrency,
            )?,
        })
    }
}

/// Timing of the lifecycle-gauge refresh (`[quota-enforcement.gauges]`). The
/// elected replica reads the active-Quota counts every `refresh_secs`, bounds
/// each read by `refresh_deadline_secs`, and withdraws the published sample
/// once no refresh succeeded for `stale_after_secs`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "configuration keys carry their unit, as `sweeper_stop_timeout_secs` does"
)]
pub struct GaugesSection {
    /// Seconds between two refreshes.
    pub refresh_secs: u64,
    /// Seconds one refresh may take before it counts as failed.
    pub refresh_deadline_secs: u64,
    /// Seconds without a successful refresh before the sample is withdrawn.
    pub stale_after_secs: u64,
}

impl Default for GaugesSection {
    fn default() -> Self {
        Self {
            refresh_secs: 30,
            refresh_deadline_secs: 10,
            stale_after_secs: 180,
        }
    }
}

impl GaugesSection {
    /// Reject timings under which the refresh cannot keep a sample fresh.
    ///
    /// # Errors
    ///
    /// Returns an error when a value is zero, the deadline exceeds the
    /// interval, or the staleness bound does not exceed the interval.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.refresh_secs == 0 {
            anyhow::bail!("[quota-enforcement.gauges].refresh_secs must be at least 1");
        }
        if self.refresh_deadline_secs == 0 || self.refresh_deadline_secs > self.refresh_secs {
            anyhow::bail!(
                "[quota-enforcement.gauges].refresh_deadline_secs must be within 1..=refresh_secs"
            );
        }
        if self.stale_after_secs <= self.refresh_secs {
            anyhow::bail!(
                "[quota-enforcement.gauges].stale_after_secs must exceed refresh_secs, else the \
                 sample is withdrawn before the next refresh"
            );
        }
        Ok(())
    }

    /// The domain view of the timing.
    #[must_use]
    pub const fn to_timing(&self) -> GaugeTiming {
        GaugeTiming {
            refresh: Duration::from_secs(self.refresh_secs),
            refresh_deadline: Duration::from_secs(self.refresh_deadline_secs),
            stale_after: Duration::from_secs(self.stale_after_secs),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;

/// Bounds of the consumption hot path (`[quota-enforcement.operations]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OperationsSection {
    /// Replay records the in-process cache holds before evicting the least
    /// recently used one.
    pub idempotency_cache_entries: usize,
    /// How long a cached record may answer a replay, in milliseconds. An entry
    /// never outlives the record's own retention whatever this says.
    pub idempotency_cache_ttl_ms: u64,
}

impl Default for OperationsSection {
    fn default() -> Self {
        Self {
            idempotency_cache_entries: 4096,
            // The P1 reference default of the idempotency-replay algorithm.
            idempotency_cache_ttl_ms: 5_000,
        }
    }
}

impl OperationsSection {
    /// Reject bounds the hot path cannot serve with.
    ///
    /// # Errors
    ///
    /// Returns an error naming the field that is out of its range.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.idempotency_cache_entries == 0 {
            anyhow::bail!(
                "[quota-enforcement.operations].idempotency_cache_entries must be at least 1"
            );
        }
        if self.idempotency_cache_ttl_ms == 0 {
            anyhow::bail!(
                "[quota-enforcement.operations].idempotency_cache_ttl_ms must be at least 1"
            );
        }
        Ok(())
    }

    /// How long a cached record may answer.
    #[must_use]
    pub const fn idempotency_cache_ttl(&self) -> Duration {
        Duration::from_millis(self.idempotency_cache_ttl_ms)
    }
}

/// Timing of the retention sweeper (`[quota-enforcement.retention]`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSection {
    /// Seconds between sweeps.
    pub interval_secs: u64,
    /// Rows one delete statement reclaims.
    pub batch_size: u32,
    /// Days operation-log rows are kept. Idempotency retention is
    /// per-`(tenant, metric)` configuration the storage plugin reads itself.
    pub operation_log_retention_days: u32,
}

impl Default for RetentionSection {
    fn default() -> Self {
        Self {
            interval_secs: 300,
            batch_size: 1_000,
            operation_log_retention_days: 30,
        }
    }
}

impl RetentionSection {
    /// Reject timings the sweeper cannot run with.
    ///
    /// # Errors
    ///
    /// Returns an error naming the field that is out of its range.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.interval_secs == 0 {
            anyhow::bail!("[quota-enforcement.retention].interval_secs must be at least 1");
        }
        if self.batch_size == 0 {
            anyhow::bail!("[quota-enforcement.retention].batch_size must be at least 1");
        }
        if self.operation_log_retention_days == 0 {
            anyhow::bail!(
                "[quota-enforcement.retention].operation_log_retention_days must be at least 1"
            );
        }
        Ok(())
    }

    /// The domain view of the sweeper timing.
    ///
    /// # Errors
    ///
    /// Returns an error when a bound is zero, which [`Self::validate`] has
    /// already rejected at startup.
    pub fn to_timing(&self) -> anyhow::Result<crate::domain::operations::RetentionTiming> {
        Ok(crate::domain::operations::RetentionTiming {
            interval: Duration::from_secs(self.interval_secs),
            batch_size: std::num::NonZeroU32::new(self.batch_size)
                .ok_or_else(|| anyhow::anyhow!("retention batch size must be at least 1"))?,
            operation_log_retention: time::Duration::days(i64::from(
                self.operation_log_retention_days,
            )),
        })
    }
}
