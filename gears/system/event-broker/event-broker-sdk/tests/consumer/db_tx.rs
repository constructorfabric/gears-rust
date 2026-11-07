use event_broker_sdk::Sequence;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::consumer::LOCAL_DB_OFFSET_STORE_MIGRATION_SQL;
use event_broker_sdk::dlq::DeadLetterRecord;
use event_broker_sdk::gts_id;
use event_broker_sdk::{
    CommitOffsetInTx, ConsumerBatching, ConsumerBuilder, ConsumerError, ConsumerGroupRef, Delivery,
    EventBatch, EventBrokerError, EventV1, Fallback, HandlerOutcome, LocalDbOffsetManager,
    OffsetStore, Position, StoredPosition, TxBatchHandler, TxCommitHandle, TxEventHandler,
};
use sea_orm::{ConnectionTrait, Database, EntityTrait, PaginatorTrait, Set, Statement};
use toolkit_db::secure::{AccessScope, SecureInsertExt};
use uuid::Uuid;

use super::common::{publish_json, topic, topic_fixture, wait_until};

const TX_SINGLE_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.txsingle.v1");
const TX_SINGLE_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.txsingle.v1~");
const TX_BATCH_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.txbatch.v1");
const TX_BATCH_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.txbatch.v1~");

mod dlq_row {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "showcase_dead_letters")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub event_id: Uuid,
        pub topic: String,
        pub event_type: String,
        pub partition: i32,
        pub offset: i64,
        pub reason: String,
        pub payload: String,
        pub occurred_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl toolkit_db::secure::ScopableEntity for Entity {
        const IS_UNRESTRICTED: bool = true;

        fn tenant_col() -> Option<Self::Column> {
            None
        }

        fn resource_col() -> Option<Self::Column> {
            None
        }

        fn owner_col() -> Option<Self::Column> {
            None
        }

        fn type_col() -> Option<Self::Column> {
            None
        }

        fn resolve_property(_property: &str) -> Option<Self::Column> {
            None
        }
        fn scope_columns() -> Vec<Self::Column> {
            Vec::new()
        }
    }
}

struct TxSingleProjector {
    db: toolkit_db::Db,
    committed_offsets: Arc<Mutex<Vec<i64>>>,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, LocalDbOffsetManager> for TxSingleProjector {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
        commit: TxCommitHandle<LocalDbOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let db = self.db.clone();
        let offset = event.sequence().expect("a delivered event is stamped");
        db.transaction_ref(move |tx| {
            Box::pin(async move {
                commit
                    .commit_offset_in_tx(tx, offset)
                    .await
                    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
                Ok(())
            })
        })
        .await
        .map_err(|err| EventBrokerError::Internal(err.to_string()))?;

        self.committed_offsets.lock().unwrap().push(offset.as_i64());
        Ok(HandlerOutcome::Success)
    }
}

struct TxBatchProjector {
    db: toolkit_db::Db,
    committed_offsets: Arc<Mutex<Vec<i64>>>,
}

#[async_trait]
impl TxBatchHandler<serde_json::Value, LocalDbOffsetManager> for TxBatchProjector {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
        commit: TxCommitHandle<LocalDbOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let target_offset = batch
            .iter()
            .last()
            .ok_or_else(|| EventBrokerError::Internal("empty transactional batch".to_owned()))?
            .sequence()
            .expect("a delivered event is stamped");
        let db = self.db.clone();

        db.transaction_ref(move |tx| {
            Box::pin(async move {
                commit
                    .commit_offset_in_tx(tx, target_offset)
                    .await
                    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
                Ok(())
            })
        })
        .await
        .map_err(|err| EventBrokerError::Internal(err.to_string()))?;

        self.committed_offsets
            .lock()
            .unwrap()
            .push(target_offset.as_i64());
        Ok(HandlerOutcome::Success)
    }
}

static DB_SEQ: AtomicU64 = AtomicU64::new(1);

