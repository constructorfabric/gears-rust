//! Integration tests for the transactional outbox module.

#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::similar_names,
    clippy::drop_non_drop
)]

use std::time::Duration;

use std::sync::atomic::{AtomicU32, Ordering};

use modkit_db::outbox::{
    ClaimCfg, OutboxDispatcher, OutboxMessage, OutboxStatus, OutboxStore, RetryCfg, enqueue,
    setup_outbox_table,
};
use modkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn setup() -> modkit_db::Db {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect");
    setup_outbox_table(&db).await.expect("migration");
    db
}

fn test_retry_cfg() -> RetryCfg {
    RetryCfg {
        max_attempts: 3,
        base_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(10),
    }
}

fn test_claim_cfg() -> ClaimCfg {
    ClaimCfg {
        batch_size: 10,
        lease_duration: Duration::from_secs(60),
    }
}

// ---------------------------------------------------------------------------
// enqueue tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn enqueue_inserts_row() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    let id = enqueue(
        &conn,
        OutboxMessage {
            namespace: "test-ns",
            topic: "test-topic",
            tenant_id: None,
            dedupe_key: None,
            payload: json!({"hello": "world"}),
        },
    )
    .await
    .expect("enqueue");

    // Verify UUID is valid (non-nil).
    assert_ne!(id, Uuid::nil());
}

#[tokio::test]
async fn enqueue_inside_transaction() {
    let db = setup().await;

    let id = db
        .transaction_ref(|tx| {
            Box::pin(async move {
                enqueue(
                    tx,
                    OutboxMessage {
                        namespace: "test-ns",
                        topic: "tx-topic",
                        tenant_id: None,
                        dedupe_key: None,
                        payload: json!({"tx": true}),
                    },
                )
                .await
            })
        })
        .await
        .expect("tx enqueue");

    assert_ne!(id, Uuid::nil());
}

#[tokio::test]
async fn enqueue_dedupe_idempotent() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    let msg = || OutboxMessage {
        namespace: "test-ns",
        topic: "test-topic",
        tenant_id: None,
        dedupe_key: Some("unique-key-1".into()),
        payload: json!({"attempt": 1}),
    };

    let id1 = enqueue(&conn, msg()).await.expect("first enqueue");
    // Second enqueue with same dedupe key should return same ID.
    let id2 = enqueue(&conn, msg()).await.expect("second enqueue");

    assert_eq!(id1, id2, "idempotent enqueue must return same ID");
}

#[tokio::test]
async fn enqueue_with_tenant_and_arbitrary_dedupe_key() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    // dedupe_key is opaque — any format is accepted alongside tenant_id.
    let id = enqueue(
        &conn,
        OutboxMessage {
            namespace: "test-ns",
            topic: "test-topic",
            tenant_id: Some(Uuid::new_v4()),
            dedupe_key: Some("arbitrary-key-no-tenant-prefix".into()),
            payload: json!({"ok": true}),
        },
    )
    .await
    .expect("enqueue with arbitrary dedupe_key");

    assert_ne!(id, Uuid::nil());
}

#[tokio::test]
async fn enqueue_allows_tenant_without_dedupe_key() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    // tenant_id present, dedupe_key absent — no validation needed.
    let id = enqueue(
        &conn,
        OutboxMessage {
            namespace: "test-ns",
            topic: "test-topic",
            tenant_id: Some(Uuid::new_v4()),
            dedupe_key: None,
            payload: json!({"tenant_only": true}),
        },
    )
    .await
    .expect("enqueue with tenant but no dedupe_key");

    assert_ne!(id, Uuid::nil());
}

#[tokio::test]
async fn enqueue_allows_dedupe_key_without_tenant() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    // dedupe_key present, tenant_id absent — no validation needed.
    let id = enqueue(
        &conn,
        OutboxMessage {
            namespace: "test-ns",
            topic: "test-topic",
            tenant_id: None,
            dedupe_key: Some("system-event-key".into()),
            payload: json!({"system": true}),
        },
    )
    .await
    .expect("enqueue with dedupe_key but no tenant");

    assert_ne!(id, Uuid::nil());
}

