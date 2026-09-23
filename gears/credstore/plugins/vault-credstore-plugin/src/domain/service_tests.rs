use credstore_sdk::{CredStoreError, SecretValue, TenantId, ValueId};
use httpmock::prelude::*;
use uuid::Uuid;

use super::*;
use crate::config::VaultCredStorePluginConfig;

fn config_for(server: &MockServer) -> VaultCredStorePluginConfig {
    VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token: crate::config::VaultToken::from("test-token"),
        mount: "secret".to_owned(),
        path_prefix: "credstore".to_owned(),
        ..VaultCredStorePluginConfig::default()
    }
}

fn tid() -> TenantId {
    TenantId(Uuid::new_v4())
}
fn vid() -> ValueId {
    ValueId::new_v4()
}

#[tokio::test]
async fn get_missing_returns_none_on_404() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path_includes("/v1/secret/data/credstore/")
            .header("X-Vault-Token", "test-token");
        then.status(404)
            .json_body(serde_json::json!({"errors": []}));
    });

    let svc = Service::from_config(&config_for(&server)).expect("builds");
    let got = svc.get_value(&t, &v).await.expect("ok");
    assert!(got.is_none());
    mock.assert();
}

#[tokio::test]
async fn get_200_decodes_value_and_sends_token_header() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    let expected_path = format!("/v1/secret/data/credstore/{}/{}", t.0, v.0);
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path(expected_path.clone())
            .header("X-Vault-Token", "test-token");
        then.status(200).json_body(serde_json::json!({
            "data": { "data": { "value": wire::encode_value(b"hello-from-openbao") } }
        }));
    });

    let svc = Service::from_config(&config_for(&server)).expect("builds");
    let got = svc.get_value(&t, &v).await.expect("ok").expect("some");
    assert_eq!(got.as_bytes(), b"hello-from-openbao");
    mock.assert();
}

#[tokio::test]
async fn put_sends_cas_zero_body_to_data_path() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    let expected_path = format!("/v1/secret/data/credstore/{}/{}", t.0, v.0);
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path(expected_path.clone())
            .header("X-Vault-Token", "test-token")
            .json_body(serde_json::json!({
                "options": {"cas": 0},
                "data": {"value": wire::encode_value(b"s3cret")}
            }));
        then.status(200)
            .json_body(serde_json::json!({"data": {"version": 1}}));
    });

    let svc = Service::from_config(&config_for(&server)).expect("builds");
    svc.put_value(&t, &v, SecretValue::from("s3cret"))
        .await
        .expect("ok");
    mock.assert();
}

#[tokio::test]
async fn put_cas_mismatch_maps_to_conflict() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/data/credstore/");
        then.status(400).json_body(serde_json::json!({
            "errors": ["check-and-set parameter did not match the current version"]
        }));
    });

    let svc = Service::from_config(&config_for(&server)).expect("builds");
    let err = svc
        .put_value(&t, &v, SecretValue::from("v1"))
        .await
        .unwrap_err();
    assert!(matches!(err, CredStoreError::Conflict));
}

#[tokio::test]
async fn delete_sends_to_metadata_path_and_204_is_ok() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    let expected_path = format!("/v1/secret/metadata/credstore/{}/{}", t.0, v.0);
    let mock = server.mock(|when, then| {
        when.method(DELETE)
            .path(expected_path.clone())
            .header("X-Vault-Token", "test-token");
        then.status(204);
    });

    let svc = Service::from_config(&config_for(&server)).expect("builds");
    svc.delete_value(&t, &v).await.expect("ok");
    mock.assert();
}

#[tokio::test]
async fn delete_404_is_idempotent_success() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    server.mock(|when, then| {
        when.method(DELETE)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(404);
    });

    let svc = Service::from_config(&config_for(&server)).expect("builds");
    svc.delete_value(&t, &v).await.expect("ok, idempotent");
}

#[tokio::test]
async fn get_5xx_maps_to_service_unavailable() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(500);
    });

    let svc = Service::from_config(&config_for(&server)).expect("builds");
    let err = svc.get_value(&t, &v).await.unwrap_err();
    assert!(err.is_unavailable());
}

#[tokio::test]
async fn namespace_header_sent_when_configured() {
    let server = MockServer::start();
    let (t, v) = (tid(), vid());
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path_includes("/v1/secret/data/credstore/")
            .header("X-Vault-Namespace", "team-a");
        then.status(404);
    });

    let mut cfg = config_for(&server);
    cfg.namespace = Some("team-a".to_owned());
    let svc = Service::from_config(&cfg).expect("builds");
    svc.get_value(&t, &v).await.expect("ok");
    mock.assert();
}

#[tokio::test]
async fn connection_failure_maps_to_service_unavailable() {
    // Nothing listening on this port — connect must fail.
    let cfg = VaultCredStorePluginConfig {
        address: "http://127.0.0.1:1".to_owned(),
        token: crate::config::VaultToken::from("test-token"),
        timeout_secs: 2,
        ..VaultCredStorePluginConfig::default()
    };
    let svc = Service::from_config(&cfg).expect("builds");
    let err = svc.get_value(&tid(), &vid()).await.unwrap_err();
    assert!(err.is_unavailable());
}
