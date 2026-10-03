use credstore_sdk::{
    CredStorePluginClientV2, DestroySelector, SecretValue, StoreKey, TenantId, ValueVersion,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::StaticCredStorePluginConfig;
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

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

#[tokio::test]
async fn get_missing_returns_none() {
    let svc = empty_service();
    let got = svc
        .get(&ctx(), &key(), &ValueVersion::new("1"))
        .await
        .unwrap();
    assert!(got.is_none());
}

#[tokio::test]
async fn put_then_get_roundtrip() {
    let svc = empty_service();
    let k = key();
    let v = svc.put(&ctx(), &k, SecretValue::from("v1")).await.unwrap();
    let got = svc.get(&ctx(), &k, &v).await.unwrap();
    assert_eq!(got.unwrap().as_bytes(), b"v1");
}

#[tokio::test]
async fn each_put_is_a_new_immutable_version() {
    let svc = empty_service();
    let k = key();
    let v1 = svc.put(&ctx(), &k, SecretValue::from("v1")).await.unwrap();
    let v2 = svc.put(&ctx(), &k, SecretValue::from("v2")).await.unwrap();
    assert_ne!(v1, v2);
    assert_eq!(
        svc.get(&ctx(), &k, &v1).await.unwrap().unwrap().as_bytes(),
        b"v1"
    );
    assert_eq!(
        svc.get(&ctx(), &k, &v2).await.unwrap().unwrap().as_bytes(),
        b"v2"
    );
}

#[tokio::test]
async fn declares_and_implements_destroy() {
    let svc = empty_service();
    assert!(svc.supports_destroy());
    let k = key();
    let v1 = svc.put(&ctx(), &k, SecretValue::from("v1")).await.unwrap();
    let v2 = svc.put(&ctx(), &k, SecretValue::from("v2")).await.unwrap();
    svc.destroy(&ctx(), &k, DestroySelector::Below(v2.clone()))
        .await
        .unwrap();
    assert!(svc.get(&ctx(), &k, &v1).await.unwrap().is_none());
    assert!(svc.get(&ctx(), &k, &v2).await.unwrap().is_some());
    svc.destroy(&ctx(), &k, DestroySelector::Exactly(v2.clone()))
        .await
        .unwrap();
    assert!(svc.get(&ctx(), &k, &v2).await.unwrap().is_none());
}

#[tokio::test]
async fn delete_key_removes_all_versions() {
    let svc = empty_service();
    let k = key();
    let v = svc.put(&ctx(), &k, SecretValue::from("v1")).await.unwrap();
    svc.delete_key(&ctx(), &k).await.unwrap();
    assert!(svc.get(&ctx(), &k, &v).await.unwrap().is_none());
}

#[tokio::test]
async fn delete_key_missing_is_success() {
    empty_service().delete_key(&ctx(), &key()).await.unwrap();
}
