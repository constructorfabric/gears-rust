// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Behaviour of the plugin against a real `hashicorp/vault` dev server in
//! Docker: the response shapes Vault gives for soft-deleted, destroyed and
//! evicted versions and how the plugin maps them, `destroy(Below)` over many
//! versions, a rotated token file, and failures (missing mount, sealed
//! server, a token without rights).
//!
//! Each test starts its own server and runs the plugin with a token holding
//! only the minimal ACL policy of `docs/DESIGN.md`; Vault's own state is
//! inspected and changed through its HTTP API with the root token.
//!
//! All tests are `#[ignore]`d (they need Docker and never run in CI):
//!
//! ```text
//! cargo test -p cf-gears-vault-credstore-plugin-v2 --test vault_behaviour -- --ignored
//! ```
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

mod common;
use common::PluginErrorExt as _;

use common::{MOUNT, VaultFixture, fresh_key, key_path, plugin_from};
use credstore_sdk::{CredStorePluginError, DestroySelector, SecretValue, StoreKey, ValueVersion};
use serde_json::{Value, json};
use vault_credstore_plugin_v2::domain::Service;

macro_rules! docker_test {
    ($(#[$meta:meta])* async fn $name:ident($vault:ident) $body:block) => {
        #[tokio::test]
        #[ignore = "requires Docker (starts a hashicorp/vault container); run manually with `-- --ignored`"]
        $(#[$meta])*
        async fn $name() {
            let Some($vault) = VaultFixture::start().await else {
                return;
            };
            $body
        }
    };
}

fn vv(n: u64) -> ValueVersion {
    ValueVersion::new(n.to_string())
}

async fn put(svc: &Service, key: &StoreKey, text: &str) -> ValueVersion {
    svc.put_value(key, SecretValue::from(text))
        .await
        .expect("put succeeds")
}

async fn read(svc: &Service, key: &StoreKey, version: &ValueVersion) -> Option<String> {
    svc.get_value(key, version)
        .await
        .expect("get succeeds")
        .map(|v| String::from_utf8(v.as_bytes().to_vec()).expect("utf-8 test value"))
}

/// `(status, body)` of a raw `GET data/...?version=N` with the root token.
async fn raw_read(vault: &VaultFixture, key: &StoreKey, version: u64) -> (u16, Value) {
    let path = format!("{}?version={version}", key_path(vault, "data", key));
    vault.raw("GET", &path, None).await
}

/// Version metadata of every version Vault still lists for `key`.
async fn metadata_versions(vault: &VaultFixture, key: &StoreKey) -> Value {
    let (status, body) = vault
        .raw("GET", &key_path(vault, "metadata", key), None)
        .await;
    assert_eq!(status, 200, "metadata of an existing key: {body}");
    body["data"]["versions"].clone()
}

fn is_destroyed(versions: &Value, n: u64) -> bool {
    versions[n.to_string()]["destroyed"] == json!(true)
}

// -- response shapes -------------------------------------------------------------

docker_test! {
    async fn soft_deleted_version_answers_404_and_reads_as_none(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        let (v1, v2, v3) = (put(&svc, &key, "one").await, put(&svc, &key, "two").await, put(&svc, &key, "three").await);
        assert_eq!((v1.as_str(), v2.as_str(), v3.as_str()), ("1", "2", "3"));

        // Soft-delete version 2 through Vault's own API (the plugin never does).
        let (status, _) = vault
            .raw("POST", &key_path(&vault, "delete", &key), Some(json!({"versions": [2]})))
            .await;
        assert_eq!(status, 204);

        // What Vault answers: 404, no `errors`, `data.data` null, `deletion_time` set.
        let (status, body) = raw_read(&vault, &key, 2).await;
        assert_eq!(status, 404, "{body}");
        assert!(body.get("errors").is_none(), "{body}");
        assert!(body["data"]["data"].is_null(), "{body}");
        assert!(
            !body["data"]["metadata"]["deletion_time"].as_str().unwrap().is_empty(),
            "{body}"
        );
        assert_eq!(body["data"]["metadata"]["destroyed"], json!(false), "{body}");

        // The plugin: gone. The neighbours are untouched.
        assert_eq!(read(&svc, &key, &v2).await, None);
        assert_eq!(read(&svc, &key, &v1).await.as_deref(), Some("one"));
        assert_eq!(read(&svc, &key, &v3).await.as_deref(), Some("three"));

        // Undeleting brings it back, so the `None` above was the soft delete.
        let (status, _) = vault
            .raw("POST", &key_path(&vault, "undelete", &key), Some(json!({"versions": [2]})))
            .await;
        assert_eq!(status, 204);
        assert_eq!(read(&svc, &key, &v2).await.as_deref(), Some("two"));
    }
}

docker_test! {
    async fn destroyed_version_answers_404_and_reads_as_none(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        let v1 = put(&svc, &key, "one").await;
        let v2 = put(&svc, &key, "two").await;

        svc.destroy_value(&key, &DestroySelector::Exactly(v1.clone())).await.unwrap();

        // What Vault answers: 404, no `errors`, `data.data` null, `destroyed` true.
        let (status, body) = raw_read(&vault, &key, 1).await;
        assert_eq!(status, 404, "{body}");
        assert!(body.get("errors").is_none(), "{body}");
        assert!(body["data"]["data"].is_null(), "{body}");
        assert_eq!(body["data"]["metadata"]["destroyed"], json!(true), "{body}");

        assert_eq!(read(&svc, &key, &v1).await, None);
        assert_eq!(read(&svc, &key, &v2).await.as_deref(), Some("two"));

        // A destroyed version number is not reissued, even after the newest
        // version was destroyed.
        svc.destroy_value(&key, &DestroySelector::Exactly(v2.clone())).await.unwrap();
        let v3 = put(&svc, &key, "three").await;
        assert_eq!(v3.as_str(), "3");
    }
}

docker_test! {
    async fn unissued_versions_answer_404_and_version_zero_is_never_sent(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        let v1 = put(&svc, &key, "only").await;

        // Absent version: 404 with an empty `errors` list.
        let (status, body) = raw_read(&vault, &key, 99).await;
        assert_eq!(status, 404);
        assert_eq!(body["errors"], json!([]));
        assert_eq!(read(&svc, &key, &vv(99)).await, None);

        // A key that was never written: the same answer.
        let (status, body) = raw_read(&vault, &fresh_key(), 1).await;
        assert_eq!(status, 404);
        assert_eq!(body["errors"], json!([]));
        assert_eq!(read(&svc, &fresh_key(), &v1).await, None);

        // Vault reads the *latest* version for `?version=0`, which would be
        // the wrong bytes: the plugin must answer `None` without asking.
        let (status, body) = raw_read(&vault, &key, 0).await;
        assert_eq!(status, 200, "documents Vault's behaviour: {body}");
        assert_eq!(body["data"]["metadata"]["version"], json!(1));
        assert_eq!(read(&svc, &key, &vv(0)).await, None);
    }
}

docker_test! {
    async fn entries_not_written_by_the_plugin_are_internal(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        let data = key_path(&vault, "data", &key);
        // Hand-written rows: no `value` field, and a `value` that is not base64.
        let (status, _) = vault.raw("POST", &data, Some(json!({"data": {"password": "x"}}))).await;
        assert_eq!(status, 200);
        let (status, _) = vault.raw("POST", &data, Some(json!({"data": {"value": "not base64!!"}}))).await;
        assert_eq!(status, 200);

        for n in [1, 2] {
            let err = svc.get_value(&key, &vv(n)).await.unwrap_err();
            assert!(matches!(err, CredStorePluginError::Internal { .. }), "version {n}: {err:?}");
        }
    }
}

/// Lowers the mount's retention limit (the fixture sets a large one).
async fn set_mount_max_versions(vault: &VaultFixture, max_versions: u32) {
    let (status, body) = vault
        .raw(
            "POST",
            &format!("{MOUNT}/config"),
            Some(json!({"max_versions": max_versions})),
        )
        .await;
    assert!(status < 300, "{status} {body}");
}

/// Writes a version with a raw `POST data/...` (root token), bypassing the
/// plugin, so the key's own `max_versions` stays whatever the test set.
async fn raw_write(vault: &VaultFixture, key: &StoreKey, text: &str) {
    let body = json!({"data": {"value": text}});
    let (status, resp) = vault
        .raw("POST", &key_path(vault, "data", key), Some(body))
        .await;
    assert_eq!(status, 200, "{resp}");
}

async fn key_max_versions(vault: &VaultFixture, key: &StoreKey) -> Value {
    let (status, body) = vault
        .raw("GET", &key_path(vault, "metadata", key), None)
        .await;
    assert_eq!(status, 200, "metadata of an existing key: {body}");
    body["data"]["max_versions"].clone()
}

docker_test! {
    async fn put_sets_the_key_limit_to_100_whatever_the_mount_says(vault) {
        set_mount_max_versions(&vault, 3).await;
        let svc = vault.plugin();
        let key = fresh_key();
        let mut versions = Vec::new();
        for i in 1..=5 {
            versions.push(put(&svc, &key, &format!("value {i}")).await);
        }

        assert_eq!(key_max_versions(&vault, &key).await, json!(100));
        // The mount's 3 does not apply: the larger limit of the two wins.
        for (i, v) in versions.iter().enumerate() {
            assert_eq!(read(&svc, &key, v).await, Some(format!("value {}", i + 1)));
        }
        assert_eq!(metadata_versions(&vault, &key).await.as_object().unwrap().len(), 5);
    }
}

docker_test! {
    async fn exceeding_100_versions_evicts_the_oldest_and_reads_as_none(vault) {
        set_mount_max_versions(&vault, 3).await;
        let svc = vault.plugin();
        let key = fresh_key();
        let mut versions = Vec::new();
        for i in 1..=101 {
            versions.push(put(&svc, &key, &format!("value {i}")).await);
        }

        // The oldest is gone for good; Vault answers 404 `{"errors":[]}`.
        let (status, body) = raw_read(&vault, &key, 1).await;
        assert_eq!(status, 404);
        assert_eq!(body["errors"], json!([]));
        assert_eq!(read(&svc, &key, &versions[0]).await, None);
        assert_eq!(read(&svc, &key, &versions[1]).await.as_deref(), Some("value 2"));
        assert_eq!(read(&svc, &key, &versions[100]).await.as_deref(), Some("value 101"));
        let meta = metadata_versions(&vault, &key).await;
        assert_eq!(meta.as_object().unwrap().len(), 100, "{meta}");

        // Cleanup still works on what is left.
        svc.destroy_value(&key, &DestroySelector::Below(versions[100].clone())).await.unwrap();
        assert_eq!(read(&svc, &key, &versions[1]).await, None);
        assert_eq!(read(&svc, &key, &versions[100]).await.as_deref(), Some("value 101"));
    }
}

docker_test! {
    async fn a_per_key_max_versions_can_only_raise_the_mount_limit(vault) {
        // Vault's own rule: the effective limit is the larger of the mount's
        // and the key's `max_versions` (0 = unset; 10 when both are unset). A
        // smaller per-key value does not shrink the mount's limit, which is
        // why the plugin's key limit of 100 holds under any lower mount value.
        // Written with raw calls: a plugin `put` would reset the key to 100.
        set_mount_max_versions(&vault, 3).await;

        let smaller = fresh_key();
        let (status, _) = vault
            .raw("POST", &key_path(&vault, "metadata", &smaller), Some(json!({"max_versions": 2})))
            .await;
        assert_eq!(status, 204);
        let larger = fresh_key();
        let (status, _) = vault
            .raw("POST", &key_path(&vault, "metadata", &larger), Some(json!({"max_versions": 5})))
            .await;
        assert_eq!(status, 204);
        for i in 1..=8 {
            raw_write(&vault, &smaller, &format!("v{i}")).await;
            raw_write(&vault, &larger, &format!("v{i}")).await;
        }
        assert_eq!(metadata_versions(&vault, &smaller).await.as_object().unwrap().len(), 3);
        assert_eq!(metadata_versions(&vault, &larger).await.as_object().unwrap().len(), 5);
    }
}

docker_test! {
    async fn default_retention_without_max_versions_is_ten_versions(vault) {
        // Vault's own rule: mount `max_versions = 0` (unset) and no per-key
        // value keep 10 versions (0 is not unlimited).
        set_mount_max_versions(&vault, 0).await;
        let svc = vault.plugin();
        let raw_key = fresh_key();
        for i in 1..=12 {
            raw_write(&vault, &raw_key, &format!("v{i}")).await;
        }
        // (Raw values are not base64, so they are inspected at Vault, not
        // through the plugin's `get`.)
        assert_eq!(raw_read(&vault, &raw_key, 1).await.0, 404);
        assert_eq!(raw_read(&vault, &raw_key, 2).await.0, 404);
        assert_eq!(raw_read(&vault, &raw_key, 3).await.0, 200);
        assert_eq!(metadata_versions(&vault, &raw_key).await.as_object().unwrap().len(), 10);

        // The same mount, a key written by the plugin: its own 100 applies.
        let key = fresh_key();
        let mut versions = Vec::new();
        for i in 1..=12 {
            versions.push(put(&svc, &key, &format!("v{i}")).await);
        }
        for (i, v) in versions.iter().enumerate() {
            assert_eq!(read(&svc, &key, v).await, Some(format!("v{}", i + 1)));
        }
        assert_eq!(metadata_versions(&vault, &key).await.as_object().unwrap().len(), 12);
    }
}

// -- destroy(Below) --------------------------------------------------------------

docker_test! {
    async fn destroy_below_over_many_versions(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        let mut versions = Vec::new();
        for i in 1..=30_u64 {
            versions.push(put(&svc, &key, &format!("secret {i}")).await);
        }
        assert_eq!(versions[29].as_str(), "30");

        // Soft-delete one version below the cut: it must be destroyed too.
        let (status, _) = vault
            .raw("POST", &key_path(&vault, "delete", &key), Some(json!({"versions": [7]})))
            .await;
        assert_eq!(status, 204);

        svc.destroy_value(&key, &DestroySelector::Below(vv(25))).await.unwrap();

        let meta = metadata_versions(&vault, &key).await;
        for n in 1..=24 {
            assert!(is_destroyed(&meta, n), "version {n} must be destroyed: {meta}");
            assert_eq!(read(&svc, &key, &vv(n)).await, None, "version {n}");
        }
        for n in 25..=30 {
            assert!(!is_destroyed(&meta, n), "version {n} must be kept");
            assert_eq!(
                read(&svc, &key, &vv(n)).await.as_deref(),
                Some(format!("secret {n}").as_str()),
                "version {n}"
            );
        }

        // Idempotent, also with a higher cut that skips what is already destroyed.
        svc.destroy_value(&key, &DestroySelector::Below(vv(25))).await.unwrap();
        svc.destroy_value(&key, &DestroySelector::Below(vv(28))).await.unwrap();
        let meta = metadata_versions(&vault, &key).await;
        for n in 25..=27 {
            assert!(is_destroyed(&meta, n), "version {n}");
        }
        for n in 28..=30 {
            assert!(!is_destroyed(&meta, n), "version {n}");
        }

        // Exactly: one version, idempotent, and a version never issued is fine.
        svc.destroy_value(&key, &DestroySelector::Exactly(vv(29))).await.unwrap();
        svc.destroy_value(&key, &DestroySelector::Exactly(vv(29))).await.unwrap();
        svc.destroy_value(&key, &DestroySelector::Exactly(vv(500))).await.unwrap();
        assert_eq!(read(&svc, &key, &vv(29)).await, None);
        assert_eq!(read(&svc, &key, &vv(28)).await.as_deref(), Some("secret 28"));
        assert_eq!(read(&svc, &key, &vv(30)).await.as_deref(), Some("secret 30"));
    }
}

docker_test! {
    async fn destroy_and_delete_on_missing_keys_succeed(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        svc.destroy_value(&key, &DestroySelector::Below(vv(9))).await.unwrap();
        svc.destroy_value(&key, &DestroySelector::Exactly(vv(3))).await.unwrap();
        svc.delete_key_value(&key).await.unwrap();
        svc.delete_key_value(&key).await.unwrap();
    }
}

docker_test! {
    async fn delete_key_removes_every_version(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        let v1 = put(&svc, &key, "one").await;
        let v2 = put(&svc, &key, "two").await;
        svc.delete_key_value(&key).await.unwrap();

        let (status, _) = vault.raw("GET", &key_path(&vault, "metadata", &key), None).await;
        assert_eq!(status, 404, "the key and its metadata are gone");
        assert_eq!(read(&svc, &key, &v1).await, None);
        assert_eq!(read(&svc, &key, &v2).await, None);

        // Vault restarts the numbering of a deleted key; the gear never reuses
        // a key (record ids are minted once), so this is only documented here.
        assert_eq!(put(&svc, &key, "again").await.as_str(), "1");
    }
}

// -- token ---------------------------------------------------------------------------

docker_test! {
    async fn token_file_rotation_is_picked_up_after_a_403(vault) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("vault-token");
        let token_a = vault.create_token().await;
        std::fs::write(&file, format!("{token_a}\n")).unwrap();

        let mut cfg = vault.config();
        cfg.token = None;
        cfg.token_file = Some(file.to_string_lossy().into_owned());
        let svc = plugin_from(&cfg);
        let key = fresh_key();
        let v1 = put(&svc, &key, "before rotation").await;

        // A sidecar writes the new token and the old one is revoked.
        let token_b = vault.create_token().await;
        std::fs::write(&file, format!("{token_b}\n")).unwrap();
        vault.revoke_token(&token_a).await;
        let (status, _) = vault.raw_as(&token_a, "GET", &key_path(&vault, "data", &key), None).await;
        assert_eq!(status, 403, "the revoked token is dead");

        // The plugin still holds `token_a`: Vault answers 403, the file is
        // re-read, and the same request goes out once more with `token_b`.
        assert_eq!(read(&svc, &key, &v1).await.as_deref(), Some("before rotation"));
        // `token_b` is cached now; writes work without another 403.
        let v2 = put(&svc, &key, "after rotation").await;
        assert_eq!(read(&svc, &key, &v2).await.as_deref(), Some("after rotation"));

        // Rotation of a write: the re-read applies to `put` as well (the 403
        // proves nothing was executed, so resending is safe).
        let token_c = vault.create_token().await;
        std::fs::write(&file, &token_c).unwrap();
        vault.revoke_token(&token_b).await;
        let v3 = put(&svc, &key, "second rotation").await;
        assert_eq!(read(&svc, &key, &v3).await.as_deref(), Some("second rotation"));

        // Persistent 403: the token is revoked and the file holds nothing new.
        vault.revoke_token(&token_c).await;
        let err = svc.get_value(&key, &v3).await.unwrap_err();
        assert!(err.is_credentials_rejected(), "{err:?}");
        let message = err.to_string();
        assert!(message.contains("403"), "{message}");
        for token in [&token_a, &token_b, &token_c] {
            assert!(!message.contains(token.as_str()), "the token must not leak: {message}");
        }
        let err = svc
            .put_value(&key, SecretValue::from("rejected"))
            .await
            .unwrap_err();
        assert!(err.is_credentials_rejected(), "{err:?}");

        // The operator (or the sidecar) provides a working token: recovered
        // without a restart.
        let token_d = vault.create_token().await;
        std::fs::write(&file, &token_d).unwrap();
        assert_eq!(read(&svc, &key, &v3).await.as_deref(), Some("second rotation"));
        svc.delete_key_value(&key).await.unwrap();
    }
}

docker_test! {
    async fn token_file_that_appears_after_startup_is_used(vault) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("vault-token");
        let mut cfg = vault.config();
        cfg.token = None;
        cfg.token_file = Some(file.to_string_lossy().into_owned());
        let svc = plugin_from(&cfg);
        let key = fresh_key();

        // The sidecar has not written the token yet.
        let err = svc.put_value(&key, SecretValue::from("early")).await.unwrap_err();
        assert!(err.is_unavailable(), "{err:?}");

        std::fs::write(&file, &vault.token).unwrap();
        let v = put(&svc, &key, "late").await;
        assert_eq!(read(&svc, &key, &v).await.as_deref(), Some("late"));
    }
}

