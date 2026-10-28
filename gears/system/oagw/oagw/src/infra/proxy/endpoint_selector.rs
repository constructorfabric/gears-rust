//! Default in-process [`EndpointSelector`]: one health-checked, DNS-refreshed
//! backend pool per multi-endpoint upstream (ADR-0019).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;
use toolkit_http_proxy::balance::{
    BackendPool, BackendPoolOptions, DnsResolver, Seed, SelectError, TcpHealthCheck,
};
use tracing::debug;
use uuid::Uuid;

use super::engine::SsrfFilter;
use crate::domain::model::Endpoint;
use crate::domain::services::{EndpointSelector, SelectedEndpoint, SelectionError};
use crate::domain::ssrf::SsrfGuard;

/// How often a pool's background task health-checks its backends over TCP.
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(10);
/// How often a pool's background task re-resolves its endpoints' DNS.
const DNS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// Connect budget for one TCP health probe.
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(1);
/// A pool not selected for this long is dropped by the idle sweep. Well above
/// the health-check and DNS intervals, so only upstreams nobody routes to any
/// more go, e.g. ones deleted through another replica.
const POOL_IDLE_TTL: Duration = Duration::from_secs(600);
/// How often idle pools are swept.
const POOL_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

const _: () = assert!(
    POOL_IDLE_TTL.as_secs() > DNS_REFRESH_INTERVAL.as_secs()
        && POOL_IDLE_TTL.as_secs() > HEALTH_CHECK_INTERVAL.as_secs()
        && POOL_SWEEP_INTERVAL.as_secs() < POOL_IDLE_TTL.as_secs(),
    "an idle pool must outlive its own background refresh, and the sweep must run within the TTL"
);

/// Cache entry: the pool plus the endpoint list it was built from.
struct PoolEntry {
    /// Endpoints the pool was built from; a different list rebuilds the pool.
    endpoints: Vec<Endpoint>,
    pool: Arc<BackendPool<Endpoint>>,
    /// Seconds since the selector's `epoch` at the last `select()`.
    last_used_secs: AtomicU64,
}

impl PoolEntry {
    fn touch(&self, now_secs: u64) {
        // Write only when the second changes, so a busy upstream does not
        // bounce the cache line on every request.
        if self.last_used_secs.load(Ordering::Relaxed) != now_secs {
            self.last_used_secs.store(now_secs, Ordering::Relaxed);
        }
    }
}

/// Round-robin over healthy, SSRF-filtered, DNS-refreshed backends.
///
/// Lazily builds a [`BackendPool`] per upstream on first `select()`, caches
/// it, and rebuilds it whenever `select()` receives a different endpoint
/// list, so replicas sharing a database pick up endpoint changes made
/// elsewhere. Dropping the cache entry (via `invalidate()`, a rebuild or the
/// idle sweep) stops the pool's background task.
pub struct PoolEndpointSelector {
    cache: DashMap<Uuid, PoolEntry>,
    ssrf_guard: Arc<SsrfGuard>,
    /// Base for the entries' `last_used_secs`.
    epoch: tokio::time::Instant,
}

impl PoolEndpointSelector {
    /// Create a selector with an empty per-upstream cache.
    pub fn new(ssrf_guard: Arc<SsrfGuard>) -> Self {
        Self {
            cache: DashMap::new(),
            ssrf_guard,
            epoch: tokio::time::Instant::now(),
        }
    }

    /// Start dropping pools idle for longer than [`POOL_IDLE_TTL`] every
    /// [`POOL_SWEEP_INTERVAL`]. The task ends when `cancel` fires or this
    /// selector is dropped.
    pub fn spawn_idle_sweeper(self: &Arc<Self>, cancel: CancellationToken) {
        tokio::spawn(sweep_idle_pools(
            Arc::downgrade(self),
            POOL_SWEEP_INTERVAL,
            POOL_IDLE_TTL,
            cancel,
        ));
    }

    fn now_secs(&self) -> u64 {
        self.epoch.elapsed().as_secs()
    }

