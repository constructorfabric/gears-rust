//! Per-mode service/route gating tests
//! (`openspec/changes/eb-service-topology/specs/event-broker-deployment-topology/spec.md`).

use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx};
use uuid::Uuid;

use super::EventBrokerModule;
use crate::config::DeploymentMode;

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

fn make_ctx(mode: &str) -> GearCtx {
    let cfg = json!({
        "event-broker": {
            "config": {
                "mode": mode,
                "default_storage_backend": "memory",
            }
        }
    });
    GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider { root: cfg }),
        Arc::new(ClientHub::new()),
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
