//! Replica cache coherence, the broker-free half: what a request touched, what
//! becomes of each publish, and what a replica evicts when one arrives.
//!
//! Everything here is pure or reaches the broker through [`BroadcastSink`], so
//! the rules run without one. Building the events (the envelope, the size
//! accounting, the split) belongs to the broker client in `infra::broadcast`;
//! the sink keeps the broker SDK's `Event` as the unit it publishes.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
// The broker-native event is the unit a sink publishes and an outcome reports
// on, so the broker SDK's `Event` is the one type of it this module names.
use event_broker_sdk::Event;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::domain::resolution::{EffectiveCache, scope_class};
use toolkit_macros::domain_model;

mod publisher;

pub use publisher::{BroadcastGateway, CoherencePublisher, correlation_id};
#[cfg(test)]
pub mod testing;

// ---------------------------------------------------------------------------
// Accumulation
// ---------------------------------------------------------------------------

/// Where a change landed: a named tenant, or the root tenant, whose id only
/// the broker client knows and the Reconciler and writers never resolve.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// A tenant written at.
    Tenant(Uuid),
    /// The root tenant: a platform-scope write or a declaration change.
    Root,
}

/// One change of a request, in the order the request evaluated it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeOutcome {
    /// The setting key.
    pub key: String,
    /// Where the change landed.
    pub target: Target,
    /// Whether it committed; a refused change has nothing to evict.
    pub committed: bool,
}

impl ChangeOutcome {
    /// A value write: at the target tenant (a clone's target, never its
    /// source), or at the root for a `global` one, which is a platform-scope
    /// write whatever tenant the request named.
    #[must_use]
    pub fn value_write(
        key: &str,
        tenant: Uuid,
        declaration_scope_class: &str,
        committed: bool,
    ) -> Self {
        let target = if declaration_scope_class == scope_class::GLOBAL {
            Target::Root
        } else {
            Target::Tenant(tenant)
        };
        Self {
            key: key.to_owned(),
            target,
            committed,
        }
    }

    /// A committed declaration retire, reactivation, evolve or
    /// reclassification, by either path: at the root tenant.
    #[must_use]
    pub fn declaration_change(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            target: Target::Root,
            committed: true,
        }
    }
}

/// One tenant of a broadcast: the bare lowercase UUID string the body carries
/// and the request's changed keys there, each once, in first-touch order.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantKeys {
    /// The tenant, a bare lowercase UUID string.
    pub tenant: String,
    /// The keys changed there.
    pub keys: Vec<String>,
}

/// The request-scoped accumulator of the tenants a request touched.
#[domain_model]
#[derive(Debug, Default)]
pub struct TouchedTenants {
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-1
    entries: Vec<TouchedEntry>,
    index: HashMap<Target, usize>,
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-1
}

/// One touched target: its keys in first-touch order, and the set that keeps
/// each of them once without a scan of the list.
#[domain_model]
#[derive(Debug)]
struct TouchedEntry {
    target: Target,
    keys: Vec<String>,
    seen: HashSet<String>,
}

impl TouchedTenants {
    /// An empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the outcomes of a request's changes, in order.
    pub fn record_all(&mut self, outcomes: impl IntoIterator<Item = ChangeOutcome>) {
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-2
        for outcome in outcomes {
            self.record(outcome);
        }
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-2
    }

    /// Add one change outcome.
    pub fn record(&mut self, outcome: ChangeOutcome) {
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-3
        if !outcome.committed {
            return;
        }
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-3
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-4
        let slot = *self.index.entry(outcome.target).or_insert_with(|| {
            self.entries.push(TouchedEntry {
                target: outcome.target,
                keys: Vec::new(),
                seen: HashSet::new(),
            });
            self.entries.len() - 1
        });
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-4
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-5
        let entry = &mut self.entries[slot];
        if entry.seen.insert(outcome.key.clone()) {
            entry.keys.push(outcome.key);
        }
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-5
    }

    /// Whether no change committed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many tenants the request touched, the root counted once while its
    /// id is unknown: what a request that cannot publish counts `skipped`.
    #[must_use]
    pub fn touched_count(&self) -> usize {
        self.entries.len()
    }

