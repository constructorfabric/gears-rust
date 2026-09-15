// Created: 2026-09-07 by Constructor Tech
//! The adapter against a store that behaves like credstore's client: entries
//! keyed by tenant, owner and reference, create-only conflicts, existence
//! preconditions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretValue, SharingMode,
    TenantId, WriteOptions, WritePrecondition,
};
use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::CredStoreSecretManager;
use crate::domain::error::DomainError;
use crate::domain::ports::SecretManager;

const KEY: &str = "gts.cf.core.settings.setting_type.v1~cf.demo.security.api_token.v1~";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Slot {
    tenant: Uuid,
    owner: Uuid,
    reference: String,
    sharing: String,
}

#[derive(Default)]
struct MemoryCredStore {
    entries: Mutex<HashMap<Slot, Vec<u8>>>,
    down: std::sync::atomic::AtomicBool,
}

impl MemoryCredStore {
    fn slot(ctx: &SecurityContext, key: &SecretRef, sharing: SharingMode) -> Slot {
        Slot {
            tenant: ctx.subject_tenant_id(),
            owner: ctx.subject_id(),
            reference: key.as_ref().to_owned(),
            sharing: format!("{sharing:?}"),
        }
    }

    fn check(&self) -> Result<(), CredStoreError> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CredStoreError::service_unavailable("down"));
        }
        Ok(())
    }

    fn slots(&self) -> Vec<Slot> {
        self.entries.lock().expect("lock").keys().cloned().collect()
    }
}

#[async_trait]
impl CredStoreClientV1 for MemoryCredStore {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        self.check()?;
        let slot = Self::slot(ctx, key, SharingMode::Private);
        Ok(self
            .entries
            .lock()
            .expect("lock")
            .get(&slot)
            .map(|bytes| GetSecretResponse {
                value: SecretValue::new(bytes.clone()),
                id: Uuid::new_v4(),
                owner_tenant_id: TenantId(slot.tenant),
                sharing: SharingMode::Private,
                is_inherited: false,
                version: 1,
                secret_type: "gts.cf.core.credstore.secret.v1~generic.v1~".to_owned(),
                expires_at: None,
            }))
    }

    async fn put_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        precondition: WritePrecondition,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.check()?;
        let slot = Self::slot(ctx, key, sharing);
        let mut entries = self.entries.lock().expect("lock");
        if !entries.contains_key(&slot) {
            return Err(CredStoreError::NotFound);
        }
        assert!(matches!(precondition, WritePrecondition::Exists));
        entries.insert(slot, value.as_bytes().to_vec());
        Ok(())
    }

    async fn create_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        _opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        self.check()?;
        let slot = Self::slot(ctx, key, sharing);
        let mut entries = self.entries.lock().expect("lock");
        if entries.contains_key(&slot) {
            return Err(CredStoreError::Conflict);
        }
        entries.insert(slot, value.as_bytes().to_vec());
        Ok(())
    }

    async fn delete(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        _precondition: WritePrecondition,
    ) -> Result<(), CredStoreError> {
        self.check()?;
        let slot = Self::slot(ctx, key, SharingMode::Private);
        self.entries
            .lock()
            .expect("lock")
            .remove(&slot)
            .map(|_| ())
            .ok_or(CredStoreError::NotFound)
    }
}

fn manager() -> (Arc<MemoryCredStore>, CredStoreSecretManager) {
    let store = Arc::new(MemoryCredStore::default());
    let manager = CredStoreSecretManager::new(Arc::clone(&store) as Arc<dyn CredStoreClientV1>);
    (store, manager)
}

#[tokio::test]
async fn a_secret_is_stored_private_to_the_gear_principal_in_the_target_tenant() {
    let (store, manager) = manager();
    let tenant = Uuid::new_v4();
    let reference = manager
        .store_secret(KEY, tenant, &json!("hunter2"))
        .await
        .expect("stored");

    // The reference names the pair through a fixed prefix, within the store's
    // alphabet, carries the key only as a name-based UUID, and is unique to
    // this write.
    let prefix = manager.reference_prefix(KEY, tenant);
    assert!(reference.starts_with(&format!("{prefix}-")));
    assert!(prefix.starts_with("cf-settings-"));
    assert!(prefix.ends_with(&tenant.to_string()));
    assert!(SecretRef::new(reference.as_str()).is_ok());
    assert!(!reference.contains("api_token"));
    assert_ne!(
        manager.reference(KEY, tenant),
        manager.reference(KEY, tenant)
    );

    let slots = store.slots();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].tenant, tenant);
    assert_eq!(slots[0].owner, manager.principal());
    assert_eq!(slots[0].sharing, "Private");
    assert_ne!(manager.principal(), Uuid::nil());

    let plaintext = manager
        .resolve_plaintext(KEY, tenant, &reference)
        .await
        .expect("resolved");
    assert_eq!(plaintext, "hunter2");
}