async fn db_with_showcase_tables() -> (sea_orm::DatabaseConnection, toolkit_db::Db) {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dsn = format!("sqlite:file:evbk_showcase_db_tx_{seq}?mode=memory&cache=shared");
    let raw = Database::connect(&dsn).await.expect("raw sqlite connect");
    let backend = raw.get_database_backend();
    raw.execute_raw(Statement::from_string(
        backend,
        LOCAL_DB_OFFSET_STORE_MIGRATION_SQL.to_owned(),
    ))
    .await
    .expect("offset table");
    raw.execute_raw(Statement::from_string(
        backend,
        r#"
CREATE TABLE IF NOT EXISTS showcase_dead_letters (
    event_id UUID NOT NULL PRIMARY KEY,
    topic TEXT NOT NULL,
    event_type TEXT NOT NULL,
    partition INTEGER NOT NULL,
    offset BIGINT NOT NULL,
    reason TEXT NOT NULL,
    payload TEXT NOT NULL,
    occurred_at TIMESTAMP NOT NULL
);
"#
        .to_owned(),
    ))
    .await
    .expect("dead-letter table");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("toolkit db");

    (raw, db)
}

/// An event as a consumer is handed it, with the delivery it came on: partition 0,
/// sequence 42.
fn raw_event_for_dead_letter() -> (event_broker_sdk::EventV1, Delivery) {
    let event = event_broker_sdk::EventV1::builder_for(
        event_broker_sdk::GtsTypeId::new("gts.cf.core.events.event.v1~example.showcase.db.dlq.v1~"),
        serde_json::json!({ "reject": true }),
    )
    .id(Uuid::from_u128(0xd1e_0042))
    .tenant_id(Uuid::nil())
    .subject("db-dlq-1")
    .subject_type_id(event_broker_sdk::GtsTypeId::new(
        "gts.x.eb.test.subject.v1~",
    ))
    .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
    .build()
    .unwrap()
    .stamp_position(StoredPosition {
        partition: 0,
        sequence: Sequence::assigned(42),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    });
    let delivery = Delivery::new(
        event_broker_sdk::GtsInstanceId::try_new(
            "gts.cf.core.events.topic.v1~example.showcase.db.dlq.v1",
        )
        .unwrap(),
        0,
        1,
    );
    (event, delivery)
}

async fn insert_dead_letter<TX>(
    tx: &TX,
    record: &DeadLetterRecord,
) -> Result<(), toolkit_db::DbError>
where
    TX: toolkit_db::secure::DBRunner + Sync,
{
    dlq_row::Entity::insert(dlq_row::ActiveModel {
        event_id: Set(record.event_id),
        topic: Set(record.topic.as_ref().to_owned()),
        event_type: Set(record.event_type.as_ref().to_owned()),
        partition: Set(record.partition as i32),
        offset: Set(record.offset.as_i64()),
        reason: Set(record.reason.clone()),
        payload: Set(record.payload.to_string()),
        occurred_at: Set(record.occurred_at),
    })
    .secure()
    .scope_unchecked(&AccessScope::allow_all())
    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?
    .exec(tx)
    .await
    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
    Ok(())
}

async fn dead_letter_count(conn: &sea_orm::DatabaseConnection) -> u64 {
    dlq_row::Entity::find()
        .count(conn)
        .await
        .expect("count dead-letter rows")
}

