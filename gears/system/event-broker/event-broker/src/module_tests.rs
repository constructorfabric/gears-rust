//! Per-mode service/route gating tests
//! (`openspec/changes/eb-service-topology/specs/event-broker-deployment-topology/spec.md`).

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::DiscoveryFilter;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx};
use uuid::Uuid;

use super::EventBrokerModule;
use crate::config::DeploymentMode;
use crate::domain::cluster::EventBrokerCluster;

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

fn make_ctx(mode: &str) -> GearCtx {
    make_ctx_with_hub(mode, Arc::new(ClientHub::new()))
}

fn make_ctx_with_hub(mode: &str, hub: Arc<ClientHub>) -> GearCtx {
    let cfg = json!({
        "event-broker": {
            "config": {
                "mode": mode,
                "default_storage_backend": "memory",
                // Only load-bearing for cluster_ingest/cluster_delivery
                // (registration `init`-time validation, `eb-dispatcher-routing`
                // design.md D5); harmless for the other two modes.
                "registration": { "advertise_addr": "127.0.0.1:8080" },
            }
        }
    });
    GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider { root: cfg }),
        hub,
        CancellationToken::new(),
    )
}

async fn mode_after_init(mode: &str) -> DeploymentMode {
    let module = EventBrokerModule::default();
    let ctx = make_ctx(mode);
    module.init(&ctx).await.expect("init must succeed");
    module.mode()
}

#[tokio::test]
async fn standalone_activates_ingest_delivery_and_reaper_but_no_dispatcher() {
    let mode = mode_after_init("standalone").await;
    assert!(mode.ingest_active());
    assert!(mode.delivery_active());
    assert!(!mode.dispatcher_active());
    assert!(mode.reaper_active());
}

#[tokio::test]
async fn cluster_ingest_activates_only_ingest_and_reaper() {
    let mode = mode_after_init("cluster_ingest").await;
    assert!(mode.ingest_active());
    assert!(!mode.delivery_active());
    assert!(!mode.dispatcher_active());
    assert!(mode.reaper_active());
}

#[tokio::test]
async fn cluster_delivery_activates_only_delivery_and_reaper() {
    let mode = mode_after_init("cluster_delivery").await;
    assert!(!mode.ingest_active());
    assert!(mode.delivery_active());
    assert!(!mode.dispatcher_active());
    assert!(mode.reaper_active());
}

#[tokio::test]
async fn cluster_dispatcher_activates_only_the_dispatcher() {
    let mode = mode_after_init("cluster_dispatcher").await;
    assert!(!mode.ingest_active());
    assert!(!mode.delivery_active());
    assert!(mode.dispatcher_active());
    assert!(!mode.reaper_active());
}

#[tokio::test]
async fn init_fails_for_unknown_mode() {
    let module = EventBrokerModule::default();
    let ctx = make_ctx("not_a_real_mode");

    let err = module
        .init(&ctx)
        .await
        .expect_err("init must reject an unrecognized mode string rather than silently accepting it or panicking");

    let message = format!("{err:#}");
    assert_eq!(
        message,
        "invalid config for gear 'event-broker': unknown variant `not_a_real_mode`, \
         expected one of `standalone`, `cluster_ingest`, `cluster_delivery`, `cluster_dispatcher`"
    );
}

/// Spec "Advertise-address resolution", "No `advertise_addr`, wildcard bind
/// address, cluster mode" - the `init`-level integration (the resolver-level
/// unit tests live in `infra::cluster::advertise_address::tests`).
#[tokio::test]
async fn init_fails_on_wildcard_bind_with_no_advertise_addr_in_cluster_mode() {
    let cfg = json!({
        "event-broker": {
            "config": {
                "mode": "cluster_ingest",
                "default_storage_backend": "memory",
                // `registration` omitted entirely -> defaults to
                // `listen_addr: "0.0.0.0:0"`, `advertise_addr: None`.
            }
        }
    });
    let ctx = GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider { root: cfg }),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    );

    let module = EventBrokerModule::default();
    let err = module
        .init(&ctx)
        .await
        .expect_err("a wildcard bind with no advertise_addr must fail init in cluster mode");

    assert!(
        format!("{err:#}").contains("wildcard address"),
        "error: {err:#}"
    );
}

/// Spec "Ingest/delivery self-registration": booting in `cluster_ingest`
/// mode registers with `ServiceDiscoveryV1` under `"ingest"` at the
/// resolved advertise address.
#[tokio::test]
async fn cluster_ingest_serve_registers_with_service_discovery() {
    let (hub, _cluster) = crate::test_support::standalone_event_broker_cluster().await;
    let ctx = make_ctx_with_hub("cluster_ingest", Arc::clone(&hub));

    let module = Arc::new(EventBrokerModule::default());
    module.init(&ctx).await.expect("init must succeed");

    let cancel = CancellationToken::new();
    let serve_module = Arc::clone(&module);
    let serve_cancel = cancel.clone();
    let serve_task = tokio::spawn(async move { serve_module.serve(serve_cancel).await });

    // The standalone backend is in-process with no real I/O, so a short
    // yield is enough for the spawned `serve()` to reach and complete its
    // `register()` call before this test's own `discover()` runs.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let cluster = EventBrokerCluster::resolve(&hub).expect("event-broker profile is bound");
    let instances = cluster
        .service_discovery
        .discover("ingest", DiscoveryFilter::default())
        .await
        .expect("discover must not fail");
    assert_eq!(instances.len(), 1, "instances: {instances:?}");
    assert_eq!(instances[0].address, "http://127.0.0.1:8080");

    cancel.cancel();
    serve_task
        .await
        .expect("serve task must not panic")
        .expect("serve must return Ok on cancellation");
}

#[test]
fn for_mode_matches_design_md_deployment_modes_table_exhaustively() {
    // Mirrors `DESIGN.md:2224`'s table row by row - if a fifth mode is ever
    // added, this list forces an explicit decision here too.
    let table = [
        (DeploymentMode::Standalone, (true, true, false, true)),
        (DeploymentMode::ClusterIngest, (true, false, false, true)),
        (DeploymentMode::ClusterDelivery, (false, true, false, true)),
        (
            DeploymentMode::ClusterDispatcher,
            (false, false, true, false),
        ),
    ];

    for (mode, (ingest, delivery, dispatcher, reaper)) in table {
        assert_eq!(mode.ingest_active(), ingest, "ingest mismatch for {mode:?}");
        assert_eq!(
            mode.delivery_active(),
            delivery,
            "delivery mismatch for {mode:?}"
        );
        assert_eq!(
            mode.dispatcher_active(),
            dispatcher,
            "dispatcher mismatch for {mode:?}"
        );
        assert_eq!(mode.reaper_active(), reaper, "reaper mismatch for {mode:?}");
    }

    // Dispatcher is exclusive to cluster_dispatcher; every other mode has
    // no dispatcher constructed (`docs/ADR/0007-service-decomposition.md` D4).
    for (mode, _) in table {
        if mode != DeploymentMode::ClusterDispatcher {
            assert!(
                !mode.dispatcher_active(),
                "{mode:?} must not activate a dispatcher"
            );
        }
    }
}
