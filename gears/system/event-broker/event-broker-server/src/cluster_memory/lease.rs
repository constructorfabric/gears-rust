//! Lease record operations adapted from the cluster defaults' `lease.rs`.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cluster_sdk::cache::{CacheEntry, ClusterCacheBackend, PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use cluster_sdk::lease::{FENCE_RETENTION_DEFAULT, LeaseRecord, LeaseToken};
use cluster_sdk::observability::{self, ClusterMetrics, ResourceId, result, spans};
use rand::RngExt as _;
use tracing::Instrument as _;

// A rejected insert can race with removal before the read. Bound retries on that vacancy.
const VACANCY_ATTEMPTS: usize = 3;
pub(super) const MEMORY_PROVIDER: &str = "eb-memory";

pub(super) enum Acquisition {
    Acquired(ClaimedLease),
    Contended { lapse_in: Option<Duration> },
}

pub(super) struct ClaimedLease {
    pub(super) token: LeaseToken,
    pub(super) deadline: Duration,
    value: Vec<u8>,
}

pub(super) struct Claim<'a> {
    pub(super) name: &'a str,
    pub(super) owner: &'a str,
    pub(super) ttl: Duration,
}

enum Slot {
    Vacant,
    Foreign,
    Lease(StoredLease, CacheEntry),
}

// A fractional election deadline wraps the SDK bytes so the owner stays unchanged.
const FRACTIONAL_ELECTION: &[u8] = b"EBEL\x01";

pub(super) struct StoredLease {
    pub(super) base: LeaseRecord,
    nanos_past_ms: u32,
}

impl StoredLease {
    pub(super) fn deadline(&self) -> Duration {
        Duration::from_millis(self.base.deadline_ms)
            + Duration::from_nanos(self.nanos_past_ms.into())
    }

    fn encode(&self) -> Vec<u8> {
        let ordinary = self.base.encode();
        if self.nanos_past_ms == 0 {
            return ordinary;
        }
        let mut value = Vec::with_capacity(FRACTIONAL_ELECTION.len() + 4 + ordinary.len());
        value.extend_from_slice(FRACTIONAL_ELECTION);
        value.extend_from_slice(&self.nanos_past_ms.to_be_bytes());
        value.extend_from_slice(&ordinary);
        value
    }

