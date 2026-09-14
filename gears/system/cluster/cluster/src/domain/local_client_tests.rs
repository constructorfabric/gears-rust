//! Tests for [`LocalClusterClient`](super::LocalClusterClient), driven by the
//! real wiring: the bound-profile set `ClusterWiring::from_config` returns is the
//! registry's only data source, so a fabricated `BoundProfile` would test a shape
//! nothing produces.
//!
//! Three properties carry the item:
//!
//! - the factory methods hand back the registry's **real** backend `Arc`s, with
//!   nothing interposed (invariant I14) - asserted by pointer equality, because
//!   a wrapper is exactly the kind of change that reads as harmless;
//! - they are callable from a synchronous context, which keeps
//!   `resolve()`'s only `await` the descriptor (DESIGN section 3.1);
//! - `descriptor()` resolves without I/O, asserted as ready-on-first-poll, and
//!   carries the profile's *live* health rather than the value frozen at wiring
//!   time (DESIGN section 4.4).

use std::fmt::Write as _;
use std::sync::Arc;

use cluster_sdk::{ClusterClient, ClusterError, ProfileHealth};
use futures_util::FutureExt as _;
use toolkit::client_hub::ClientHub;

use super::LocalClusterClient;
use crate::domain::registry::ProfileRegistry;
use crate::{ClusterConfig, ClusterHandle, ClusterWiring, ProviderRegistry};
use standalone_cluster_plugin::StandaloneCacheProvider;

const PROFILE: &str = "orders";

/// Wires `names` as standalone-cache profiles, publishes them into a fresh
/// registry, and returns the handle, the registry and a client over it.
async fn client_over(names: &[&str]) -> (ClusterHandle, Arc<ProfileRegistry>, LocalClusterClient) {
    let mut yaml = String::from("profiles:\n");
    for name in names {
        writeln!(yaml, "  {name}:\n    cache: {{ provider: standalone }}")
            .expect("writing to a String cannot fail");
    }
    let cfg: ClusterConfig = serde_saphyr::from_str(&yaml).expect("config parses");
    let providers = ProviderRegistry::new().with_cache_provider(Arc::new(StandaloneCacheProvider));
    let (handle, bound) = ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
        .await
        .expect("wiring starts");

    let registry = Arc::new(ProfileRegistry::new());
    registry.publish(bound);
    let client = LocalClusterClient::new(Arc::clone(&registry));
    (handle, registry, client)
}

#[tokio::test]
async fn the_factory_methods_hand_back_the_registrys_real_backends() {
    // Invariant I14: Profile 1's hot path is unchanged, which means the object a
    // consumer ends up calling is the *same instance* the registry holds - not a
    // wrapper that forwards to it. Pointer equality is the only assertion that
    // can fail if a wrapper is ever introduced.
    let (handle, registry, client) = client_over(&[PROFILE, "audit"]).await;
    let bound = registry.resolve(PROFILE).expect("published");

    let cache = client.cache_backend(PROFILE).expect("bound");
    assert!(
        Arc::ptr_eq(&cache, &bound.cache),
        "cache_backend must return the registry's own Arc, with no wrapper interposed"
    );

    let lock = client.lock_backend(PROFILE).expect("bound");
    assert!(
        Arc::ptr_eq(&lock, &bound.lock),
        "lock_backend must return the registry's own Arc"
    );

    let leader = client.leader_election_backend(PROFILE).expect("bound");
    assert!(
        Arc::ptr_eq(&leader, &bound.leader_election),
        "leader_election_backend must return the registry's own Arc"
    );

    // The second profile is what keeps the three assertions above honest: pointer
    // equality has to be able to *fail* in this setup, and it does - each profile
    // builds its own cache instance.
    let other = client.cache_backend("audit").expect("bound");
    assert!(
        !Arc::ptr_eq(&cache, &other),
        "two profiles must not resolve to one backend, or the assertions above \
         would hold for any implementation"
    );

    handle.stop().await;
}