// ---------------------------------------------------------------------------
// OutboxStore claim / ack / nack tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_batch_returns_pending_rows() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    // Enqueue two rows.
    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("a".into()),
            payload: json!(1),
        },
    )
    .await
    .expect("e1");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("b".into()),
            payload: json!(2),
        },
    )
    .await
    .expect("e2");

    drop(conn);

    let provider: DBProvider<DbError> = DBProvider::new(db);
    let store = OutboxStore::new(provider, Uuid::new_v4(), "ns".into(), test_retry_cfg());

    let batch = store.claim_batch(test_claim_cfg()).await.expect("claim");

    assert_eq!(batch.len(), 2, "should claim both pending rows");
    for msg in &batch {
        assert_eq!(msg.attempts, 1, "first claim should set attempts=1");
        assert_eq!(msg.namespace, "ns");
    }
}

#[tokio::test]
async fn claim_batch_respects_batch_size() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    for i in 0..5 {
        enqueue(
            &conn,
            OutboxMessage {
                namespace: "ns",
                topic: "t",
                tenant_id: None,
                dedupe_key: Some(format!("key-{i}")),
                payload: json!(i),
            },
        )
        .await
        .expect("enqueue");
    }
    drop(conn);

    let provider: DBProvider<DbError> = DBProvider::new(db);
    let store = OutboxStore::new(provider, Uuid::new_v4(), "ns".into(), test_retry_cfg());

    let batch = store
        .claim_batch(ClaimCfg {
            batch_size: 2,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("claim");

    assert_eq!(batch.len(), 2, "should respect batch_size limit");
}

#[tokio::test]
async fn ack_marks_delivered() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("ack-test".into()),
            payload: json!("data"),
        },
    )
    .await
    .expect("enqueue");
    drop(conn);

    let provider: DBProvider<DbError> = DBProvider::new(db);
    let store = OutboxStore::new(provider, Uuid::new_v4(), "ns".into(), test_retry_cfg());

    let batch = store.claim_batch(test_claim_cfg()).await.expect("claim");

    assert_eq!(batch.len(), 1);
    store.ack(batch[0].id).await.expect("ack");

    // A second claim should return nothing (row is delivered).
    let batch2 = store.claim_batch(test_claim_cfg()).await.expect("claim2");

    assert!(batch2.is_empty(), "delivered row must not be reclaimed");
}

#[tokio::test]
async fn nack_reschedules_for_retry() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("nack-test".into()),
            payload: json!("data"),
        },
    )
    .await
    .expect("enqueue");
    drop(conn);

    let retry_cfg = RetryCfg {
        max_attempts: 5,
        base_delay: Duration::from_millis(1), // tiny for test speed
        max_delay: Duration::from_millis(10),
    };
    let provider: DBProvider<DbError> = DBProvider::new(db);
    let store = OutboxStore::new(provider, Uuid::new_v4(), "ns".into(), retry_cfg);

    // Claim and nack.
    let batch = store
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("claim");
    assert_eq!(batch.len(), 1);

    store
        .nack(batch[0].id, "transient error")
        .await
        .expect("nack");

    // After nack, the row is pending again with next_attempt_at in the near future.
    // A tiny sleep ensures next_attempt_at <= now() for the re-claim.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let batch2 = store
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("reclaim");

    assert_eq!(batch2.len(), 1, "nacked row should be reclaimable");
    assert_eq!(
        batch2[0].attempts, 2,
        "attempts should increment on reclaim"
    );
}