#[tokio::test]
async fn if_i_want_transactional_single_event_handling_i_commit_the_delivered_offset_in_my_tx() {
    let fixture = topic_fixture(TX_SINGLE_TOPIC, TX_SINGLE_EVENT, 1).await;
    let (_raw, db) = db_with_showcase_tables().await;
    let group = fixture
        .broker
        .create_consumer_group(
            &fixture.ctx,
            event_broker_sdk::CreateConsumerGroupRequest {
                client_agent: "db-tx-single".to_owned(),
                description: None,
            },
        )
        .await
        .expect("consumer group created")
        .id;
    let topic_id = event_broker_sdk::TopicId::from_gts(TX_SINGLE_TOPIC);
    let committed_offsets = Arc::new(Mutex::new(Vec::new()));
    let assertion_manager = LocalDbOffsetManager::new(db.clone(), Fallback::Earliest);

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::existing(group))
        .topics([topic(TX_SINGLE_TOPIC)])
        .offset_manager(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest))
        .handler(TxSingleProjector {
            db: db.clone(),
            committed_offsets: committed_offsets.clone(),
        })
        .start()
        .await
        .expect("transactional single consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        TX_SINGLE_EVENT,
        "tx-single-1",
        None,
        serde_json::json!({ "tx": "single" }),
    )
    .await;

    wait_until(|| committed_offsets.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");

    let committed = committed_offsets.lock().unwrap()[0];
    assert_eq!(
        assertion_manager
            .load_position(&group, &topic_id, 0)
            .await
            .unwrap(),
        Position::Exact(Sequence::assigned(committed))
    );
}

#[tokio::test]
async fn if_i_want_transactional_batch_handling_i_commit_the_last_handled_offset_in_my_tx() {
    let fixture = topic_fixture(TX_BATCH_TOPIC, TX_BATCH_EVENT, 1).await;
    let (_raw, db) = db_with_showcase_tables().await;
    let group = fixture
        .broker
        .create_consumer_group(
            &fixture.ctx,
            event_broker_sdk::CreateConsumerGroupRequest {
                client_agent: "db-tx-batch".to_owned(),
                description: None,
            },
        )
        .await
        .expect("consumer group created")
        .id;
    let topic_id = event_broker_sdk::TopicId::from_gts(TX_BATCH_TOPIC);
    let committed_offsets = Arc::new(Mutex::new(Vec::new()));
    let assertion_manager = LocalDbOffsetManager::new(db.clone(), Fallback::Earliest);

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::existing(group))
        .topics([topic(TX_BATCH_TOPIC)])
        .batching(ConsumerBatching {
            max_events: 8,
            max_wait: Duration::from_millis(20),
        })
        .offset_manager(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest))
        .batch_handler(TxBatchProjector {
            db: db.clone(),
            committed_offsets: committed_offsets.clone(),
        })
        .start()
        .await
        .expect("transactional batch consumer starts");

    for subject in ["tx-batch-1", "tx-batch-2"] {
        publish_json(
            &fixture.broker,
            &fixture.ctx,
            TX_BATCH_EVENT,
            subject,
            Some(0),
            serde_json::json!({ "tx": "batch", "subject": subject }),
        )
        .await;
    }

    wait_until(|| !committed_offsets.lock().unwrap().is_empty()).await;
    handle.stop().await.expect("consumer stops");

    let committed = *committed_offsets.lock().unwrap().last().unwrap();
    assert_eq!(
        assertion_manager
            .load_position(&group, &topic_id, 0)
            .await
            .unwrap(),
        Position::Exact(Sequence::assigned(committed))
    );
}

#[tokio::test]
async fn if_i_want_db_transactional_progress_i_write_business_rows_and_offset_together() {
    let (_raw, db) = db_with_showcase_tables().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest));
    let group = event_broker_sdk::ConsumerGroupId::from_gts("showcase-db-tx");
    let topic_id = event_broker_sdk::TopicId::from_gts("showcase-db-tx-topic");
    let tx_manager = manager.clone();

    db.transaction_ref(move |tx| {
        Box::pin(async move {
            // Application business writes should use secure repositories with
            // this same `tx`; the offset save joins that transaction.
            tx_manager
                .commit_in_tx(tx, &group, &topic_id, 0, Sequence::assigned(41))
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            Ok(())
        })
    })
    .await
    .expect("transaction commits");

    assert_eq!(
        manager.load_position(&group, &topic_id, 0).await.unwrap(),
        Position::Exact(Sequence::assigned(41))
    );
}