    fn decode(value: &[u8], election: bool) -> Option<Self> {
        if election && let Some(body) = value.strip_prefix(FRACTIONAL_ELECTION) {
            let (remainder, ordinary) = body.split_first_chunk::<4>()?;
            let nanos_past_ms = u32::from_be_bytes(*remainder);
            if !(1..1_000_000).contains(&nanos_past_ms) {
                return None;
            }
            return Some(Self {
                base: LeaseRecord::decode(ordinary)?,
                nanos_past_ms,
            });
        }
        Some(Self {
            base: LeaseRecord::decode(value)?,
            nanos_past_ms: 0,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum LockOperation {
    TryLock,
    Lock,
    Renew,
    Release,
}

enum LeaseClock {
    System,
    #[cfg(test)]
    Virtual(tokio::time::Instant),
}

/// Implements conditional operations on the profile's reserved lease records.
pub(super) struct CacheLeaseStore {
    pub(super) cache: Arc<dyn ClusterCacheBackend>,
    election: bool,
    clock: LeaseClock,
    retention: Duration,
    ttl_warned: AtomicBool,
    nonce: AtomicU64,
    metrics: Arc<dyn ClusterMetrics>,
}

impl CacheLeaseStore {
    pub(super) fn new(
        cache: Arc<dyn ClusterCacheBackend>,
        metrics: Arc<dyn ClusterMetrics>,
    ) -> Self {
        Self {
            cache,
            election: false,
            clock: LeaseClock::System,
            retention: FENCE_RETENTION_DEFAULT,
            ttl_warned: AtomicBool::new(false),
            nonce: AtomicU64::new(rand::rng().random()),
            metrics,
        }
    }

    pub(super) fn for_elections(
        cache: Arc<dyn ClusterCacheBackend>,
        metrics: Arc<dyn ClusterMetrics>,
    ) -> Self {
        Self {
            election: true,
            ..Self::new(cache, metrics)
        }
    }

    #[cfg(test)]
    pub(super) fn set_test_clock(&mut self, retention: Duration, nonce: u64) {
        self.clock = LeaseClock::Virtual(tokio::time::Instant::now());
        self.retention = retention;
        self.nonce = AtomicU64::new(nonce);
    }

    pub(super) fn wall_now(&self) -> Duration {
        match self.clock {
            LeaseClock::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default(),
            #[cfg(test)]
            LeaseClock::Virtual(started) => started.elapsed(),
        }
    }

    #[cfg(test)]
    pub(super) fn now(&self) -> u64 {
        Self::whole_millis(self.wall_now())
    }

    fn whole_millis(duration: Duration) -> u64 {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    fn now_for_record(&self) -> Duration {
        let now = self.wall_now();
        if self.election {
            now
        } else {
            Duration::from_millis(Self::whole_millis(now))
        }
    }

    pub(super) fn key(name: &str) -> String {
        format!("lock/{name}")
    }

    fn physical_ttl(&self, ttl: Duration) -> Ttl {
        Ttl::Of(ttl.saturating_add(self.retention))
    }

    fn record(&self, owner: String, fence: u64, ttl: Duration) -> StoredLease {
        let (deadline_ms, nanos_past_ms) = if self.election {
            let expiry = self
                .wall_now()
                .saturating_add(ttl)
                .min(Duration::from_millis(u64::MAX));
            (
                Self::whole_millis(expiry),
                expiry.subsec_nanos() % 1_000_000,
            )
        } else {
            (
                Self::whole_millis(self.wall_now()).saturating_add(Self::whole_millis(ttl)),
                0,
            )
        };
        StoredLease {
            base: LeaseRecord {
                owner,
                fence,
                deadline_ms,
                nonce: self.nonce.fetch_add(1, Ordering::Relaxed),
            },
            nanos_past_ms,
        }
    }

    async fn slot(&self, key: &str) -> Result<Slot, ClusterError> {
        let Some(entry) = self.cache.get(key).await? else {
            return Ok(Slot::Vacant);
        };
        Ok(match StoredLease::decode(&entry.value, self.election) {
            Some(record) => Slot::Lease(record, entry),
            None => Slot::Foreign,
        })
    }

    pub(super) async fn read(&self, key: &str) -> Result<Option<StoredLease>, ClusterError> {
        Ok(self
            .cache
            .get(key)
            .await?
            .and_then(|entry| StoredLease::decode(&entry.value, self.election)))
    }

    pub(super) async fn try_acquire(
        &self,
        key: &str,
        claim: &Claim<'_>,
    ) -> Result<Acquisition, ClusterError> {
        let ttl = claim.ttl;
        if ttl >= self.retention && !self.ttl_warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                ttl = ?ttl,
                fence_retention = ?self.retention,
                "lease TTL reaches or exceeds the fence retention window"
            );
        }
        for _ in 0..VACANCY_ATTEMPTS {
            let record = self.record(claim.owner.to_owned(), 1, ttl);
            let value = record.encode();
            if self
                .cache
                .put_if_absent(PutRequest {
                    key,
                    value: &value,
                    ttl: self.physical_ttl(ttl),
                })
                .await?
                .is_some()
            {
                return Ok(Acquisition::Acquired(ClaimedLease {
                    token: LeaseToken::new(claim.name, claim.owner, 1),
                    deadline: record.deadline(),
                    value,
                }));
            }

            match self.slot(key).await? {
                Slot::Vacant => {}
                Slot::Foreign => return Ok(Acquisition::Contended { lapse_in: None }),
                Slot::Lease(record, entry) => {
                    let now = self.now_for_record();
                    if record.deadline() > now {
                        return Ok(Acquisition::Contended {
                            lapse_in: Some(record.deadline().saturating_sub(now)),
                        });
                    }
                    let fence = record.base.fence.saturating_add(1);
                    let record = self.record(claim.owner.to_owned(), fence, ttl);
                    let value = record.encode();
                    return match self
                        .cache
                        .compare_and_swap_value(key, &entry.value, &value, self.physical_ttl(ttl))
                        .await
                    {
                        Ok(_) => Ok(Acquisition::Acquired(ClaimedLease {
                            token: LeaseToken::new(claim.name, claim.owner, fence),
                            deadline: record.deadline(),
                            value,
                        })),
                        Err(ClusterError::CasConflict { .. }) => {
                            Ok(Acquisition::Contended { lapse_in: None })
                        }
                        Err(error) => Err(error),
                    };
                }
            }
        }
        Ok(Acquisition::Contended { lapse_in: None })
    }

    pub(super) async fn renew(
        &self,
        token: &LeaseToken,
        ttl: Duration,
    ) -> Result<(), ClusterError> {
        self.observe(LockOperation::Renew, &token.name, async {
            self.renew_record(&Self::key(&token.name), token, ttl)
                .await
                .map(|_| ())
        })
        .await
    }

    pub(super) async fn renew_record(
        &self,
        key: &str,
        token: &LeaseToken,
        ttl: Duration,
    ) -> Result<Duration, ClusterError> {
        let expired = || ClusterError::LockExpired {
            name: token.name.clone(),
        };
        let Slot::Lease(record, entry) = self.slot(key).await? else {
            return Err(expired());
        };
        if !record.base.matches(token) || record.deadline() <= self.now_for_record() {
            return Err(expired());
        }

        let renewed = self.record(record.base.owner, record.base.fence, ttl);
        // The complete record identifies the write across cache version resets.
        match self
            .cache
            .compare_and_swap_value(key, &entry.value, &renewed.encode(), self.physical_ttl(ttl))
            .await
        {
            Ok(_) => Ok(renewed.deadline()),
            Err(ClusterError::CasConflict { .. }) => Err(expired()),
            Err(error) => Err(error),
        }
    }

    pub(super) async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        self.observe(
            LockOperation::Release,
            &token.name,
            self.release_record(&Self::key(&token.name), token),
        )
        .await
    }

    pub(super) async fn release_record(
        &self,
        key: &str,
        token: &LeaseToken,
    ) -> Result<(), ClusterError> {
        let Slot::Lease(record, entry) = self.slot(key).await? else {
            return Ok(());
        };
        if record.base.matches(token) {
            self.cache.compare_and_delete(key, &entry.value).await?;
        }
        Ok(())
    }

    pub(super) async fn discard(&self, claimed: &ClaimedLease) -> Result<(), ClusterError> {
        self.discard_record(&Self::key(&claimed.token.name), claimed)
            .await
    }

    pub(super) async fn discard_record(
        &self,
        key: &str,
        claimed: &ClaimedLease,
    ) -> Result<(), ClusterError> {
        // Shutdown can race admission. Remove only the exact unpublished write.
        self.cache.compare_and_delete(key, &claimed.value).await?;
        Ok(())
    }

    pub(super) async fn observe<T>(
        &self,
        operation: LockOperation,
        name: &str,
        work: impl Future<Output = Result<T, ClusterError>>,
    ) -> Result<T, ClusterError> {
        let provider = MEMORY_PROVIDER;
        let (label, span) = match operation {
            LockOperation::TryLock => (
                "try_lock",
                tracing::info_span!(spans::LOCK_TRY_LOCK, provider, lock = name),
            ),
            LockOperation::Lock => (
                "lock",
                tracing::info_span!(spans::LOCK_LOCK, provider, lock = name),
            ),
            LockOperation::Renew => (
                "renew",
                tracing::info_span!(spans::LOCK_RENEW, provider, lock = name),
            ),
            LockOperation::Release => (
                "release",
                tracing::info_span!(spans::LOCK_RELEASE, provider, lock = name),
            ),
        };
        let started = Instant::now();
        let outcome = work.instrument(span).await;
        self.metrics
            .lock_op_duration(label, started.elapsed().as_secs_f64());
        self.metrics.lock_op(label, result::label(&outcome));
        if let Err(error) = &outcome {
            observability::emit_provider_error(
                &*self.metrics,
                provider,
                label,
                ResourceId::Lock(name),
                error,
            );
        }
        outcome
    }
}
