// Created: 2026-06-24 by Constructor Tech
//! Service-discovery conformance scenarios (`SC-DISC-*`).
//!
//! See the [scenario catalog](../docs/scenarios/discovery.md). Cache-only
//! plugins feed the `cluster` gear's `CacheBasedServiceDiscoveryBackend`
//! (`cluster::defaults::CacheBasedServiceDiscoveryBackend` — not a
//! `cluster-conformance` dependency, so not an intra-doc link here) into this
//! suite. The suite must NOT assume a result ordering — `discover` returns
//! instances in an unspecified order.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::discovery::{
    DiscoveryFilter, InstanceState, MetaMatch, ServiceDiscoveryBackend, ServiceRegistration,
    ServiceWatchEvent, StateFilter, TopologyChange,
};

/// Runs every implemented L2 discovery scenario against a fresh backend from
/// `make`.
///
/// # Panics
/// Requires a `current_thread` Tokio runtime — `scenario_disc_006` calls
/// `tokio::time::pause()`, which panics under `multi_thread`.
pub async fn run_discovery_conformance<F>(make: F)
where
    F: Fn() -> Arc<dyn ServiceDiscoveryBackend>,
{
    scenario_disc_001(make()).await;
    scenario_disc_002(make()).await;
    scenario_disc_003(make()).await;
    scenario_disc_004(make()).await;
    scenario_disc_005(make()).await;
    scenario_disc_006(make()).await;
    scenario_disc_007(make()).await;
}

/// SC-DISC-001: a registered, enabled instance is returned by `discover` under
/// the default (enabled-only) filter.
pub async fn scenario_disc_001(backend: Arc<dyn ServiceDiscoveryBackend>) {
    let handle = backend
        .register(registration("svc", "10.0.0.1:8080", &[]))
        .await
        .expect("register must succeed");
    assert!(
        !handle.instance_id().is_empty(),
        "SC-DISC-001: a registered instance must be assigned a non-empty id"
    );
    let found = backend
        .discover("svc", DiscoveryFilter::default())
        .await
        .expect("discover must succeed");
    let instance = found
        .iter()
        .find(|inst| inst.instance_id == handle.instance_id())
        .expect("SC-DISC-001: a registered enabled instance must be discoverable");
    assert_eq!(
        instance.state,
        InstanceState::Enabled,
        "SC-DISC-001: a freshly registered instance must default to Enabled"
    );
}

/// SC-DISC-002: the default filter returns only `Enabled` instances;
/// `StateFilter::Any` includes disabled ones too.
pub async fn scenario_disc_002(backend: Arc<dyn ServiceDiscoveryBackend>) {
    let ha = backend
        .register(registration("svc", "a:1", &[]))
        .await
        .expect("register a");
    let hb = backend
        .register(registration("svc", "b:1", &[]))
        .await
        .expect("register b");
    hb.set_state(InstanceState::Disabled)
        .await
        .expect("disable b");

    let enabled_only = backend
        .discover("svc", DiscoveryFilter::default())
        .await
        .expect("discover enabled");
    let ids: std::collections::HashSet<&str> = enabled_only
        .iter()
        .map(|i| i.instance_id.as_str())
        .collect();
    assert!(
        ids.contains(ha.instance_id()),
        "SC-DISC-002: enabled instance must appear under the default filter"
    );
    assert!(
        !ids.contains(hb.instance_id()),
        "SC-DISC-002: disabled instance must be excluded by the default filter"
    );

    let mut any_filter = DiscoveryFilter::default();
    any_filter.state = StateFilter::Any;
    let all = backend
        .discover("svc", any_filter)
        .await
        .expect("discover any");
    assert!(
        all.iter().any(|i| i.instance_id == hb.instance_id()),
        "SC-DISC-002: disabled instance must appear under StateFilter::Any"
    );
}