#[tokio::test]
async fn nack_dead_letters_after_max_attempts() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("dead-test".into()),
            payload: json!("data"),
        },
    )
    .await
    .expect("enqueue");
    drop(conn);

    let provider: DBProvider<DbError> = DBProvider::new(db);
    let store = OutboxStore::new(
        provider,
        Uuid::new_v4(),
        "ns".into(),
        RetryCfg {
            max_attempts: 1, // dead after first attempt
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        },
    );

    // Claim (attempts goes to 1, which equals max_attempts).
    let batch = store
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("claim");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].attempts, 1);

    // Nack should dead-letter since attempts (1) >= max_attempts (1).
    store.nack(batch[0].id, "permanent").await.expect("nack");

    // Row should NOT be reclaimable.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let batch2 = store
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("claim2");

    assert!(batch2.is_empty(), "dead-lettered row must not be reclaimed");
}

#[tokio::test]
async fn claim_ignores_other_namespace() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "other-ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: None,
            payload: json!("ignored"),
        },
    )
    .await
    .expect("enqueue");
    drop(conn);

    let provider: DBProvider<DbError> = DBProvider::new(db);
    let store = OutboxStore::new(provider, Uuid::new_v4(), "my-ns".into(), test_retry_cfg());

    let batch = store.claim_batch(test_claim_cfg()).await.expect("claim");

    assert!(
        batch.is_empty(),
        "should not claim rows from other namespace"
    );
}

#[tokio::test]
async fn lease_expiry_allows_reclaim_by_another_worker() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("lease-test".into()),
            payload: json!("data"),
        },
    )
    .await
    .expect("enqueue");
    drop(conn);

    // Worker A claims with a very short lease.
    // SQLite timestamps now use millisecond precision (strftime %f), so a
    // 1s lease + 1.5s sleep reliably exceeds the expiry.
    let worker_a_id = Uuid::new_v4();
    let provider_a: DBProvider<DbError> = DBProvider::new(db.clone());
    let store_a = OutboxStore::new(provider_a, worker_a_id, "ns".into(), test_retry_cfg());

    let batch_a = store_a
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(1),
        })
        .await
        .expect("claim A");
    assert_eq!(batch_a.len(), 1, "worker A should claim the row");
    assert_eq!(batch_a[0].attempts, 1);

    // Wait for the lease to expire.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Worker B should be able to reclaim the expired-lease row.
    let worker_b_id = Uuid::new_v4();
    let provider_b: DBProvider<DbError> = DBProvider::new(db);
    let store_b = OutboxStore::new(provider_b, worker_b_id, "ns".into(), test_retry_cfg());

    let batch_b = store_b
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("claim B");
    assert_eq!(
        batch_b.len(),
        1,
        "worker B should reclaim expired-lease row"
    );
    assert_eq!(
        batch_b[0].attempts, 2,
        "attempts should increment on reclaim"
    );

    // Worker A's ack should now fail (no longer the lease holder).
    let ack_result = store_a.ack(batch_a[0].id).await;
    assert!(
        ack_result.is_err(),
        "worker A ack should fail after lease expiry"
    );
}

// ---------------------------------------------------------------------------
// OutboxStatus tests
// ---------------------------------------------------------------------------

#[test]
fn outbox_status_roundtrip() {
    let statuses = [
        OutboxStatus::Pending,
        OutboxStatus::Processing,
        OutboxStatus::Delivered,
        OutboxStatus::Dead,
    ];

    for status in &statuses {
        let s = status.as_str();
        let parsed: OutboxStatus = s.parse().expect("parse status");
        assert_eq!(*status, parsed, "roundtrip failed for {s}");
    }
}

#[test]
fn outbox_status_display() {
    assert_eq!(OutboxStatus::Pending.to_string(), "pending");
    assert_eq!(OutboxStatus::Processing.to_string(), "processing");
    assert_eq!(OutboxStatus::Delivered.to_string(), "delivered");
    assert_eq!(OutboxStatus::Dead.to_string(), "dead");
}

#[test]
fn outbox_status_from_str_rejects_unknown() {
    let result = "unknown".parse::<OutboxStatus>();
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// OutboxDispatcher tests
// ---------------------------------------------------------------------------

fn make_dispatcher(db: modkit_db::Db) -> OutboxDispatcher<DbError> {
    let provider: DBProvider<DbError> = DBProvider::new(db);
    let store = OutboxStore::new(
        provider,
        Uuid::new_v4(),
        "ns".into(),
        RetryCfg {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        },
    );
    OutboxDispatcher::new(
        store,
        ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        },
        Duration::from_millis(50),
    )
}