    /// The collection, with the root tenant's id substituted where a change
    /// landed there; a named tenant that is the root merges with it, at the
    /// position of the first touch.
    #[must_use]
    pub fn resolve(&self, root_tenant: Uuid) -> Vec<TenantKeys> {
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-6
        // Each output tenant carries the set of its keys, so a merge of the root
        // with a named tenant that is the root never rescans a list.
        let mut out: Vec<(TenantKeys, HashSet<String>)> = Vec::new();
        for entry in &self.entries {
            let tenant = match entry.target {
                Target::Tenant(id) => id,
                Target::Root => root_tenant,
            }
            .as_hyphenated()
            .to_string();
            if let Some((existing, seen)) = out.iter_mut().find(|(e, _)| e.tenant == tenant) {
                for key in &entry.keys {
                    if seen.insert(key.clone()) {
                        existing.keys.push(key.clone());
                    }
                }
            } else {
                out.push((
                    TenantKeys {
                        tenant,
                        keys: entry.keys.clone(),
                    },
                    entry.seen.clone(),
                ));
            }
        }
        out.into_iter().map(|(tenant, _)| tenant).collect()
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-accumulate:p1:inst-rc-accumulate-6
    }
}

// ---------------------------------------------------------------------------
// What a request's broadcasts share
// ---------------------------------------------------------------------------

/// What every event of one request shares.
#[domain_model]
#[derive(Debug, Clone)]
pub struct BroadcastContext {
    /// The root tenant's id: the envelope `tenant_id`.
    pub root_tenant: Uuid,
    /// The request's correlation id, or the id a reconcile pass minted.
    pub change_set_id: Uuid,
    /// The request's trace context, when one is present.
    pub trace_parent: Option<String>,
}

// ---------------------------------------------------------------------------
// The broadcast outcome
// ---------------------------------------------------------------------------

/// Why a publish did not complete.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishError {
    /// The broker refused it, a missing `produce` grant included.
    Refused(String),
    /// The broker did not accept it within `publish_timeout_seconds`.
    TimedOut,
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "refused: {reason}"),
            Self::TimedOut => f.write_str("not accepted within the publish timeout"),
        }
    }
}

/// The publishing end of the broker client: hands the broker one event and
/// waits at most `publish_timeout_seconds` for its acceptance.
#[async_trait]
pub trait BroadcastSink: Send + Sync {
    /// Publish `event` under the publishing identity.
    async fn publish(&self, event: Event) -> Result<(), PublishError>;
}

/// The result of one broadcast.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastResult {
    /// The broker accepted it.
    Sent,
    /// It was never attempted.
    Skipped,
    /// It was refused or not accepted in time.
    Failed,
}

impl BroadcastResult {
    /// The `result` label of `settings_cache_broadcast_total`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// `settings_cache_broadcast_total{result}`.
pub trait BroadcastMetrics: Send + Sync {
    /// Count one broadcast under `result` (`sent`, `skipped` or `failed`).
    fn broadcast(&self, result: &'static str);
}

/// What the broker client knows when a request ends.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastState {
    /// The cache topic and the `cache_invalidate` type are registered.
    pub types_registered: bool,
    /// The root tenant's id is cached.
    pub root_tenant_known: bool,
}

/// One request's publishing: the state it found and whether one of its
/// publishes has already failed.
#[domain_model]
pub struct RequestBroadcast<'a> {
    sink: Option<&'a dyn BroadcastSink>,
    metrics: &'a dyn BroadcastMetrics,
    state: BroadcastState,
    change_set_id: Uuid,
    failed: bool,
}

impl<'a> RequestBroadcast<'a> {
    /// A request's publishing; `sink` is `None` when no broker resolved.
    #[must_use]
    pub fn new(
        sink: Option<&'a dyn BroadcastSink>,
        metrics: &'a dyn BroadcastMetrics,
        state: BroadcastState,
        change_set_id: Uuid,
    ) -> Self {
        Self {
            sink,
            metrics,
            state,
            change_set_id,
            failed: false,
        }
    }

    /// Whether an earlier publish of this request failed.
    #[must_use]
    pub const fn has_failed(&self) -> bool {
        self.failed
    }

