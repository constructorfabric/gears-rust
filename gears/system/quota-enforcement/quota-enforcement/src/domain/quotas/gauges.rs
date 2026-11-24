//! Storage-backed refresh of the lifecycle gauges (`features/quota-lifecycle.md`,
//! "Lifecycle Telemetry Gauges").
//!
//! The elected replica runs [`LifecycleGaugeRefresher::run`] as leader work.
//! Every `refresh` interval it reads the active-Quota counts from storage,
//! takes each metric's `enforcement` from its catalogue entry, and publishes
//! one sample to the sink; the observable gauges read that sample without any
//! I/O. A sample is published only after a refresh that succeeded in the
//! current leadership term. Failures keep the last sample until `stale_after`
//! has passed without a success, then withdraw it. Cancellation, an abort, or
//! a drop of the future withdraws the sample through a guard, and a refresh
//! that completes after cancellation is discarded.

use std::future::{Future, pending};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::{MetricId, QuotaEnforcementStoragePluginV1, QuotaMetadataViolation};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;

use crate::domain::catalog::{CatalogEntry, MetricCatalog, MetricMode};
use crate::domain::error::DomainError;
use crate::domain::ports::coordination::{LeaderWork, LeaderWorkFuture};
use crate::domain::ports::lifecycle_gauges::{LifecycleCounts, LifecycleGaugeSink};
use crate::domain::ports::metrics::ValidationReason;

const LOG_TARGET: &str = "qe.lifecycle_gauges";

/// Timing of the refresh.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GaugeTiming {
    /// Interval between two refreshes.
    pub refresh: Duration,
    /// Budget of one refresh, storage read and catalogue reads together.
    pub refresh_deadline: Duration,
    /// Time without a successful refresh after which the sample is withdrawn.
    pub stale_after: Duration,
}

/// Why one refresh produced no sample.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefreshError {
    /// Storage did not answer.
    #[error("storage read failed: {0}")]
    Storage(DomainError),
    /// The refresh exceeded its deadline.
    #[error("refresh exceeded {0:?}")]
    TimedOut(Duration),
    /// Leadership or the process ended mid-refresh.
    #[error("refresh cancelled")]
    Cancelled,
}

/// The refresh, one instance per process.
#[domain_model]
pub struct LifecycleGaugeRefresher {
    storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
    catalog: Arc<MetricCatalog>,
    sink: Arc<dyn LifecycleGaugeSink>,
    timing: GaugeTiming,
}

type Refresh = Pin<Box<dyn Future<Output = Result<LifecycleCounts, RefreshError>> + Send>>;
type Timer = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Withdraws the sample when the refresh loop ends for any reason: return,
/// cancellation, or a drop of the future by an abort.
struct Withdraw<'a>(&'a dyn LifecycleGaugeSink);

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        self.0.publish(None);
    }
}

fn never() -> Timer {
    Box::pin(pending())
}

impl LifecycleGaugeRefresher {
    /// Assemble the refresh.
    #[must_use]
    pub fn new(
        storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
        catalog: Arc<MetricCatalog>,
        sink: Arc<dyn LifecycleGaugeSink>,
        timing: GaugeTiming,
    ) -> Arc<Self> {
        Arc::new(Self {
            storage,
            catalog,
            sink,
            timing,
        })
    }