#[tokio::test]
async fn dispatcher_delivers_and_acks() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("dispatch-1".into()),
            payload: json!({"v": 1}),
        },
    )
    .await
    .expect("enqueue");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("dispatch-2".into()),
            payload: json!({"v": 2}),
        },
    )
    .await
    .expect("enqueue");
    drop(conn);

    let delivered = std::sync::Arc::new(AtomicU32::new(0));
    let delivered_clone = delivered.clone();

    let dispatcher = make_dispatcher(db.clone());
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = tokio::spawn(async move {
        dispatcher
            .run(cancel_clone, |_msg| {
                let d = delivered_clone.clone();
                async move {
                    d.fetch_add(1, Ordering::Relaxed);
                    Ok::<(), String>(())
                }
            })
            .await;
    });

    // Give the dispatcher time to process.
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    handle.await.expect("dispatcher task");

    assert_eq!(
        delivered.load(Ordering::Relaxed),
        2,
        "should deliver both rows"
    );

    // Verify rows are delivered (no more claimable).
    let provider2: DBProvider<DbError> = DBProvider::new(db);
    let store2 = OutboxStore::new(
        provider2,
        Uuid::new_v4(),
        "ns".into(),
        RetryCfg {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        },
    );
    let remaining = store2
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("claim");

    assert!(remaining.is_empty(), "all rows should be delivered");
}

#[tokio::test]
async fn dispatcher_nacks_on_publish_failure() {
    let db = setup().await;
    let conn = db.conn().expect("conn");

    enqueue(
        &conn,
        OutboxMessage {
            namespace: "ns",
            topic: "t",
            tenant_id: None,
            dedupe_key: Some("fail-publish".into()),
            payload: json!("data"),
        },
    )
    .await
    .expect("enqueue");
    drop(conn);

    // Use high max_attempts so the dispatcher can't dead-letter within the
    // test window (multiple poll cycles may fire during the sleep below).
    let provider: DBProvider<DbError> = DBProvider::new(db.clone());
    let store = OutboxStore::new(
        provider,
        Uuid::new_v4(),
        "ns".into(),
        RetryCfg {
            max_attempts: 100,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        },
    );
    let dispatcher = OutboxDispatcher::new(
        store,
        ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        },
        Duration::from_millis(50),
    );

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = tokio::spawn(async move {
        dispatcher
            .run(cancel_clone, |_msg| async {
                Err::<(), String>("publish failed".into())
            })
            .await;
    });

    // Let the dispatcher run a couple of cycles.
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    handle.await.expect("dispatcher task");

    // The row should still be reclaimable (nacked -> pending).
    tokio::time::sleep(Duration::from_millis(50)).await;
    let provider2: DBProvider<DbError> = DBProvider::new(db);
    let store2 = OutboxStore::new(
        provider2,
        Uuid::new_v4(),
        "ns".into(),
        RetryCfg {
            max_attempts: 100,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(10),
        },
    );
    let batch = store2
        .claim_batch(ClaimCfg {
            batch_size: 10,
            lease_duration: Duration::from_secs(60),
        })
        .await
        .expect("claim");

    assert_eq!(batch.len(), 1, "nacked row should be reclaimable");
    assert!(batch[0].attempts >= 2, "attempts should have incremented");
}

#[tokio::test]
async fn dispatcher_stops_on_cancel() {
    let db = setup().await;
    let dispatcher = make_dispatcher(db);
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = tokio::spawn(async move {
        dispatcher
            .run(cancel_clone, |_msg| async { Ok::<(), String>(()) })
            .await;
    });

    // Cancel immediately.
    cancel.cancel();

    // Should exit promptly.
    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(result.is_ok(), "dispatcher should stop promptly on cancel");
}