/// SC-DISC-003: metadata predicates AND-combine — `Equals` and `OneOf` both
/// participate, and an instance is returned only when it satisfies *every*
/// predicate.
pub async fn scenario_disc_003(backend: Arc<dyn ServiceDiscoveryBackend>) {
    // Instance A: region=us-east, tier=gold  → matches both predicates
    // Instance B: region=us-east, tier=silver → fails the tier predicate
    // Instance C: region=eu-west, tier=gold   → fails the region predicate
    let ha = backend
        .register(registration(
            "svc",
            "a:1",
            &[("region", "us-east"), ("tier", "gold")],
        ))
        .await
        .expect("register a");
    let hb = backend
        .register(registration(
            "svc",
            "b:1",
            &[("region", "us-east"), ("tier", "silver")],
        ))
        .await
        .expect("register b");
    let hc = backend
        .register(registration(
            "svc",
            "c:1",
            &[("region", "eu-west"), ("tier", "gold")],
        ))
        .await
        .expect("register c");

    // region=us-east (Equals) AND tier=OneOf([gold]) → only A matches.
    let mut filter = DiscoveryFilter::default();
    filter.metadata = vec![
        ("region".to_owned(), MetaMatch::Equals("us-east".to_owned())),
        ("tier".to_owned(), MetaMatch::OneOf(vec!["gold".to_owned()])),
    ];
    let found = backend
        .discover("svc", filter)
        .await
        .expect("discover must succeed");
    let ids: std::collections::HashSet<&str> =
        found.iter().map(|i| i.instance_id.as_str()).collect();
    assert!(
        ids.contains(ha.instance_id()),
        "SC-DISC-003: A (us-east + gold) must satisfy both predicates"
    );
    assert!(
        !ids.contains(hb.instance_id()),
        "SC-DISC-003: B (us-east + silver) must fail the tier predicate"
    );
    assert!(
        !ids.contains(hc.instance_id()),
        "SC-DISC-003: C (eu-west + gold) must fail the region predicate"
    );
}

/// SC-DISC-004: `discover`'s result order is unspecified — the suite must
/// compare result sets, never assume positional ordering.
pub async fn scenario_disc_004(backend: Arc<dyn ServiceDiscoveryBackend>) {
    let h1 = backend
        .register(registration("svc", "a:1", &[]))
        .await
        .expect("register a");
    let h2 = backend
        .register(registration("svc", "b:1", &[]))
        .await
        .expect("register b");
    let found = backend
        .discover("svc", DiscoveryFilter::default())
        .await
        .expect("discover must succeed");
    let ids: std::collections::HashSet<&str> =
        found.iter().map(|i| i.instance_id.as_str()).collect();
    assert_eq!(
        ids,
        [h1.instance_id(), h2.instance_id()].into_iter().collect(),
        "SC-DISC-004: discover must return both instances regardless of registration order"
    );
}

/// SC-DISC-005: draining an instance via `set_state(Disabled)` removes it from
/// the default filter; explicit `deregister()` sends `Left` and removes it fully.
pub async fn scenario_disc_005(backend: Arc<dyn ServiceDiscoveryBackend>) {
    let mut watch = backend.watch("svc").await.expect("watch");
    let handle = backend
        .register(registration("svc", "a:1", &[]))
        .await
        .expect("register");
    // Wait for Joined.
    let id = handle.instance_id().to_owned();
    wait_for_discovery_event(
        &mut watch,
        |e| matches!(e, TopologyChange::Joined(i) if i.instance_id == id),
    )
    .await;

    // Drain.
    handle
        .set_state(InstanceState::Disabled)
        .await
        .expect("disable");
    let after_drain = backend
        .discover("svc", DiscoveryFilter::default())
        .await
        .expect("discover after drain");
    assert!(
        !after_drain.iter().any(|i| i.instance_id == id),
        "SC-DISC-005: disabled instance must be excluded by the default filter"
    );

    // Deregister.
    handle.deregister().await.expect("deregister");
    assert!(
        wait_for_discovery_event(&mut watch, |e| {
            matches!(e, TopologyChange::Left { instance_id } if instance_id == &id)
        })
        .await,
        "SC-DISC-005: deregister must emit Left"
    );
    let after_dereg = backend
        .discover("svc", DiscoveryFilter::any())
        .await
        .expect("discover any after deregister");
    assert!(
        !after_dereg.iter().any(|i| i.instance_id == id),
        "SC-DISC-005: deregistered instance must not appear under any filter"
    );
}

