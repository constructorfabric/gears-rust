//! Tests for the key-purge outbox: the handler contract (unit) and the real
//! platform outbox end to end on `SQLite` (delete transaction -> enqueue ->
//! handler -> `delete_key`).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use credstore_sdk::{OwnerId, SecretRef, SecretType, SharingMode, StoreKey, TenantId};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{OutboxPurgeEnqueuer, PurgeEnqueuer, PurgeHandler, PurgeMessage, start};
use crate::domain::error::DomainError;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::model::{Fallback, NewSecret};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::test_support::{
    FakeMetrics, FakePlugin, FakePluginSelector, NoPluginSelector,
};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo_impl::SecretRepoImpl;

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

fn message(payload: Vec<u8>) -> OutboxMessage {
    OutboxMessage {
        partition_id: 0,
        seq: 1,
        payload,
        payload_type: super::PURGE_PAYLOAD_TYPE.to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 0,
    }
}

fn purge_payload(k: &StoreKey) -> Vec<u8> {
    serde_json::to_vec(&PurgeMessage::from(k)).expect("serialize")
}

fn handler(plugins: Arc<dyn PluginSelector>, metrics: &Arc<FakeMetrics>) -> PurgeHandler {
    PurgeHandler::new(plugins, Arc::clone(metrics) as Arc<_>)
}

#[tokio::test]
async fn purge_message_wire_form_carries_only_the_key() {
    let k = key();
    let json: serde_json::Value = serde_json::from_slice(&purge_payload(&k)).expect("json");
    assert_eq!(json.as_object().expect("object").len(), 2);
    assert_eq!(json["tenant_id"], k.tenant_id.0.to_string());
    assert_eq!(json["record_id"], k.record_id.to_string());
}

#[tokio::test]
async fn handler_deletes_the_key_with_all_its_versions() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let k = key();
    plugin.seed(&k, b"a");
    plugin.seed(&k, b"b");
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);

    let res = h.handle(&message(purge_payload(&k))).await;

    assert!(matches!(res, MessageResult::Ok));
    assert_eq!(plugin.delete_key_calls(), vec![k.clone()]);
    assert!(!plugin.holds_key(&k));
    assert_eq!(metrics.outbox_purge_failed_total(), 0);
}

#[tokio::test]
async fn handler_retries_on_a_plugin_error_and_counts_the_failure() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let k = key();
    plugin.seed(&k, b"a");
    plugin.fail_next_delete_keys(1);
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);

    let first = h.handle(&message(purge_payload(&k))).await;
    assert!(matches!(first, MessageResult::Retry));
    assert_eq!(metrics.outbox_purge_failed_total(), 1);
    assert!(plugin.holds_key(&k), "nothing was deleted yet");

    // The outbox redelivers: at-least-once, idempotent delete_key.
    let second = h.handle(&message(purge_payload(&k))).await;
    assert!(matches!(second, MessageResult::Ok));
    assert!(!plugin.holds_key(&k));
    let third = h.handle(&message(purge_payload(&k))).await;
    assert!(
        matches!(third, MessageResult::Ok),
        "a duplicate delivery is harmless"
    );
}

#[tokio::test]
async fn handler_retries_when_no_plugin_is_available() {
    let metrics = FakeMetrics::new();
    let h = handler(Arc::new(NoPluginSelector), &metrics);
    let res = h.handle(&message(purge_payload(&key()))).await;
    assert!(matches!(res, MessageResult::Retry));
    assert_eq!(metrics.outbox_purge_failed_total(), 1);
}

#[tokio::test]
async fn handler_rejects_a_corrupt_payload() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);
    let res = h.handle(&message(b"not json".to_vec())).await;
    assert!(matches!(res, MessageResult::Reject(_)));
    assert!(plugin.delete_key_calls().is_empty());
}

#[tokio::test]
async fn enqueue_before_the_outbox_is_started_is_an_internal_error() {
    let enqueuer = OutboxPurgeEnqueuer::new();
    let db = connect_db(
        &format!(
            "sqlite:file:credstore_outbox_unstarted_{}?mode=memory&cache=shared",
            Uuid::new_v4()
        ),
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect");
    let provider = DBProvider::<DomainError>::new(db);
    let conn = provider.conn().expect("conn");
    let err = enqueuer
        .enqueue_purge(&conn, &key())
        .await
        .expect_err("not started");
    assert!(matches!(err, DomainError::Internal { .. }));
}

/// The whole path on a real `SQLite` outbox: the delete transaction enqueues the
/// purge, the pipeline delivers it, and the handler removes the key.
#[tokio::test]
async fn delete_transaction_enqueues_a_purge_that_the_pipeline_delivers() {
    let dsn = format!(
        "sqlite:file:credstore_outbox_e2e_{}?mode=memory&cache=shared",
        Uuid::new_v4()
    );
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect sqlite");
    let mut migrations = Migrator::migrations();
    migrations.extend(super::migrations().expect("outbox migrations"));
    run_migrations_for_testing(&db, migrations)
        .await
        .expect("migrations");

    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let enqueuer = Arc::new(OutboxPurgeEnqueuer::new());
    let handle = start(
        db.clone(),
        &enqueuer,
        handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics),
    )
    .await
    .expect("start outbox");

    let repo = SecretRepoImpl::new(
        Arc::new(DBProvider::<DomainError>::new(db)),
        Arc::clone(&enqueuer) as Arc<dyn PurgeEnqueuer>,
    );
    let tenant = Uuid::new_v4();
    let new = NewSecret {
        id: Uuid::new_v4(),
        tenant_id: TenantId(tenant),
        reference: SecretRef::new("k").expect("ref"),
        sharing: SharingMode::Tenant,
        owner_id: OwnerId(Uuid::new_v4()),
        secret_type_uuid: SecretType::generic().uuid(),
        expires_at: None,
        value_version: credstore_sdk::ValueVersion::new("1"),
        fallback: Fallback::Inherit,
    };
    let store_key = StoreKey::new(TenantId(tenant), new.id);
    plugin.seed(&store_key, b"v");
    let scope = AccessScope::for_tenant(tenant);
    repo.insert_active(&scope, &new).await.expect("insert");

    repo.delete_by_id(&scope, &store_key, None)
        .await
        .expect("delete");

    let mut purged = false;
    for _ in 0..100 {
        if !plugin.holds_key(&store_key) && !plugin.delete_key_calls().is_empty() {
            purged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    handle.stop().await;
    assert!(
        purged,
        "the outbox must deliver the purge and delete the key"
    );
    assert_eq!(plugin.delete_key_calls(), vec![store_key]);
}
