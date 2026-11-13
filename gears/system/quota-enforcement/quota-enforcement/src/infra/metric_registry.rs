//! `CachedMetricRegistry`: the [`MetricRegistry`] port over the platform
//! `types-registry` SDK, with the bounded in-process cache the
//! quota-lifecycle feature requires ("Metric Validation"). A request's miss
//! reads through `TypesRegistryApi` under the caller's context; the gear's own
//! through `PlatformTypesRegistryApi`. Both are consumed contracts, resolved
//! from the hub per call. A deleted metric instance is unregistered.
//!
//! Every answer is read from the metric instance document the registry serves
//! and parsed into closed enums; a registered metric whose document lacks a
//! usable classification is an error, never a default. Answers are cached by
//! metric id with least-recently-used eviction and refreshed after `ttl`.
//! Only a global metric is cached: every caller sees it alike. One a tenant
//! owns is answered to its reader alone and re-read on every call (the SDK
//! caches it per reader tenant). When the registry does not answer, a cached
//! entry is served as `Stale` only while it is younger than
//! `ttl + stale_grace`; beyond that the caller fails closed. A registry that
//! refuses a request's caller (401/403) answered: the refusal is returned, never
//! a stale entry. Negative answers (`NotFound`, another type) are never cached,
//! so a late registration is seen at once. Every failure lifts through a named
//! function (DE1302).
//!
//! # Provisional classification contract
//!
//! The metric base `gts.cf.core.qe.metric_type.v1~` is a platform namespace (PRD
//! sections 3.2 and 13). Until the platform publishes its schema, QE reads two
//! fields of the instance document, named in [`contract`]:
//! `kind ∈ {counter, gauge}` and `enforcement ∈ {quota_gated, direct}`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use gts::GtsInstanceId;
use quota_enforcement_sdk::{METRIC_BASE_TYPE, MetricId, MetricKind};
use serde_json::Value;
use tokio::time::Instant;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, Instance, LifecycleStatus, Ownership, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, ReadOptions, TypesRegistryApi, TypesRegistryApiExt,
};

use crate::domain::error::DomainError;
use crate::domain::ports::contracts::RegistryCaller;
use crate::domain::ports::metric_registry::{
    Classified, Freshness, MetricDescriptor, MetricMode, MetricRegistry,
};
use crate::infra::types_registry::refused_caller;

const LOG_TARGET: &str = "qe.metrics_registry";

/// Default budget for one registry call.
pub const DEFAULT_REGISTRY_DEADLINE: Duration = Duration::from_secs(10);

/// Field names and values of the provisional metric classification contract.
pub mod contract {
    /// The metric kind field of an instance document.
    pub const KIND: &str = "kind";
    /// `kind` value of a cumulative metric.
    pub const KIND_COUNTER: &str = "counter";
    /// `kind` value of a level metric.
    pub const KIND_GAUGE: &str = "gauge";
    /// The enforcement-mode field of an instance document.
    pub const ENFORCEMENT: &str = "enforcement";
    /// `enforcement` value of a metric whose usage flows through QE.
    pub const ENFORCEMENT_QUOTA_GATED: &str = "quota_gated";
    /// `enforcement` value of a metric recorded directly.
    pub const ENFORCEMENT_DIRECT: &str = "direct";
}

/// The bounded cache and its adapter.
pub struct CachedMetricRegistry {
    hub: Arc<ClientHub>,
    deadline: Duration,
    ttl: Duration,
    stale_grace: Duration,
    cache: Mutex<MetricCache>,
}

struct CacheEntry {
    descriptor: MetricDescriptor,
    fetched_at: Instant,
    last_used: u64,
}

struct MetricCache {
    capacity: usize,
    tick: u64,
    entries: HashMap<MetricId, CacheEntry>,
}

impl MetricCache {
    fn touch(&mut self, metric: &MetricId) -> Option<&CacheEntry> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(metric)?;
        entry.last_used = tick;
        Some(entry)
    }

    fn insert(&mut self, metric: MetricId, descriptor: MetricDescriptor, now: Instant) {
        // Evict the least recently used entry. Linear in the capacity, which
        // is a small configured bound.
        if !self.entries.contains_key(&metric)
            && self.entries.len() >= self.capacity
            && let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| id.clone())
        {
            self.entries.remove(&victim);
        }
        self.tick += 1;
        self.entries.insert(
            metric,
            CacheEntry {
                descriptor,
                fetched_at: now,
                last_used: self.tick,
            },
        );
    }
}