/// SC-DISC-006: a registration that stops heartbeating disappears after its TTL
/// lapses — purely via time, no explicit `set_state`/`deregister` call.
///
/// # Panics
/// Requires a `current_thread` Tokio runtime — `tokio::time::pause()` panics
/// under `multi_thread`.
pub async fn scenario_disc_006(backend: Arc<dyn ServiceDiscoveryBackend>) {
    tokio::time::pause();
    let handle = backend
        .register(registration("svc", "a:1", &[]))
        .await
        .expect("register");
    let id = handle.instance_id().to_owned();
    // Advance past the backend's default TTL (30 s) without renewing.
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::task::yield_now().await;
    let found = backend
        .discover("svc", DiscoveryFilter::any())
        .await
        .expect("discover");
    assert!(
        !found.iter().any(|i| i.instance_id == id),
        "SC-DISC-006: registration must disappear after TTL lapses without heartbeat"
    );
    tokio::time::resume();
}

/// SC-DISC-007: `watch` surfaces raw `Joined`/`Updated`/`Left` events unfiltered
/// — the watch is not subject to `DiscoveryFilter` (filtering is consumer-side).
pub async fn scenario_disc_007(backend: Arc<dyn ServiceDiscoveryBackend>) {
    let mut watch = backend.watch("svc").await.expect("watch");

    let handle = backend
        .register(registration("svc", "a:1", &[("env", "prod")]))
        .await
        .expect("register");
    let id = handle.instance_id().to_owned();

    // Joined.
    assert!(
        wait_for_discovery_event(&mut watch, |e| {
            matches!(e, TopologyChange::Joined(i) if i.instance_id == id)
        })
        .await,
        "SC-DISC-007: register must emit Joined"
    );

    // Updated (metadata change).
    let mut new_meta = HashMap::new();
    new_meta.insert("env".to_owned(), "staging".to_owned());
    handle
        .update_metadata(new_meta)
        .await
        .expect("update metadata");
    assert!(
        wait_for_discovery_event(&mut watch, |e| {
            matches!(e, TopologyChange::Updated(i) if i.instance_id == id)
        })
        .await,
        "SC-DISC-007: update_metadata must emit Updated"
    );

    // Left.
    handle.deregister().await.expect("deregister");
    assert!(
        wait_for_discovery_event(&mut watch, |e| {
            matches!(e, TopologyChange::Left { instance_id } if instance_id == &id)
        })
        .await,
        "SC-DISC-007: deregister must emit Left"
    );
}

// TODO(SC-DISC-008): service name is scoped; metadata is not scoped — requires
//   the `ServiceDiscoveryV1::scoped()` facade (needs `ClientHub`, deferred to L3).
// TODO(SC-DISC-009) [L4]: recover membership after lag/reset — fault-injection harness.

/// Polls a service watch for up to 64 events, returning `true` once one satisfies
/// `pred`. Bounded so a missing event fails fast rather than hanging.
async fn wait_for_discovery_event<P>(
    watch: &mut cluster_sdk::discovery::ServiceWatch,
    pred: P,
) -> bool
where
    P: Fn(&TopologyChange) -> bool,
{
    for _ in 0..64 {
        match watch.recv().await {
            Some(ServiceWatchEvent::Change(change)) if pred(&change) => return true,
            Some(ServiceWatchEvent::Closed(_)) | None => return false,
            _ => {}
        }
    }
    false
}

/// Builds a registration with `metadata` key/value pairs and a backend-assigned
/// instance id.
fn registration(name: &str, address: &str, metadata: &[(&str, &str)]) -> ServiceRegistration {
    ServiceRegistration {
        name: name.to_owned(),
        instance_id: None,
        address: address.to_owned(),
        metadata: metadata
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<HashMap<_, _>>(),
    }
}
