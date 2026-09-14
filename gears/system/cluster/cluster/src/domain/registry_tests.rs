//! Tests for the profile registry, driven by the real wiring: the bound-profile
//! set `ClusterWiring::from_config` returns is the registry's only data source, so
//! publishing anything else would test a shape nothing produces.

use std::fmt::Write as _;
use std::sync::Arc;

use cluster_sdk::{ClusterError, intern_existing};
use toolkit::client_hub::ClientHub;

use super::{InstanceId, ProfileRegistry};
use crate::{ClusterConfig, ClusterHandle, ClusterWiring, ProviderRegistry};
use standalone_cluster_plugin::StandaloneCacheProvider;

/// Wires `names` as standalone-cache profiles and returns the handle plus the
/// bound set, ready to publish.
async fn wire(names: &[&str]) -> (ClusterHandle, Vec<Arc<super::BoundProfile>>) {
    let mut yaml = String::from("profiles:\n");
    for name in names {
        writeln!(yaml, "  {name}:\n    cache: {{ provider: standalone }}")
            .expect("writing to a String cannot fail");
    }
    let cfg: ClusterConfig = serde_saphyr::from_str(&yaml).expect("config parses");
    let providers = ProviderRegistry::new().with_cache_provider(Arc::new(StandaloneCacheProvider));
    ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
        .await
        .expect("wiring starts")
}

#[test]
fn a_new_registry_is_empty_at_generation_zero() {
    let registry = ProfileRegistry::new();
    assert_eq!(registry.generation(), 0);
    assert!(registry.snapshot().profiles.is_empty());
}

#[tokio::test]
async fn resolve_before_publish_reports_profile_not_bound() {
    // The window between `init` (which creates the registry) and `start` (which
    // publishes into it). A request arriving here must be refused with the frozen
    // error model's existing variant - no new variant, invariant I3.
    let registry = ProfileRegistry::new();

    let err = registry
        .resolve("event-broker")
        .expect_err("nothing is bound before publish");
    assert!(
        matches!(err, ClusterError::ProfileNotBound { .. }),
        "expected ProfileNotBound, got: {err}"
    );

    // And once published, the same lookup succeeds - the registry is the only
    // thing that changed.
    let (handle, bound) = wire(&["event-broker"]).await;
    registry.publish(bound);
    assert!(registry.resolve("event-broker").is_ok());

    handle.stop().await;
}

