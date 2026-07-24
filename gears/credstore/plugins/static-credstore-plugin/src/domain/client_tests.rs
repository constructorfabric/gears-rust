// Created: 2026-04-07 by Constructor Tech
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

// --- Runtime writes: put / delete (in-memory) ---

fn empty_service() -> Service {
    Service::from_config(&StaticCredStorePluginConfig::default()).unwrap()
}

#[tokio::test]
async fn put_then_get_returns_stored_private_secret() {
    let service = empty_service();
    let plugin: &dyn CredStorePluginClientV1 = &service;
    let key = SecretRef::new("tok").unwrap();
    let caller = ctx(tenant_a(), owner_a());

    plugin
        .put(
            &caller,
            &key,
            SecretValue::from("access-token"),
            SharingMode::Private,
        )
        .await
        .unwrap();

    let meta = plugin.get(&caller, &key).await.unwrap().unwrap();
    assert_eq!(meta.value.as_bytes(), b"access-token");
    assert_eq!(meta.sharing, SharingMode::Private);
    assert_eq!(meta.owner_id, OwnerId(owner_a()));
    assert_eq!(meta.owner_tenant_id, TenantId(tenant_a()));
}

#[tokio::test]
async fn put_overwrites_existing_value() {
    let service = empty_service();
    let plugin: &dyn CredStorePluginClientV1 = &service;
    let key = SecretRef::new("tok").unwrap();
    let caller = ctx(tenant_a(), owner_a());

    plugin
        .put(&caller, &key, SecretValue::from("v1"), SharingMode::Private)
        .await
        .unwrap();
    plugin
        .put(&caller, &key, SecretValue::from("v2"), SharingMode::Private)
        .await
        .unwrap();

    let meta = plugin.get(&caller, &key).await.unwrap().unwrap();
    assert_eq!(meta.value.as_bytes(), b"v2");
}

#[tokio::test]
async fn delete_then_get_returns_none() {
    let service = empty_service();
    let plugin: &dyn CredStorePluginClientV1 = &service;
    let key = SecretRef::new("tok").unwrap();
    let caller = ctx(tenant_a(), owner_a());

    plugin
        .put(&caller, &key, SecretValue::from("v"), SharingMode::Private)
        .await
        .unwrap();
    plugin.delete(&caller, &key).await.unwrap();

    assert!(plugin.get(&caller, &key).await.unwrap().is_none());
}

#[tokio::test]
async fn delete_is_idempotent_for_missing_key() {
    let service = empty_service();
    let plugin: &dyn CredStorePluginClientV1 = &service;
    let key = SecretRef::new("missing").unwrap();
    // Should not error even though nothing is stored.
    plugin
        .delete(&ctx(tenant_a(), owner_a()), &key)
        .await
        .unwrap();
}

#[tokio::test]
async fn private_put_isolated_across_owners() {
    let service = empty_service();
    let plugin: &dyn CredStorePluginClientV1 = &service;
    let key = SecretRef::new("tok").unwrap();

    plugin
        .put(
            &ctx(tenant_a(), owner_a()),
            &key,
            SecretValue::from("a-secret"),
            SharingMode::Private,
        )
        .await
        .unwrap();

    // Different owner in the same tenant cannot read a Private secret.
    assert!(
        plugin
            .get(&ctx(tenant_a(), owner_b()), &key)
            .await
            .unwrap()
            .is_none()
    );
    // The owner can.
    assert_eq!(
        plugin
            .get(&ctx(tenant_a(), owner_a()), &key)
            .await
            .unwrap()
            .unwrap()
            .value
            .as_bytes(),
        b"a-secret"
    );
}

#[tokio::test]
async fn private_put_isolated_across_tenants() {
    let service = empty_service();
    let plugin: &dyn CredStorePluginClientV1 = &service;
    let key = SecretRef::new("tok").unwrap();

    plugin
        .put(
            &ctx(tenant_a(), owner_a()),
            &key,
            SecretValue::from("a-secret"),
            SharingMode::Private,
        )
        .await
        .unwrap();

    // Same subject id but a different tenant must not resolve the secret.
    assert!(
        plugin
            .get(&ctx(tenant_b(), owner_a()), &key)
            .await
            .unwrap()
            .is_none()
    );
}
