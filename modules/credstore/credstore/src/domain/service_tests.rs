// Created: 2026-04-07 by Constructor Tech
use std::sync::Arc;

use credstore_sdk::{OwnerId, SecretMetadata, SecretValue, SharingMode, TenantId};
use modkit::client_hub::{ClientHub, ClientScope};
use types_registry_sdk::TypesRegistryError;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

use super::*;
use crate::domain::test_support::{MockPlugin, test_ctx};

// ── helpers ──────────────────────────────────────────────────────────────

fn empty_hub() -> Arc<ClientHub> {
    Arc::new(ClientHub::default())
}

/// Build the GTS instance ID string for a credstore plugin test instance.
fn test_instance_id() -> String {
    // schema prefix + instance suffix (5-token: vendor.package.namespace.type.vMAJOR)
    format!(
        "{}test.credstore.mock.instance.v1",
        CredStorePluginSpecV1::gts_schema_id()
    )
}

/// Build the JSON content for a `BaseModkitPluginV1`<CredStorePluginSpecV1>
/// instance that `choose_plugin_instance` can successfully parse.
fn plugin_content(gts_id: &str, vendor: &str) -> serde_json::Value {
    serde_json::json!({
        "id": gts_id,
        "vendor": vendor,
        "priority": 0,
        "properties": {}
    })
}

// ── helper to build a fully-wired hub ────────────────────────────────────

/// Wires a counting `MockTypesRegistryClient` and a scoped plugin into a `ClientHub`.
/// Returns `(hub, registry_arc)` so tests can inspect `list_instance_calls()`.
fn hub_with_counting_registry_and_plugin(
    instance_id: &str,
    vendor: &str,
    plugin: Arc<dyn CredStorePluginClientV1>,
) -> (Arc<ClientHub>, Arc<MockTypesRegistryClient>) {
    let hub = Arc::new(ClientHub::default());

    let instance = make_test_instance(instance_id, plugin_content(instance_id, vendor));
    let registry = Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    hub.register::<dyn TypesRegistryClient>(registry.clone() as Arc<dyn TypesRegistryClient>);

    hub.register_scoped::<dyn CredStorePluginClientV1>(ClientScope::gts_id(instance_id), plugin);

    (hub, registry)
}

fn hub_with_registry_and_plugin(
    instance_id: &str,
    vendor: &str,
    plugin: Arc<dyn CredStorePluginClientV1>,
) -> Arc<ClientHub> {
    hub_with_counting_registry_and_plugin(instance_id, vendor, plugin).0
}

#[tokio::test]
async fn get_returns_registry_unavailable_when_hub_empty() {
    let svc = Service::new(empty_hub(), "cyberfabric".into());
    let key = SecretRef::new("my-key").unwrap();
    let err = svc.get(&test_ctx(), &key).await.unwrap_err();
    assert!(
        matches!(err, DomainError::TypesRegistryUnavailable(_)),
        "expected TypesRegistryUnavailable, got: {err:?}"
    );
}

#[tokio::test]
async fn get_retries_resolution_on_each_call_when_registry_absent() {
    // GtsPluginSelector does not cache errors, so each call re-attempts resolution.
    // Use a failing registry (not an empty hub) so list() is actually invoked and
    // we can assert the call count proves no caching.
    let hub = Arc::new(ClientHub::default());
    let registry = Arc::new(
        MockTypesRegistryClient::new().with_list_error(TypesRegistryError::internal("unavailable")),
    );
    hub.register::<dyn TypesRegistryClient>(registry.clone() as Arc<dyn TypesRegistryClient>);
    let svc = Service::new(hub, "cyberfabric".into());
    let key = SecretRef::new("my-key").unwrap();
    assert!(svc.get(&test_ctx(), &key).await.is_err());
    assert!(svc.get(&test_ctx(), &key).await.is_err());
    assert_eq!(registry.list_instance_calls(), 2);
}

// ── resolve_plugin ───────────────────────────────────────────────────────

#[tokio::test]
async fn resolve_plugin_returns_plugin_not_found_when_no_instances() {
    let hub = Arc::new(ClientHub::default());
    let registry: Arc<dyn TypesRegistryClient> = Arc::new(MockTypesRegistryClient::new());
    hub.register::<dyn TypesRegistryClient>(registry);

    let svc = Service::new(hub, "cyberfabric".into());
    let err = svc.resolve_plugin().await.unwrap_err();
    assert!(
        matches!(err, DomainError::PluginNotFound { .. }),
        "expected PluginNotFound, got: {err:?}"
    );
}

#[tokio::test]
async fn resolve_plugin_returns_plugin_not_found_when_vendor_mismatch() {
    let instance_id = test_instance_id();
    let hub = Arc::new(ClientHub::default());
    let instance = make_test_instance(&instance_id, plugin_content(&instance_id, "other-vendor"));
    let registry: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    hub.register::<dyn TypesRegistryClient>(registry);

    let svc = Service::new(hub, "cyberfabric".into());
    let err = svc.resolve_plugin().await.unwrap_err();
    assert!(
        matches!(err, DomainError::PluginNotFound { .. }),
        "expected PluginNotFound, got: {err:?}"
    );
}