#[tokio::test]
async fn publish_increments_the_generation_on_every_swap() {
    let registry = ProfileRegistry::new();
    let (handle, bound) = wire(&["event-broker"]).await;

    registry.publish(bound.clone());
    assert_eq!(
        registry.generation(),
        1,
        "the first publish is generation 1"
    );
    assert_eq!(registry.snapshot().generation, 1, "and the snapshot agrees");

    registry.publish(bound);
    assert_eq!(
        registry.generation(),
        2,
        "a re-publish of the same set is still a new generation - it is what a \
         client detects a change by"
    );

    registry.clear();
    assert_eq!(registry.generation(), 3, "clearing is also a swap");

    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publishes_each_land_a_distinct_generation() {
    // The generation bump lives inside `ArcSwap::rcu`, whose compare-and-swap
    // re-runs its closure on contention - so N publishes racing from N tasks must
    // produce exactly N increments, with no duplicate or skipped generation. A
    // naive load-then-store would drop increments under this race and leave a
    // client holding a stale descriptor set it believes current (§5.6). Run on a
    // multi-threaded runtime so the publishes genuinely overlap rather than
    // interleaving cooperatively on one thread.
    const PUBLISHES: u64 = 64;

    let (handle, bound) = wire(&["event-broker"]).await;
    let registry = Arc::new(ProfileRegistry::new());

    let tasks: Vec<_> = (0..PUBLISHES)
        .map(|_| {
            let registry = Arc::clone(&registry);
            // Cheap: clones the `Arc`s, so every task publishes the same backends.
            let bound = bound.clone();
            tokio::spawn(async move { registry.publish(bound) })
        })
        .collect();
    for task in tasks {
        task.await.expect("a publish task must not panic");
    }

    assert_eq!(
        registry.generation(),
        PUBLISHES,
        "each concurrent publish must land its own generation - the rcu retry \
         means none is lost to a load-then-store race"
    );

    handle.stop().await;
}

#[tokio::test]
async fn resolve_returns_the_real_backends_with_no_wrapper() {
    // Invariant I14: the embedded hot path must be unchanged, so what comes back
    // is the very same instance the wiring built - asserted by instance identity,
    // not by behaviour, since a wrapper would behave identically.
    let (handle, bound) = wire(&["event-broker"]).await;
    let expected = bound[0].instances;
    let registry = ProfileRegistry::new();
    registry.publish(bound);

    let resolved = registry
        .resolve("event-broker")
        .expect("the profile resolves");

    assert_eq!(InstanceId::of(&resolved.cache), expected.cache);
    assert_eq!(InstanceId::of(&resolved.lock), expected.lock);
    assert_eq!(
        InstanceId::of(&resolved.leader_election),
        expected.leader_election
    );

    handle.stop().await;
}

#[tokio::test]
async fn a_snapshot_enumerates_every_published_profile() {
    let (handle, bound) = wire(&["event-broker", "scheduler"]).await;
    let registry = ProfileRegistry::new();
    registry.publish(bound);

    let snapshot = registry.snapshot();
    assert_eq!(
        snapshot.profiles.keys().copied().collect::<Vec<_>>(),
        vec!["event-broker", "scheduler"],
        "profiles enumerate in name order, so DescribeProfiles is deterministic"
    );
    assert!(registry.resolve("scheduler").is_ok());
    assert!(matches!(
        registry.resolve("shipping"),
        Err(ClusterError::ProfileNotBound { .. })
    ));

    handle.stop().await;
}

#[tokio::test]
async fn clear_stops_resolving_the_profiles_it_held() {
    let (handle, bound) = wire(&["event-broker"]).await;
    let registry = ProfileRegistry::new();
    registry.publish(bound);
    assert!(registry.resolve("event-broker").is_ok());

    registry.clear();

    assert!(
        matches!(
            registry.resolve("event-broker"),
            Err(ClusterError::ProfileNotBound { .. })
        ),
        "a cleared registry refuses the profiles it held, before their backends \
         are torn down"
    );

    handle.stop().await;
}

#[tokio::test]
async fn a_bound_profile_name_is_interned_and_an_unknown_one_is_not() {
    // `ProfileNotBound.profile` is a `&'static str` and the error model is frozen
    // (I3), so a reportable name must be interned. Interning happens at publish -
    // a bounded, config-derived set - and NOT on the resolve path, where the name
    // arrives in a request and a caller looping over made-up names would otherwise
    // grow the intern table without bound.
    let registry = ProfileRegistry::new();
    let (handle, bound) = wire(&["interning-probe-profile"]).await;
    registry.publish(bound);

    assert!(
        intern_existing("interning-probe-profile").is_some(),
        "publishing interns the profile name"
    );

    // A removed profile still names itself: the name was interned while it was
    // bound, so the diagnostic survives the removal.
    registry.clear();
    let Err(ClusterError::ProfileNotBound { profile }) =
        registry.resolve("interning-probe-profile")
    else {
        panic!("a cleared profile must report ProfileNotBound");
    };
    assert_eq!(profile, "interning-probe-profile");

    // A name that was never bound is refused without being promoted.
    let Err(ClusterError::ProfileNotBound { profile }) = registry.resolve("never-bound-probe")
    else {
        panic!("an unknown profile must report ProfileNotBound");
    };
    assert_eq!(
        profile, "<unknown>",
        "an unregistered name is not echoed into the typed error"
    );
    assert!(
        intern_existing("never-bound-probe").is_none(),
        "and resolving it must not have interned it"
    );

    handle.stop().await;
}
