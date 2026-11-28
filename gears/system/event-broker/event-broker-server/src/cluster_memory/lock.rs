//! SDK lock coordination adapted from the cluster defaults' `lock.rs`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::cache::{CacheConsistency, CacheWatchEvent, ClusterCacheBackend};
use cluster_sdk::error::{ClusterError, ProviderErrorKind};
use cluster_sdk::lease::LeaseToken;
use cluster_sdk::lock::{DistributedLockBackend, LockFeatures, LockGuard};
use cluster_sdk::observability::ClusterMetrics;
use tokio::time::{Instant, sleep};
use toolkit::async_trait;
use uuid::Uuid;

use super::guard::GuardWorkers;
use super::lease::{Acquisition, CacheLeaseStore, Claim, ClaimedLease, LockOperation};

const WATCH_RETRY_DELAY: Duration = Duration::from_millis(50);
const MAX_WATCH_ENDS: usize = 8;

/// Coordinates locks on the reserved cache view of one Event Broker profile.
pub struct MemoryLock {
    leases: Arc<CacheLeaseStore>,
    guards: GuardWorkers,
}

impl fmt::Debug for MemoryLock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("MemoryLock").finish_non_exhaustive()
    }
}

impl MemoryLock {
    /// Uses the same reserved cache view as the profile's other lease primitives.
    ///
    /// # Errors
    ///
    /// The cache must declare linearizable consistency.
    pub(crate) fn new(
        cache: Arc<dyn ClusterCacheBackend>,
        metrics: Arc<dyn ClusterMetrics>,
    ) -> Result<Self, ClusterError> {
        if cache.consistency() != CacheConsistency::Linearizable {
            return Err(ClusterError::InvalidConfig {
                reason: "event-broker memory lock requires a linearizable cache. Use the linearizable profile cache.".to_owned(),
            });
        }
        Ok(Self {
            leases: Arc::new(CacheLeaseStore::new(cache, metrics)),
            guards: GuardWorkers::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_clock(mut self, retention: Duration, nonce: u64) -> Self {
        Arc::get_mut(&mut self.leases)
            .expect("the test clock is set before sharing the lease store")
            .set_test_clock(retention, nonce);
        self
    }

    #[cfg(test)]
    pub(crate) fn lease_now(&self) -> u64 {
        self.leases.now()
    }

    #[cfg(test)]
    pub(crate) fn registered_guards(&self) -> usize {
        self.guards.len()
    }

    /// Closes admission and joins the guard workers. Held leases lapse at their own deadlines.
    pub(crate) async fn revoke(&self) {
        self.guards.revoke().await;
    }

    pub(super) fn abort(&self) {
        self.guards.shutdown().cancel();
    }

    fn require_open(&self) -> Result<(), ClusterError> {
        if self.guards.shutdown().is_cancelled() {
            return Err(ClusterError::Shutdown);
        }
        Ok(())
    }

    async fn try_claim(&self, claim: Claim<'_>) -> Result<ClaimedLease, ClusterError> {
        self.require_open()?;
        match self
            .leases
            .try_acquire(&CacheLeaseStore::key(claim.name), &claim)
            .await?
        {
            Acquisition::Acquired(token) => Ok(token),
            Acquisition::Contended { .. } => {
                self.require_open()?;
                Err(ClusterError::LockContended {
                    name: claim.name.to_owned(),
                })
            }
        }
    }

    async fn wait_for_claim(
        &self,
        claim: Claim<'_>,
        budget: Duration,
    ) -> Result<ClaimedLease, ClusterError> {
        self.require_open()?;
        let started = Instant::now();
        let key = CacheLeaseStore::key(claim.name);
        let shutdown = self.guards.shutdown();
        let mut ended = 0;
        let timed_out = || ClusterError::LockTimeout {
            name: claim.name.to_owned(),
            waited: if budget.is_zero() {
                Duration::ZERO
            } else {
                started.elapsed()
            },
        };

        // Subscribe before reading so a release cannot fall between the claim and the wait.
        loop {
            let mut watch = tokio::select! {
                () = shutdown.cancelled() => return Err(ClusterError::Shutdown),
                watch = self.leases.cache.watch(&key) => watch?,
            };
            let watched_since = Instant::now();
            loop {
                self.require_open()?;
                let lapse_in = match self.leases.try_acquire(&key, &claim).await? {
                    Acquisition::Acquired(token) => return Ok(token),
                    Acquisition::Contended { lapse_in } => lapse_in,
                };
                self.require_open()?;
                let remaining = budget.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(timed_out());
                }
                let wake = lapse_in.map_or(remaining, |lapse| remaining.min(lapse));
                debug_assert!(!wake.is_zero());
                tokio::select! {
                    () = shutdown.cancelled() => return Err(ClusterError::Shutdown),
                    () = sleep(wake) => {
                        if wake == remaining {
                            return Err(timed_out());
                        }
                        ended = 0;
                    }
                    event = watch.recv() => match event {
                        Some(CacheWatchEvent::Closed(error)) => return Err(error),
                        Some(_) => ended = 0,
                        None => break,
                    }
                }
                if started.elapsed() >= budget {
                    return Err(timed_out());
                }
            }

            // Native cache shutdown supplies Closed. The cap bounds retries if a feed ends early.
            if watched_since.elapsed() >= WATCH_RETRY_DELAY {
                ended = 0;
                if started.elapsed() >= budget {
                    return Err(timed_out());
                }
                continue;
            }
            ended += 1;
            if ended == MAX_WATCH_ENDS {
                return Err(ClusterError::Provider {
                    kind: ProviderErrorKind::Other,
                    message: "memory lock watch ended eight consecutive times".to_owned(),
                });
            }
            let remaining = budget.saturating_sub(started.elapsed());
            tokio::select! {
                () = shutdown.cancelled() => return Err(ClusterError::Shutdown),
                () = sleep(WATCH_RETRY_DELAY.min(remaining)) => {}
            }
            if started.elapsed() >= budget {
                return Err(timed_out());
            }
        }
    }

    async fn finish_guard(&self, claimed: ClaimedLease) -> Result<LockGuard, ClusterError> {
        match self.guards.attach(self.leases.clone(), claimed) {
            Ok(guard) => Ok(guard),
            Err(claimed) => Err(self.discard_unpublished(&claimed).await),
        }
    }

    async fn finish_token(&self, claimed: ClaimedLease) -> Result<LeaseToken, ClusterError> {
        if self.require_open().is_err() {
            return Err(self.discard_unpublished(&claimed).await);
        }
        Ok(claimed.token)
    }

    async fn discard_unpublished(&self, claimed: &ClaimedLease) -> ClusterError {
        if let Err(error) = self.leases.discard(claimed).await {
            tracing::warn!(%error, "could not release an unpublished lock claim");
        }
        ClusterError::Shutdown
    }
}

#[async_trait]
impl DistributedLockBackend for MemoryLock {
    fn features(&self) -> LockFeatures {
        LockFeatures::new(true)
    }

    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        self.leases
            .observe(LockOperation::TryLock, name, async {
                let owner = Uuid::new_v4().to_string();
                let token = self
                    .try_claim(Claim {
                        name,
                        owner: &owner,
                        ttl,
                    })
                    .await?;
                self.finish_guard(token).await
            })
            .await
    }

    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        self.leases
            .observe(LockOperation::Lock, name, async {
                let owner = Uuid::new_v4().to_string();
                let token = self
                    .wait_for_claim(
                        Claim {
                            name,
                            owner: &owner,
                            ttl,
                        },
                        timeout,
                    )
                    .await?;
                self.finish_guard(token).await
            })
            .await
    }

    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        self.leases
            .observe(LockOperation::TryLock, name, async {
                let token = self.try_claim(Claim { name, owner, ttl }).await?;
                self.finish_token(token).await
            })
            .await
    }

    async fn acquire_waiting(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        self.leases
            .observe(LockOperation::Lock, name, async {
                let token = self
                    .wait_for_claim(Claim { name, owner, ttl }, timeout)
                    .await?;
                self.finish_token(token).await
            })
            .await
    }

    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        self.require_open()?;
        self.leases.renew(token, ttl).await
    }

    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        self.require_open()?;
        self.leases.release(token).await
    }
}
