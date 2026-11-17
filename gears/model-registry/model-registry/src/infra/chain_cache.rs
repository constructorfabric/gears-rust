//! In-process ancestor-chain cache (DESIGN §3.2 Chain Cache).
//!
//! Maps a tenant id to its chain `[T0, a1, …, ak]` (T0 first, root last). A
//! miss or an expired entry calls Tenant Resolver once per tenant at a time
//! (single flight), bounded by the configured deadline, with the default
//! `BarrierMode::Respect`. Expiry counts from the start of that call.
//! Failures are not cached: a failed call is reported to the callers that
//! waited on it, and the next caller calls again. TTL 0 disables caching.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use lru::LruCache;
use tenant_resolver_sdk::{GetAncestorsOptions, TenantId, TenantResolverClient};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::clock::Clock;
use crate::domain::error::{Dependency, DomainError, Resource};

/// The TTL of each stored entry is reduced by a random jitter of up to
/// `ttl / JITTER_DIVISOR` (10%).
const JITTER_DIVISOR: u128 = 10;

struct Entry {
    chain: Arc<[Uuid]>,
    expires_at: DateTime<Utc>,
}

/// A failed flight, kept for the callers that waited on it.
#[derive(Clone)]
enum Failure {
    NotPermitted {
        resource: Resource,
        operation: &'static str,
    },
    Unavailable {
        dependency: Dependency,
        diagnostic: String,
    },
}

impl Failure {
    fn of(err: &DomainError) -> Option<Self> {
        match err {
            DomainError::NotPermitted {
                resource,
                operation,
            } => Some(Self::NotPermitted {
                resource: *resource,
                operation,
            }),
            DomainError::DependencyUnavailable {
                dependency,
                diagnostic,
            } => Some(Self::Unavailable {
                dependency: *dependency,
                diagnostic: diagnostic.clone(),
            }),
            _ => None,
        }
    }

    fn into_error(self) -> DomainError {
        match self {
            Self::NotPermitted {
                resource,
                operation,
            } => DomainError::NotPermitted {
                resource,
                operation,
            },
            Self::Unavailable {
                dependency,
                diagnostic,
            } => DomainError::DependencyUnavailable {
                dependency,
                diagnostic,
            },
        }
    }
}

/// The single-flight gate of one tenant. The lock serializes the Tenant
/// Resolver calls and holds the last flight's failure with its number.
#[derive(Default)]
struct Gate {
    flight: tokio::sync::Mutex<Option<(u64, Failure)>>,
    /// Number of finished flights.
    finished: AtomicU64,
}

/// A caller's reference to a tenant gate. Dropping it, on return or on
/// cancellation, releases the gate.
struct GateGuard<'a> {
    cache: &'a ChainCache,
    tenant_id: Uuid,
    gate: Arc<Gate>,
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        self.cache.release_gate(self.tenant_id);
    }
}

pub struct ChainCache {
    /// `None` when caching is disabled (TTL 0).
    entries: Option<Mutex<LruCache<Uuid, Entry>>>,
    inflight: Mutex<HashMap<Uuid, Arc<Gate>>>,
    ttl: Duration,
    deadline: Duration,
    clock: Arc<dyn Clock>,
    resolver: Arc<dyn TenantResolverClient>,
}

