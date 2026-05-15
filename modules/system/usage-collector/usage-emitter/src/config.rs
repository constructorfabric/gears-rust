use std::time::Duration;

use serde::Deserialize;

/// Configuration for [`crate::UsageEmitterRuntime`].
///
/// Host modules embed this inside their own config struct and forward it to
/// [`crate::UsageEmitterRuntime::build`]. All fields have sensible defaults so
/// `#[serde(default)]` on the embedding struct is sufficient for zero-config usage.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UsageEmitterConfig {
    /// Maximum age of an [`crate::UsageEmitter`] handle after
    /// [`crate::UsageEmitterFactory::authorize`] before
    /// [`crate::UsageEmitter::enqueue`] / [`crate::UsageEmitter::enqueue_in`]
    /// reject it with `UsageEmitterError::Unauthenticated`.
    pub authorization_max_age: Duration,

    /// Outbox queue name for usage records delivered to the collector.
    pub outbox_queue: String,

    /// Number of outbox partitions. Must be a power of 2 in 1-64.
    pub outbox_partition_count: u16,

    /// Maximum exponential-backoff delay for outbox delivery retries.
    ///
    /// Maps to [`modkit_db::outbox::WorkerTuning::retry_max`].
    /// MUST remain below 15 minutes to satisfy `cpt-cf-usage-collector-nfr-recovery`
    /// (inst-dlv-6a / inst-emit-10a).
    pub outbox_backoff_max: Duration,

    /// Per-call timeout applied to each external call made during
    /// [`crate::UsageEmitterFactory::authorize`] — the PDP `access_scope_with` call and the
    /// [`usage_collector_sdk::UsageCollectorClientV1::get_module_config`] call.
    ///
    /// When the trait is implemented by a REST/remote client, an unresponsive backend would
    /// otherwise stall every authorize-and-emit caller indefinitely. An elapsed timeout maps to
    /// [`usage_collector_sdk::UsageRecordError::deadline_exceeded`] so it surfaces as the
    /// canonical `DeadlineExceeded` variant (HTTP 504 at the REST boundary).
    pub authorize_call_timeout: Duration,
}

impl Default for UsageEmitterConfig {
    fn default() -> Self {
        Self {
            authorization_max_age: Duration::from_secs(30),
            outbox_queue: "usage-records".to_owned(),
            outbox_partition_count: 4,
            outbox_backoff_max: Duration::from_mins(10), // 10 minutes — well below the 15-minute NFR ceiling
            authorize_call_timeout: Duration::from_secs(5),
        }
    }
}

impl UsageEmitterConfig {
    /// # Errors
    ///
    /// Returns an error when any configuration field is invalid.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.outbox_queue.trim().is_empty(),
            "outbox_queue must not be empty"
        );
        anyhow::ensure!(
            (1..=64).contains(&self.outbox_partition_count)
                && self.outbox_partition_count.is_power_of_two(),
            "outbox_partition_count must be a power of 2 in 1-64, got {}",
            self.outbox_partition_count
        );
        anyhow::ensure!(
            !self.authorization_max_age.is_zero(),
            "authorization_max_age must be > 0"
        );
        anyhow::ensure!(
            self.outbox_backoff_max > Duration::ZERO,
            "outbox_backoff_max must be greater than zero"
        );
        anyhow::ensure!(
            self.outbox_backoff_max < Duration::from_mins(15),
            "outbox_backoff_max must be below 15 minutes (cpt-cf-usage-collector-nfr-recovery), got {:?}",
            self.outbox_backoff_max
        );
        anyhow::ensure!(
            !self.authorize_call_timeout.is_zero(),
            "authorize_call_timeout must be > 0"
        );
        anyhow::ensure!(
            self.authorize_call_timeout <= Duration::from_secs(30),
            "authorize_call_timeout must not exceed 30s, got {:?}",
            self.authorize_call_timeout
        );

        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