    /// Count `n` broadcasts `skipped` without building them: what a request
    /// does for the tenants it touched when the root tenant's id is unknown.
    pub fn skip(&self, n: usize) {
        for _ in 0..n {
            self.metrics.broadcast(BroadcastResult::Skipped.label());
        }
    }

    /// The broadcast outcome of one built event.
    pub async fn publish(&mut self, event: Event) -> BroadcastResult {
        let result = self.outcome(event).await;
        self.metrics.broadcast(result.label());
        result
    }

    /// Each event in order; see [`Self::publish`].
    pub async fn publish_all(&mut self, events: Vec<Event>) -> Vec<BroadcastResult> {
        let mut results = Vec::with_capacity(events.len());
        for event in events {
            results.push(self.publish(event).await);
        }
        results
    }

    async fn outcome(&mut self, event: Event) -> BroadcastResult {
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-1
        let Some(sink) = self
            .sink
            .filter(|_| self.state.types_registered && self.state.root_tenant_known)
        else {
            return BroadcastResult::Skipped;
        };
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-1
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-2
        if self.failed {
            return BroadcastResult::Skipped;
        }
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-2
        let change_set_id = self.change_set_id;
        let tenant = event.subject.clone();
        // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-3
        let published = sink.publish(event).await;
        // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-3
        match published {
            // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-4
            Ok(()) => BroadcastResult::Sent,
            // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-4
            // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-5
            Err(reason) => {
                tracing::warn!(
                    %change_set_id,
                    %tenant,
                    %reason,
                    "cache_invalidate broadcast dropped; peers converge within cache_ttl_seconds"
                );
                self.failed = true;
                BroadcastResult::Failed
            } // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-outcome:p1:inst-rc-outcome-5
        }
    }
}

// ---------------------------------------------------------------------------
// Consume: decode and evict key-wide
// ---------------------------------------------------------------------------

/// Evict every cached scope of each key, on this replica only.
///
/// No declaration is read: the eviction is the same whatever a key's Scope
/// Class, and the tenant a broadcast names does not narrow it.
pub fn evict_keys(cache: &EffectiveCache, keys: &[String]) {
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-1
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-2
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-3
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-4
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-5
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-7
    // Key-wide, every key in one pass over the cache: descendant scopes of a
    // cascading value, every tenant that cached a global one, and a local key's
    // other tenants all re-resolve on their next read. Each eviction moves the
    // key's generation, so a read in flight cannot store what it read before.
    cache.invalidate_keys(keys);
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-7
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-5
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-4
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-3
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-2
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-1
    // @cpt-begin:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-6
    // Only the named keys are touched; every other entry stays.
    // @cpt-end:cpt-cf-settings-service-algo-replica-coherence-evict:p1:inst-rc-evict-6
}

/// What a delivery came to.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeOutcome {
    /// The named keys were evicted (also when it names none).
    Evicted {
        /// How many keys.
        keys: usize,
    },
    /// The delivery was malformed: logged at `warn`, nothing evicted.
    Malformed,
}

/// The attributes of `data` this version reads; any other is ignored.
#[derive(Deserialize)]
struct Delivery {
    tenant: String,
    changed_keys: Vec<String>,
}

/// Decode a `cache_invalidate` delivery's `data` and evict what it names.
///
/// `event_id` is the delivered event's id, for the warning.
pub fn consume_cache_invalidation(
    cache: &EffectiveCache,
    event_id: Uuid,
    data: Option<&Value>,
) -> ConsumeOutcome {
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-3
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-4
    // `Delivery` names only what this version needs; serde skips the rest.
    let decoded = data
        .and_then(|d| Delivery::deserialize(d).ok())
        .filter(|d| Uuid::parse_str(&d.tenant).is_ok());
    let Some(delivery) = decoded else {
        tracing::warn!(
            %event_id,
            "malformed cache_invalidate delivery ignored; cache_ttl_seconds bounds what this replica serves"
        );
        return ConsumeOutcome::Malformed;
    };
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-4
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-3
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-5
    evict_keys(cache, &delivery.changed_keys);
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-5
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-6
    // Nothing to acknowledge; a repeated delivery evicts what is already gone.
    ConsumeOutcome::Evicted {
        keys: delivery.changed_keys.len(),
    }
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-6
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