docker_test! {
    async fn the_documented_policy_is_confined_to_the_prefix(vault) {
        // Reads under another prefix, and on another mount, are denied.
        let other_prefix = format!("{MOUNT}/data/not-{}/t/r", vault.prefix);
        let (status, _) = vault.raw_as(&vault.token, "GET", &other_prefix, None).await;
        assert_eq!(status, 403);
        let (status, _) = vault.raw_as(&vault.token, "GET", "secret/data/anything", None).await;
        assert_eq!(status, 403);
        // Soft delete and undelete are not granted (the plugin never uses them).
        let key = fresh_key();
        let svc = vault.plugin();
        put(&svc, &key, "x").await;
        let (status, _) = vault
            .raw_as(&vault.token, "POST", &key_path(&vault, "delete", &key), Some(json!({"versions": [1]})))
            .await;
        assert_eq!(status, 403);
    }
}

// -- failures --------------------------------------------------------------------------

docker_test! {
    async fn a_missing_mount_is_an_error_not_a_miss(vault) {
        // Vault answers 404 with an explanation for a path under no mount.
        let (status, body) = vault.raw("GET", "no-such-mount/data/x/y", None).await;
        assert_eq!(status, 404);
        assert!(!body["errors"].as_array().unwrap().is_empty(), "{body}");

        // The root token, so the answer is the routing error, not a 403.
        let mut cfg = vault.config();
        cfg.token = Some(vault_credstore_plugin_v2::config::VaultToken::from(common::ROOT_TOKEN));
        cfg.mount = "no-such-mount".to_owned();
        let svc = plugin_from(&cfg);
        let key = fresh_key();

        assert!(matches!(svc.get_value(&key, &vv(1)).await, Err(CredStorePluginError::Internal { .. })));
        assert!(matches!(
            svc.put_value(&key, SecretValue::from("x")).await,
            Err(CredStorePluginError::Internal { .. })
        ));
        assert!(matches!(svc.delete_key_value(&key).await, Err(CredStorePluginError::Internal { .. })));
        assert!(matches!(
            svc.destroy_value(&key, &DestroySelector::Exactly(vv(1))).await,
            Err(CredStorePluginError::Internal { .. })
        ));
        assert!(matches!(
            svc.destroy_value(&key, &DestroySelector::Below(vv(5))).await,
            Err(CredStorePluginError::Internal { .. })
        ));
    }
}

