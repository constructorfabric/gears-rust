use crate::sequence::Sequence;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use toolkit_gts::GTS_ID_PREFIX;

use gts::GtsInstanceId;
use sea_orm::{ConnectionTrait, Database, Statement};
use uuid::Uuid;

use super::{
    CommitOffsetInTx, Fallback, LOCAL_DB_OFFSET_STORE_MIGRATION_SQL, LocalDbOffsetManager,
    OffsetStore, Position,
};
use crate::ids::ConsumerGroupId;

static DB_SEQ: AtomicU64 = AtomicU64::new(1);

/// One `PRAGMA table_info` row: cid, name, declared type, notnull, default and key
/// position.
type ColumnInfo = (i32, String, String, i32, Option<String>, i32);

/// One fixed group id per name, so groups the tests tell apart stay distinct.
fn group(name: &str) -> ConsumerGroupId {
    let n = match name {
        "billing" => 1,
        "a" => 2,
        "b" => 3,
        other => panic!("no fixed group id for {other:?}"),
    };
    ConsumerGroupId::new(Uuid::from_u128(n))
}

fn topic(name: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(&format!(
        "{GTS_ID_PREFIX}cf.test.events.topic.v1~cf.test.events.{name}.v1"
    ))
    .expect("valid GTS instance id")
}

async fn db_with_offsets_table() -> (sea_orm::DatabaseConnection, toolkit_db::Db) {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dsn = format!("sqlite:file:evbk_offsets_{seq}?mode=memory&cache=shared");
    let raw = Database::connect(&dsn).await.expect("raw sqlite connect");
    raw.execute_raw(Statement::from_string(
        raw.get_database_backend(),
        LOCAL_DB_OFFSET_STORE_MIGRATION_SQL.to_owned(),
    ))
    .await
    .expect("create offset table");

    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("toolkit db connect");

    (raw, db)
}

#[tokio::test]
async fn local_db_load_position_returns_fallback_when_no_row_exists() {
    let (_raw, db) = db_with_offsets_table().await;
    let manager = LocalDbOffsetManager::new(db, Fallback::Latest);

    let pos = manager
        .load_position(&group("billing"), &topic("orders"), 0)
        .await
        .expect("load position");

    assert_eq!(pos, Position::Latest);
}

#[tokio::test]
async fn local_db_load_position_uses_override_before_fallback() {
    let (_raw, db) = db_with_offsets_table().await;
    let orders = topic("orders");
    let manager = LocalDbOffsetManager::new(db, Fallback::Earliest)
        .with_overrides([((orders.clone(), 2), Sequence::assigned(41))]);

    let pos = manager
        .load_position(&group("billing"), &orders, 2)
        .await
        .expect("load position");

    assert_eq!(pos, Position::Exact(Sequence::assigned(41)));
}

#[tokio::test]
async fn local_db_commit_in_tx_upserts_and_load_reads_committed_row() {
    let (_raw, db) = db_with_offsets_table().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest));
    let group = group("billing");
    let topic = topic("orders");
    let tx_topic = topic.clone();
    let tx_manager = manager.clone();

    db.transaction_ref(move |tx| {
        Box::pin(async move {
            tx_manager
                .commit_in_tx(tx, &group, &tx_topic, 3, Sequence::assigned(99))
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            Ok(())
        })
    })
    .await
    .expect("commit transaction");

    let pos = manager
        .load_position(&group, &topic, 3)
        .await
        .expect("load committed position");

    assert_eq!(pos, Position::Exact(Sequence::assigned(99)));
}

#[tokio::test]
async fn local_db_commit_in_tx_rollback_does_not_advance_position() {
    let (_raw, db) = db_with_offsets_table().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest));
    let group = group("billing");
    let topic = topic("payments");
    let tx_topic = topic.clone();
    let tx_manager = manager.clone();

    let result: Result<(), toolkit_db::DbError> = db
        .transaction_ref(move |tx| {
            Box::pin(async move {
                tx_manager
                    .commit_in_tx(tx, &group, &tx_topic, 0, Sequence::assigned(12))
                    .await
                    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
                Err(toolkit_db::DbError::InvalidConfig(
                    "force rollback".to_owned(),
                ))
            })
        })
        .await;

    assert!(result.is_err());
    let pos = manager
        .load_position(&group, &topic, 0)
        .await
        .expect("load position after rollback");

    assert_eq!(pos, Position::Earliest);
}

