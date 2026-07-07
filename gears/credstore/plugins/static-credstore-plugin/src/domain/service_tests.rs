// Created: 2026-06-06 — tests for the static credstore in-memory value store.
use uuid::Uuid;

use credstore_sdk::{OwnerId, SecretRef, SecretValue, SharingMode, TenantId};

use crate::config::{SecretConfig, StaticCredStorePluginConfig};

use super::Service;

const T1: &str = "00000000-0000-0000-0000-000000000001";
const T2: &str = "00000000-0000-0000-0000-000000000002";
const O1: &str = "11111111-0000-0000-0000-000000000001";
const O2: &str = "22222222-0000-0000-0000-000000000002";

fn tid(s: &str) -> TenantId {
    TenantId(Uuid::parse_str(s).unwrap())
}
fn oid(s: &str) -> OwnerId {
    OwnerId(Uuid::parse_str(s).unwrap())
}
fn sref(s: &str) -> SecretRef {
    SecretRef::new(s).unwrap()
}

fn secret(tenant: Option<&str>, owner: Option<&str>, key: &str, value: &str) -> SecretConfig {
    SecretConfig {
        tenant_id: tenant.map(|t| Uuid::parse_str(t).unwrap()),
        owner_id: owner.map(|o| Uuid::parse_str(o).unwrap()),
        key: key.to_owned(),
        value: value.to_owned(),
        sharing: None,
    }
}

fn cfg(secrets: Vec<SecretConfig>) -> StaticCredStorePluginConfig {
    StaticCredStorePluginConfig {
        secrets,
        ..Default::default()
    }
}

fn svc(secrets: Vec<SecretConfig>) -> Service {
    Service::from_config(&cfg(secrets)).expect("config builds")
}

#[track_caller]
fn assert_value(v: Option<SecretValue>, expected: &str) {
    assert_eq!(
        v.expect("value present").as_bytes(),
        expected.as_bytes(),
        "secret value mismatch"
    );
}

#[test]
fn tenant_class_read() {
    let s = svc(vec![secret(Some(T1), None, "openai-key", "tenant-val")]);

    assert_value(
        s.get_value(&tid(T1), &sref("openai-key"), None),
        "tenant-val",
    );
    // Other tenant cannot see it.
    assert!(s.get_value(&tid(T2), &sref("openai-key"), None).is_none());
    // Tenant secret is not exposed to the private key class.
    assert!(
        s.get_value(&tid(T1), &sref("openai-key"), Some(&oid(O1)))
            .is_none()
    );
}

#[test]
fn private_class_read_is_owner_scoped() {
    let s = svc(vec![secret(
        Some(T1),
        Some(O1),
        "openai-key",
        "private-val",
    )]);

    assert_value(
        s.get_value(&tid(T1), &sref("openai-key"), Some(&oid(O1))),
        "private-val",
    );
    // Wrong owner -> miss.
    assert!(
        s.get_value(&tid(T1), &sref("openai-key"), Some(&oid(O2)))
            .is_none()
    );
    // Tenant-class read does not see a private secret.
    assert!(s.get_value(&tid(T1), &sref("openai-key"), None).is_none());
}

#[test]
fn global_secret_is_a_tenant_class_fallback() {
    // No tenant_id -> global (resolved sharing == Shared).
    let s = svc(vec![secret(None, None, "azure-key", "global-val")]);

    assert_value(
        s.get_value(&tid(T1), &sref("azure-key"), None),
        "global-val",
    );
    assert_value(
        s.get_value(&tid(T2), &sref("azure-key"), None),
        "global-val",
    );
    // Not visible to the private key class.
    assert!(
        s.get_value(&tid(T1), &sref("azure-key"), Some(&oid(O1)))
            .is_none()
    );
}

#[test]
fn shared_secret_is_a_tenant_class_fallback() {
    let mut entry = secret(Some(T1), None, "shared-key", "shared-val");
    entry.sharing = Some(SharingMode::Shared);
    let s = svc(vec![entry]);

    assert_value(
        s.get_value(&tid(T1), &sref("shared-key"), None),
        "shared-val",
    );
    // Scoped to the owning tenant (gear handles hierarchical walk-up).
    assert!(s.get_value(&tid(T2), &sref("shared-key"), None).is_none());
}

