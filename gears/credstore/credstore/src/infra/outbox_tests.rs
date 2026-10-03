//! Tests for the store-cleanup outbox: the handler contract (unit) and the
//! real platform outbox end to end on `SQLite` (repo transaction -> enqueue ->
//! handler -> `delete_key` / `destroy`).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use credstore_sdk::{
    DestroySelector, OwnerId, SecretRef, SecretType, SharingMode, StoreKey, TenantId, ValueVersion,
};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{
    CleanupEnqueuer, CleanupHandler, DESTROY_PAYLOAD_TYPE, DestroyMessage, OutboxCleanupEnqueuer,
    PURGE_PAYLOAD_TYPE, PurgeMessage, start,
};
use crate::domain::error::DomainError;
use crate::domain::ports::metrics::CleanupOp;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::model::{CleanupTask, Fallback, IntentCommit, NewSecret, WriteAttempt};
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::test_support::{
    FakeMetrics, FakePlugin, FakePluginSelector, NoPluginSelector,
};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo_impl::SecretRepoImpl;

fn key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::new_v4()), Uuid::new_v4())
}

fn message_of(payload_type: &str, payload: Vec<u8>) -> OutboxMessage {
    OutboxMessage {
        partition_id: 0,
        seq: 1,
        payload,
        payload_type: payload_type.to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 0,
    }
}

fn message(payload: Vec<u8>) -> OutboxMessage {
    message_of(PURGE_PAYLOAD_TYPE, payload)
}

fn purge_payload(k: &StoreKey) -> Vec<u8> {
    serde_json::to_vec(&PurgeMessage::from(k)).expect("serialize")
}

fn destroy_message(k: &StoreKey, selector: &DestroySelector) -> OutboxMessage {
    message_of(
        DESTROY_PAYLOAD_TYPE,
        serde_json::to_vec(&DestroyMessage::new(k, selector)).expect("serialize"),
    )
}

fn handler(plugins: Arc<dyn PluginSelector>, metrics: &Arc<FakeMetrics>) -> CleanupHandler {
    CleanupHandler::new(plugins, Arc::clone(metrics) as Arc<_>)
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
async fn destroy_message_wire_form_names_the_selector_and_the_version() {
    let k = key();
    let below = serde_json::to_value(DestroyMessage::new(
        &k,
        &DestroySelector::Below(ValueVersion::new("7")),
    ))
    .expect("json");
    assert_eq!(below.as_object().expect("object").len(), 4);
    assert_eq!(below["tenant_id"], k.tenant_id.0.to_string());
    assert_eq!(below["record_id"], k.record_id.to_string());
    assert_eq!(below["selector"], "below");
    assert_eq!(below["version"], "7");

    let exactly = serde_json::to_value(DestroyMessage::new(
        &k,
        &DestroySelector::Exactly(ValueVersion::new("3")),
    ))
    .expect("json");
    assert_eq!(exactly["selector"], "exactly");
    assert_eq!(exactly["version"], "3");
}

#[tokio::test]
async fn the_queue_is_the_store_cleanup_queue() {
    assert_eq!(super::QUEUE, "credstore.store_cleanup");
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
    assert!(plugin.destroy_calls().is_empty());
    assert!(metrics.store_cleanup_failed().is_empty());
}

#[tokio::test]
async fn handler_retries_a_purge_on_a_plugin_error_and_counts_the_failure() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let k = key();
    plugin.seed(&k, b"a");
    plugin.fail_next_delete_keys(1);
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);

    let first = h.handle(&message(purge_payload(&k))).await;
    assert!(matches!(first, MessageResult::Retry));
    assert_eq!(metrics.store_cleanup_failed(), vec![CleanupOp::Purge]);
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
async fn handler_destroys_below_the_selected_version() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let k = key();
    plugin.seed(&k, b"1");
    plugin.seed(&k, b"2");
    let v3 = plugin.seed(&k, b"3");
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);

    let res = h
        .handle(&destroy_message(&k, &DestroySelector::Below(v3.clone())))
        .await;

    assert!(matches!(res, MessageResult::Ok));
    assert_eq!(
        plugin.destroy_calls(),
        vec![(k.clone(), DestroySelector::Below(v3))]
    );
    assert_eq!(plugin.versions(&k), vec!["3"]);
    assert!(plugin.delete_key_calls().is_empty(), "never delete_key");
}

#[tokio::test]
async fn handler_destroys_exactly_the_selected_version() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let k = key();
    plugin.seed(&k, b"1");
    let v2 = plugin.seed(&k, b"2");
    plugin.seed(&k, b"3");
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);

    let res = h
        .handle(&destroy_message(&k, &DestroySelector::Exactly(v2.clone())))
        .await;

    assert!(matches!(res, MessageResult::Ok));
    assert_eq!(
        plugin.destroy_calls(),
        vec![(k.clone(), DestroySelector::Exactly(v2))]
    );
    assert_eq!(plugin.versions(&k), vec!["1", "3"]);
}