impl CachedMetricRegistry {
    /// Bind to the registry clients of `hub` with a cache of `capacity` entries refreshed after
    /// `ttl` and served stale for at most `stale_grace` beyond that.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, capacity: usize, ttl: Duration, stale_grace: Duration) -> Self {
        Self {
            hub,
            deadline: DEFAULT_REGISTRY_DEADLINE,
            ttl,
            stale_grace,
            cache: Mutex::new(MetricCache {
                capacity: capacity.max(1),
                tick: 0,
                entries: HashMap::new(),
            }),
        }
    }

    /// Override the budget one registry call may take.
    #[must_use]
    pub const fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MetricCache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A cached descriptor younger than `ttl`.
    fn fresh_hit(&self, metric: &MetricId, now: Instant) -> Option<MetricDescriptor> {
        let mut cache = self.lock();
        let entry = cache.touch(metric)?;
        (now.duration_since(entry.fetched_at) <= self.ttl).then_some(entry.descriptor)
    }

    /// A cached descriptor within the stale grace, served because the
    /// registry did not answer; otherwise the failure itself.
    fn stale_or(
        &self,
        metric: &MetricId,
        now: Instant,
        failure: DomainError,
    ) -> Result<Option<Classified>, DomainError> {
        let stale = {
            let mut cache = self.lock();
            cache.touch(metric).and_then(|entry| {
                (now.duration_since(entry.fetched_at) <= self.ttl + self.stale_grace)
                    .then_some(entry.descriptor)
            })
        };
        match stale {
            Some(descriptor) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    metric = %metric,
                    error = %failure,
                    "types registry did not answer; serving the cached metric classification"
                );
                Ok(Some(Classified {
                    descriptor,
                    freshness: Freshness::Stale,
                }))
            }
            None => Err(failure),
        }
    }

    fn evict(&self, metric: &MetricId) {
        self.lock().entries.remove(metric);
    }

    /// The metric instance with its content, read by `caller`.
    async fn fetch(
        &self,
        caller: RegistryCaller<'_>,
        id: &GtsInstanceId,
    ) -> Result<Instance, CanonicalError> {
        let options =
            ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
            ])));
        let unwired = |e: &dyn std::fmt::Display| {
            CanonicalError::service_unavailable()
                .with_detail(format!("types registry client unavailable: {e}"))
                .create()
        };
        match caller {
            RegistryCaller::Platform => {
                let platform = self
                    .hub
                    .get::<dyn PlatformTypesRegistryApi>()
                    .map_err(|e| unwired(&e))?;
                platform
                    .get_instance(&PlatformSecurityContext::outbound_marker(), id, options)
                    .await
            }
            RegistryCaller::Request(ctx) => {
                let tenant = self
                    .hub
                    .get::<dyn TypesRegistryApi>()
                    .map_err(|e| unwired(&e))?;
                tenant.get_instance(ctx, id, options).await
            }
        }
    }
}

/// The classification an instance document declares, if usable.
fn parse_classification(object: &Value) -> Option<MetricDescriptor> {
    let kind = match object.get(contract::KIND)?.as_str()? {
        contract::KIND_COUNTER => MetricKind::Counter,
        contract::KIND_GAUGE => MetricKind::Gauge,
        _ => return None,
    };
    let mode = match object.get(contract::ENFORCEMENT)?.as_str()? {
        contract::ENFORCEMENT_QUOTA_GATED => MetricMode::QuotaGated,
        contract::ENFORCEMENT_DIRECT => MetricMode::Direct,
        _ => return None,
    };
    Some(MetricDescriptor { kind, mode })
}

#[async_trait]
impl MetricRegistry for CachedMetricRegistry {
    async fn describe(
        &self,
        caller: RegistryCaller<'_>,
        metric: &MetricId,
    ) -> Result<Option<Classified>, DomainError> {
        let now = Instant::now();
        if let Some(descriptor) = self.fresh_hit(metric, now) {
            return Ok(Some(Classified {
                descriptor,
                freshness: Freshness::Fresh,
            }));
        }
        let request = matches!(caller, RegistryCaller::Request(_));
        match tokio::time::timeout(self.deadline, self.fetch(caller, metric.as_gts())).await {
            Ok(Ok(instance)) => {
                // A deleted instance is no longer registered.
                if instance.lifecycle_status != LifecycleStatus::Active
                    || instance.type_id.as_ref() != METRIC_BASE_TYPE
                {
                    self.evict(metric);
                    return Ok(None);
                }
                let Some(descriptor) = instance.content.as_ref().and_then(parse_classification)
                else {
                    self.evict(metric);
                    return Err(classification_invalid(metric));
                };
                // A tenant-owned metric is its reader's alone: a cached entry
                // would answer every later caller.
                if instance.ownership == Ownership::Global {
                    self.lock().insert(metric.clone(), descriptor, now);
                }
                Ok(Some(Classified {
                    descriptor,
                    freshness: Freshness::Fresh,
                }))
            }
            Ok(Err(CanonicalError::NotFound { .. })) => {
                self.evict(metric);
                Ok(None)
            }
            Ok(Err(err)) => match refused_caller(&err).filter(|_| request) {
                Some(refusal) => Err(refusal),
                None => self.stale_or(metric, now, unavailable(&err)),
            },
            Err(_elapsed) => self.stale_or(metric, now, timed_out(self.deadline)),
        }
    }
}

/// A registered metric without a usable classification. Logged with the field
/// names so the owner knows what to publish.
fn classification_invalid(metric: &MetricId) -> DomainError {
    tracing::warn!(
        target: LOG_TARGET,
        metric = %metric,
        kind_field = contract::KIND,
        enforcement_field = contract::ENFORCEMENT,
        "metric instance carries no usable classification"
    );
    DomainError::MetricClassificationInvalid {
        metric: metric.as_str().to_owned(),
    }
}

/// A transport or registry failure. The cause is kept as text: the domain
/// error is `Clone + Eq` and crosses the layer boundary as a value.
fn unavailable(err: &CanonicalError) -> DomainError {
    DomainError::TypesRegistryUnavailable(format!("types registry `get_instance` failed: {err}"))
}

fn timed_out(deadline: Duration) -> DomainError {
    DomainError::TypesRegistryUnavailable(format!(
        "types registry `get_instance` exceeded {deadline:?}"
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metric_registry_tests.rs"]
mod metric_registry_tests;
