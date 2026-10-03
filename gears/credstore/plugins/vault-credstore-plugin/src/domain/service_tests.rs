use std::sync::Arc;

use credstore_sdk::{
    CredStoreError, DestroySelector, SecretValue, StoreKey, TenantId, ValueVersion,
};
use httpmock::prelude::*;
use uuid::Uuid;

use super::*;
use crate::config::VaultCredStorePluginConfig;
use crate::infra::http::ReqwestTransport;

/// Builds a service over the real `reqwest` transport, so these tests drive
/// the whole stack (request building, headers, classification) against the
/// mock server.
fn service_from(cfg: &VaultCredStorePluginConfig) -> Service {
    let transport = ReqwestTransport::from_config(cfg).expect("builds");
    Service::new(Arc::new(transport), cfg)
}

fn config_for(server: &MockServer) -> VaultCredStorePluginConfig {
    VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token: crate::config::VaultToken::from("test-token"),
        mount: "secret".to_owned(),
        path_prefix: "credstore".to_owned(),
        ..VaultCredStorePluginConfig::default()
    }
}

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

fn data_path(k: &StoreKey) -> String {
    format!(
        "/v1/secret/data/credstore/{}/{}",
        k.tenant_id.0, k.record_id
    )
}

fn meta_path(k: &StoreKey) -> String {
    format!(
        "/v1/secret/metadata/credstore/{}/{}",
        k.tenant_id.0, k.record_id
    )
}

fn destroy_path(k: &StoreKey) -> String {
    format!(
        "/v1/secret/destroy/credstore/{}/{}",
        k.tenant_id.0, k.record_id
    )
}

fn vv(s: &str) -> ValueVersion {
    ValueVersion::new(s)
}

#[tokio::test]
async fn get_missing_version_returns_none_on_404() {
    let server = MockServer::start();
    let k = key();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path(data_path(&k))
            .query_param("version", "3")
            .header("X-Vault-Token", "test-token");
        then.status(404).json_body(
            serde_json::json!({"data": {"data": null, "metadata": {"destroyed": true}}}),
        );
    });

    let svc = service_from(&config_for(&server));
    assert!(svc.get_value(&k, &vv("3")).await.expect("ok").is_none());
    mock.assert();
}

#[tokio::test]
async fn get_200_decodes_value() {
    let server = MockServer::start();
    let k = key();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path(data_path(&k))
            .query_param("version", "2");
        then.status(200).json_body(serde_json::json!({
            "data": { "data": { "value": wire::encode_value(b"hello-from-openbao") } }
        }));
    });

    let svc = service_from(&config_for(&server));
    let got = svc
        .get_value(&k, &vv("2"))
        .await
        .expect("ok")
        .expect("some");
    assert_eq!(got.as_bytes(), b"hello-from-openbao");
    mock.assert();
}

#[tokio::test]
async fn get_non_numeric_version_is_internal_error() {
    let server = MockServer::start();
    let svc = service_from(&config_for(&server));
    let err = svc.get_value(&key(), &vv("x")).await.unwrap_err();
    assert!(matches!(err, CredStoreError::Internal(_)));
}

#[tokio::test]
async fn put_sends_no_cas_and_returns_assigned_version() {
    let server = MockServer::start();
    let k = key();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path(data_path(&k))
            .header("X-Vault-Token", "test-token")
            .json_body(serde_json::json!({"data": {"value": wire::encode_value(b"s3cret")}}));
        then.status(200)
            .json_body(serde_json::json!({"data": {"version": 5}}));
    });

    let svc = service_from(&config_for(&server));
    let v = svc
        .put_value(&k, SecretValue::from("s3cret"))
        .await
        .expect("ok");
    assert_eq!(v, vv("5"));
    mock.assert();
}

#[tokio::test]
async fn put_5xx_is_unavailable() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/data/credstore/");
        then.status(503);
    });
    let svc = service_from(&config_for(&server));
    let err = svc
        .put_value(&key(), SecretValue::from("x"))
        .await
        .unwrap_err();
    assert!(err.is_unavailable());
}