#[test]
fn the_factory_methods_are_callable_from_a_synchronous_context() {
    // The reason the trait's three factories are sync (DESIGN section 3.1): the
    // only thing `resolve()` may await is the descriptor. Calling them outside
    // `block_on`, with no runtime entered, is the assertion - an `async fn` or an
    // internal `block_on` would not compile or would panic here.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime");
    let (handle, _registry, client) = runtime.block_on(client_over(&[PROFILE]));

    assert!(client.cache_backend(PROFILE).is_ok());
    assert!(client.lock_backend(PROFILE).is_ok());
    assert!(client.leader_election_backend(PROFILE).is_ok());

    runtime.block_on(handle.stop());
}

#[tokio::test]
async fn descriptor_is_ready_on_the_first_poll() {
    // "Resolves without I/O", asserted as the property that actually matters to
    // `K4`: the future completes on its first poll, so the bounded await it will
    // be wrapped in can never fire in Profile 1. A duration assertion would pass
    // for a fast round trip too.
    let (handle, _registry, client) = client_over(&[PROFILE]).await;

    let descriptor = client
        .descriptor(PROFILE)
        .now_or_never()
        .expect("the local descriptor future must be ready on its first poll")
        .expect("the profile is bound");
    assert_eq!(descriptor.name, PROFILE);
    assert_eq!(descriptor.cache.provider, "standalone");

    // The same, on the error path: an unbound profile must not defer either.
    let unbound = client
        .descriptor("nowhere")
        .now_or_never()
        .expect("the refusal is ready on the first poll too");
    assert!(matches!(unbound, Err(ClusterError::ProfileNotBound { .. })));

    handle.stop().await;
}

#[tokio::test]
async fn descriptor_serves_live_health_not_the_wired_value() {
    // `BoundProfile::wired_descriptor` freezes health at wiring time and health
    // moves without republishing the registry (DESIGN section 4.4), so cloning
    // that field - which section 12.4's sketch does - would serve a permanently
    // `Serving` profile no matter what the readiness probes found.
    let (handle, registry, client) = client_over(&[PROFILE]).await;
    let bound = registry.resolve(PROFILE).expect("published");

    let previous = bound.set_health(ProfileHealth::Degraded);
    assert_eq!(previous, ProfileHealth::Serving, "wiring seeds Serving");
    assert_eq!(
        bound.wired_descriptor.health,
        ProfileHealth::Serving,
        "and the wired value is untouched by the probe, which is the trap"
    );

    let descriptor = client.descriptor(PROFILE).await.expect("bound");
    assert_eq!(
        descriptor.health,
        ProfileHealth::Degraded,
        "the served descriptor must overlay the live health"
    );

    handle.stop().await;
}

#[tokio::test]
async fn every_member_reports_profile_not_bound_when_nothing_is_published() {
    // The window between the gear's `init` (which creates the registry) and its
    // `start` (which publishes into it), and again after `stop` clears it. No new
    // error variant exists for this seam - invariant I3.
    let client = LocalClusterClient::new(Arc::new(ProfileRegistry::new()));

    assert!(matches!(
        client.cache_backend(PROFILE),
        Err(ClusterError::ProfileNotBound { .. })
    ));
    assert!(matches!(
        client.lock_backend(PROFILE),
        Err(ClusterError::ProfileNotBound { .. })
    ));
    assert!(matches!(
        client.leader_election_backend(PROFILE),
        Err(ClusterError::ProfileNotBound { .. })
    ));
    assert!(matches!(
        client.descriptor(PROFILE).await,
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

#[tokio::test]
async fn the_client_follows_the_registry_rather_than_pinning_a_snapshot() {
    // It holds the registry, not a snapshot of it: a client handed out before
    // `start` still sees what `start` publishes, and one still held after `stop`
    // sees the cleared set. Both directions, through one client instance.
    let registry = Arc::new(ProfileRegistry::new());
    let client = LocalClusterClient::new(Arc::clone(&registry));
    assert!(
        client.cache_backend(PROFILE).is_err(),
        "nothing published yet"
    );

    let (handle, published, _unused) = client_over(&[PROFILE]).await;
    registry.publish(vec![published.resolve(PROFILE).expect("published")]);
    assert!(
        client.cache_backend(PROFILE).is_ok(),
        "the pre-existing client sees a later publish"
    );

    registry.clear();
    assert!(
        matches!(
            client.cache_backend(PROFILE),
            Err(ClusterError::ProfileNotBound { .. })
        ),
        "and sees the clearing swap `stop` performs"
    );

    handle.stop().await;
}
