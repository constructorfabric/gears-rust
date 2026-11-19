// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
use std::sync::Arc;

use credstore_sdk::{
    CredStorePluginError, DestroySelector, SecretValue, StoreKey, TenantId, ValueVersion,
};
use httpmock::prelude::*;
use uuid::Uuid;

use super::*;
use crate::config::{RetryConfig, VaultCredStorePluginConfig, VaultToken};
use crate::domain::test_ext::PluginErrorExt as _;
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
        token: Some(VaultToken::from("test-token")),
        mount: "secret".to_owned(),
        path_prefix: "credstore".to_owned(),
        // Real backoff, but short: retries are exercised without slowing the suite.
        retry: RetryConfig {
            max_attempts: 3,
            base_delay_ms: 1,
        },
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
    assert!(matches!(err, CredStorePluginError::Internal { .. }));
}

#[tokio::test]
async fn put_sets_max_versions_on_the_key_then_writes_without_cas() {
    let server = MockServer::start();
    let k = key();
    let limit = server.mock(|when, then| {
        when.method(POST)
            .path(meta_path(&k))
            .header("X-Vault-Token", "test-token")
            .json_body(serde_json::json!({"max_versions": 100}));
        then.status(204);
    });
    let write = server.mock(|when, then| {
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
    limit.assert();
    write.assert();
}

#[tokio::test]
async fn put_fails_without_writing_when_the_limit_call_fails() {
    for status in [500_u16, 503] {
        let server = MockServer::start();
        let k = key();
        let limit = server.mock(|when, then| {
            when.method(POST).path(meta_path(&k));
            then.status(status);
        });
        let write = server.mock(|when, then| {
            when.method(POST).path(data_path(&k));
            then.status(200)
                .json_body(serde_json::json!({"data": {"version": 1}}));
        });
        let svc = service_from(&config_for(&server));
        let err = svc.put_value(&k, SecretValue::from("x")).await.unwrap_err();
        assert!(err.is_unavailable(), "status {status}");
        assert_eq!(limit.calls(), 1, "the limit call is not retried");
        assert_eq!(write.calls(), 0, "no data write after a failed limit call");
    }
}

#[tokio::test]
async fn put_403_on_the_limit_call_is_credentials_rejected_and_writes_nothing() {
    let server = MockServer::start();
    let k = key();
    let limit = server.mock(|when, then| {
        when.method(POST).path(meta_path(&k));
        then.status(403)
            .json_body(serde_json::json!({"errors": ["permission denied"]}));
    });
    let write = server.mock(|when, then| {
        when.method(POST).path(data_path(&k));
        then.status(200)
            .json_body(serde_json::json!({"data": {"version": 1}}));
    });
    let svc = service_from(&config_for(&server));
    let err = svc.put_value(&k, SecretValue::from("x")).await.unwrap_err();
    assert!(err.is_credentials_rejected());
    assert_eq!(write.calls(), 0);
    limit.assert();
}

#[tokio::test]
async fn put_5xx_is_unavailable_and_not_retried() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(204);
    });
    let mock = server.mock(|when, then| {
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
    assert_eq!(mock.calls(), 1, "put must never be retried");
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
async fn get_5xx_is_retried_then_maps_to_service_unavailable() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(500);
    });
    let svc = service_from(&config_for(&server));
    let err = svc.get_value(&key(), &vv("1")).await.unwrap_err();
    assert!(err.is_unavailable());
    assert_eq!(mock.calls(), 3, "max_attempts attempts, then give up");
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
        token: Some(VaultToken::from("test-token")),
        timeout_secs: 2,
        retry: RetryConfig {
            max_attempts: 2,
            base_delay_ms: 1,
        },
        ..VaultCredStorePluginConfig::default()
    };
    let svc = service_from(&cfg);
    let err = svc.get_value(&key(), &vv("1")).await.unwrap_err();
    assert!(err.is_unavailable());
}

#[tokio::test]
async fn get_soft_deleted_version_is_none() {
    // Real Vault answer: 404, data.data = null, deletion_time set.
    let server = MockServer::start();
    let k = key();
    server.mock(|when, then| {
        when.method(GET)
            .path(data_path(&k))
            .query_param("version", "2");
        then.status(404).json_body(serde_json::json!({
            "data": {"data": null, "metadata": {
                "deletion_time": "2026-10-03T21:22:01.220348343Z",
                "destroyed": false, "version": 2}}
        }));
    });
    let svc = service_from(&config_for(&server));
    assert!(svc.get_value(&k, &vv("2")).await.expect("ok").is_none());
}

#[tokio::test]
async fn get_version_zero_is_none_without_asking_vault() {
    // Vault reads "latest" for `version=0`: never send it.
    let server = MockServer::start();
    let any = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });
    let svc = service_from(&config_for(&server));
    assert!(svc.get_value(&key(), &vv("0")).await.expect("ok").is_none());
    assert_eq!(any.calls(), 0);
}

#[tokio::test]
async fn get_entry_without_value_is_internal() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(200)
            .json_body(serde_json::json!({"data": {"data": {"password": "x"}}}));
    });
    let svc = service_from(&config_for(&server));
    let err = svc.get_value(&key(), &vv("1")).await.unwrap_err();
    assert!(matches!(err, CredStorePluginError::Internal { .. }));
}