impl ChainCache {
    /// `max_entries` must be greater than 0; the config check guarantees it.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn TenantResolverClient>,
        clock: Arc<dyn Clock>,
        ttl: Duration,
        max_entries: usize,
        deadline: Duration,
    ) -> Self {
        let entries = (!ttl.is_zero())
            .then(|| NonZeroUsize::new(max_entries))
            .flatten()
            .map(|cap| Mutex::new(LruCache::new(cap)));
        Self {
            entries,
            inflight: Mutex::new(HashMap::new()),
            ttl,
            deadline,
            clock,
            resolver,
        }
    }

    /// The caller's chain, T0 first. On a miss the caller waits for the
    /// Tenant Resolver call; concurrent misses for the same tenant share it.
    pub async fn chain(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Arc<[Uuid]>, DomainError> {
        if let Some(chain) = self.lookup(tenant_id) {
            tracing::debug!(%tenant_id, "chain cache hit");
            return Ok(chain);
        }
        if self.entries.is_none() {
            return self.fetch(ctx, tenant_id).await;
        }
        let gate = GateGuard {
            cache: self,
            tenant_id,
            gate: self.gate(tenant_id),
        };
        let finished_before = gate.gate.finished.load(Ordering::Acquire);
        let mut flight = gate.gate.flight.lock().await;
        // A flight that failed while this caller waited is its answer too.
        if let Some((number, failure)) = &*flight
            && *number > finished_before
        {
            tracing::debug!(%tenant_id, "chain cache shared a failed flight");
            return Err(failure.clone().into_error());
        }
        let result = self.fill(ctx, tenant_id).await;
        let number = gate.gate.finished.fetch_add(1, Ordering::AcqRel) + 1;
        *flight = result
            .as_ref()
            .err()
            .and_then(Failure::of)
            .map(|failure| (number, failure));
        result
    }

    /// Under the per-tenant flight lock: a re-check, then one fetch and store.
    async fn fill(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Arc<[Uuid]>, DomainError> {
        if let Some(chain) = self.lookup(tenant_id) {
            tracing::debug!(%tenant_id, "chain cache hit after shared flight");
            return Ok(chain);
        }
        tracing::debug!(%tenant_id, "chain cache miss");
        let started_at = self.clock.now();
        let chain = self.fetch(ctx, tenant_id).await?;
        self.store(tenant_id, Arc::clone(&chain), started_at);
        Ok(chain)
    }

    fn gate(&self, tenant_id: Uuid) -> Arc<Gate> {
        let mut inflight = self
            .inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(inflight.entry(tenant_id).or_default())
    }

    /// Called by a releasing caller that still holds its reference: the gate
    /// is removed when the map and that caller are its only holders.
    fn release_gate(&self, tenant_id: Uuid) {
        let mut inflight = self
            .inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inflight
            .get(&tenant_id)
            .is_some_and(|g| Arc::strong_count(g) == 2)
        {
            inflight.remove(&tenant_id);
        }
    }

    #[cfg(test)]
    fn inflight_len(&self) -> usize {
        self.inflight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn lookup(&self, tenant_id: Uuid) -> Option<Arc<[Uuid]>> {
        let entries = self.entries.as_ref()?;
        let mut entries = entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = self.clock.now();
        match entries.get(&tenant_id) {
            Some(entry) if entry.expires_at > now => Some(Arc::clone(&entry.chain)),
            Some(_) => {
                entries.pop(&tenant_id);
                None
            }
            None => None,
        }
    }

    /// Stores the chain with a TTL counted from `started_at`, the start of
    /// the Tenant Resolver call. A result already expired is not stored.
    fn store(&self, tenant_id: Uuid, chain: Arc<[Uuid]>, started_at: DateTime<Utc>) {
        let Some(entries) = &self.entries else {
            return;
        };
        let ttl = self.ttl_with_jitter();
        let expires_at = started_at
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::zero());
        if expires_at <= self.clock.now() {
            return;
        }
        let mut entries = entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() == entries.cap().get() && !entries.contains(&tenant_id) {
            tracing::debug!(%tenant_id, "chain cache eviction");
        }
        entries.put(tenant_id, Entry { chain, expires_at });
    }

    // The division is the jitter bound itself.
    #[allow(clippy::integer_division)]
    fn ttl_with_jitter(&self) -> Duration {
        let max_jitter_ms =
            u64::try_from(self.ttl.as_millis() / JITTER_DIVISOR).unwrap_or(u64::MAX);
        if max_jitter_ms == 0 {
            return self.ttl;
        }
        let jitter = Duration::from_millis(rand::random_range(0..=max_jitter_ms));
        self.ttl.saturating_sub(jitter)
    }

    async fn fetch(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Arc<[Uuid]>, DomainError> {
        let options = GetAncestorsOptions::default();
        let call = self
            .resolver
            .get_ancestors(ctx, TenantId(tenant_id), &options);
        let response = match tokio::time::timeout(self.deadline, call).await {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => return Err(DomainError::from_tenant_resolver(err, "get_ancestors")),
            Err(_elapsed) => {
                return Err(DomainError::unavailable(
                    Dependency::TenantResolver,
                    format!("get_ancestors did not answer within {:?}", self.deadline),
                ));
            }
        };
        let chain: Arc<[Uuid]> = std::iter::once(tenant_id)
            .chain(response.ancestors.iter().map(|a| a.id.0))
            .collect();
        Ok(chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::clock::ManualClock;
    use crate::testing::{StaticTenantResolver, security_context};
    use tenant_resolver_sdk::BarrierMode;

    fn cache(
        resolver: Arc<StaticTenantResolver>,
        clock: Arc<ManualClock>,
        ttl_s: u64,
    ) -> ChainCache {
        ChainCache::new(
            resolver,
            clock,
            Duration::from_secs(ttl_s),
            2,
            Duration::from_millis(50),
        )
    }

    #[tokio::test]
    async fn chain_starts_with_the_tenant_and_is_cached() {
        let t0 = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let root = Uuid::new_v4();
        let resolver = Arc::new(StaticTenantResolver::new().with_chain(t0, vec![parent, root]));
        let clock = Arc::new(ManualClock::fixed());
        let cache = cache(Arc::clone(&resolver), clock, 30);
        let ctx = security_context(t0);

        let chain = cache.chain(&ctx, t0).await.unwrap();
        assert_eq!(&*chain, &[t0, parent, root]);
        let again = cache.chain(&ctx, t0).await.unwrap();
        assert_eq!(&*again, &[t0, parent, root]);
        assert_eq!(resolver.calls(), 1);
        assert_eq!(resolver.last_barrier_mode(), Some(BarrierMode::Respect));
    }

    #[tokio::test]
    async fn empty_chain_of_a_root_tenant_is_a_hit() {
        let root = Uuid::new_v4();
        let resolver = Arc::new(StaticTenantResolver::new().with_chain(root, vec![]));
        let cache = cache(Arc::clone(&resolver), Arc::new(ManualClock::fixed()), 30);
        let ctx = security_context(root);
        assert_eq!(&*cache.chain(&ctx, root).await.unwrap(), &[root]);
        assert_eq!(&*cache.chain(&ctx, root).await.unwrap(), &[root]);
        assert_eq!(resolver.calls(), 1);
    }

    #[tokio::test]
    async fn expired_entries_are_refreshed() {
        let t0 = Uuid::new_v4();
        let resolver = Arc::new(StaticTenantResolver::new().with_chain(t0, vec![]));
        let clock = Arc::new(ManualClock::fixed());
        let cache = cache(Arc::clone(&resolver), Arc::clone(&clock), 30);
        let ctx = security_context(t0);
        cache.chain(&ctx, t0).await.unwrap();
        // Jitter takes at most 10% off, so 26 s is inside every possible TTL.
        clock.advance(chrono::Duration::seconds(26));
        cache.chain(&ctx, t0).await.unwrap();
        assert_eq!(resolver.calls(), 1);
        clock.advance(chrono::Duration::seconds(5));
        cache.chain(&ctx, t0).await.unwrap();
        assert_eq!(resolver.calls(), 2);
    }

    #[tokio::test]
    async fn expiry_counts_from_the_start_of_the_resolver_call() {
        let t0 = Uuid::new_v4();
        let clock = Arc::new(ManualClock::fixed());
        // The call takes 20 s of clock time.
        let slow = Arc::clone(&clock);
        let resolver = Arc::new(
            StaticTenantResolver::new()
                .with_chain(t0, vec![])
                .on_call(move || slow.advance(chrono::Duration::seconds(20))),
        );
        let cache = cache(Arc::clone(&resolver), Arc::clone(&clock), 30);
        let ctx = security_context(t0);
        cache.chain(&ctx, t0).await.unwrap();
        // 20 s into every possible TTL (27 to 30 s): 6 s more is still a hit,
        // 5 s after that is past 30 s from the call start.
        clock.advance(chrono::Duration::seconds(6));
        cache.chain(&ctx, t0).await.unwrap();
        assert_eq!(resolver.calls(), 1);
        clock.advance(chrono::Duration::seconds(5));
        cache.chain(&ctx, t0).await.unwrap();
        assert_eq!(resolver.calls(), 2);
    }

    #[tokio::test]
    async fn a_result_expired_on_arrival_is_served_and_not_stored() {
        let t0 = Uuid::new_v4();
        let clock = Arc::new(ManualClock::fixed());
        let slow = Arc::clone(&clock);
        let resolver = Arc::new(
            StaticTenantResolver::new()
                .with_chain(t0, vec![])
                .on_call(move || slow.advance(chrono::Duration::seconds(31))),
        );
        let cache = cache(Arc::clone(&resolver), Arc::clone(&clock), 30);
        let ctx = security_context(t0);
        assert_eq!(&*cache.chain(&ctx, t0).await.unwrap(), &[t0]);
        assert_eq!(&*cache.chain(&ctx, t0).await.unwrap(), &[t0]);
        assert_eq!(resolver.calls(), 2);
    }

    #[tokio::test]
    async fn ttl_zero_calls_the_resolver_every_time() {
        let t0 = Uuid::new_v4();
        let resolver = Arc::new(StaticTenantResolver::new().with_chain(t0, vec![]));
        let cache = cache(Arc::clone(&resolver), Arc::new(ManualClock::fixed()), 0);
        let ctx = security_context(t0);
        cache.chain(&ctx, t0).await.unwrap();
        cache.chain(&ctx, t0).await.unwrap();
        assert_eq!(resolver.calls(), 2);
    }

    #[tokio::test]
    async fn failures_are_not_cached_and_map_per_design() {
        let t0 = Uuid::new_v4();
        let resolver = Arc::new(StaticTenantResolver::new().failing_with(|| {
            tenant_resolver_sdk::TenantResolverError::ServiceUnavailable("down".into())
        }));
        let cache = cache(Arc::clone(&resolver), Arc::new(ManualClock::fixed()), 30);
        let ctx = security_context(t0);
        let err = cache.chain(&ctx, t0).await.unwrap_err();
        assert!(matches!(
            err,
            DomainError::DependencyUnavailable {
                dependency: Dependency::TenantResolver,
                ..
            }
        ));
        let err = cache.chain(&ctx, t0).await.unwrap_err();
        assert!(matches!(err, DomainError::DependencyUnavailable { .. }));
        assert_eq!(resolver.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_expiry_is_dependency_unavailable() {
        let t0 = Uuid::new_v4();
        let resolver = Arc::new(
            StaticTenantResolver::new()
                .with_chain(t0, vec![])
                .with_delay(Duration::from_secs(10)),
        );
        let cache = cache(Arc::clone(&resolver), Arc::new(ManualClock::fixed()), 30);
        let ctx = security_context(t0);
        let err = cache.chain(&ctx, t0).await.unwrap_err();
        assert!(matches!(
            err,
            DomainError::DependencyUnavailable {
                dependency: Dependency::TenantResolver,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn lru_evicts_the_least_recently_used_entry() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let resolver = Arc::new(
            StaticTenantResolver::new()
                .with_chain(a, vec![])
                .with_chain(b, vec![])
                .with_chain(c, vec![]),
        );
        let cache = cache(Arc::clone(&resolver), Arc::new(ManualClock::fixed()), 30);
        let ctx = security_context(a);
        cache.chain(&ctx, a).await.unwrap();
        cache.chain(&ctx, b).await.unwrap();
        cache.chain(&ctx, a).await.unwrap(); // a is now most recent
        cache.chain(&ctx, c).await.unwrap(); // evicts b
        assert_eq!(resolver.calls(), 3);
        cache.chain(&ctx, a).await.unwrap();
        assert_eq!(resolver.calls(), 3);
        cache.chain(&ctx, b).await.unwrap();
        assert_eq!(resolver.calls(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_misses_share_one_call() {
        let t0 = Uuid::new_v4();
        let resolver = Arc::new(
            StaticTenantResolver::new()
                .with_chain(t0, vec![])
                .with_delay(Duration::from_millis(10)),
        );
        let cache = Arc::new(cache(
            Arc::clone(&resolver),
            Arc::new(ManualClock::fixed()),
            30,
        ));
        let ctx = security_context(t0);
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let ctx = ctx.clone();
                tokio::spawn(async move { cache.chain(&ctx, t0).await.map(|c| c.len()) })
            })
            .collect();
        for task in tasks {
            assert_eq!(task.await.unwrap().unwrap(), 1);
        }
        assert_eq!(resolver.calls(), 1);
        assert_eq!(cache.inflight_len(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_share_a_failed_call_and_the_next_caller_retries() {
        let t0 = Uuid::new_v4();
        let resolver = Arc::new(
            StaticTenantResolver::new()
                .with_chain(t0, vec![])
                .with_delay(Duration::from_secs(10)),
        );
        let cache = Arc::new(cache(
            Arc::clone(&resolver),
            Arc::new(ManualClock::fixed()),
            30,
        ));
        let ctx = security_context(t0);
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let ctx = ctx.clone();
                tokio::spawn(async move { cache.chain(&ctx, t0).await })
            })
            .collect();
        for task in tasks {
            assert!(matches!(
                task.await.unwrap(),
                Err(DomainError::DependencyUnavailable {
                    dependency: Dependency::TenantResolver,
                    ..
                })
            ));
        }
        assert_eq!(resolver.calls(), 1, "one deadline for the whole burst");
        assert_eq!(cache.inflight_len(), 0);

        cache.chain(&ctx, t0).await.unwrap_err();
        assert_eq!(resolver.calls(), 2, "the failure is not cached");
    }

    #[tokio::test]
    async fn gates_are_released_after_each_call() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        // `b` is unknown to the resolver, so its call fails.
        let resolver = Arc::new(StaticTenantResolver::new().with_chain(a, vec![]));
        let cache = cache(Arc::clone(&resolver), Arc::new(ManualClock::fixed()), 30);
        cache.chain(&security_context(a), a).await.unwrap();
        cache.chain(&security_context(b), b).await.unwrap_err();
        assert_eq!(cache.inflight_len(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_call_releases_its_gate() {
        let t0 = Uuid::new_v4();
        let resolver = Arc::new(
            StaticTenantResolver::new()
                .with_chain(t0, vec![])
                .with_delay(Duration::from_secs(10)),
        );
        let cache = ChainCache::new(
            resolver,
            Arc::new(ManualClock::fixed()),
            Duration::from_secs(30),
            2,
            Duration::from_secs(60),
        );
        let ctx = security_context(t0);
        let call = cache.chain(&ctx, t0);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), call)
                .await
                .is_err()
        );
        assert_eq!(cache.inflight_len(), 0);
    }
}
