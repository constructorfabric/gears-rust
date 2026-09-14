//! `K3`'s exit criteria over the real `ConsumerRegistration` and the real
//! `ClientHub` (DESIGN.md).
//!
//! Two things are asserted here that no other test can reach. The `wire` closure
//! is a private `fn` submitted to `inventory`, so this module calls it directly
//! rather than through the framework's phase — `cluster/tests/oop_probe_ordering.rs`
//! is where the *replay* is exercised end to end, because that needs a host
//! runtime. And `derive_endpoint` reads `POD_NAMESPACE`, so every case that
//! touches it goes through `temp_env`: `std::env::set_var` is `unsafe` in edition
//! 2024 and the workspace forbids `unsafe_code`, which makes the hermetic helper
//! the only available shape rather than a stylistic preference.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;
use toolkit::discovery::{EndpointResolver, NullEndpointResolver, WireOutcome};

use super::{
    CLUSTER_GEAR, CLUSTER_GRPC_PORT, POD_NAMESPACE_ENV, derive_endpoint, register_remote_client,
    wire,
};
use crate::ClusterError;
use crate::client::ClusterClient;
use crate::profile::{ClusterProfile, registered_profiles};
use crate::test_support::StubClusterClient;

/// A namespace no real deployment would use, so a leaked variable is obvious.
const NS: &str = "platform-test";

fn resolver() -> Arc<dyn EndpointResolver> {
    Arc::new(NullEndpointResolver)
}

// derive_endpoint

#[test]
fn endpoint_is_the_kubernetes_service_name_and_the_convention_port() {
    temp_env::with_var(POD_NAMESPACE_ENV, Some(NS), || {
        let endpoint = derive_endpoint().expect("a valid namespace yields an endpoint");
        assert_eq!(
            endpoint,
            format!("http://{CLUSTER_GEAR}.{NS}.svc.cluster.local:{CLUSTER_GRPC_PORT}"),
            "the endpoint is built from convention alone - gear name, namespace, port"
        );
        // Spelled out once, so a change to any of the three constants is visible
        // here as a diff rather than as a formatted-string tautology.
        assert_eq!(
            endpoint, "http://cluster.platform-test.svc.cluster.local:50051",
            "the literal an operator's Service and NetworkPolicy must match"
        );
    });
}

#[test]
fn a_missing_namespace_is_a_named_error_not_a_default() {
    temp_env::with_var_unset(POD_NAMESPACE_ENV, || {
        let Err(ClusterError::InvalidConfig { reason }) = derive_endpoint() else {
            panic!("an unset namespace must be InvalidConfig, not a guess at `default`");
        };
        assert!(
            reason.contains(POD_NAMESPACE_ENV),
            "the error must name the variable an operator has to set, got: {reason}"
        );
    });
}

#[test]
fn an_empty_or_blank_namespace_reads_as_unset() {
    for value in ["", "   "] {
        temp_env::with_var(POD_NAMESPACE_ENV, Some(value), || {
            assert!(
                matches!(derive_endpoint(), Err(ClusterError::InvalidConfig { .. })),
                "a blank namespace (`{value}`) must not build an endpoint with an empty label"
            );
        });
    }
}

/// A namespace that is not a DNS label would silently change the *host* the
/// channel connects to, which is the one failure mode a `format!` cannot survive.
#[test]
fn a_namespace_that_is_not_a_dns_label_is_rejected() {
    for value in [
        "ns/evil",      // a path separator: changes the authority
        "ns.other.svc", // extra labels: a different service entirely
        "ns:9999",      // an embedded port
        "under_score",  // legal in a cluster name, illegal in a DNS label
        "has space",
    ] {
        temp_env::with_var(POD_NAMESPACE_ENV, Some(value), || {
            let result = derive_endpoint();
            assert!(
                matches!(result, Err(ClusterError::InvalidConfig { .. })),
                "`{value}` must be rejected as a namespace, got {result:?}"
            );
        });
    }
}

// The wire closure

#[derive(Clone, Copy)]
struct OrdersProfile;
impl ClusterProfile for OrdersProfile {
    const NAME: &'static str = "orders";
}