#[tokio::test]
async fn local_db_key_isolates_groups_topics_and_partitions() {
    let (_raw, db) = db_with_offsets_table().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Latest));
    let group_a = group("a");
    let group_b = group("b");
    let topic_a = topic("orders");
    let topic_b = topic("orders_archive");
    let (tx_topic_a, tx_topic_b) = (topic_a.clone(), topic_b.clone());
    let tx_manager = manager.clone();

    db.transaction_ref(move |tx| {
        Box::pin(async move {
            tx_manager
                .commit_in_tx(tx, &group_a, &tx_topic_a, 0, Sequence::assigned(10))
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            tx_manager
                .commit_in_tx(tx, &group_a, &tx_topic_a, 1, Sequence::assigned(11))
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            tx_manager
                .commit_in_tx(tx, &group_b, &tx_topic_a, 0, Sequence::assigned(20))
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            tx_manager
                .commit_in_tx(tx, &group_a, &tx_topic_b, 0, Sequence::assigned(30))
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            Ok(())
        })
    })
    .await
    .expect("commit offsets");

    assert_eq!(
        manager.load_position(&group_a, &topic_a, 0).await.unwrap(),
        Position::Exact(Sequence::assigned(10))
    );
    assert_eq!(
        manager.load_position(&group_a, &topic_a, 1).await.unwrap(),
        Position::Exact(Sequence::assigned(11))
    );
    assert_eq!(
        manager.load_position(&group_b, &topic_a, 0).await.unwrap(),
        Position::Exact(Sequence::assigned(20))
    );
    assert_eq!(
        manager.load_position(&group_a, &topic_b, 0).await.unwrap(),
        Position::Exact(Sequence::assigned(30))
    );
}

#[tokio::test]
async fn local_db_load_position_preserves_db_failure_source() {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dsn = format!("sqlite:file:evbk_offsets_missing_{seq}?mode=memory&cache=shared");
    let _raw = Database::connect(&dsn).await.expect("raw sqlite connect");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("toolkit db connect");

    let manager = LocalDbOffsetManager::new(db, Fallback::Earliest);
    let err = manager
        .load_position(&ConsumerGroupId::new(Uuid::new_v4()), &topic("missing"), 0)
        .await
        .expect_err("missing table should fail");

    assert!(err.source().is_some(), "DB error source must be preserved");
}

/// A committed offset is stored under the topic's GTS id as text, so the row names
/// the topic itself; another topic's position stays at the fallback.
#[tokio::test]
async fn an_offset_is_stored_under_its_topics_gts_id() {
    let (raw, db) = db_with_offsets_table().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Latest));
    let group = group("billing");
    let orders = topic("orders");
    let tx_orders = orders.clone();
    let tx_manager = manager.clone();

    let before = chrono::Utc::now();
    db.transaction_ref(move |tx| {
        Box::pin(async move {
            tx_manager
                .commit_in_tx(tx, &group, &tx_orders, 0, Sequence::assigned(42))
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            Ok(())
        })
    })
    .await
    .expect("commit transaction");
    let after = chrono::Utc::now();

    let rows = raw
        .query_all_raw(Statement::from_string(
            raw.get_database_backend(),
            "SELECT consumer_group_id, topic, partition, offset, updated_at \
             FROM evbk_consumer_offsets"
                .to_owned(),
        ))
        .await
        .expect("read offset rows");
    let [row] = rows.as_slice() else {
        panic!("expected exactly one offset row, got {}", rows.len());
    };
    let stored: (Uuid, String, i32, i64) = (
        row.try_get("", "consumer_group_id")
            .expect("consumer_group_id"),
        row.try_get("", "topic").expect("topic"),
        row.try_get("", "partition").expect("partition"),
        row.try_get("", "offset").expect("offset"),
    );
    assert_eq!(
        stored,
        (
            group.as_uuid(),
            "gts.cf.test.events.topic.v1~cf.test.events.orders.v1".to_owned(),
            0,
            42
        )
    );
    let updated_at: chrono::DateTime<chrono::Utc> =
        row.try_get("", "updated_at").expect("updated_at");
    assert!(
        before <= updated_at && updated_at <= after,
        "updated_at {updated_at} outside the commit window [{before}, {after}]"
    );

    assert_eq!(
        manager.load_position(&group, &orders, 0).await.unwrap(),
        Position::Exact(Sequence::assigned(42))
    );
    assert_eq!(
        manager
            .load_position(&group, &topic("payments"), 0)
            .await
            .unwrap(),
        Position::Latest
    );
}

/// The offsets table's key is the consumer group, the topic's GTS id in a column sized
/// for one, and the partition, in that order.
#[tokio::test]
async fn the_offsets_table_keys_topics_by_gts_id() {
    let (raw, _db) = db_with_offsets_table().await;

    let rows = raw
        .query_all_raw(Statement::from_string(
            raw.get_database_backend(),
            "PRAGMA table_info(evbk_consumer_offsets)".to_owned(),
        ))
        .await
        .expect("read table info");
    let columns: Vec<ColumnInfo> = rows
        .iter()
        .map(|row| {
            (
                row.try_get("", "cid").expect("cid"),
                row.try_get("", "name").expect("name"),
                row.try_get("", "type").expect("type"),
                row.try_get("", "notnull").expect("notnull"),
                row.try_get("", "dflt_value").expect("dflt_value"),
                row.try_get("", "pk").expect("pk"),
            )
        })
        .collect();

    assert_eq!(
        columns,
        vec![
            (
                0,
                "consumer_group_id".to_owned(),
                "UUID".to_owned(),
                1,
                None,
                1
            ),
            (
                1,
                "topic".to_owned(),
                "VARCHAR(1024)".to_owned(),
                1,
                None,
                2
            ),
            (2, "partition".to_owned(), "INTEGER".to_owned(), 1, None, 3),
            (3, "offset".to_owned(), "BIGINT".to_owned(), 1, None, 0),
            (
                4,
                "updated_at".to_owned(),
                "TIMESTAMP".to_owned(),
                1,
                None,
                0
            ),
        ]
    );
}
