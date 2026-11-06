//! Managed registry reads are available at init after GTS admission.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::{gear_ctx_over, registry};
use policy_engine::PolicyEngine;
use policy_engine_sdk::PolicyManagementClientV1;
use serde_json::json;
use toolkit::{ClientHub, Gear};
use types_registry_sdk::testing_platform::Call;

const KNOWN_TYPE: &str = "gts.cf.toolkit.authz.permission.v1~";

fn builtin_over(resource_type: &str) -> serde_json::Value {
    json!({ "builtin_policies": [{
        "id": "guard", "resource_types": [resource_type],
        "content": "package p\ndeny := false",
    }] })
}

#[tokio::test]
async fn builtins_are_checked_in_init_through_the_platform_contract_without_a_legacy_phase() {
    let hub = Arc::new(ClientHub::new());
    let registry = registry();
    registry.install(&hub);
    let ctx = gear_ctx_over(&builtin_over(KNOWN_TYPE), Arc::clone(&hub)).await;
    PolicyEngine::default().init(&ctx).await.unwrap();
    assert!(
        hub.try_get_local::<dyn PolicyManagementClientV1>()
            .is_some()
    );
    assert_eq!(registry.platform_calls(Call::BatchGet), 1);
    assert!(registry.caller_contexts(Call::BatchGet).is_empty());
    assert_eq!(registry.calls(Call::Register), 0, "init must not publish");
}

#[tokio::test]
async fn an_unknown_concrete_type_fails_init_before_clients_are_exposed() {
    let hub = Arc::new(ClientHub::new());
    registry().install(&hub);
    let ctx = gear_ctx_over(
        &builtin_over("gts.cf.core.example.gadget.v1~"),
        Arc::clone(&hub),
    )
    .await;
    let error = PolicyEngine::default().init(&ctx).await.unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("built-in policies rejected") && message.contains("RESOURCE_TYPE_UNKNOWN"),
        "{message}"
    );
    assert!(
        hub.try_get_local::<dyn PolicyManagementClientV1>()
            .is_none()
    );
}