#[tokio::test]
async fn get_on_a_missing_mount_is_an_error_not_a_miss() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(404).json_body(serde_json::json!({
            "errors": ["no handler for route \"secret/data/x\". route entry not found."]
        }));
    });
    let svc = service_from(&config_for(&server));
    let err = svc.get_value(&key(), &vv("1")).await.unwrap_err();
    assert!(matches!(err, CredStorePluginError::Internal { .. }));
}

#[tokio::test]
async fn forbidden_maps_to_credentials_rejected_and_is_not_retried() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path_includes("/v1/secret/data/credstore/");
        then.status(403)
            .json_body(serde_json::json!({"errors": ["permission denied"]}));
    });
    let svc = service_from(&config_for(&server));
    let err = svc.get_value(&key(), &vv("1")).await.unwrap_err();
    assert!(err.is_credentials_rejected());
    assert!(!err.to_string().contains("test-token"));
    // The transport re-reads an inline token never (it cannot change), so
    // exactly one request reaches Vault, and the service does not retry a 403.
    assert_eq!(mock.calls(), 1);
}

#[tokio::test]
async fn put_400_is_internal_with_vault_text_and_without_the_secret() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(204);
    });
    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/data/credstore/");
        then.status(400).json_body(serde_json::json!({
            "errors": [format!("rejected {}", wire::encode_value(b"s3cret"))]
        }));
    });
    let svc = service_from(&config_for(&server));
    let err = svc
        .put_value(&key(), SecretValue::from("s3cret"))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, CredStorePluginError::Internal { .. }));
    assert!(msg.contains("rejected"), "{msg}");
    assert!(!msg.contains(&wire::encode_value(b"s3cret")), "{msg}");
}

#[tokio::test]
async fn retry_after_header_reaches_the_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(DELETE)
            .path_includes("/v1/secret/metadata/credstore/");
        then.status(429).header("Retry-After", "9");
    });
    let svc = service_from(&config_for(&server));
    let err = svc.delete_key_value(&key()).await.unwrap_err();
    assert!(err.is_unavailable());
    assert_eq!(err.retry_after().map(|d| d.as_secs()), Some(9));
}

#[tokio::test]
async fn destroy_exactly_version_zero_makes_no_call() {
    let server = MockServer::start();
    let any = server.mock(|when, then| {
        when.any_request();
        then.status(204);
    });
    let svc = service_from(&config_for(&server));
    svc.destroy_value(&key(), &DestroySelector::Exactly(vv("0")))
        .await
        .expect("ok");
    assert_eq!(any.calls(), 0);
}

#[tokio::test]
async fn destroy_below_one_makes_no_call() {
    let server = MockServer::start();
    let any = server.mock(|when, then| {
        when.any_request();
        then.status(204);
    });
    let svc = service_from(&config_for(&server));
    for below in ["0", "1"] {
        svc.destroy_value(&key(), &DestroySelector::Below(vv(below)))
            .await
            .expect("ok");
    }
    assert_eq!(any.calls(), 0, "nothing is older than version 1");
}

#[tokio::test]
async fn destroy_below_includes_soft_deleted_but_not_destroyed_versions() {
    let server = MockServer::start();
    let k = key();
    server.mock(|when, then| {
        when.method(GET).path(meta_path(&k));
        then.status(200)
            .json_body(serde_json::json!({"data": {"versions": {
                "1": {"destroyed": true, "deletion_time": ""},
                "2": {"destroyed": false, "deletion_time": "2026-10-03T21:22:01Z"},
                "3": {"destroyed": false, "deletion_time": ""},
                "4": {"destroyed": false, "deletion_time": ""}
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
    destroy.assert();
}

#[tokio::test]
async fn destroy_below_retries_the_metadata_read_and_the_destroy_call() {
    let server = MockServer::start();
    let k = key();
    let meta = server.mock(|when, then| {
        when.method(GET).path(meta_path(&k));
        then.status(502);
    });
    let svc = service_from(&config_for(&server));
    let err = svc
        .destroy_value(&k, &DestroySelector::Below(vv("5")))
        .await
        .unwrap_err();
    assert!(err.is_unavailable());
    assert_eq!(meta.calls(), 3);

    let server = MockServer::start();
    let destroy = server.mock(|when, then| {
        when.method(POST).path(destroy_path(&k));
        then.status(503);
    });
    let svc = service_from(&config_for(&server));
    let err = svc
        .destroy_value(&k, &DestroySelector::Exactly(vv("5")))
        .await
        .unwrap_err();
    assert!(err.is_unavailable());
    assert_eq!(destroy.calls(), 3);
}

#[tokio::test]
async fn destroy_on_a_missing_mount_is_an_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/secret/destroy/credstore/");
        then.status(404).json_body(serde_json::json!({
            "errors": ["no handler for route \"secret/destroy/x\". route entry not found."]
        }));
    });
    let svc = service_from(&config_for(&server));
    let err = svc
        .destroy_value(&key(), &DestroySelector::Exactly(vv("3")))
        .await
        .unwrap_err();
    assert!(matches!(err, CredStorePluginError::Internal { .. }));
}
