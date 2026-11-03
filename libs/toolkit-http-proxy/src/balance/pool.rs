//! Live backend set with background refresh.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::{
    AddressFilter, AllowAll, Balancer, DnsResolver, HealthCheck, Resolver, RoundRobin,
    TcpHealthCheck,
};

/// A logical endpoint: the caller's descriptor plus the name to resolve.
#[derive(Debug, Clone)]
pub struct Seed<K> {
    /// Caller data returned on selection (e.g. the configured endpoint).
    pub key: K,
    /// Host name or IP literal.
    pub host: String,
    /// Port.
    pub port: u16,
}

/// Health of one backend: the current verdict, and how many probes in a row
/// have disagreed with it. Shared across refreshes for as long as the
/// address stays resolved.
///
/// The verdict is an atomic so selection reads it without a lock; every
/// transition (a probe, a passive mark) runs under `streak`'s lock, so a
/// probe can never count towards a verdict that changed under it.
#[derive(Debug)]
struct Health {
    healthy: AtomicBool,
    streak: parking_lot::Mutex<u32>,
}

impl Health {
    fn new(healthy: bool) -> Self {
        Self {
            healthy: AtomicBool::new(healthy),
            streak: parking_lot::Mutex::new(0),
        }
    }

    /// Record a probe. The verdict flips only after `threshold` probes in a
    /// row disagree with it (`healthy_threshold` to rejoin rotation,
    /// `unhealthy_threshold` to leave it); one that agrees resets the
    /// count. Returns the new verdict when it changed.
    fn record(&self, probe: bool, opts: &BackendPoolOptions) -> Option<bool> {
        let mut streak = self.streak.lock();
        if probe == self.healthy.load(Ordering::Relaxed) {
            *streak = 0;
            return None;
        }
        let threshold = if probe {
            opts.healthy_threshold
        } else {
            opts.unhealthy_threshold
        };
        *streak = streak.saturating_add(1);
        if *streak < threshold.max(1) {
            return None;
        }
        *streak = 0;
        self.healthy.store(probe, Ordering::Relaxed);
        Some(probe)
    }

    fn mark_unhealthy(&self) {
        let mut streak = self.streak.lock();
        *streak = 0;
        self.healthy.store(false, Ordering::Relaxed);
    }
}

/// One resolved backend address.
#[derive(Debug, Clone)]
pub struct Member {
    addr: SocketAddr,
    seed: usize,
    health: Arc<Health>,
}

impl Member {
    /// Resolved socket address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Current health verdict (members start healthy).
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.health.healthy.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_test(addr: SocketAddr, healthy: bool) -> Self {
        Self {
            addr,
            seed: 0,
            health: Arc::new(Health::new(healthy)),
        }
    }

    #[cfg(test)]
    pub fn set_healthy(&self, healthy: bool) {
        self.health.healthy.store(healthy, Ordering::Relaxed);
    }
}

/// Why no backend could be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    /// No seed resolved to an allowed address.
    #[error("no backends resolved")]
    NoBackendsResolved,
    /// Backends exist but every one failed its health check.
    #[error("all backends unhealthy")]
    AllBackendsUnhealthy,
}

/// Pluggable parts and timings of a [`BackendPool`].
#[derive(Clone)]
#[non_exhaustive]
pub struct BackendPoolOptions {
    /// Name resolution.
    pub resolver: Arc<dyn Resolver>,
    /// Address policy applied to every resolved address (SSRF guard).
    pub filter: Arc<dyn AddressFilter>,
    /// Active health probe; `None` keeps every member healthy.
    pub health_check: Option<Arc<dyn HealthCheck>>,
    /// Selection strategy.
    pub balancer: Arc<dyn Balancer>,
    /// DNS re-resolution period; `None` resolves once.
    pub refresh_interval: Option<Duration>,
    /// Health-probe period; `None` disables periodic probes.
    pub health_interval: Option<Duration>,
    /// Successful probes in a row an unhealthy member needs to rejoin
    /// rotation (values below 1 count as 1). Damps a flapping backend.
    pub healthy_threshold: u32,
    /// Failed probes in a row that take a healthy member out of rotation
    /// (values below 1 count as 1). A passive [`BackendPool::mark_unhealthy`]
    /// still acts at once.
    pub unhealthy_threshold: u32,
}

impl Default for BackendPoolOptions {
    fn default() -> Self {
        Self {
            resolver: Arc::new(DnsResolver::default()),
            filter: Arc::new(AllowAll),
            health_check: Some(Arc::new(TcpHealthCheck::default())),
            balancer: Arc::new(RoundRobin::default()),
            refresh_interval: Some(Duration::from_secs(30)),
            health_interval: Some(Duration::from_secs(10)),
            healthy_threshold: 1,
            unhealthy_threshold: 1,
        }
    }
}

/// Resolved, filtered and health-checked backends for one upstream.
///
/// The background refresh task holds only a weak reference and stops when
/// the pool is dropped.
pub struct BackendPool<K> {
    seeds: Vec<Seed<K>>,
    members: ArcSwap<Vec<Member>>,
    opts: BackendPoolOptions,
    cancel: CancellationToken,
}

