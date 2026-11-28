//! Composition of the Event Broker process's local Cluster SDK backends.

use std::collections::{BTreeMap, btree_map::Entry};
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::cache::{ClusterCacheBackend, reserved_lease_cache};
use cluster_sdk::client::ClusterClient;
use cluster_sdk::dto::{
    CacheDescriptor, LeaderElectionDescriptor, LockDescriptor, ProfileDescriptor, ProfileHealth,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::intern::intern_existing;
use cluster_sdk::leader::LeaderElectionBackend;
use cluster_sdk::lock::DistributedLockBackend;
use cluster_sdk::observability::{ClusterMetrics, InstrumentedCache};
use tokio_util::sync::CancellationToken;
use toolkit::async_trait;
use toolkit::bootstrap::OopCompositionError;
use toolkit::runtime::{ClientCleanup, ClientCleanupError};
use toolkit_taskward::TaskSet;

use super::cache::{MemoryCache, PreparedSweep};
use super::leader::MemoryLeader;
use super::lease::MEMORY_PROVIDER;
use super::lock::MemoryLock;

const PROFILE_FIELD: &str = "event-broker.process.cluster.profiles";
const SWEEP_INTERVAL_FIELD: &str = "event-broker.process.cluster.sweep_interval";

/// The inputs for preparing the memory client in standalone mode.
pub struct EbMemoryOptions {
    /// Distinct cluster profile names served by this process.
    pub(crate) profiles: Vec<String>,
    /// Interval between expiry sweeps for each profile's cache.
    pub(crate) sweep_interval: Duration,
    /// Metrics shared by the profile caches, locks, and leader elections.
    pub(crate) metrics: Arc<dyn ClusterMetrics>,
}

/// All profile backends and their unstarted expiry workers.
pub struct PreparedMemory {
    client: Arc<MemoryClient>,
    sweeps: BTreeMap<String, PreparedSweep>,
    abort: CancellationToken,
    #[cfg(test)]
    stages: CleanupStages,
}

/// The SDK client with its process-owned cleanup.
pub struct RunningMemory {
    /// Client for the broker's local Cluster SDK binding.
    pub(crate) client: Arc<dyn ClusterClient>,
    /// Process-owned cleanup that shuts down profile resources and joins expiry workers.
    pub(crate) cleanup: ClientCleanup,
}

/// Validates and builds all profiles before starting any worker.
///
/// # Errors
///
/// Returns a static error category when options are invalid or a backend rejects construction.
pub fn prepare_memory(options: EbMemoryOptions) -> Result<PreparedMemory, OopCompositionError> {
    PreparedMemory::prepare(options, |_, cache| cache, ClockMode::Native)
}

#[derive(Clone, Copy)]
enum ClockMode {
    Native,
    #[cfg(test)]
    Virtual,
}

impl PreparedMemory {
    fn prepare(
        options: EbMemoryOptions,
        mut cache_backend: impl FnMut(&str, Arc<MemoryCache>) -> Arc<dyn ClusterCacheBackend>,
        clock: ClockMode,
    ) -> Result<Self, OopCompositionError> {
        if !(1..=64).contains(&options.profiles.len()) {
            return Err(OopCompositionError::InvalidConfig {
                field: PROFILE_FIELD,
            });
        }
        if options.sweep_interval.is_zero() {
            return Err(OopCompositionError::InvalidConfig {
                field: SWEEP_INTERVAL_FIELD,
            });
        }

        let abort = CancellationToken::new();
        let mut profiles = BTreeMap::new();
        let mut sweeps = BTreeMap::new();
        for name in options.profiles {
            if !cluster_sdk::is_valid_cluster_name(&name) {
                return Err(OopCompositionError::InvalidConfig {
                    field: PROFILE_FIELD,
                });
            }
            let Entry::Vacant(entry) = profiles.entry(name.clone()) else {
                return Err(OopCompositionError::InvalidConfig {
                    field: PROFILE_FIELD,
                });
            };
            let raw = MemoryCache::new();
            let cache = Arc::new(InstrumentedCache::new(
                cache_backend(&name, raw.clone()),
                MEMORY_PROVIDER,
                options.metrics.clone(),
            ));
            let reserved = reserved_lease_cache(cache.clone());
            let lock =
                MemoryLock::new(reserved.clone(), options.metrics.clone()).map_err(|_| {
                    OopCompositionError::ClientConstruction {
                        component: "event-broker memory lock",
                    }
                })?;
            let leader = MemoryLeader::new(reserved, options.metrics.clone(), abort.child_token())
                .map_err(|_| OopCompositionError::ClientConstruction {
                    component: "event-broker memory election",
                })?;
            let (lock, leader) = match clock {
                ClockMode::Native => (lock, leader),
                #[cfg(test)]
                ClockMode::Virtual => (
                    lock.with_test_clock(TEST_RETENTION, Self::test_nonce_seed()),
                    leader.with_test_clock(TEST_RETENTION, Self::test_nonce_seed()),
                ),
            };
            let descriptor = ProfileDescriptor {
                name: name.clone(),
                cache: CacheDescriptor {
                    consistency: cache.consistency().into(),
                    features: cache.features().into(),
                    provider: cache.provider_name().to_owned(),
                },
                lock: LockDescriptor {
                    features: lock.features().into(),
                    provider: lock.provider_name().to_owned(),
                },
                leader_election: LeaderElectionDescriptor {
                    features: leader.features().into(),
                    provider: leader.provider_name().to_owned(),
                },
                health: ProfileHealth::Serving,
            };
            sweeps.insert(name, raw.prepare_sweep(options.sweep_interval));
            entry.insert(Profile {
                raw,
                cache,
                lock: Arc::new(lock),
                leader: Arc::new(leader),
                descriptor,
            });
        }

        Ok(Self {
            client: Arc::new(MemoryClient { profiles }),
            sweeps,
            abort,
            #[cfg(test)]
            stages: Arc::default(),
        })
    }

    /// Starts prepared expiry workers and transfers ownership to the cleanup.
    pub(crate) fn start(self) -> RunningMemory {
        let sweeps = self
            .sweeps
            .into_iter()
            .map(|(name, sweep)| (name, sweep.start()))
            .collect();
        let owners = MemoryOwners {
            client: self.client.clone(),
            sweeps,
            abort: self.abort.clone(),
            #[cfg(test)]
            stages: self.stages,
        };
        RunningMemory {
            client: self.client,
            cleanup: ClientCleanup::new(Box::pin(owners.finish()), self.abort),
        }
    }

    #[cfg(test)]
    pub(crate) fn prepare_for_test(
        options: EbMemoryOptions,
        cache_backend: impl FnMut(&str, Arc<MemoryCache>) -> Arc<dyn ClusterCacheBackend>,
    ) -> Result<Self, OopCompositionError> {
        let clock = if tokio::runtime::Handle::try_current().is_ok() {
            ClockMode::Virtual
        } else {
            ClockMode::Native
        };
        Self::prepare(options, cache_backend, clock)
    }

    /// Shares one initial counter across virtual lease fixtures so tests can
    /// predict exact record bytes from the fixture input.
    #[cfg(test)]
    pub(crate) fn test_nonce_seed() -> u64 {
        static SEED: std::sync::LazyLock<u64> = std::sync::LazyLock::new(rand::random::<u64>);
        *SEED
    }

    #[cfg(test)]
    pub(crate) fn test_profile(&self, name: &str) -> TestProfile {
        let profile = self.client.profile(name).expect("profile is prepared");
        TestProfile {
            cache: profile.raw.clone(),
            lock: profile.lock.clone(),
            leader: profile.leader.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_abort(&self) -> CancellationToken {
        self.abort.clone()
    }

    #[cfg(test)]
    pub(crate) fn test_cleanup_stages(&self) -> CleanupStages {
        self.stages.clone()
    }
}

struct Profile {
    raw: Arc<MemoryCache>,
    cache: Arc<InstrumentedCache>,
    lock: Arc<MemoryLock>,
    leader: Arc<MemoryLeader>,
    descriptor: ProfileDescriptor,
}

struct MemoryClient {
    profiles: BTreeMap<String, Profile>,
}

impl MemoryClient {
    fn profile(&self, name: &str) -> Result<&Profile, ClusterError> {
        self.profiles
            .get(name)
            .ok_or_else(|| ClusterError::ProfileNotBound {
                profile: intern_existing(name).unwrap_or("<unknown>"),
            })
    }
}

#[async_trait]
impl ClusterClient for MemoryClient {
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
        Ok(self.profile(profile)?.cache.clone())
    }

    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError> {
        Ok(self.profile(profile)?.lock.clone())
    }

    fn leader_election_backend(
        &self,
        profile: &str,
    ) -> Result<Arc<dyn LeaderElectionBackend>, ClusterError> {
        Ok(self.profile(profile)?.leader.clone())
    }

    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError> {
        let bound = self.profile(profile)?;
        let mut descriptor = bound.descriptor.clone();
        if bound.raw.is_closed() {
            descriptor.health = ProfileHealth::Degraded;
        }
        Ok(descriptor)
    }
}

struct MemoryOwners {
    client: Arc<MemoryClient>,
    sweeps: BTreeMap<String, TaskSet>,
    abort: CancellationToken,
    #[cfg(test)]
    stages: CleanupStages,
}

impl MemoryOwners {
    async fn finish(mut self) -> Result<(), ClientCleanupError> {
        for profile in self.client.profiles.values() {
            profile.leader.close();
        }
        let mut result = Ok(());
        for profile in self.client.profiles.values() {
            if profile.leader.join_workers().await.is_err() {
                result = Err(ClientCleanupError::WorkerFailed);
            }
            #[cfg(test)]
            self.record_stage(&profile.descriptor.name, "leaders");
        }
        for profile in self.client.profiles.values() {
            profile.lock.revoke().await;
            #[cfg(test)]
            self.record_stage(&profile.descriptor.name, "locks");
        }
        for profile in self.client.profiles.values() {
            profile.raw.shutdown();
            #[cfg(test)]
            self.record_stage(&profile.descriptor.name, "cache");
        }
        for (name, sweep) in std::mem::take(&mut self.sweeps) {
            sweep.shutdown().await;
            #[cfg(test)]
            self.record_stage(&name, "sweep");
            #[cfg(not(test))]
            let _ = name;
        }
        result
    }

    #[cfg(test)]
    fn record_stage(&self, name: &str, stage: &'static str) {
        self.stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((name.to_owned(), stage));
    }
}

impl Drop for MemoryOwners {
    fn drop(&mut self) {
        if self.abort.is_cancelled() {
            for profile in self.client.profiles.values() {
                profile.leader.close();
                profile.lock.abort();
                profile.raw.shutdown();
            }
        }
    }
}

#[cfg(test)]
const TEST_RETENTION: Duration = Duration::from_millis(100);

#[cfg(test)]
type CleanupStages = Arc<std::sync::Mutex<Vec<(String, &'static str)>>>;

#[cfg(test)]
pub struct TestProfile {
    pub(crate) cache: Arc<MemoryCache>,
    pub(crate) lock: Arc<MemoryLock>,
    pub(crate) leader: Arc<MemoryLeader>,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct WorkerCounts {
    started: std::sync::atomic::AtomicUsize,
    live: std::sync::atomic::AtomicUsize,
    joined: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl WorkerCounts {
    pub(super) fn track(self: &Arc<Self>) -> WorkerLifetime {
        use std::sync::atomic::Ordering;
        self.started.fetch_add(1, Ordering::Relaxed);
        self.live.fetch_add(1, Ordering::Relaxed);
        WorkerLifetime(self.clone())
    }

    pub(super) fn joined(&self) {
        self.joined
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn snapshot(&self) -> (usize, usize, usize) {
        use std::sync::atomic::Ordering;
        (
            self.started.load(Ordering::Relaxed),
            self.live.load(Ordering::Relaxed),
            self.joined.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
pub(super) struct WorkerLifetime(Arc<WorkerCounts>);

#[cfg(test)]
impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        self.0
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}