#[tokio::test]
async fn resolve_plugin_returns_invalid_when_content_malformed() {
    let instance_id = test_instance_id();
    let hub = Arc::new(ClientHub::default());
    let instance = make_test_instance(
        &instance_id,
        serde_json::json!({ "not": "valid-plugin-content" }),
    );
    let registry: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    hub.register::<dyn TypesRegistryClient>(registry);

    let svc = Service::new(hub, "cyberfabric".into());
    let err = svc.resolve_plugin().await.unwrap_err();
    assert!(
        matches!(err, DomainError::InvalidPluginInstance { .. }),
        "expected InvalidPluginInstance, got: {err:?}"
    );
}

#[tokio::test]
async fn resolve_plugin_returns_internal_when_registry_list_fails() {
    let hub = Arc::new(ClientHub::default());
    let registry: Arc<dyn TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new().with_list_error(TypesRegistryError::internal("db down")),
    );
    hub.register::<dyn TypesRegistryClient>(registry);

    let svc = Service::new(hub, "cyberfabric".into());
    let err = svc.resolve_plugin().await.unwrap_err();
    assert!(
        matches!(err, DomainError::Internal(ref msg) if msg.contains("db down")),
        "expected Internal containing 'db down', got: {err:?}"
    );
}

#[tokio::test]
async fn resolve_plugin_succeeds_with_matching_vendor() {
    let instance_id = test_instance_id();
    let hub = hub_with_registry_and_plugin(&instance_id, "cyberfabric", MockPlugin::returns(None));

    let svc = Service::new(hub, "cyberfabric".into());
    let resolved = svc.resolve_plugin().await.unwrap();
    assert_eq!(resolved, instance_id);
}

// ── get_plugin ───────────────────────────────────────────────────────────

#[tokio::test]
async fn get_plugin_returns_unavailable_when_not_in_hub() {
    // Registry resolves successfully, but the scoped client is absent.
    let instance_id = test_instance_id();
    let hub = Arc::new(ClientHub::default());
    let instance = make_test_instance(&instance_id, plugin_content(&instance_id, "cyberfabric"));
    let registry: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    hub.register::<dyn TypesRegistryClient>(registry);

    let svc = Service::new(hub, "cyberfabric".into());
    let err = svc.get_plugin().await.err().expect("expected Err");
    assert!(
        matches!(err, DomainError::PluginUnavailable { .. }),
        "expected PluginUnavailable, got: {err:?}"
    );
}

#[tokio::test]
async fn get_plugin_caches_resolved_instance() {
    let instance_id = test_instance_id();
    let (hub, registry) = hub_with_counting_registry_and_plugin(
        &instance_id,
        "cyberfabric",
        MockPlugin::returns(None),
    );

    let svc = Service::new(hub, "cyberfabric".into());
    let p1 = svc.get_plugin().await.unwrap();
    let p2 = svc.get_plugin().await.unwrap();

    assert_eq!(
        registry.list_instance_calls(),
        1,
        "resolve_plugin should be called exactly once; second call must use cached value"
    );
    assert!(
        Arc::ptr_eq(&p1, &p2),
        "both calls should return the same plugin Arc (same mock instance)"
    );
}

// ── get ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_returns_some_response_on_success() {
    let instance_id = test_instance_id();
    let meta = SecretMetadata {
        value: SecretValue::from("s3cr3t"),
        owner_id: OwnerId::nil(),
        sharing: SharingMode::Tenant,
        owner_tenant_id: TenantId::nil(),
    };
    let hub = hub_with_registry_and_plugin(
        &instance_id,
        "cyberfabric",
        MockPlugin::returns(Some(&meta)),
    );

    let svc = Service::new(hub, "cyberfabric".into());
    let key = SecretRef::new("my-key").unwrap();
    let resp = svc.get(&test_ctx(), &key).await.unwrap();

    let resp = resp.expect("expected Some response");
    assert_eq!(resp.value.as_bytes(), b"s3cr3t");
    assert_eq!(resp.sharing, SharingMode::Tenant);
    assert!(!resp.is_inherited, "is_inherited must always be false here");
    assert_eq!(resp.owner_tenant_id, TenantId::nil());
}

#[tokio::test]
async fn get_returns_none_when_plugin_returns_none() {
    let instance_id = test_instance_id();
    let hub = hub_with_registry_and_plugin(&instance_id, "cyberfabric", MockPlugin::returns(None));

    let svc = Service::new(hub, "cyberfabric".into());
    let key = SecretRef::new("missing-key").unwrap();
    let result = svc.get(&test_ctx(), &key).await.unwrap();
    assert!(result.is_none(), "expected None for missing secret");
}

#[tokio::test]
async fn get_propagates_plugin_error() {
    let instance_id = test_instance_id();
    let hub = hub_with_registry_and_plugin(
        &instance_id,
        "cyberfabric",
        MockPlugin::errors_internal("backend failure"),
    );

    let svc = Service::new(hub, "cyberfabric".into());
    let key = SecretRef::new("any-key").unwrap();
    let err = svc.get(&test_ctx(), &key).await.unwrap_err();
    assert!(
        matches!(err, DomainError::Internal(_)),
        "expected Internal, got: {err:?}"
    );
}
