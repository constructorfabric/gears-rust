//! Gear-level smoke tests: `init` wires the management client and the scoped
//! admission engine plugin into `ClientHub`, and the plugin decides over the
//! real database and tenant resolver fake.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use admission_control_sdk::{AdmissionEnginePluginClientV1, EngineRequest, EngineResult};
use common::{TENANT, WIDGET, gear_ctx, tenant_user};
use policy_engine::PolicyEngine;
use policy_engine::domain::engine_plugin;
use policy_engine_sdk::{PolicyManagementClientV1, reason};
use serde_json::json;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use uuid::Uuid;

fn request(tenant: Uuid) -> EngineRequest {
    EngineRequest {
        correlation_id: Uuid::new_v4(),
        enforcing_gear: "test-gear".to_owned(),
        action: "read".to_owned(),
        resource_type: WIDGET.to_owned(),
        resource_id: None,
        resource_tenant_id: tenant,
        properties: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn gear_init_registers_management_and_scoped_plugin_clients() {
    let (ctx, hub) = gear_ctx(&json!({})).await;
    let gear = PolicyEngine::default();
    gear.init(&ctx).await.expect("init");

    hub.get::<dyn PolicyManagementClientV1>()
        .expect("PolicyManagementClientV1 must be registered");

    // The default engine-plugin vendor and priority (see
    // `PolicyEngineConfig::default`).
    let (plugin_instance_id, _) =
        engine_plugin::registration("constructorfabric", 100).expect("plugin registration payload");
    hub.get_scoped::<dyn AdmissionEnginePluginClientV1>(&ClientScope::gts_id(&plugin_instance_id))
        .expect("AdmissionEnginePluginClientV1 must be scoped-registered");
}

#[tokio::test]
async fn the_plugin_permits_without_content_and_refuses_an_unreachable_tenant() {
    let (ctx, hub) = gear_ctx(&json!({})).await;
    PolicyEngine::default().init(&ctx).await.expect("init");
    let (plugin_instance_id, _) = engine_plugin::registration("constructorfabric", 100).unwrap();
    let plugin = hub
        .get_scoped::<dyn AdmissionEnginePluginClientV1>(&ClientScope::gts_id(&plugin_instance_id))
        .unwrap();

    let permitted = plugin
        .evaluate(&tenant_user(), &request(TENANT))
        .await
        .expect("evaluation should not fail closed");
    assert!(matches!(permitted, EngineResult::Permit { .. }));

    // The resolver fake knows one tenant; the caller is not its ancestor.
    let other = Uuid::from_u128(0xB2);
    let refused = plugin.evaluate(&tenant_user(), &request(other)).await;
    match refused {
        Ok(EngineResult::Deny { reason_code, .. }) => {
            assert_eq!(reason_code, reason::TENANT_BOUNDARY);
        }
        other => panic!("expected a boundary denial, got {other:?}"),
    }
}
