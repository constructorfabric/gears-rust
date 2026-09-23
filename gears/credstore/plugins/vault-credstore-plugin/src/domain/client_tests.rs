//! Confirms `CredStorePluginClientV1` delegates to `Service`'s HTTP methods
//! (the pure-logic and network-mapping behavior is covered in
//! `service_tests.rs` / `wire_tests.rs`).
use credstore_sdk::{CredStoreError, CredStorePluginClientV1, SecretValue, TenantId, ValueId};
use httpmock::prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::{VaultCredStorePluginConfig, VaultToken};
use crate::domain::service::Service;

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build()
        .expect("test security context")
}

fn tid() -> TenantId {
    TenantId(Uuid::new_v4())
}
fn vid() -> ValueId {
    ValueId::new_v4()
}

fn service_for(server: &MockServer) -> Service {
    let cfg = VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token: VaultToken::from("test-token"),
        ..VaultCredStorePluginConfig::default()
    };
    Service::from_config(&cfg).expect("builds")
}

#[tokio::test]
async fn get_missing_returns_none() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(404);
    });

    let svc = service_for(&server);
    let got = svc.get(&ctx(), &tid(), &vid()).await.unwrap();
    assert!(got.is_none());
}

#[tokio::test]
async fn put_then_get_roundtrip_through_trait() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());

    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/data/credstore/");
        then.status(200)
            .json_body(serde_json::json!({"data": {"version": 1}}));
    });
    server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(200).json_body(serde_json::json!({
            "data": {"data": {"value": "aGVsbG8="}}
        }));
    });

    let svc = service_for(&server);
    svc.put(&ctx(), &t, &v, SecretValue::from("hello"))
        .await
        .unwrap();
    let got = svc.get(&ctx(), &t, &v).await.unwrap();
    assert_eq!(got.unwrap().as_bytes(), b"hello");
}

#[tokio::test]
async fn put_on_existing_id_conflicts_through_trait() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/data/credstore/");
        then.status(400).json_body(serde_json::json!({
            "errors": ["check-and-set parameter did not match the current version"]
        }));
    });

    let svc = service_for(&server);
    let err = svc
        .put(&ctx(), &tid(), &vid(), SecretValue::from("v2"))
        .await
        .unwrap_err();
    assert!(matches!(err, CredStoreError::Conflict));
}

#[tokio::test]
async fn delete_missing_is_success_through_trait() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(DELETE)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(404);
    });

    let svc = service_for(&server);
    svc.delete(&ctx(), &tid(), &vid()).await.unwrap();
}