#[tokio::test]
async fn handler_acknowledges_a_destroy_for_a_plugin_without_destroy_without_a_call() {
    let plugin = FakePlugin::without_destroy();
    let metrics = FakeMetrics::new();
    let k = key();
    let v1 = plugin.seed(&k, b"1");
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);

    let res = h
        .handle(&destroy_message(&k, &DestroySelector::Exactly(v1)))
        .await;

    assert!(matches!(res, MessageResult::Ok));
    assert!(plugin.destroy_calls().is_empty(), "no call was made");
    assert_eq!(plugin.versions(&k), vec!["1"]);
    assert!(metrics.store_cleanup_failed().is_empty());
}

#[tokio::test]
async fn handler_retries_a_destroy_on_a_plugin_error_and_counts_the_failure() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let k = key();
    plugin.seed(&k, b"1");
    let v2 = plugin.seed(&k, b"2");
    plugin.fail_next_destroys(1);
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);
    let msg = destroy_message(&k, &DestroySelector::Below(v2));

    let first = h.handle(&msg).await;
    assert!(matches!(first, MessageResult::Retry));
    assert_eq!(metrics.store_cleanup_failed(), vec![CleanupOp::Destroy]);
    assert_eq!(plugin.versions(&k), vec!["1", "2"], "nothing destroyed yet");

    let second = h.handle(&msg).await;
    assert!(matches!(second, MessageResult::Ok));
    assert_eq!(plugin.versions(&k), vec!["2"]);
}

#[tokio::test]
async fn handler_treats_a_destroy_on_a_purged_key_as_success() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let k = key();
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);

    let res = h
        .handle(&destroy_message(
            &k,
            &DestroySelector::Exactly(ValueVersion::new("5")),
        ))
        .await;
    assert!(matches!(res, MessageResult::Ok));
}

#[tokio::test]
async fn handler_retries_when_no_plugin_is_available() {
    let metrics = FakeMetrics::new();
    let h = handler(Arc::new(NoPluginSelector), &metrics);
    let res = h.handle(&message(purge_payload(&key()))).await;
    assert!(matches!(res, MessageResult::Retry));
    let res = h
        .handle(&destroy_message(
            &key(),
            &DestroySelector::Below(ValueVersion::new("2")),
        ))
        .await;
    assert!(matches!(res, MessageResult::Retry));
    assert_eq!(
        metrics.store_cleanup_failed(),
        vec![CleanupOp::Purge, CleanupOp::Destroy]
    );
}

#[tokio::test]
async fn handler_rejects_a_corrupt_payload() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);
    let res = h.handle(&message(b"not json".to_vec())).await;
    assert!(matches!(res, MessageResult::Reject(_)));
    let res = h
        .handle(&message_of(DESTROY_PAYLOAD_TYPE, b"not json".to_vec()))
        .await;
    assert!(matches!(res, MessageResult::Reject(_)));
    // A destroy payload with an unknown selector is malformed, too.
    let k = key();
    let bad = serde_json::json!({
        "tenant_id": k.tenant_id.0,
        "record_id": k.record_id,
        "selector": "everything",
        "version": "1",
    });
    let res = h
        .handle(&message_of(
            DESTROY_PAYLOAD_TYPE,
            serde_json::to_vec(&bad).expect("serialize"),
        ))
        .await;
    assert!(matches!(res, MessageResult::Reject(_)));
    assert!(plugin.delete_key_calls().is_empty());
    assert!(plugin.destroy_calls().is_empty());
}

#[tokio::test]
async fn handler_rejects_an_unknown_payload_type() {
    let plugin = FakePlugin::new();
    let metrics = FakeMetrics::new();
    let h = handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics);
    let res = h
        .handle(&message_of(
            "application/vnd.credstore.something-else+json",
            purge_payload(&key()),
        ))
        .await;
    assert!(matches!(res, MessageResult::Reject(_)));
    assert!(plugin.delete_key_calls().is_empty());
}

#[tokio::test]
async fn enqueue_before_the_outbox_is_started_is_an_internal_error() {
    let enqueuer = OutboxCleanupEnqueuer::new();
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
        .enqueue(&conn, &CleanupTask::Purge(key()))
        .await
        .expect_err("not started");
    assert!(matches!(err, DomainError::Internal { .. }));
}

/// A repo over a real `SQLite` outbox: transactions enqueue, the pipeline
/// delivers to the handler, which acts on the fake plugin.
struct Harness {
    repo: SecretRepoImpl,
    plugin: Arc<FakePlugin>,
    handle: toolkit_db::outbox::OutboxHandle,
}

impl Harness {
    async fn start(plugin: Arc<FakePlugin>) -> Self {
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

        let metrics = FakeMetrics::new();
        let enqueuer = Arc::new(OutboxCleanupEnqueuer::new());
        let handle = start(
            db.clone(),
            &enqueuer,
            handler(Arc::new(FakePluginSelector::new(plugin.clone())), &metrics),
        )
        .await
        .expect("start outbox");
        let repo = SecretRepoImpl::new(
            Arc::new(DBProvider::<DomainError>::new(db)),
            Arc::clone(&enqueuer) as Arc<dyn CleanupEnqueuer>,
        );
        Self {
            repo,
            plugin,
            handle,
        }
    }

