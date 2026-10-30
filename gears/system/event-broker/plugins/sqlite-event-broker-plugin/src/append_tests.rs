use std::time::Duration;

use event_broker_sdk::backend::{Head, Partition, TopicPartition};
use event_broker_sdk::{GtsInstanceId, Sequence};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use crate::append::finish;
use crate::hub;

/// A commit issued and then abandoned by its caller still lands, and its head
/// still reaches the hub.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_outlives_its_caller_and_publishes_its_heads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.db");
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TABLE t (n INTEGER NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    let hub = hub::of(&path).unwrap();
    let at = TopicPartition::new(
        GtsInstanceId::try_new(&format!(
            "{}cf.sqlite_test.finish.t1.v1",
            toolkit_gts::gts_id!("cf.core.events.topic.v1~")
        ))
        .unwrap(),
        Partition::new(0),
    );

    let writer = std::sync::Arc::clone(&hub.writer).lock_owned().await;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query("INSERT INTO t (n) VALUES (7)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let heads = vec![Head {
        at: at.clone(),
        sequence: Sequence::assigned(7),
    }];
    drop(finish(tx, writer, std::sync::Arc::clone(&hub), heads));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while hub.heads.known(&at).is_none() {
        assert!(tokio::time::Instant::now() < deadline, "no head within 5s");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let stored: Vec<i64> = sqlx::query_scalar("SELECT n FROM t")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        (hub.heads.known(&at), stored, hub.writer.try_lock().is_ok()),
        (Some(Sequence::assigned(7)), vec![7], true)
    );
}