#[tokio::test]
async fn a_second_set_creates_a_new_entry_and_leaves_the_first_untouched() {
    let (store, manager) = manager();
    let tenant = Uuid::new_v4();
    let first = manager
        .store_secret(KEY, tenant, &json!("one"))
        .await
        .expect("stored");
    let second = manager
        .store_secret(KEY, tenant, &json!("two"))
        .await
        .expect("stored again");
    assert_ne!(first, second);
    assert_eq!(store.slots().len(), 2);
    assert_eq!(
        manager
            .resolve_plaintext(KEY, tenant, &first)
            .await
            .expect("resolved"),
        "one"
    );
    assert_eq!(
        manager
            .resolve_plaintext(KEY, tenant, &second)
            .await
            .expect("resolved"),
        "two"
    );
}

#[tokio::test]
async fn different_tenants_and_keys_get_different_entries() {
    let (store, manager) = manager();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    manager
        .store_secret(KEY, a, &json!("a"))
        .await
        .expect("stored");
    manager
        .store_secret(KEY, b, &json!("b"))
        .await
        .expect("stored");
    manager
        .store_secret("other.key~", a, &json!("c"))
        .await
        .expect("stored");
    assert_eq!(store.slots().len(), 3);
    assert_ne!(
        manager.reference_prefix(KEY, a),
        manager.reference_prefix(KEY, b)
    );
    assert_ne!(
        manager.reference_prefix(KEY, a),
        manager.reference_prefix("other.key~", a)
    );
}

#[tokio::test]
async fn a_non_string_plaintext_is_stored_as_its_json_text() {
    let (_store, manager) = manager();
    let tenant = Uuid::new_v4();
    let reference = manager
        .store_secret(KEY, tenant, &json!({"user": "u", "pass": "p"}))
        .await
        .expect("stored");
    assert_eq!(
        manager
            .resolve_plaintext(KEY, tenant, &reference)
            .await
            .expect("resolved"),
        r#"{"pass":"p","user":"u"}"#
    );
}

#[tokio::test]
async fn an_absent_entry_is_not_found_on_the_value_and_a_down_store_is_unavailable() {
    let (store, manager) = manager();
    let tenant = Uuid::new_v4();
    let reference = manager.reference(KEY, tenant);
    let missing = manager
        .resolve_plaintext(KEY, tenant, &reference)
        .await
        .expect_err("absent");
    assert!(
        matches!(missing, DomainError::NotFound { resource } if resource == settings_service_sdk::gts::VALUE_SCHEMA),
        "{missing:?}"
    );

    store.down.store(true, std::sync::atomic::Ordering::SeqCst);
    for err in [
        manager
            .store_secret(KEY, tenant, &json!("x"))
            .await
            .expect_err("down"),
        manager
            .resolve_plaintext(KEY, tenant, &reference)
            .await
            .expect_err("down"),
        manager
            .delete_secret(KEY, tenant, &reference)
            .await
            .expect_err("down"),
    ] {
        assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
    }
}

#[tokio::test]
async fn delete_releases_the_entry_and_an_absent_entry_is_already_done() {
    let (store, manager) = manager();
    let tenant = Uuid::new_v4();
    let reference = manager
        .store_secret(KEY, tenant, &json!("gone soon"))
        .await
        .expect("stored");
    manager
        .delete_secret(KEY, tenant, &reference)
        .await
        .expect("released");
    assert!(store.slots().is_empty());
    manager
        .delete_secret(KEY, tenant, &reference)
        .await
        .expect("already done");
}

#[tokio::test]
async fn another_principal_in_the_same_tenant_reads_nothing_back() {
    // What `private` sharing buys: the entry is keyed by the gear's principal,
    // so a tenant user asking the store directly finds no entry.
    let (store, manager) = manager();
    let tenant = Uuid::new_v4();
    let reference = manager
        .store_secret(KEY, tenant, &json!("mine"))
        .await
        .expect("stored");
    let user = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .expect("context");
    let found = store
        .get(&user, &SecretRef::new(reference).expect("ref"))
        .await
        .expect("asked");
    assert!(found.is_none());
}