#[tokio::test]
async fn delete_key_sends_to_metadata_path_and_204_is_ok() {
    let server = MockServer::start();
    let k = key();
    let mock = server.mock(|when, then| {
        when.method(DELETE)
            .path(meta_path(&k))
            .header("X-Vault-Token", "test-token");
        then.status(204);
    });

    let svc = service_from(&config_for(&server));
    svc.delete_key_value(&k).await.expect("ok");
    mock.assert();
}

#[tokio::test]
async fn delete_key_404_is_idempotent_success() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(DELETE)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(404);
    });
    let svc = service_from(&config_for(&server));
    svc.delete_key_value(&key()).await.expect("ok, idempotent");
}

#[tokio::test]
async fn destroy_exactly_posts_the_one_version_without_listing() {
    let server = MockServer::start();
    let k = key();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path(destroy_path(&k))
            .json_body(serde_json::json!({"versions": [4]}));
        then.status(204);
    });
    let svc = service_from(&config_for(&server));
    svc.destroy_value(&k, &DestroySelector::Exactly(vv("4")))
        .await
        .expect("ok");
    mock.assert();
}

#[tokio::test]
async fn destroy_below_lists_metadata_and_destroys_live_older_versions() {
    let server = MockServer::start();
    let k = key();
    let list = server.mock(|when, then| {
        when.method(GET).path(meta_path(&k));
        then.status(200)
            .json_body(serde_json::json!({"data": {"versions": {
                "1": {"destroyed": true},
                "2": {"destroyed": false},
                "3": {"destroyed": false},
                "4": {"destroyed": false}
            }}}));
    });
    let destroy = server.mock(|when, then| {
        when.method(POST)
            .path(destroy_path(&k))
            .json_body(serde_json::json!({"versions": [2, 3]}));
        then.status(204);
    });
    let svc = service_from(&config_for(&server));
    svc.destroy_value(&k, &DestroySelector::Below(vv("4")))
        .await
        .expect("ok");
    list.assert();
    destroy.assert();
}

#[tokio::test]
async fn destroy_below_with_nothing_older_makes_no_destroy_call() {
    let server = MockServer::start();
    let k = key();
    server.mock(|when, then| {
        when.method(GET).path(meta_path(&k));
        then.status(200)
            .json_body(serde_json::json!({"data": {"versions": {
                "5": {"destroyed": false}
            }}}));
    });
    let destroy = server.mock(|when, then| {
        when.method(POST).path(destroy_path(&k));
        then.status(204);
    });
    let svc = service_from(&config_for(&server));
    svc.destroy_value(&k, &DestroySelector::Below(vv("5")))
        .await
        .expect("ok");
    assert_eq!(destroy.calls(), 0);
}

#[tokio::test]
async fn destroy_below_on_missing_key_is_success() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(404);
    });
    let svc = service_from(&config_for(&server));
    svc.destroy_value(&key(), &DestroySelector::Below(vv("9")))
        .await
        .expect("ok");
}

#[tokio::test]
async fn get_5xx_maps_to_service_unavailable() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(500);
    });
    let svc = service_from(&config_for(&server));
    let err = svc.get_value(&key(), &vv("1")).await.unwrap_err();
    assert!(err.is_unavailable());
}

#[tokio::test]
async fn namespace_header_sent_when_configured() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path_includes("/v1/secret/data/credstore/")
            .header("X-Vault-Namespace", "team-a");
        then.status(404);
    });

    let mut cfg = config_for(&server);
    cfg.namespace = Some("team-a".to_owned());
    let svc = service_from(&cfg);
    svc.get_value(&key(), &vv("1")).await.expect("ok");
    mock.assert();
}

#[tokio::test]
async fn connection_failure_maps_to_service_unavailable() {
    // Nothing listening on this port - connect must fail.
    let cfg = VaultCredStorePluginConfig {
        address: "http://127.0.0.1:1".to_owned(),
        token: crate::config::VaultToken::from("test-token"),
        timeout_secs: 2,
        ..VaultCredStorePluginConfig::default()
    };
    let svc = service_from(&cfg);
    let err = svc.get_value(&key(), &vv("1")).await.unwrap_err();
    assert!(err.is_unavailable());
}
