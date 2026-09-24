//! The metric classification snapshot the evaluation path reads.
//!
//! `types-registry` is the authoritative catalogue, and QE builds local
//! snapshots from it at bootstrap and at Quota and Policy writes. The
//! evaluation path does not call the registry
//! (`cpt-cf-quota-enforcement-constraint-types-registry-delegation`), so the
//! classification of every admitted metric is read once, while the gear is
//! still starting, and held for the life of the process.
//!
//! A registry that cannot answer fails bootstrap: starting with an unknown hot
//! path would turn every guarded operation into a 503 at run time. A metric the
//! registry has *removed*, by contrast, is simply left out of the snapshot and
//! the deployment starts, which is the established rule for a Quota stranded on
//! a removed metric. Leaving it out is still fail-closed: a debit on it is
//! refused as unregistered rather than treated as gated.

use std::collections::HashMap;

use quota_enforcement_sdk::MetricId;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::ports::metric_registry::{MetricDescriptor, MetricMode, MetricRegistry};
use crate::domain::ports::metrics::MetricLabel;

/// Classification of every metric the catalogue admits, frozen at bootstrap.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct MetricClassifications {
    by_metric: HashMap<MetricId, MetricDescriptor>,
    /// The telemetry label of each classified metric, built once.
    labels: HashMap<MetricId, MetricLabel>,
}

impl MetricClassifications {
    /// Read the classification of every admitted metric.
    ///
    /// # Errors
    ///
    /// The registry's own error when it cannot answer. A metric it no longer
    /// knows is left out of the snapshot instead, and every operation on it is
    /// then refused as unregistered.
    pub async fn load(
        metrics: impl IntoIterator<Item = MetricId>,
        registry: &dyn MetricRegistry,
    ) -> Result<Self, DomainError> {
        let mut by_metric = HashMap::new();
        let mut absent = Vec::new();
        for metric in metrics {
            if by_metric.contains_key(&metric) || absent.contains(&metric) {
                continue;
            }
            if let Some(classified) = registry.describe(&metric).await? {
                by_metric.insert(metric, classified.descriptor);
            } else {
                tracing::warn!(
                    target: "qe.bootstrap",
                    metric = %metric,
                    "the types registry no longer knows this metric; operations on it \
                     are refused and Quotas bound to it are inert"
                );
                absent.push(metric);
            }
        }
        Ok(Self::of(by_metric))
    }

    fn of(by_metric: HashMap<MetricId, MetricDescriptor>) -> Self {
        let labels = by_metric
            .keys()
            .map(|metric| (metric.clone(), MetricLabel::admitted(metric)))
            .collect();
        Self { by_metric, labels }
    }

    /// A snapshot built from known classifications, for tests and for the
    /// in-memory composition the bench harness uses.
    #[must_use]
    pub fn from_pairs(pairs: impl IntoIterator<Item = (MetricId, MetricDescriptor)>) -> Self {
        Self::of(pairs.into_iter().collect())
    }

    /// The classification of `metric`, if the catalogue admits it.
    #[must_use]
    pub fn describe(&self, metric: &MetricId) -> Option<MetricDescriptor> {
        self.by_metric.get(metric).copied()
    }

    /// The telemetry label of `metric`, if the snapshot classified it. The
    /// label set is closed at bootstrap, so a metric outside it has none and
    /// is left out of the metric-labelled instruments.
    #[must_use]
    pub fn label(&self, metric: &MetricId) -> Option<MetricLabel> {
        self.labels.get(metric).cloned()
    }

    /// The labels of every quota-gated metric in the snapshot: the metrics a
    /// lease can be held on, which a backlog gauge reports even at zero.
    pub fn quota_gated_labels(&self) -> impl Iterator<Item = &MetricLabel> {
        self.labels.iter().filter_map(|(metric, label)| {
            self.by_metric
                .get(metric)
                .filter(|descriptor| descriptor.mode != MetricMode::Direct)
                .map(|_| label)
        })
    }

    /// Refuse a metric whose usage does not flow through Quota Enforcement.
    ///
    /// # Errors
    ///
    /// - [`DomainError::MetricNotRegistered`] when the catalogue does not admit
    ///   it, which is also what an unknown metric looks like from here.
    /// - [`DomainError::MetricNotQuotaGated`] when it is recorded directly, so
    ///   Quotas on it are inert and a debit would mean nothing.
    pub fn ensure_quota_gated(&self, metric: &MetricId) -> Result<(), DomainError> {
        let descriptor = self
            .describe(metric)
            .ok_or_else(|| DomainError::MetricNotRegistered {
                metric: metric.as_str().to_owned(),
            })?;
        if descriptor.mode == MetricMode::Direct {
            return Err(DomainError::MetricNotQuotaGated {
                metric: metric.as_str().to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "classifications_tests.rs"]
mod tests;
