// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! `client_from_config` validates like the gear, sends nothing while building,
//! and returns the real client (the HTTP behavior itself is covered in
//! `domain/service_tests.rs` and `infra/http_tests.rs`).
use credstore_sdk::{SecretValue, StoreKey, TenantId, ValueVersion};
use httpmock::prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::*;
use crate::config::VaultToken;

fn config_for(server: &MockServer) -> VaultCredStorePluginConfig {
    VaultCredStorePluginConfig {
        address: format!("http://127.0.0.1:{}", server.port()),
        token: Some(VaultToken::from("s.test-token")),
        ..VaultCredStorePluginConfig::default()
    }
}

#[test]
fn building_sends_nothing_to_vault() {
    let server = MockServer::start();
    let any = server.mock(|when, then| {
        when.any_request();
        then.status(200);
    });

    let client = client_from_config(&config_for(&server)).expect("a valid configuration builds");

    assert!(client.destroyer().is_some());
    any.assert_calls(0);
}

#[test]
fn a_token_file_is_not_read_while_building() {
    let cfg = VaultCredStorePluginConfig {
        token_file: Some("/nonexistent/vault-token".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    client_from_config(&cfg).expect("the file is read on the first request, not here");
}

#[test]
fn an_invalid_configuration_is_rejected_as_the_gear_rejects_it() {
    let no_token = VaultCredStorePluginConfig::default();
    let two_tokens = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("s.secret-value")),
        token_env: Some("VAULT_TOKEN".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    let bad_mount = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("s.secret-value")),
        mount: "/secret/".to_owned(),
        ..VaultCredStorePluginConfig::default()
    };
    for cfg in [no_token, two_tokens, bad_mount] {
        let expected = cfg.validate().expect_err("invalid").to_string();
        let error = client_from_config(&cfg)
            .err()
            .expect("an invalid configuration must not build");
        assert_eq!(error.to_string(), expected);
        assert!(!error.to_string().contains("s.secret-value"), "{error}");
    }
}

#[tokio::test]
async fn the_client_writes_through_the_configured_mount_and_prefix() {
    let server = MockServer::start();
    let limit = server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/kv-new/metadata/migrated/")
            .header("X-Vault-Token", "s.test-token")
            .json_body(serde_json::json!({"max_versions": 100}));
        then.status(204);
    });
    let put = server.mock(|when, then| {
        when.method(POST)
            .path_includes("/v1/kv-new/data/migrated/")
            .header("X-Vault-Token", "s.test-token");
        then.status(200)
            .json_body(serde_json::json!({"data": {"version": 1}}));
    });
    let cfg = VaultCredStorePluginConfig {
        mount: "kv-new".to_owned(),
        path_prefix: "migrated".to_owned(),
        ..config_for(&server)
    };

    let client = client_from_config(&cfg).expect("builds");
    let version = client
        .put(
            &SecurityContext::anonymous(),
            &StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4()),
            SecretValue::from("hello"),
        )
        .await
        .expect("put");

    assert_eq!(version, ValueVersion::new("1"));
    limit.assert_calls(1);
    put.assert_calls(1);
}