#[tokio::test]
async fn if_the_business_transaction_rolls_back_the_offset_rolls_back_too() {
    let (_raw, db) = db_with_showcase_tables().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest));
    let group = event_broker_sdk::ConsumerGroupId::from_gts("showcase-db-tx-rollback");
    let topic_id = event_broker_sdk::TopicId::from_gts("showcase-db-tx-rollback-topic");
    let tx_manager = manager.clone();

    let result: Result<(), toolkit_db::DbError> = db
        .transaction_ref(move |tx| {
            Box::pin(async move {
                tx_manager
                    .commit_in_tx(tx, &group, &topic_id, 0, Sequence::assigned(77))
                    .await
                    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
                Err(toolkit_db::DbError::InvalidConfig(
                    "force rollback".to_owned(),
                ))
            })
        })
        .await;

    assert!(result.is_err());
    assert_eq!(
        manager.load_position(&group, &topic_id, 0).await.unwrap(),
        Position::Earliest
    );
}

#[tokio::test]
async fn if_i_want_transactional_dlq_i_write_the_record_and_offset_in_one_transaction() {
    let (raw, db) = db_with_showcase_tables().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest));
    let group = event_broker_sdk::ConsumerGroupId::from_gts("showcase-db-dlq");
    let topic_id = event_broker_sdk::TopicId::from_gts("showcase-db-dlq-topic");
    let tx_manager = manager.clone();
    let (event, delivery) = raw_event_for_dead_letter();
    let record = DeadLetterRecord::builder(&event, &delivery, "permanent validation failure")
        .group_id(group)
        .attempts(6)
        .build();
    let partition = delivery.partition();
    let offset = event.sequence().expect("a delivered event is stamped");

    db.transaction_ref(move |tx| {
        Box::pin(async move {
            insert_dead_letter(tx, &record).await?;
            tx_manager
                .commit_in_tx(tx, &group, &topic_id, partition, offset)
                .await
                .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
            Ok(())
        })
    })
    .await
    .expect("transaction commits");

    assert_eq!(dead_letter_count(&raw).await, 1);
    assert_eq!(
        manager
            .load_position(&group, &topic_id, delivery.partition())
            .await
            .unwrap(),
        Position::Exact(event.sequence().expect("a delivered event is stamped"))
    );
}

#[tokio::test]
async fn if_the_transactional_dlq_rolls_back_neither_parking_nor_offset_is_durable() {
    let (raw, db) = db_with_showcase_tables().await;
    let manager = Arc::new(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest));
    let group = event_broker_sdk::ConsumerGroupId::from_gts("showcase-db-dlq-rollback");
    let topic_id = event_broker_sdk::TopicId::from_gts("showcase-db-dlq-rollback-topic");
    let tx_manager = manager.clone();
    let (event, delivery) = raw_event_for_dead_letter();
    let record = DeadLetterRecord::builder(&event, &delivery, "permanent validation failure")
        .group_id(group)
        .attempts(6)
        .build();
    let partition = delivery.partition();
    let offset = event.sequence().expect("a delivered event is stamped");

    let result: Result<(), toolkit_db::DbError> = db
        .transaction_ref(move |tx| {
            Box::pin(async move {
                insert_dead_letter(tx, &record).await?;
                tx_manager
                    .commit_in_tx(tx, &group, &topic_id, partition, offset)
                    .await
                    .map_err(|err| toolkit_db::DbError::InvalidConfig(err.to_string()))?;
                Err(toolkit_db::DbError::InvalidConfig(
                    "force rollback".to_owned(),
                ))
            })
        })
        .await;

    assert!(result.is_err());
    assert_eq!(dead_letter_count(&raw).await, 0);
    assert_eq!(
        manager
            .load_position(&group, &topic_id, delivery.partition())
            .await
            .unwrap(),
        Position::Earliest
    );
}