    /// Waits (bounded) for `cond` to hold, then stops the pipeline.
    async fn wait_for(self, what: &str, cond: impl Fn(&FakePlugin) -> bool) {
        let mut done = false;
        for _ in 0..100 {
            if cond(&self.plugin) {
                done = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.handle.stop().await;
        assert!(done, "the outbox must deliver: {what}");
    }

    /// Creates a row `k` in `tenant` whose key holds one stored version.
    async fn create(&self, tenant: Uuid) -> (NewSecret, StoreKey, WriteAttempt) {
        let new = NewSecret {
            id: Uuid::new_v4(),
            tenant_id: TenantId(tenant),
            reference: SecretRef::new("k").expect("ref"),
            sharing: SharingMode::Tenant,
            owner_id: OwnerId(Uuid::new_v4()),
            secret_type_uuid: SecretType::generic().uuid(),
            expires_at: None,
            value_version: ValueVersion::new("1"),
            fallback: Fallback::Inherit,
        };
        let key = StoreKey::new(TenantId(tenant), new.id);
        let v1 = self.plugin.seed(&key, b"v1");
        assert_eq!(v1, new.value_version);
        let attempt = WriteAttempt {
            attempt_id: Uuid::new_v4(),
            key: key.clone(),
            destroy_supported: true,
        };
        self.repo
            .begin_write_intent(&attempt, Duration::from_mins(5))
            .await
            .expect("begin");
        let outcome = self
            .repo
            .insert_active(&AccessScope::for_tenant(tenant), &new, &attempt)
            .await
            .expect("insert");
        assert!(matches!(outcome, IntentCommit::Committed { .. }));
        (new, key, attempt)
    }
}

/// The whole path on a real `SQLite` outbox: the delete transaction enqueues
/// the purge, the pipeline delivers it, and the handler removes the key.
#[tokio::test]
async fn delete_transaction_enqueues_a_purge_that_the_pipeline_delivers() {
    let h = Harness::start(FakePlugin::new()).await;
    let tenant = Uuid::new_v4();
    let (_new, key, _) = h.create(tenant).await;

    h.repo
        .delete_by_id(&AccessScope::for_tenant(tenant), &key, None)
        .await
        .expect("delete");

    let plugin = h.plugin.clone();
    h.wait_for("the purge", |p| {
        !p.holds_key(&key) && !p.delete_key_calls().is_empty()
    })
    .await;
    assert_eq!(plugin.delete_key_calls().len(), 1);
}

/// A rotation's commit transaction enqueues `destroy(Below(new))`; the
/// pipeline delivers it and the handler removes the superseded version.
#[tokio::test]
async fn rotation_commit_enqueues_a_destroy_that_the_pipeline_delivers() {
    let h = Harness::start(FakePlugin::new()).await;
    let tenant = Uuid::new_v4();
    let (new, key, _) = h.create(tenant).await;

    // The rotating writer: announce, put (version 2), commit.
    let attempt = WriteAttempt {
        attempt_id: Uuid::new_v4(),
        key: key.clone(),
        destroy_supported: true,
    };
    h.repo
        .begin_write_intent(&attempt, Duration::from_mins(5))
        .await
        .expect("begin");
    let v2 = h.plugin.seed(&key, b"v2");
    let outcome = h
        .repo
        .switch_value(
            &AccessScope::for_tenant(tenant),
            new.id,
            1,
            SharingMode::Tenant,
            Fallback::Inherit,
            None,
            v2,
            &attempt,
        )
        .await
        .expect("switch_value");
    assert!(matches!(outcome, IntentCommit::Committed { .. }));

    h.wait_for("destroy(below)", |p| p.versions(&key) == vec!["2"])
        .await;
}

/// A create that loses the reference race purges its fresh key through the
/// outbox, atomically with the intent deletion.
#[tokio::test]
async fn lost_create_enqueues_a_purge_that_the_pipeline_delivers() {
    let h = Harness::start(FakePlugin::new()).await;
    let tenant = Uuid::new_v4();
    let (first, _, _) = h.create(tenant).await;

    // Same reference, new record id: loses the unique index.
    let loser = NewSecret {
        id: Uuid::new_v4(),
        value_version: ValueVersion::new("1"),
        ..first
    };
    let loser_key = StoreKey::new(TenantId(tenant), loser.id);
    h.plugin.seed(&loser_key, b"loser");
    let attempt = WriteAttempt {
        attempt_id: Uuid::new_v4(),
        key: loser_key.clone(),
        destroy_supported: true,
    };
    h.repo
        .begin_write_intent(&attempt, Duration::from_mins(5))
        .await
        .expect("begin");
    let outcome = h
        .repo
        .insert_active(&AccessScope::for_tenant(tenant), &loser, &attempt)
        .await
        .expect("insert");
    assert!(matches!(outcome, IntentCommit::Lost { .. }), "{outcome:?}");

    h.wait_for("the purge of the loser's key", |p| !p.holds_key(&loser_key))
        .await;
}
