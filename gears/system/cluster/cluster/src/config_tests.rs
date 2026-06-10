// Created: 2026-06-15 by Constructor Tech
//! Tests for the operator YAML schema and the config-driven wiring path, wired
//! against the real [`StandaloneCacheProvider`] from the plugin crate — the same
//! provider a host assembles into the registry in production.

use std::sync::Arc;

use cluster_sdk::{
    CacheCapability, ClusterCacheV1, ClusterError, ClusterProfile, DistributedLockV1,
    LeaderElectionV1, ServiceDiscoveryV1,
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

    let result = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry());
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
async fn from_config_explicit_non_cache_binding_is_rejected() {
    // Explicit non-cache bindings aren't constructible yet — fail loudly rather
    // than silently ignore the operator's intent.
    let yaml = "
profiles:
  event-broker:
    cache: { provider: standalone }
    leader_election: { provider: standalone }
";
    let cfg: ClusterConfig = serde_saphyr::from_str(yaml).expect("config parses");
    let hub = Arc::new(ClientHub::new());

    let result = ClusterWiring::from_config(Arc::clone(&hub), &cfg, &standalone_registry());
    assert!(
        matches!(result, Err(ClusterError::InvalidConfig { .. })),
        "an explicit non-cache binding must be rejected"
    );
}
