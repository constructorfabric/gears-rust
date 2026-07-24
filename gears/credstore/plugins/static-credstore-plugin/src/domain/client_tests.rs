// Created: 2026-06-06 — tests for the `CredStorePluginClientV1` trait impl.
use credstore_sdk::{CredStorePluginClientV1, OwnerId, SecretRef, SecretValue, TenantId};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::{SecretConfig, StaticCredStorePluginConfig};
use crate::domain::service::Service;

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build()
        .expect("test security context")
}

fn empty_service() -> Service {
    Service::from_config(&StaticCredStorePluginConfig::default()).expect("config builds")
}

fn seeded_service() -> Service {
    let cfg = StaticCredStorePluginConfig {
        secrets: vec![SecretConfig {
            tenant_id: Some(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()),
            owner_id: None,
            key: "openai-key".to_owned(),
            value: "seeded".to_owned(),
            sharing: None,
        }],
        ..Default::default()
    };
    Service::from_config(&cfg).expect("config builds")
}

fn tid() -> TenantId {
    TenantId(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
}

#[tokio::test]
async fn get_seeded_tenant_secret() {
    let svc = seeded_service();
    let got = svc
        .get(&ctx(), &tid(), &SecretRef::new("openai-key").unwrap(), None)
        .await
        .unwrap()
        .expect("secret present");
    assert_eq!(got.as_bytes(), b"seeded");
}

#[tokio::test]
async fn get_missing_returns_none() {
    let svc = empty_service();
    let got = svc
        .get(&ctx(), &tid(), &SecretRef::new("absent").unwrap(), None)
        .await
        .unwrap();
    assert!(got.is_none());
}

#[tokio::test]
async fn put_then_get_roundtrip() {
    let svc = empty_service();
    let key = SecretRef::new("written").unwrap();

    svc.put(&ctx(), &tid(), &key, SecretValue::from("v1"), None)
        .await
        .unwrap();

    let got = svc.get(&ctx(), &tid(), &key, None).await.unwrap();
    assert_eq!(got.unwrap().as_bytes(), b"v1");
}

#[tokio::test]
async fn private_put_is_owner_scoped() {
    let svc = empty_service();
    let key = SecretRef::new("owned").unwrap();
    let owner = OwnerId(Uuid::new_v4());

    svc.put(
        &ctx(),
        &tid(),
        &key,
        SecretValue::from("secret"),
        Some(&owner),
    )
    .await
    .unwrap();

    // Visible to the owner, not to the tenant class.
    assert!(
        svc.get(&ctx(), &tid(), &key, Some(&owner))
            .await
            .unwrap()
            .is_some()
    );
    assert!(svc.get(&ctx(), &tid(), &key, None).await.unwrap().is_none());
}

#[tokio::test]
async fn delete_removes_value() {
    let svc = seeded_service();
    let key = SecretRef::new("openai-key").unwrap();

    svc.delete(&ctx(), &tid(), &key, None).await.unwrap();
    assert!(svc.get(&ctx(), &tid(), &key, None).await.unwrap().is_none());
}