docker_test! {
    async fn a_token_without_the_policy_is_credentials_rejected(vault) {
        let mut cfg = vault.config();
        cfg.token = Some(vault_credstore_plugin_v2::config::VaultToken::from("hvs.not-a-token"));
        let svc = plugin_from(&cfg);
        let key = fresh_key();

        let err = svc.get_value(&key, &vv(1)).await.unwrap_err();
        assert!(err.is_credentials_rejected(), "{err:?}");
        assert!(!err.to_string().contains("hvs.not-a-token"));
        assert!(svc.put_value(&key, SecretValue::from("x")).await.unwrap_err().is_credentials_rejected());
        assert!(svc.delete_key_value(&key).await.unwrap_err().is_credentials_rejected());
    }
}

docker_test! {
    async fn a_sealed_vault_is_service_unavailable(vault) {
        let svc = vault.plugin();
        let key = fresh_key();
        let v1 = put(&svc, &key, "before").await;

        let (status, _) = vault.raw("PUT", "sys/seal", None).await;
        assert_eq!(status, 204);

        // 503 "Vault is sealed" on every operation; the idempotent ones were
        // retried, `put` was not (this test cannot count requests, the unit
        // tests do; here only the final classification is checked).
        let err = svc.get_value(&key, &v1).await.unwrap_err();
        assert!(err.is_unavailable(), "{err:?}");
        assert!(err.to_string().contains("503"), "{err}");
        assert!(svc.put_value(&key, SecretValue::from("x")).await.unwrap_err().is_unavailable());
        assert!(svc.delete_key_value(&key).await.unwrap_err().is_unavailable());
        assert!(
            svc.destroy_value(&key, &DestroySelector::Below(vv(5)))
                .await
                .unwrap_err()
                .is_unavailable()
        );
    }
}
