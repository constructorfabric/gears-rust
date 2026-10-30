//! Gear-level smoke tests: `init` wires the management client and the scoped
//! admission engine plugin into `ClientHub`, and the plugin decides over the
//! real database and tenant resolver fake.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use admission_control_sdk::{AdmissionEnginePluginClientV1, EngineRequest, EngineResult};
use common::{TENANT, WIDGET, gear_ctx, tenant_user};
use policy_engine::PolicyEngine;
use policy_engine::domain::engine_plugin;
use policy_engine_sdk::management::{AssignmentSpec, DocumentSpec, NewBundle, VersionContent};
use policy_engine_sdk::{PolicyManagementClientV1, reason};
use serde_json::json;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::contracts::SystemCapability;
use toolkit::runtime::{GearManager, GrpcInstallerStore, SystemContext};
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

fn builtin(id: &str, resource_type: &str, content: &str) -> serde_json::Value {
    json!({ "id": id, "resource_types": [resource_type], "content": content })
}

#[tokio::test]
async fn a_configured_builtin_refuses_for_every_tenant() {
    let config = json!({ "builtin_policies": [
        builtin("reads", WIDGET, "package p\ndeny if input.action == \"read\""),
    ] });
    let (ctx, hub) = gear_ctx(&config).await;
    PolicyEngine::default().init(&ctx).await.expect("init");
    let (plugin_instance_id, _) = engine_plugin::registration("constructorfabric", 100).unwrap();
    let plugin = hub
        .get_scoped::<dyn AdmissionEnginePluginClientV1>(&ClientScope::gts_id(&plugin_instance_id))
        .unwrap();

    // The second tenant is unknown to the resolver: the built-in decides
    // before reachability is read.
    for tenant in [TENANT, Uuid::from_u128(0xB2)] {
        match plugin.evaluate(&tenant_user(), &request(tenant)).await {
            Ok(EngineResult::Deny {
                reason_code,
                denials,
                ..
            }) => {
                assert_eq!(reason_code, reason::BUILTIN_POLICY_DENIED);
                assert!(denials.is_empty());
            }
            other => panic!("expected a built-in denial, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn invalid_builtins_fail_startup() {
    let cases = [
        (
            "does not compile",
            builtin("broken", WIDGET, "package p\ndeny :="),
        ),
        (
            "denylisted builtin",
            builtin("clock", WIDGET, "package p\ndeny if time.now_ns() > 0"),
        ),
    ];
    for (case, policy) in cases {
        let (ctx, _hub) = gear_ctx(&json!({ "builtin_policies": [policy] })).await;
        let err = PolicyEngine::default().init(&ctx).await.expect_err(case);
        assert!(
            format!("{err:#}").contains("built-in policies rejected"),
            "{case}: {err:#}"
        );
    }

    // Whether a concrete type is known is asked once the registry is ready,
    // in `post_init`.
    let unknown = json!({ "builtin_policies": [builtin(
        "gadgets",
        "gts.cf.core.example.gadget.v1~",
        "package p\ndeny := true",
    )] });
    let (ctx, _hub) = gear_ctx(&unknown).await;
    let gear = PolicyEngine::default();
    gear.init(&ctx)
        .await
        .expect("init defers the registry check");
    let err = gear
        .post_init(&SystemContext::new(
            Uuid::new_v4(),
            Arc::new(GearManager::new()),
            Arc::new(GrpcInstallerStore::new()),
        ))
        .await
        .expect_err("unknown concrete type");
    let message = format!("{err:#}");
    assert!(
        message.contains("built-in policies rejected") && message.contains("RESOURCE_TYPE_UNKNOWN"),
        "{message}"
    );

    let duplicate = json!({ "builtin_policies": [
        builtin("same", WIDGET, "package p\ndeny := false"),
        builtin("same", WIDGET, "package p\ndeny := false"),
    ] });
    let (ctx, _hub) = gear_ctx(&duplicate).await;
    let err = PolicyEngine::default()
        .init(&ctx)
        .await
        .expect_err("duplicate ids");
    let message = format!("{err:#}");
    assert!(
        message.contains("DUPLICATE_DOCUMENT_NAME") && message.contains("`same`"),
        "{message}"
    );
}

/// The `policy-engine` config of `config/quickstart.yaml`, so its example
/// built-in stays valid.
fn quickstart_config() -> serde_json::Value {
    let yaml: serde_json::Value =
        serde_saphyr::from_str(include_str!("../../../../../config/quickstart.yaml"))
            .expect("quickstart.yaml parses");
    yaml["gears"]["policy-engine"]["config"].clone()
}

#[tokio::test]
async fn the_quickstart_builtin_refuses_reserved_names() {
    let (ctx, hub) = gear_ctx(&quickstart_config()).await;
    PolicyEngine::default().init(&ctx).await.expect("init");
    let (plugin_instance_id, _) = engine_plugin::registration("constructorfabric", 100).unwrap();
    let plugin = hub
        .get_scoped::<dyn AdmissionEnginePluginClientV1>(&ClientScope::gts_id(&plugin_instance_id))
        .unwrap();

    for (action, name, refused) in [
        ("create", "cf-widget", true),
        ("rename", "system-widget", true),
        ("create", "widget", false),
        ("read", "cf-widget", false),
    ] {
        let mut request = request(TENANT);
        request.action = action.to_owned();
        request.properties.insert("name".to_owned(), json!(name));
        let result = plugin.evaluate(&tenant_user(), &request).await.unwrap();
        let denied = matches!(
            &result,
            EngineResult::Deny { reason_code, .. } if reason_code == reason::BUILTIN_POLICY_DENIED
        );
        assert_eq!(denied, refused, "{action} {name}: {result:?}");
    }
}

#[tokio::test]
async fn authored_active_content_decides_through_the_store() {
    let (ctx, hub) = gear_ctx(&json!({})).await;
    PolicyEngine::default().init(&ctx).await.expect("init");
    let management = hub.get::<dyn PolicyManagementClientV1>().unwrap();
    let (plugin_instance_id, _) = engine_plugin::registration("constructorfabric", 100).unwrap();
    let plugin = hub
        .get_scoped::<dyn AdmissionEnginePluginClientV1>(&ClientScope::gts_id(&plugin_instance_id))
        .unwrap();
    let admin = tenant_user();

    let bundle = management
        .create_bundle(&admin, NewBundle::new("reads"))
        .await
        .unwrap();
    let draft = management
        .create_draft_version(&admin, bundle.id, None)
        .await
        .unwrap();
    management
        .replace_draft_content(
            &admin,
            bundle.id,
            draft.id,
            VersionContent {
                documents: vec![DocumentSpec {
                    name: "no-reads".to_owned(),
                    content: "package p\ndeny if input.action == \"read\"".to_owned(),
                    resource_types: vec![WIDGET.to_owned()],
                    actions: Vec::new(),
                }],
            },
        )
        .await
        .unwrap();
    management
        .activate_version(&admin, bundle.id, draft.id)
        .await
        .unwrap();
    management
        .assign(&admin, AssignmentSpec::new(bundle.id, TENANT))
        .await
        .unwrap();

    match plugin.evaluate(&admin, &request(TENANT)).await {
        Ok(EngineResult::Deny {
            reason_code,
            denials,
            ..
        }) => {
            assert_eq!(reason_code, reason::POLICY_DENIED);
            assert_eq!(denials.len(), 1);
            assert_eq!(denials[0].document_name, "no-reads");
            assert_eq!(denials[0].bundle_id, bundle.id.0);
            assert_eq!(denials[0].version_id, draft.id.0);
        }
        other => panic!("expected the stored policy to deny, got {other:?}"),
    }

    let mut write = request(TENANT);
    write.action = "update".to_owned();
    assert!(matches!(
        plugin.evaluate(&admin, &write).await,
        Ok(EngineResult::Permit { .. })
    ));
}
