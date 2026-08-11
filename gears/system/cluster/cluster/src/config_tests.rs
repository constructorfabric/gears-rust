//! Tests for the operator YAML schema and the config-driven wiring path, wired
//! against the real [`StandaloneCacheProvider`] from the plugin crate — the same
//! provider a host assembles into the registry in production.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::cache::ClusterCacheBackend;
use cluster_sdk::discovery::{
    ServiceDiscoveryBackend, ServiceRegistration, ServiceWatchEvent, TopologyChange,
};
use cluster_sdk::lock::{DistributedLockBackend, LockFeatures, LockGuard};
use cluster_sdk::{
    CacheCapability, ClusterCacheProvider, ClusterCacheV1, ClusterError, ClusterLockProvider,
    ClusterProfile, DistributedLockV1, LeaderElectionV1, ServiceDiscoveryV1, StopHook,
};
use standalone_cluster_plugin::StandaloneCacheProvider;
use toolkit::client_hub::ClientHub;

use crate::defaults::test_cache::MemoryCache;
use crate::defaults::{CacheBasedServiceDiscoveryBackend, ShutdownRevoke};
use crate::{ClusterConfig, ClusterWiring, ProviderRegistry};

fn standalone_registry() -> ProviderRegistry {
    ProviderRegistry::new().with_cache_provider(Arc::new(StandaloneCacheProvider))
}

// The profile the config fixtures name; matches the `event-broker` YAML key.
#[derive(Clone, Copy)]
struct EventBroker;
impl ClusterProfile for EventBroker {
    const NAME: &'static str = "event-broker";
}

#[test]
fn parses_omit_default_profile() {
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let profile = cfg.profiles.get("event-broker").expect("profile present");
    assert_eq!(profile.cache.provider, "standalone");
    assert!(profile.cache.options.is_empty(), "no extra options");
    assert!(profile.leader_election.is_none());
    assert!(profile.lock.is_none());
    assert!(profile.service_discovery.is_none());
}

#[test]
fn parses_flattened_provider_options() {
    let yaml = "
profiles:
  event-broker:
    cache:
      provider: standalone
      sweep_interval_ms: 50
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let cache = &cfg.profiles["event-broker"].cache;
    assert_eq!(cache.provider, "standalone");
    assert_eq!(
        cache
            .options
            .get("sweep_interval_ms")
            .and_then(serde_json::Value::as_u64),
        Some(50),
        "provider-specific option flows into the flattened options map"
    );
}

#[test]
fn unknown_top_level_key_is_rejected() {
    // `deny_unknown_fields` on the profile catches operator typos.
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
    leeder_election: { provider: standalone }
";
    let parsed: Result<ClusterConfig, _> = serde_saphyr::from_str(yaml);
    assert!(
        parsed.is_err(),
        "a misspelled primitive key must be rejected"
    );
}

#[tokio::test]
async fn from_config_wires_all_four_then_stop_unbinds() {
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());

    let handle = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry())
        .await
        .expect("wiring starts from config");

    assert!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .require(CacheCapability::Linearizable)
            .resolve()
            .is_ok(),
        "the configured cache resolves"
    );
    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "omit-default leader election resolves"
    );
    assert!(
        DistributedLockV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "omit-default lock resolves"
    );
    assert!(
        ServiceDiscoveryV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "omit-default service discovery resolves"
    );

    handle.stop().await;

    assert!(matches!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .resolve(),
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

#[tokio::test]
async fn from_config_unknown_provider_fails() {
    let yaml = "
profiles:
  event-broker:
    cache: { provider: redis }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());

    let result = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry()).await;
    assert!(
        matches!(result, Err(ClusterError::InvalidConfig { .. })),
        "an unregistered provider must fail startup"
    );
    // No partial registration leaks past the failure.
    assert!(matches!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .resolve(),
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

#[tokio::test]
async fn from_config_unknown_non_cache_provider_fails() {
    // Per-primitive routing is supported, but each primitive's registry is
    // independent: `standalone` registers a *cache* provider only, so naming it
    // for `leader_election` names nothing. That must fail loudly rather than
    // silently fall back to the SDK default and ignore the operator's intent.
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
    leader_election: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());

    let result = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry()).await;
    let Err(ClusterError::InvalidConfig { reason }) = result else {
        panic!("a non-cache binding naming an unregistered provider must be rejected");
    };
    assert!(
        reason.contains("leader_election") && reason.contains("standalone"),
        "the error must name the primitive and the missing provider, got: {reason}"
    );
    // No partial registration leaks past the failure.
    assert!(matches!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .resolve(),
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

/// A native (non-cache) lock provider standing in for a shipped plugin's
/// purpose-built lock backend — the Postgres plugin's `PostgresLockProvider` is
/// the real one, but it needs a live database, so the wiring-contract test uses
/// a fake that records whether it was the instance actually invoked.
struct FakeNativeLockProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ClusterLockProvider for FakeNativeLockProvider {
    fn provider(&self) -> &'static str {
        "fake-native"
    }

    async fn build_lock(
        &self,
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn DistributedLockBackend>, StopHook), ClusterError> {
        let backend = Arc::new(FakeNativeLock {
            calls: Arc::clone(&self.calls),
        });
        Ok((backend, Box::new(|| Box::pin(async {}))))
    }
}

