//! Tests for the operator YAML schema and the config-driven wiring path, wired
//! against the real [`StandaloneCacheProvider`] from the plugin crate — the same
//! provider a host assembles into the registry in production.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::lock::{DistributedLockBackend, LockFeatures, LockGuard};
use cluster_sdk::{
    CacheCapability, CacheConsistencyDto, ClusterCacheV1, ClusterError, ClusterLockProvider,
    ClusterProfile, DistributedLockV1, LeaderElectionV1, ProfileHealth, StopHook,
};
use standalone_cluster_plugin::StandaloneCacheProvider;
use toolkit::client_hub::ClientHub;

use crate::domain::wiring::ClusterHandle;
use crate::{
    BoundProfile, ClusterConfig, ClusterWiring, InstanceId, ProfileRegistry, ProviderRegistry,
};

/// The step the gear's `start` takes after [`ClusterWiring::from_config`]:
/// publish the bound set and register the local client, which is what makes the
/// profiles resolvable in this process (DESIGN-DEPLOYABLE-GEAR §4.9.3). A test
/// standing in for the gear has to do what the gear does.
///
/// The registry comes back because clearing it at shutdown is the other half of
/// that job — the gear's `stop` does it, and so does a test that asserts stop
/// unbinds.
fn publish(handle: &mut ClusterHandle, bound: Vec<Arc<BoundProfile>>) -> Arc<ProfileRegistry> {
    let profiles = Arc::new(ProfileRegistry::new());
    handle.publish(&profiles, bound);
    profiles
}

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

// ---------------------------------------------------------------------------
// `fence_retention` (§5.8.1, item `L3`)
// ---------------------------------------------------------------------------

/// The key is written the way every other duration in platform config is, and
/// DESIGN §4.10's example YAML uses exactly this form.
#[test]
fn fence_retention_parses_a_humantime_duration() {
    let yaml = "
fence_retention: 1h
profiles:
  event-broker:
    cache: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    assert_eq!(cfg.fence_retention, Some(Duration::from_hours(1)));
    assert_eq!(
        cfg.fence_retention().expect("an hour is valid"),
        Duration::from_hours(1)
    );
}

/// Omitting it is the common case, and it must not mean "no window".
#[test]
fn fence_retention_defaults_to_the_sdk_constant() {
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    assert_eq!(cfg.fence_retention, None, "nothing was configured");
    assert_eq!(
        cfg.fence_retention().expect("the default is valid"),
        cluster_sdk::lease::FENCE_RETENTION_DEFAULT,
        "and the default is what the backends get"
    );
}

/// Zero is the one value that silently defeats the point: the record's physical
/// expiry collapses onto the lease deadline and the fence resets on the next
/// reap. It is refused, by name, before any backend is built.
#[tokio::test]
async fn a_zero_window_fails_startup_by_name() {
    let yaml = "
fence_retention: 0s
profiles:
  event-broker:
    cache: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let err = cfg
        .fence_retention()
        .expect_err("a zero window must be rejected");
    let ClusterError::InvalidConfig { reason } = &err else {
        panic!("expected InvalidConfig, got {err:?}");
    };
    assert!(
        reason.contains("fence_retention"),
        "the error must name the key an operator has to change: {reason}"
    );

    // And the wiring refuses to build anything at all, rather than starting a
    // pool and failing later.
    let hub = Arc::new(ClientHub::new());
    let wired = ClusterWiring::from_config(hub, &cfg, &standalone_registry()).await;
    assert!(
        wired.is_err(),
        "from_config must fail before a backend is constructed"
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

    let (mut handle, bound) =
        ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry())
            .await
            .expect("wiring starts from config");
    assert_eq!(
        bound.len(),
        1,
        "one configured profile is returned as bound"
    );
    let profiles = publish(&mut handle, bound);

    assert!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .require(CacheCapability::Linearizable)
            .resolve()
            .await
            .is_ok(),
        "the configured cache resolves"
    );
    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .await
            .is_ok(),
        "omit-default leader election resolves"
    );
    assert!(
        DistributedLockV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .await
            .is_ok(),
        "omit-default lock resolves"
    );

    // Both halves of the gear's `stop`: clear the published set, then tear the
    // wiring down (§4.8 phases 3-4). The cluster client stays registered, so the
    // refusal names the profile rather than reporting nothing-wired.
    profiles.clear();
    handle.stop().await;

    assert!(matches!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .await,
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

#[tokio::test]
async fn from_config_returns_provider_identity_and_declared_features() {
    // DESIGN 5.1/5.2: the hub cannot answer "which provider serves this profile"
    // or "what does it declare", so the bound-profile set carries both. Provider
    // identity is the operator's name for the backend, not the Rust type name -
    // it is what reaches a consumer in `CapabilityNotMet { provider }` (5.5).
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());

    let (handle, bound) =
        ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry())
            .await
            .expect("wiring starts from config");

    let profile = &bound[0];
    assert_eq!(profile.name, "event-broker");
    assert_eq!(profile.descriptor().name, "event-broker");
    assert_eq!(
        profile.descriptor().cache.provider,
        "standalone",
        "the cache reports the configured provider name"
    );
    // Both omitted primitives ride the SDK default over this profile's cache, so
    // the provider serving them is the cache's provider - that is where their
    // lease records live.
    assert_eq!(profile.descriptor().lock.provider, "standalone");
    assert_eq!(profile.descriptor().leader_election.provider, "standalone");

    // Declared characteristics are read off the real backends, so a descriptor
    // cannot claim a capability the backend does not declare.
    assert_eq!(
        profile.descriptor().cache.consistency,
        CacheConsistencyDto::Linearizable,
        "the standalone cache declares linearizable"
    );
    assert!(
        profile.descriptor().cache.features.prefix_watch,
        "the standalone cache declares a native prefix watch"
    );
    assert!(
        profile.descriptor().lock.features.linearizable,
        "the CAS default over a linearizable cache declares linearizable exclusion"
    );
    assert!(profile.descriptor().leader_election.features.linearizable);
    assert_eq!(
        profile.descriptor().health,
        ProfileHealth::Serving,
        "a profile whose backends all built reports Serving until a probe says otherwise"
    );

    handle.stop().await;
}