#[test]
fn put_then_get_tenant_class() {
    let s = svc(vec![]);
    s.put_value(&tid(T1), &sref("k"), SecretValue::from("written"), None);
    assert_value(s.get_value(&tid(T1), &sref("k"), None), "written");
}

#[test]
fn put_then_get_private_class() {
    let s = svc(vec![]);
    s.put_value(
        &tid(T1),
        &sref("k"),
        SecretValue::from("owned"),
        Some(&oid(O1)),
    );
    assert_value(s.get_value(&tid(T1), &sref("k"), Some(&oid(O1))), "owned");
    // Private write is invisible to the tenant class and other owners.
    assert!(s.get_value(&tid(T1), &sref("k"), None).is_none());
    assert!(s.get_value(&tid(T1), &sref("k"), Some(&oid(O2))).is_none());
}

#[test]
fn put_overwrites_existing_value() {
    let s = svc(vec![secret(Some(T1), None, "k", "old")]);
    s.put_value(&tid(T1), &sref("k"), SecretValue::from("new"), None);
    assert_value(s.get_value(&tid(T1), &sref("k"), None), "new");
}

#[test]
fn delete_removes_tenant_value() {
    let s = svc(vec![secret(Some(T1), None, "k", "v")]);
    s.delete_value(&tid(T1), &sref("k"), None);
    assert!(s.get_value(&tid(T1), &sref("k"), None).is_none());
}

#[test]
fn delete_removes_private_value_without_touching_tenant_class() {
    let s = svc(vec![
        secret(Some(T1), None, "k", "tenant-v"),
        secret(Some(T1), Some(O1), "k", "private-v"),
    ]);
    s.delete_value(&tid(T1), &sref("k"), Some(&oid(O1)));
    assert!(s.get_value(&tid(T1), &sref("k"), Some(&oid(O1))).is_none());
    // Tenant-class value under the same key is untouched.
    assert_value(s.get_value(&tid(T1), &sref("k"), None), "tenant-v");
}

#[test]
fn tenant_delete_never_destroys_config_seeded_fallbacks() {
    // Config-seeded shared/global entries serve *other* tenants too; a
    // tenant-scoped delete (or a gear reaper retry) must not sweep them.
    let mut shared_entry = secret(Some(T1), None, "shared-key", "shared-val");
    shared_entry.sharing = Some(SharingMode::Shared);
    let s = svc(vec![
        secret(None, None, "global-key", "global-val"),
        shared_entry,
    ]);

    s.delete_value(&tid(T1), &sref("global-key"), None);
    s.delete_value(&tid(T1), &sref("shared-key"), None);

    assert_value(
        s.get_value(&tid(T2), &sref("global-key"), None),
        "global-val",
    );
    assert_value(
        s.get_value(&tid(T1), &sref("shared-key"), None),
        "shared-val",
    );
}

#[test]
fn delete_missing_is_noop() {
    let s = svc(vec![]);
    s.delete_value(&tid(T1), &sref("absent"), None);
    s.delete_value(&tid(T1), &sref("absent"), Some(&oid(O1)));
    assert!(s.get_value(&tid(T1), &sref("absent"), None).is_none());
}

#[test]
fn from_config_rejects_nil_tenant() {
    let nil = "00000000-0000-0000-0000-000000000000";
    let err = Service::from_config(&cfg(vec![secret(Some(nil), None, "k", "v")]))
        .expect_err("nil tenant rejected");
    assert!(err.to_string().contains("nil UUID"), "{err}");
}

#[test]
fn from_config_rejects_duplicate_tenant_key() {
    let err = Service::from_config(&cfg(vec![
        secret(Some(T1), None, "dup", "a"),
        secret(Some(T1), None, "dup", "b"),
    ]))
    .expect_err("duplicate rejected");
    assert!(err.to_string().contains("duplicate"), "{err}");
}

#[test]
fn from_config_rejects_invalid_secret_ref() {
    let err = Service::from_config(&cfg(vec![secret(Some(T1), None, "bad key!", "v")]))
        .expect_err("invalid ref rejected");
    assert!(!err.to_string().is_empty());
}
