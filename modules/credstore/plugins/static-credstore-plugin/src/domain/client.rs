use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretMetadata, SecretRef, SecretValue,
    TenantId,
};
use modkit_security::SecurityContext;

use super::service::Service;

#[async_trait]
impl CredStorePluginClientV1 for Service {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<SecretMetadata>, CredStoreError> {
        let Some(entry) = self.get(ctx, key) else {
            return Ok(None);
        };

        // For Shared/Tenant entries the stored owner_id/owner_tenant_id are nil
        // placeholders — resolve them from the caller's security context.
        let owner_id = if entry.owner_id.is_nil() {
            OwnerId(ctx.subject_id())
        } else {
            entry.owner_id
        };
        let owner_tenant_id = if entry.owner_tenant_id.is_nil() {
            TenantId(ctx.subject_tenant_id())
        } else {
            entry.owner_tenant_id
        };

        Ok(Some(SecretMetadata {
            value: SecretValue::new(entry.value.as_bytes().to_vec()),
            owner_id,
            sharing: entry.sharing,
            owner_tenant_id,
        }))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::config::{SecretConfig, StaticCredStorePluginConfig};
    use uuid::Uuid;

    fn tenant_a() -> Uuid {
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
    }

    fn tenant_b() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    fn owner_a() -> Uuid {
        Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()
    }

    fn owner_b() -> Uuid {
        Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap()
    }

    fn ctx(tenant_id: Uuid, subject_id: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(subject_id)
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap()
    }

    /// Private secret: `tenant_a` + `owner_a`.
    fn service_with_single_secret() -> Service {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: Some(owner_a()),
                key: "openai_api_key".to_owned(),
                value: "sk-test-123".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };

        Service::from_config(&cfg).unwrap()
    }

    #[tokio::test]
    async fn get_returns_metadata_for_matching_tenant_and_owner() {
        let service = service_with_single_secret();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("openai_api_key").unwrap();

        let metadata = plugin
            .get(&ctx(tenant_a(), owner_a()), &key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metadata.value.as_bytes(), b"sk-test-123");
        assert_eq!(metadata.owner_id, OwnerId(owner_a()));
        assert_eq!(metadata.owner_tenant_id, TenantId(tenant_a()));
    }

    #[tokio::test]
    async fn get_returns_none_for_other_tenant() {
        let service = service_with_single_secret();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("openai_api_key").unwrap();

        let result = plugin.get(&ctx(tenant_b(), owner_a()), &key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_returns_none_for_other_owner() {
        let service = service_with_single_secret();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("openai_api_key").unwrap();

        let result = plugin.get(&ctx(tenant_a(), owner_b()), &key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_returns_none_for_missing_key() {
        let service = service_with_single_secret();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("missing").unwrap();

        let result = plugin.get(&ctx(tenant_a(), owner_a()), &key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_returns_none_when_no_secrets_configured() {
        let service = Service::from_config(&StaticCredStorePluginConfig::default()).unwrap();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("openai_api_key").unwrap();

        let result = plugin.get(&ctx(tenant_a(), owner_a()), &key).await.unwrap();
        assert!(result.is_none());
    }

    // --- Shared secret fills owner from SecurityContext ---

    #[tokio::test]
    async fn shared_secret_resolves_owner_from_context() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: None,
                owner_id: None,
                key: "global_key".to_owned(),
                value: "global-val".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("global_key").unwrap();

        let metadata = plugin
            .get(&ctx(tenant_a(), owner_b()), &key)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(metadata.value.as_bytes(), b"global-val");
        assert_eq!(metadata.owner_id, OwnerId(owner_b()));
        assert_eq!(metadata.owner_tenant_id, TenantId(tenant_a()));
    }

    // --- Tenant secret fills owner from SecurityContext ---

    #[tokio::test]
    async fn tenant_secret_resolves_owner_from_context() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![SecretConfig {
                tenant_id: Some(tenant_a()),
                owner_id: None,
                key: "scoped_key".to_owned(),
                value: "scoped-val".to_owned(),
                sharing: None,
            }],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("scoped_key").unwrap();

        let metadata = plugin
            .get(&ctx(tenant_a(), owner_b()), &key)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(metadata.owner_id, OwnerId(owner_b()));
        assert_eq!(metadata.owner_tenant_id, TenantId(tenant_a()));
    }

    // --- Lookup precedence via plugin ---

    #[tokio::test]
    async fn private_takes_precedence_over_tenant_and_shared_via_plugin() {
        let cfg = StaticCredStorePluginConfig {
            secrets: vec![
                SecretConfig {
                    tenant_id: None,
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "shared-val".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: None,
                    key: "k".to_owned(),
                    value: "tenant-val".to_owned(),
                    sharing: None,
                },
                SecretConfig {
                    tenant_id: Some(tenant_a()),
                    owner_id: Some(owner_a()),
                    key: "k".to_owned(),
                    value: "private-val".to_owned(),
                    sharing: None,
                },
            ],
            ..StaticCredStorePluginConfig::default()
        };
        let service = Service::from_config(&cfg).unwrap();
        let plugin: &dyn CredStorePluginClientV1 = &service;
        let key = SecretRef::new("k").unwrap();

        // owner_a in tenant_a → Private
        let meta = plugin
            .get(&ctx(tenant_a(), owner_a()), &key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(meta.value.as_bytes(), b"private-val");
        assert_eq!(meta.owner_id, OwnerId(owner_a()));

        // owner_b in tenant_a → Tenant (owner resolved from ctx)
        let meta = plugin
            .get(&ctx(tenant_a(), owner_b()), &key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(meta.value.as_bytes(), b"tenant-val");
        assert_eq!(meta.owner_id, OwnerId(owner_b()));

        // tenant_b → Shared (owner resolved from ctx)
        let meta = plugin
            .get(&ctx(tenant_b(), owner_b()), &key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(meta.value.as_bytes(), b"shared-val");
        assert_eq!(meta.owner_id, OwnerId(owner_b()));
        assert_eq!(meta.owner_tenant_id, TenantId(tenant_b()));
    }
}