#[tokio::test]
async fn from_config_returns_per_primitive_instance_refs() {
    // DESIGN 5.3: the refs say which backend *instance* serves each primitive, so
    // sharing is observable. Within one profile an auto-filled SDK default is a
    // distinct instance layered over the cache instance; across two profiles
    // nothing is deduplicated yet, so two `standalone` caches are two instances.
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
  scheduler:
    cache: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());

    let (handle, bound) =
        ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry())
            .await
            .expect("wiring starts from config");

    assert_eq!(bound.len(), 2, "both configured profiles come back bound");
    let broker_profile = bound
        .iter()
        .find(|p| p.name == "event-broker")
        .expect("event-broker is bound");
    let broker = &broker_profile.instances;
    let scheduler = &bound
        .iter()
        .find(|p| p.name == "scheduler")
        .expect("scheduler is bound")
        .instances;

    // Each id names the instance the profile actually holds - the bound set keeps
    // a strong reference to it, so the id cannot go stale while it is reachable.
    assert_eq!(broker.cache, InstanceId::of(&broker_profile.cache));

    assert_ne!(
        broker.cache, broker.lock,
        "the CAS default lock is its own instance over the cache instance"
    );
    assert_ne!(broker.lock, broker.leader_election);
    assert_ne!(
        broker.cache, scheduler.cache,
        "two profiles each build their own cache instance today (the instance cache is DESIGN 5.3, not yet wired)"
    );

    handle.stop().await;
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
    // No partial registration leaks past the failure. Since `K4` a failed wiring
    // leaves *nothing* in the hub - not even a cluster client - so the report is
    // the nothing-wired one: `resolve()` succeeds and the first call names the
    // profile (§4.9.1).
    let Ok(cache) = ClusterCacheV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .await
    else {
        panic!("an empty hub must not fail resolution");
    };
    assert!(matches!(
        cache.get("k").await,
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
    // No partial registration leaks past the failure. Since `K4` a failed wiring
    // leaves *nothing* in the hub - not even a cluster client - so the report is
    // the nothing-wired one: `resolve()` succeeds and the first call names the
    // profile (§4.9.1).
    let Ok(cache) = ClusterCacheV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .await
    else {
        panic!("an empty hub must not fail resolution");
    };
    assert!(matches!(
        cache.get("k").await,
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

    let (mut handle, bound) = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &registry)
        .await
        .expect("a mixed-backend profile must wire");

    // Per-primitive routing shows up in the descriptor: the lock names its own
    // provider while leader election still names the cache it rides.
    let descriptor = bound[0].descriptor();
    let _profiles = publish(&mut handle, bound.clone());
    assert_eq!(descriptor.cache.provider, "standalone");
    assert_eq!(descriptor.lock.provider, "fake-native");
    assert_eq!(descriptor.leader_election.provider, "standalone");
    assert!(
        descriptor.lock.features.linearizable,
        "the native lock's own declared features are reported, not the CAS default's"
    );

    // All three primitives resolve under the one profile, per the requirement's
    // "consumer gears see four working primitives" clause.
    assert!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .await
            .is_ok(),
        "the mixed profile's cache resolves"
    );
    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .await
            .is_ok(),
        "leader election still rides the omit-default auto-wrap over the cache"
    );

    let lock = DistributedLockV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .await
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