/// The backend [`FakeNativeLockProvider`] builds. Every entry point bumps the
/// shared counter, so a non-zero count proves the natively-bound backend — not
/// the CAS default the omit-default path would auto-fill — received the call.
struct FakeNativeLock {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl DistributedLockBackend for FakeNativeLock {
    fn features(&self) -> LockFeatures {
        LockFeatures::new(true)
    }

    async fn try_lock(&self, name: &str, _ttl: Duration) -> Result<LockGuard, ClusterError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ClusterError::LockContended {
            name: name.to_owned(),
        })
    }

    async fn lock(
        &self,
        name: &str,
        _ttl: Duration,
        _timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ClusterError::LockContended {
            name: name.to_owned(),
        })
    }
}

#[tokio::test]
async fn from_config_wires_a_mixed_backend_profile() {
    // UC-004 / `cpt-cf-clst-fr-routing-per-primitive`: one profile, cache served
    // by one provider and lock served by a *different*, native provider, while
    // leader election and service discovery still ride the omit-default
    // auto-wrap over the cache. All four must resolve, and the lock calls must
    // land on the natively-bound backend.
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
    lock: { provider: fake-native }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());
    let lock_calls = Arc::new(AtomicUsize::new(0));
    let registry = standalone_registry().with_lock_provider(Arc::new(FakeNativeLockProvider {
        calls: Arc::clone(&lock_calls),
    }));

    let handle = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &registry)
        .await
        .expect("a mixed-backend profile must wire");

    // All four primitives resolve under the one profile, per the requirement's
    // "consumer gears see four working primitives" clause.
    assert!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "the mixed profile's cache resolves"
    );
    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "leader election still rides the omit-default auto-wrap over the cache"
    );
    assert!(
        ServiceDiscoveryV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "service discovery still rides the omit-default auto-wrap over the cache"
    );

    let lock = DistributedLockV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .expect("the natively-bound lock resolves");
    // `LockContended` is the fake's canned answer; what matters is which instance
    // answered. The CAS default over a fresh standalone cache would have
    // granted this uncontended lock instead.
    assert!(
        matches!(
            lock.try_lock("shard-assignment", Duration::from_secs(5))
                .await,
            Err(ClusterError::LockContended { .. })
        ),
        "the natively-bound lock backend must serve the call, not the CAS default"
    );
    assert_eq!(
        lock_calls.load(Ordering::SeqCst),
        1,
        "the native lock backend must be the registered instance"
    );

    handle.stop().await;
}

/// A cache provider whose backend declares `prefix_watch: false`, forcing the
/// omit-default service-discovery backend onto its `PollingPrefixWatch`
/// polyfill — the shape the Postgres cache backend has in production.
///
/// It hands the test a clone of the cache `Arc` it built, so the test can act on
/// the same shared store from outside the wired backend (see
/// [`from_config_threads_sd_poll_interval_into_the_polling_polyfill`] for why
/// that is what makes the poll cadence observable at all).
struct PollingCacheProvider {
    built: Arc<Mutex<Option<Arc<MemoryCache>>>>,
}