    /// Drop the pools not selected for longer than `ttl`, which stops their
    /// background tasks. Returns how many were dropped.
    fn evict_idle(&self, ttl: Duration) -> usize {
        let now = self.now_secs();
        let mut evicted = 0;
        self.cache.retain(|_, entry| {
            let idle = now.saturating_sub(entry.last_used_secs.load(Ordering::Relaxed));
            let keep = idle <= ttl.as_secs();
            if !keep {
                evicted += 1;
            }
            keep
        });
        evicted
    }

    fn select_from(entry: &PoolEntry) -> Result<SelectedEndpoint, SelectionError> {
        entry
            .pool
            .select()
            .map(|(endpoint, addr)| SelectedEndpoint {
                endpoint,
                resolved_addr: Some(addr),
            })
            .map_err(|e| match e {
                SelectError::NoBackendsResolved => SelectionError::NoBackendsResolved,
                SelectError::AllBackendsUnhealthy => SelectionError::AllBackendsUnhealthy,
            })
    }

    async fn build_entry(&self, endpoints: &[Endpoint]) -> Result<PoolEntry, SelectionError> {
        let seeds = endpoints
            .iter()
            .map(|ep| Seed {
                key: ep.clone(),
                host: ep.host.clone(),
                port: ep.port,
            })
            .collect();
        let mut opts = BackendPoolOptions::default();
        opts.resolver = Arc::new(DnsResolver::default());
        opts.filter = Arc::new(SsrfFilter(Arc::clone(&self.ssrf_guard)));
        opts.health_check = Some(Arc::new(TcpHealthCheck {
            timeout: HEALTH_CHECK_TIMEOUT,
        }));
        opts.refresh_interval = Some(DNS_REFRESH_INTERVAL);
        opts.health_interval = Some(HEALTH_CHECK_INTERVAL);
        let pool = BackendPool::build(seeds, opts).await;
        if pool.members().is_empty() {
            tracing::warn!("No backends resolved for endpoints, skipping pool creation");
            return Err(SelectionError::NoBackendsResolved);
        }
        Ok(PoolEntry {
            endpoints: endpoints.to_vec(),
            pool,
            last_used_secs: AtomicU64::new(self.now_secs()),
        })
    }
}

#[async_trait]
impl EndpointSelector for PoolEndpointSelector {
    async fn select(
        &self,
        upstream_id: Uuid,
        endpoints: &[Endpoint],
    ) -> Result<SelectedEndpoint, SelectionError> {
        if endpoints.is_empty() {
            return Err(SelectionError::NoBackendsResolved);
        }
        // Fast path: pool already cached for the same endpoints.
        if let Some(entry) = self.cache.get(&upstream_id)
            && entry.endpoints == endpoints
        {
            entry.touch(self.now_secs());
            return Self::select_from(&entry);
        }

        // Slow path: build a new pool, then insert it unless a concurrent
        // builder already cached one for the same endpoints. A replaced or
        // losing entry is dropped, which stops its background task.
        let entry = self.build_entry(endpoints).await?;
        let entry_ref = match self.cache.entry(upstream_id) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                if occupied.get().endpoints != endpoints {
                    occupied.insert(entry);
                }
                occupied.into_ref()
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => vacant.insert(entry),
        };
        entry_ref.touch(self.now_secs());
        Self::select_from(&entry_ref)
    }

    fn invalidate(&self, upstream_id: Uuid) {
        self.cache.remove(&upstream_id);
    }
}

/// Drop pools idle for longer than `ttl` every `interval` until `cancel`
/// fires or the selector is gone.
async fn sweep_idle_pools(
    selector: Weak<PoolEndpointSelector>,
    interval: Duration,
    ttl: Duration,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = ticker.tick() => {
                let Some(selector) = selector.upgrade() else {
                    return;
                };
                let evicted = selector.evict_idle(ttl);
                if evicted > 0 {
                    debug!(evicted, live = selector.cache.len(), "dropped idle upstream pools");
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "endpoint_selector_tests.rs"]
mod endpoint_selector_tests;