    /// One refresh: the storage read and the catalogue entry of each metric,
    /// bounded by the deadline and raced against `cancel`. Publishes nothing.
    ///
    /// # Errors
    ///
    /// The [`RefreshError`] that prevented a fresh sample.
    pub async fn refresh_once(
        &self,
        cancel: &CancellationToken,
    ) -> Result<LifecycleCounts, RefreshError> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(RefreshError::Cancelled),
            outcome = tokio::time::timeout(self.timing.refresh_deadline, self.collect()) => {
                outcome.map_err(|_elapsed| RefreshError::TimedOut(self.timing.refresh_deadline))?
            }
        }
    }

    async fn collect(&self) -> Result<LifecycleCounts, RefreshError> {
        let counts = self
            .storage
            .read_active_quota_counts()
            .await
            .map_err(|e| RefreshError::Storage(DomainError::from(e)))?;
        // The cap gauges need no metric type, so a registry failure does not
        // cost the sample: a metric this replica does not hold is left out of
        // the two gauges that read its type.
        let loaded = self
            .catalog
            .load(counts.by_metric.keys().map(MetricId::as_gts))
            .await;
        if let Err(error) = &loaded {
            tracing::warn!(
                target: LOG_TARGET,
                error = %error,
                "could not read the metric types of the active Quotas; metrics this replica \
                 does not hold are left out of the sample"
            );
        }
        let mut for_direct_metric = 0u64;
        for (metric, active) in &counts.by_metric {
            match self.catalog.peek(metric) {
                Some(CatalogEntry::Served(served)) => {
                    if served.mode() == MetricMode::Direct {
                        for_direct_metric = for_direct_metric.saturating_add(*active);
                    }
                }
                // Its mode is unknown, so `for_direct_metric` cannot count
                // these Quotas; the sample says so in the log.
                Some(CatalogEntry::Unavailable { reason }) => {
                    warn_unavailable(metric, *active, reason);
                }
                None if loaded.is_ok() => warn_removed(metric, *active),
                None => {}
            }
        }
        // The owner may have revised a type since a Quota was written: its
        // conditions are checked again against the type as it is now.
        let mut conditions_unfit = 0u64;
        for set in &counts.conditions {
            if let Some(CatalogEntry::Served(served)) = self.catalog.peek(&set.metric) {
                let unfit = super::metadata::violations(served.schema(), &set.metadata);
                if let Some(first) = unfit.first() {
                    conditions_unfit = conditions_unfit.saturating_add(set.quotas);
                    warn_unfit(&set.metric, set.quotas, first);
                }
            }
        }
        Ok(LifecycleCounts {
            cap_zero: counts.cap_zero,
            cap_unbounded: counts.cap_unbounded,
            for_direct_metric,
            conditions_unfit,
        })
    }

    /// The leader body: refresh on a schedule until `cancel` fires. Expiry,
    /// the schedule, an in-flight refresh, and cancellation are all live in
    /// every phase; the sample is withdrawn on exit however the loop ends.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let _withdraw = Withdraw(self.sink.as_ref());
        let mut refreshing = false;
        let mut in_flight: Refresh = Box::pin(pending());
        let mut expiry: Timer = never();
        let mut tick: Timer = Box::pin(tokio::time::sleep(Duration::ZERO));
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = &mut expiry => {
                    expiry = self.withdraw_stale();
                }
                outcome = &mut in_flight, if refreshing => {
                    refreshing = false;
                    in_flight = Box::pin(pending());
                    if let Some(rearmed) = self.absorb(outcome) {
                        expiry = rearmed;
                    }
                    tick = Box::pin(tokio::time::sleep(self.timing.refresh));
                }
                () = &mut tick, if !refreshing => {
                    refreshing = true;
                    let this = Arc::clone(&self);
                    let cancel = cancel.clone();
                    in_flight = Box::pin(async move { this.refresh_once(&cancel).await });
                    tick = never();
                }
            }
        }
    }

    /// No success for `stale_after`: withdraw the sample and disarm the timer,
    /// so an elapsed timer can never fire again or cancel a recovery refresh.
    fn withdraw_stale(&self) -> Timer {
        self.sink.publish(None);
        tracing::warn!(
            target: LOG_TARGET,
            stale_after = ?self.timing.stale_after,
            "lifecycle gauge sample withdrawn: no successful refresh"
        );
        never()
    }

    /// Publish a successful refresh and re-arm expiry; keep the last sample on
    /// a failure. Returns the new expiry timer when one was armed.
    fn absorb(&self, outcome: Result<LifecycleCounts, RefreshError>) -> Option<Timer> {
        match outcome {
            Ok(counts) => {
                self.sink.publish(Some(counts));
                Some(Box::pin(tokio::time::sleep(self.timing.stale_after)))
            }
            Err(err) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %err,
                    "lifecycle gauge refresh failed; keeping the last sample"
                );
                None
            }
        }
    }

    /// The body as the coordinator runs it.
    #[must_use]
    pub fn leader_work(self: Arc<Self>) -> LeaderWork {
        Arc::new(move |cancel: CancellationToken| -> LeaderWorkFuture {
            let this = Arc::clone(&self);
            Box::pin(this.run(cancel))
        })
    }
}

fn warn_unavailable(metric: &MetricId, active: u64, reason: ValidationReason) {
    tracing::warn!(
        target: LOG_TARGET,
        metric = %metric,
        active_quotas = active,
        %reason,
        "active Quotas reference an unavailable metric; the direct-metric gauge leaves them out"
    );
}

fn warn_unfit(metric: &MetricId, active: u64, first: &QuotaMetadataViolation) {
    tracing::warn!(
        target: LOG_TARGET,
        metric = %metric,
        active_quotas = active,
        path = %first.path,
        reason = %first.reason,
        "the conditions of active quotas no longer fit the metric type: \
         these quotas may not apply to requests"
    );
}

/// The runtime half of "a persisted Quota whose metric was later removed is
/// flagged, never deactivated": every refresh names such metrics.
fn warn_removed(metric: &MetricId, active: u64) {
    tracing::warn!(
        target: LOG_TARGET,
        metric = %metric,
        active_quotas = active,
        "active Quotas reference a metric the types registry no longer knows"
    );
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gauges_tests.rs"]
mod gauges_tests;
