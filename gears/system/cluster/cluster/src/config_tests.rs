//! Tests for the operator YAML schema and the config-driven wiring path, wired
//! against the real [`StandaloneCacheProvider`] from the plugin crate — the same
//! provider a host assembles into the registry in production.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::lock::{DistributedLockBackend, LockFeatures, LockGuard};
use cluster_sdk::{
    CacheCapability, ClusterCacheV1, ClusterError, ClusterLockProvider, ClusterProfile,
    DistributedLockV1, LeaderElectionV1, StopHook,
};
use standalone_cluster_plugin::StandaloneCacheProvider;
use toolkit::client_hub::ClientHub;

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
async fn from_config_wires_all_three_then_stop_unbinds() {
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
    // leader election still rides the omit-default
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

    // All three primitives resolve under the one profile, per the requirement's
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
