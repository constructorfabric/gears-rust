//! Confirms `CredStorePluginClientV2` delegates to `Service`'s HTTP methods
//! (the pure-logic and network-mapping behavior is covered in
//! `service_tests.rs` / `wire_tests.rs`).
use std::sync::Arc;

use credstore_sdk::{
    CredStorePluginClientV2, DestroySelector, SecretValue, StoreKey, TenantId, ValueVersion,
};
use httpmock::prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::{VaultCredStorePluginConfig, VaultToken};
use crate::domain::service::Service;
use crate::infra::http::ReqwestTransport;

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(Uuid::new_v4())
        .subject_id(Uuid::new_v4())
        .build()
        .expect("test security context")
}

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

fn service_for(server: &MockServer) -> Service {
    let cfg = VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token: VaultToken::from("test-token"),
        ..VaultCredStorePluginConfig::default()
    };
    let transport = ReqwestTransport::from_config(&cfg).expect("builds");
    Service::new(Arc::new(transport), &cfg)
}

#[tokio::test]
async fn get_missing_returns_none() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(404);
    });

    let svc = service_for(&server);
    let got = svc
        .get(&ctx(), &key(), &ValueVersion::new("1"))
        .await
        .unwrap();
    assert!(got.is_none());
}

#[tokio::test]
async fn put_then_get_roundtrip_through_trait() {
    let server = MockServer::start();
    let k = key();

    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/data/credstore/");
        then.status(200)
            .json_body(serde_json::json!({"data": {"version": 1}}));
    });
    server.mock(|when, then| {
        when.method(GET)
            .path_includes("/v1/secret/data/credstore/")
            .query_param("version", "1");
        then.status(200).json_body(serde_json::json!({
            "data": {"data": {"value": "aGVsbG8="}}
        }));
    });

    let svc = service_for(&server);
    let v = svc
        .put(&ctx(), &k, SecretValue::from("hello"))
        .await
        .unwrap();
    assert_eq!(v, ValueVersion::new("1"));
    let got = svc.get(&ctx(), &k, &v).await.unwrap();
    assert_eq!(got.unwrap().as_bytes(), b"hello");
}

#[tokio::test]
async fn delete_key_missing_is_success_through_trait() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(DELETE)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(404);
    });

    let svc = service_for(&server);
    svc.delete_key(&ctx(), &key()).await.unwrap();
}

#[tokio::test]
async fn declares_and_delegates_destroy() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/destroy/credstore/");
        then.status(204);
    });

    let svc = service_for(&server);
    assert!(svc.supports_destroy());
    svc.destroy(
        &ctx(),
        &key(),
        DestroySelector::Exactly(ValueVersion::new("2")),
    )
    .await
    .unwrap();
    mock.assert();
}