/// The local-wins branch, which is the whole of the embedded/remote decision
/// (§4.9.3 step 1). Note what is asserted: not merely the returned outcome, but
/// that **no proxy was registered** — because registering one would poison
/// `try_get_local` for the rest of the process.
#[test]
fn a_local_client_wins_and_no_channel_is_built() {
    let hub = ClientHub::default();
    StubClusterClient::for_profile(OrdersProfile::NAME).register(&hub);
    // Captured before wiring, so the identity assertion below is against the exact
    // `Arc` that was there rather than against whatever is there afterwards.
    let local = hub
        .try_get_local::<dyn ClusterClient>()
        .expect("the stub registers under the trait");

    // Unset, so that a closure which ignored the local client would fail loudly
    // rather than silently building an endpoint.
    let outcome = temp_env::with_var_unset(POD_NAMESPACE_ENV, || {
        wire(&hub, resolver(), None).expect("wiring must not fail when a local client is present")
    });

    assert_eq!(outcome, WireOutcome::Local);
    assert!(
        !hub.has_remote_proxy::<dyn ClusterClient>(),
        "the local branch must register nothing - a proxy record here would make the \
         local client invisible to every later try_get_local"
    );
    let found = hub
        .try_get_local::<dyn ClusterClient>()
        .expect("the local client is still local");
    assert!(
        Arc::ptr_eq(&found, &local),
        "the very client that was there must still be the one in the hub"
    );
}

/// The remote branch. `connect_lazy` needs a reactor context (a `K2` finding), and
/// the prefetch is spawned, so this is a `#[tokio::test]` — but nothing is awaited
/// and no I/O happens: the endpoint points at a name that does not resolve.
#[tokio::test]
async fn an_empty_hub_gets_a_remote_proxy_and_reports_remote() {
    let hub = ClientHub::default();

    let outcome = temp_env::with_var(POD_NAMESPACE_ENV, Some(NS), || {
        wire(&hub, resolver(), None).expect("a derivable endpoint wires without error")
    });

    assert_eq!(outcome, WireOutcome::Remote);
    assert!(
        hub.has_remote_proxy::<dyn ClusterClient>(),
        "the remote client must be recorded as a proxy, so a second consumer wiring the \
         same contract reports Remote rather than mistaking it for a co-located impl"
    );
    assert!(
        hub.try_get_local::<dyn ClusterClient>().is_none(),
        "try_get_local must not see a proxy"
    );
    assert!(
        hub.try_get::<dyn ClusterClient>().is_some(),
        "resolve() looks it up with try_get, which does see it"
    );
}

/// Wiring twice in one process — two consumer gears, or the wiring phase followed
/// by `resolve()`'s self-construction — must not build a second channel.
#[tokio::test]
async fn wiring_twice_reuses_the_first_client() {
    let hub = ClientHub::default();

    let (first, second) = temp_env::with_var(POD_NAMESPACE_ENV, Some(NS), || {
        let first = register_remote_client(&hub, None).expect("first registration");
        let second = register_remote_client(&hub, None).expect("second registration");
        (first, second)
    });

    assert!(
        Arc::ptr_eq(&first, &second),
        "the second call must hand back the first client, not a second channel"
    );
}

/// An undeivable endpoint fails the wiring phase, which fails startup — and that
/// is the intended classification: a missing deployment variable is a permanent
/// configuration error (§4.7), not something readiness should wait out.
#[test]
fn an_underivable_endpoint_fails_the_wiring_rather_than_reporting_local() {
    let hub = ClientHub::default();
    temp_env::with_var_unset(POD_NAMESPACE_ENV, || {
        let result = wire(&hub, resolver(), None);
        assert!(
            result.is_err(),
            "an empty hub with no derivable endpoint must not report Local - that would \
             mark the dependency readiness-resolved with nothing behind it"
        );
        assert!(
            hub.try_get::<dyn ClusterClient>().is_none(),
            "a failed derivation must leave the hub untouched"
        );
    });
}

// The profile-marker registry

#[derive(Clone, Copy)]
struct InventoriedProfile;
impl ClusterProfile for InventoriedProfile {
    const NAME: &'static str = "wiring-tests-inventoried";
}
crate::register_cluster_profile!(InventoriedProfile);

// Registered twice on purpose: a gear and its test fixture may both declare the
// same marker, and the enumeration must not report it as two profiles.
crate::register_cluster_profile!(InventoriedProfile);

#[test]
fn a_registered_marker_is_enumerable_and_deduplicated() {
    let profiles = registered_profiles();
    assert!(
        profiles.contains(&InventoriedProfile::NAME),
        "register_cluster_profile! must put the marker's NAME in the inventory, got {profiles:?}"
    );
    assert_eq!(
        profiles
            .iter()
            .filter(|name| **name == InventoriedProfile::NAME)
            .count(),
        1,
        "a marker registered twice must be enumerated once"
    );
    let mut sorted = profiles.clone();
    sorted.sort_unstable();
    assert_eq!(
        profiles, sorted,
        "the order must be stable across runs - inventory's own order is link order"
    );
}