impl<K: Clone + Send + Sync + 'static> BackendPool<K> {
    /// Resolve `seeds` once, then keep them fresh in the background.
    /// Must be called inside a Tokio runtime.
    pub async fn build(seeds: Vec<Seed<K>>, opts: BackendPoolOptions) -> Arc<Self> {
        let pool = Arc::new(Self {
            seeds,
            members: ArcSwap::from_pointee(Vec::new()),
            opts,
            cancel: CancellationToken::new(),
        });
        pool.refresh().await;
        tokio::spawn(background(
            Arc::downgrade(&pool),
            pool.cancel.clone(),
            pool.opts.refresh_interval,
            pool.opts.health_interval,
        ));
        pool
    }

    /// The seeds this pool was built from.
    #[must_use]
    pub fn seeds(&self) -> &[Seed<K>] {
        &self.seeds
    }

    /// Snapshot of the current members.
    #[must_use]
    pub fn members(&self) -> Arc<Vec<Member>> {
        self.members.load_full()
    }

    /// Pick a healthy member.
    ///
    /// # Errors
    ///
    /// [`SelectError`] when nothing resolved or nothing is healthy.
    pub fn select(&self) -> Result<(K, SocketAddr), SelectError> {
        let members = self.members.load();
        if members.is_empty() {
            return Err(SelectError::NoBackendsResolved);
        }
        let idx = self
            .opts
            .balancer
            .pick(&members)
            .ok_or(SelectError::AllBackendsUnhealthy)?;
        let member = &members[idx];
        Ok((self.seeds[member.seed].key.clone(), member.addr))
    }

    /// Passive health: take `addr` out of rotation at once, until probes
    /// find it healthy again (`healthy_threshold` in a row).
    pub fn mark_unhealthy(&self, addr: SocketAddr) {
        if let Some(m) = self.members.load().iter().find(|m| m.addr == addr) {
            m.health.mark_unhealthy();
        }
    }

    /// Re-resolve every seed and swap in the new member list. Health state
    /// (verdict and probe streak) carries over for addresses that are still
    /// present. When several seeds resolve to the same address, the first
    /// seed wins.
    pub async fn refresh(&self) {
        let current = self.members.load();
        let previous: HashMap<SocketAddr, Arc<Health>> = current
            .iter()
            .map(|m| (m.addr, Arc::clone(&m.health)))
            .collect();
        let mut seen = HashSet::new();
        let mut next = Vec::with_capacity(self.seeds.len());
        for (seed_idx, seed) in self.seeds.iter().enumerate() {
            let addrs = match self.opts.resolver.resolve(&seed.host, seed.port).await {
                Ok(addrs) => addrs,
                Err(e) => {
                    // Keep the addresses this seed resolved to last time: a
                    // DNS outage must not empty a working upstream.
                    tracing::warn!(host = %seed.host, port = seed.port, error = %e, "DNS resolution failed after retries; keeping the previous addresses");
                    current
                        .iter()
                        .filter(|m| m.seed == seed_idx)
                        .map(|m| m.addr)
                        .collect()
                }
            };
            for addr in addrs {
                if let Err(reason) = self.opts.filter.check(&seed.host, &addr) {
                    tracing::warn!(host = %seed.host, resolved = %addr, reason, "resolved address blocked by policy");
                    continue;
                }
                if !seen.insert(addr) {
                    continue;
                }
                let health = previous
                    .get(&addr)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(Health::new(true)));
                next.push(Member {
                    addr,
                    seed: seed_idx,
                    health,
                });
            }
        }
        self.members.store(Arc::new(next));
    }

    /// Probe every member once, concurrently.
    pub async fn run_health_checks(&self) {
        let Some(check) = self.opts.health_check.as_ref() else {
            return;
        };
        let members = self.members.load_full();
        let opts = &self.opts;
        let probes = members.iter().map(|m| async move {
            let probe = check.check(m.addr).await;
            if let Some(healthy) = m.health.record(probe, opts) {
                tracing::info!(backend = %m.addr, healthy, "backend health changed");
            }
        });
        futures_util::future::join_all(probes).await;
    }
}

impl<K> Drop for BackendPool<K> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn background<K: Clone + Send + Sync + 'static>(
    pool: Weak<BackendPool<K>>,
    cancel: CancellationToken,
    refresh_every: Option<Duration>,
    health_every: Option<Duration>,
) {
    // The first resolution already happened in `build`; the first probe runs
    // right away so dead backends leave rotation within one connect timeout.
    let mut refresh = refresh_every.map(|every| ticker(Instant::now() + every, every));
    let mut health = health_every.map(|every| ticker(Instant::now(), every));
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tick(&mut refresh) => {
                let Some(pool) = pool.upgrade() else { return };
                pool.refresh().await;
            }
            () = tick(&mut health) => {
                let Some(pool) = pool.upgrade() else { return };
                pool.run_health_checks().await;
            }
        }
    }
}

fn ticker(start: Instant, every: Duration) -> Interval {
    let mut interval = tokio::time::interval_at(start, every);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

async fn tick(interval: &mut Option<Interval>) {
    match interval {
        Some(i) => {
            i.tick().await;
        }
        None => std::future::pending().await,
    }
}
