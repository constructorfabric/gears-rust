//! Direct tests of the SDK's client choice and a `HostRuntime` test of real
//! inventory replay. The host fixture isolates addressing with a preinstalled
//! client whose descriptor is unavailable.
//!
//! Cases that set or clear `POD_NAMESPACE` use `temp_env` to isolate changes
//! to the process environment.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit::directory::test_support::ManagerDirectoryClient;
use toolkit::discovery::{EndpointResolver, NullEndpointResolver, WireOutcome};
use toolkit::registry::RegistryBuilder;
use toolkit::runtime::{DbOptions, DependencyChecker, GearManager, HostRuntime};
use toolkit::{ConfigProvider, DirectoryClient, Gear, GearCtx};
use toolkit_contract::wiring::ClientTuning;

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

/// Local lookup has priority over the proxy map. The test checks that wiring
/// returns `Local`, preserves the original local `Arc` and adds no proxy marker.
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
        wire(&hub, resolver(), ClientTuning::default())
            .expect("wiring must not fail when a local client is present")
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

#[test]
fn a_preinstalled_cluster_proxy_is_reused_from_the_root_and_an_owner_view() {
    let root = ClientHub::new();
    let proxy: Arc<dyn ClusterClient> =
        Arc::new(StubClusterClient::for_profile(OrdersProfile::NAME));
    root.register_remote_proxy(Arc::clone(&proxy));
    let owner = root.for_gear("event-broker");

    // A synchronous caller has no Tokio runtime for descriptor prefetch.
    temp_env::with_var_unset(POD_NAMESPACE_ENV, || {
        for (hub, has_proxy) in [(&root, true), (owner.as_ref(), false)] {
            assert!(hub.try_get_local::<dyn ClusterClient>().is_none());
            assert_eq!(hub.has_remote_proxy::<dyn ClusterClient>(), has_proxy);
            let before = hub
                .try_get::<dyn ClusterClient>()
                .expect("injected cluster proxy");
            assert!(Arc::ptr_eq(&before, &proxy));

            assert_eq!(
                wire(hub, resolver(), ClientTuning::default()).expect("wiring reuses the proxy"),
                WireOutcome::RemoteGrpc {
                    service: "cluster.profile.v1.ClusterProfileApi",
                },
                "hub owner: {:?}",
                hub.owner(),
            );

            assert!(hub.try_get_local::<dyn ClusterClient>().is_none());
            assert_eq!(hub.has_remote_proxy::<dyn ClusterClient>(), has_proxy);
            let after = hub
                .try_get::<dyn ClusterClient>()
                .expect("retained cluster proxy");
            assert!(Arc::ptr_eq(&after, &proxy));
        }
    });
}

struct ConfigByGear(Value);

impl ConfigProvider for ConfigByGear {
    fn get_gear_config(&self, gear_name: &str) -> Option<&Value> {
        self.0.get(gear_name)
    }
}

struct CancelAfterWiring(CancellationToken);

#[async_trait::async_trait]
impl Gear for CancelAfterWiring {
    async fn init(&self, _ctx: &GearCtx) -> toolkit::Result<()> {
        Ok(())
    }

    async fn post_init(&self, _ctx: &GearCtx) -> toolkit::Result<()> {
        self.0.cancel();
        Ok(())
    }
}

#[tokio::test]
async fn cluster_addressing_uses_the_sdk_owners_static_grpc_endpoint() {
    const HOST_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(10);

    for (override_owner, expected_unresolved) in
        [("event-broker", &["cluster"][..]), ("cluster-sdk", &[])]
    {
        let root = Arc::new(ClientHub::new());
        let proxy: Arc<dyn ClusterClient> =
            Arc::new(StubClusterClient::for_profile(OrdersProfile::NAME).without_descriptor());
        root.register_remote_proxy(Arc::clone(&proxy));
        root.register::<dyn DirectoryClient>(Arc::new(ManagerDirectoryClient::new(Arc::new(
            GearManager::new(),
        ))));

        let cancel = CancellationToken::new();
        let mut registry = RegistryBuilder::default();
        registry.register_core_with_meta(
            "event-broker",
            &[],
            Arc::new(CancelAfterWiring(cancel.clone())),
        );
        let config = ConfigByGear(json!({
            (override_owner): {
                "clients": {
                    "consumes": {
                        "cluster": { "grpc_endpoint": "http://127.0.0.1:50051" }
                    }
                }
            }
        }));
        let host = HostRuntime::new(
            registry
                .build_topo_sorted()
                .expect("fixture registry builds"),
            Arc::new(config),
            DbOptions::None,
            Arc::clone(&root),
            cancel,
            "00000000-0000-0000-0000-000000000000"
                .parse()
                .expect("nil instance ID"),
            None,
        );
        let checker = root
            .get::<DependencyChecker>()
            .expect("host publishes a dependency checker");

        tokio::time::timeout(HOST_LIFECYCLE_TIMEOUT, host.run_gear_phases())
            .await
            .expect("the host lifecycle completes within the timeout")
            .expect("the host lifecycle completes successfully");

        let found = root
            .get::<dyn ClusterClient>()
            .expect("injected cluster proxy");
        assert!(Arc::ptr_eq(&found, &proxy));
        assert_eq!(
            checker.unresolved_deps(),
            expected_unresolved,
            "configuration owner: {override_owner}",
        );
    }
}

/// The DNS fallback needs a Tokio reactor for lazy channel construction. It
/// schedules descriptor prefetch on the current-thread runtime. The test
/// checks the outcome and registration without yielding to the prefetch task.
#[tokio::test]
async fn an_empty_hub_gets_a_remote_proxy_and_reports_remote() {
    let hub = ClientHub::default();

    let outcome = temp_env::with_var(POD_NAMESPACE_ENV, Some(NS), || {
        wire(&hub, resolver(), ClientTuning::default())
            .expect("a derivable endpoint wires without error")
    });

    assert_eq!(outcome, WireOutcome::Remote);
    assert!(
        hub.has_remote_proxy::<dyn ClusterClient>(),
        "a registered remote proxy must remain distinguishable from a local implementation"
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
        let result = wire(&hub, resolver(), ClientTuning::default());
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