#[async_trait]
impl ClusterCacheProvider for PollingCacheProvider {
    fn provider(&self) -> &'static str {
        "polling-cache"
    }

    async fn build_cache(
        &self,
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
        let cache = MemoryCache::linearizable_without_prefix_watch();
        *self
            .built
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&cache));
        Ok((cache, Box::new(|| Box::pin(async {}))))
    }
}

#[tokio::test]
async fn from_config_threads_sd_poll_interval_into_the_polling_polyfill() {
    // PGR-D3: an operator-set `sd_poll_interval_ms` on the cache binding must
    // reach `CacheBasedServiceDiscoveryBackend::with_prefix_watch_polling`
    // through the omit-default auto-wrap.
    //
    // Making that observable takes care. Registering through the *wired* backend
    // proves nothing about cadence: `register` pre-inserts into that instance's
    // own local view, so its watch reports the join immediately whatever the
    // poll interval is. The cadence only governs how fast the backend notices a
    // record it did *not* write — the real multi-process case the polyfill
    // exists for. So a second backend over the same cache stands in for another
    // instance, and the wired backend's watch can only learn of that
    // registration on a poll tick. At the configured 25ms the join lands well
    // inside the timeout; at the 5s backend default it could not.
    let yaml = "
profiles:
  event-broker:
    cache:
      provider: polling-cache
      sd_poll_interval_ms: 25
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());
    let built = Arc::new(Mutex::new(None));
    let registry = ProviderRegistry::new().with_cache_provider(Arc::new(PollingCacheProvider {
        built: Arc::clone(&built),
    }));

    let handle = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &registry)
        .await
        .expect("wiring starts from config");

    let discovery = ServiceDiscoveryV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .expect("omit-default service discovery resolves");

    let mut watch = discovery
        .watch("delivery")
        .await
        .expect("the polyfill watch must establish over a prefix_watch:false cache");

    // Let the polyfill's initial sweep land before the peer writes anything.
    // Without this the peer's registration can slip in *before* the first sweep
    // and be picked up by it, which happens at any interval — the assertion
    // below would then hold even with the cadence hard-coded to 5s. Waiting past
    // the configured 25ms (and well short of the 5s default) forces the join to
    // be observed by a *subsequent* tick, which is the thing under test.
    tokio::time::sleep(POST_FIRST_SWEEP_WAIT).await;

    // Another instance of the same service, sharing the cache but not the wired
    // backend's local view.
    let shared_cache = built
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("the provider built a cache");
    let other_instance = CacheBasedServiceDiscoveryBackend::new(shared_cache);
    let registered = other_instance
        .register(ServiceRegistration {
            name: "delivery".to_owned(),
            instance_id: None,
            address: "10.0.0.2:9000".to_owned(),
            metadata: std::collections::HashMap::new(),
        })
        .await
        .expect("the peer instance registers");

    let joined = tokio::time::timeout(SD_POLL_ASSERT_TIMEOUT, async {
        loop {
            match watch.recv().await {
                Some(ServiceWatchEvent::Change(TopologyChange::Joined(instance)))
                    if instance.instance_id == registered.instance_id() =>
                {
                    break true;
                }
                Some(_) => {}
                None => break false,
            }
        }
    })
    .await
    .expect(
        "the peer's join must arrive within the configured 25ms cadence; \
         exceeding this timeout means the wiring fell back to the 5s default",
    );
    assert!(
        joined,
        "the polyfill watch must surface a peer registration it did not write"
    );

    drop(registered);
    other_instance.revoke().await;
    handle.stop().await;
}

/// Comfortably above the 25ms cadence the test configures and comfortably below
/// the 5s backend default, so the assertion distinguishes the two rather than
/// passing under either.
const SD_POLL_ASSERT_TIMEOUT: Duration = Duration::from_millis(1_500);

/// How long to wait after opening the watch before the peer registers, so the
/// polyfill's initial sweep is certainly behind us. Many multiples of the
/// configured 25ms cadence, a small fraction of the 5s default.
const POST_FIRST_SWEEP_WAIT: Duration = Duration::from_millis(300);
