// Created: 2026-04-07 by Constructor Tech
use std::sync::Arc;

use credstore_sdk::{
    CredStorePluginClientV1, CredStorePluginSpecV1, OwnerId, SecretMetadata, SecretValue,
    SharingMode, TenantId,
};
use modkit::client_hub::{ClientHub, ClientScope};
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

use super::*;
use crate::domain::Service;
use crate::domain::test_support::{MockPlugin, test_ctx};

fn make_client() -> CredStoreLocalClient {
    let hub = Arc::new(ClientHub::default());
    let svc = Arc::new(Service::new(hub, "cyberfabric".into()));
    CredStoreLocalClient::new(svc)
}

fn make_wired_client(plugin: Arc<dyn CredStorePluginClientV1>) -> CredStoreLocalClient {
    let instance_id = format!(
        "{}test.credstore.mock.local_client.v1",
        CredStorePluginSpecV1::gts_schema_id()
    );
    let hub = Arc::new(ClientHub::default());

    let instance = make_test_instance(
        &instance_id,
        serde_json::json!({
            "id": instance_id,
            "vendor": "cyberfabric",
            "priority": 0,
            "properties": {}
        }),
    );
    let reg: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    hub.register::<dyn TypesRegistryClient>(reg);
    hub.register_scoped::<dyn CredStorePluginClientV1>(ClientScope::gts_id(&instance_id), plugin);

    let svc = Arc::new(Service::new(hub, "cyberfabric".into()));
    CredStoreLocalClient::new(svc)
}

// ── CredStoreClientV1::get — error path ──────────────────────────────────

#[tokio::test]
async fn get_trait_impl_propagates_service_error() {
    let client = make_client();
    let key = SecretRef::new("test-key").unwrap();
    // Hub is empty → TypesRegistryUnavailable → CredStoreError::Internal
    let result = client.get(&test_ctx(), &key).await;
    assert!(matches!(result.unwrap_err(), CredStoreError::Internal(_)));
}

#[tokio::test]
async fn get_trait_impl_converts_not_found_from_plugin() {
    let client = make_wired_client(MockPlugin::errors_not_found());
    let key = SecretRef::new("missing-key").unwrap();
    let result = client.get(&test_ctx(), &key).await;
    assert!(
        matches!(result.unwrap_err(), CredStoreError::NotFound),
        "DomainError::NotFound must map to CredStoreError::NotFound"
    );
}

// ── CredStoreClientV1::get — happy paths ─────────────────────────────────

#[tokio::test]
async fn get_trait_impl_returns_some_on_success() {
    let meta = SecretMetadata {
        value: SecretValue::from("val"),
        owner_id: OwnerId::nil(),
        sharing: SharingMode::Tenant,
        owner_tenant_id: TenantId::nil(),
    };
    let client = make_wired_client(MockPlugin::returns(Some(&meta)));
    let key = SecretRef::new("key").unwrap();
    let resp = client.get(&test_ctx(), &key).await.unwrap();
    let resp = resp.expect("expected Some");
    assert_eq!(resp.value.as_bytes(), b"val");
    assert!(!resp.is_inherited);
}

#[tokio::test]
async fn get_trait_impl_returns_none_when_plugin_returns_none() {
    let client = make_wired_client(MockPlugin::returns(None));
    let key = SecretRef::new("missing").unwrap();
    let resp = client.get(&test_ctx(), &key).await.unwrap();
    assert!(resp.is_none());
}
