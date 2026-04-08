//! Local (in-process) client for the credstore module.

use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef};
use modkit_macros::domain_model;
use modkit_security::SecurityContext;

use super::{DomainError, Service};

/// Local client wrapping the credstore service.
///
/// Registered in `ClientHub` by the credstore module during `init()`.
#[domain_model]
pub struct CredStoreLocalClient {
    svc: Arc<Service>,
}

impl CredStoreLocalClient {
    /// Creates a new local client wrapping the given service.
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

fn log_and_convert(op: &str, e: DomainError) -> CredStoreError {
    match &e {
        DomainError::NotFound => {
            tracing::debug!(operation = op, "credstore secret not found");
        }
        _ => {
            tracing::error!(operation = op, error = ?e, "credstore call failed");
        }
    }
    e.into()
}

#[async_trait]
impl CredStoreClientV1 for CredStoreLocalClient {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        self.svc
            .get(ctx, key)
            .await
            .map_err(|e| log_and_convert("get", e))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::sync::Arc;

    use credstore_sdk::{
        CredStorePluginClientV1, CredStorePluginSpecV1, OwnerId, SecretMetadata, SecretValue,
        SharingMode, TenantId,
    };
    use modkit::client_hub::{ClientHub, ClientScope};
    use types_registry_sdk::{GtsEntity, TypesRegistryClient};
    use uuid::Uuid;

    use super::*;
    use crate::domain::Service;
    use crate::domain::test_support::{MockPlugin, MockRegistry, test_ctx};

    fn make_client() -> CredStoreLocalClient {
        let hub = Arc::new(ClientHub::default());
        let svc = Arc::new(Service::new(hub, "hyperspot".into()));
        CredStoreLocalClient::new(svc)
    }

    fn make_wired_client(plugin: Arc<dyn CredStorePluginClientV1>) -> CredStoreLocalClient {
        let instance_id = format!(
            "{}test._.local_client_test.v1",
            CredStorePluginSpecV1::gts_schema_id()
        );
        let hub = Arc::new(ClientHub::default());

        let entity = GtsEntity {
            id: Uuid::nil(),
            gts_id: instance_id.clone(),
            segments: vec![],
            is_schema: false,
            content: serde_json::json!({
                "id": instance_id,
                "vendor": "hyperspot",
                "priority": 0,
                "properties": {}
            }),
            description: None,
        };
        let reg: Arc<dyn TypesRegistryClient> = Arc::new(MockRegistry::new(vec![entity]));
        hub.register::<dyn TypesRegistryClient>(reg);
        hub.register_scoped::<dyn CredStorePluginClientV1>(
            ClientScope::gts_id(&instance_id),
            plugin,
        );

        let svc = Arc::new(Service::new(hub, "hyperspot".into()));
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
}
