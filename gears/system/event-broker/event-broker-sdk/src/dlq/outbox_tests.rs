use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use toolkit_gts::gts_id;

use toolkit_db::outbox::OutboxError;
use uuid::Uuid;

use crate::consumer::Delivery;
use crate::gts::{EventV1, StoredPosition};
use crate::ids::ConsumerGroupId;
use crate::sequence::Sequence;

use super::{ConsumerDlqOutbox, DeadLetterEnvelope, DeadLetterRecord};

const DLQ_QUEUE: &str = "consumer-dlq";
const DLQ_PARTITIONS: u32 = 4;
const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.orders.x.x.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.orders.rejected.x.v1~");

static DB_SEQ: AtomicU64 = AtomicU64::new(1);

struct NoopProcessor;

#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for NoopProcessor {
    async fn handle(
        &self,
        _msg: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        toolkit_db::outbox::MessageResult::Ok
    }
}

type CapturedPayloads = Arc<Mutex<Vec<Vec<u8>>>>;

/// Records the raw payload of every message delivered, so a test can read back
/// what `enqueue` actually wrote rather than trust the returned id.
struct CapturingProcessor {
    seen: CapturedPayloads,
}

#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for CapturingProcessor {
    async fn handle(
        &self,
        msg: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        self.seen
            .lock()
            .expect("captured payloads lock poisoned")
            .push(msg.payload.clone());
        toolkit_db::outbox::MessageResult::Ok
    }
}

fn raw_event(offset: i64) -> EventV1 {
    EventV1::builder_for(
        gts::GtsTypeId::new(EVENT_TYPE),
        serde_json::json!({ "offset": offset }),
    )
    .id(Uuid::from_u128(u128::try_from(offset).unwrap()))
    .subject_tenant(Uuid::from_u128(0x0f1e_0001))
    .subject(format!("order-{offset}"))
    .subject_type_of(gts::GtsTypeId::new("gts.x.eb.test.subject.v1~"))
    .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
    .build()
    .unwrap()
    .stamp_position(StoredPosition {
        partition: 6,
        sequence: Sequence::assigned(offset),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    })
}

fn dead_letter_record(offset: i64) -> DeadLetterRecord {
    let delivery = Delivery::new(gts::GtsInstanceId::try_new(TOPIC).unwrap(), 6, 1);
    DeadLetterRecord::builder(&raw_event(offset), &delivery, "permanent failure")
        .consumer_group(ConsumerGroupId::new(Uuid::from_u128(1)))
        .attempts(3)
        .build()
}

async fn outbox_handle() -> (toolkit_db::outbox::OutboxHandle, toolkit_db::Db) {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dsn = format!("sqlite:file:evbk_dlq_outbox_{seq}?mode=memory&cache=shared");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("connect toolkit db");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations(),
    )
    .await
    .expect("outbox migrations");

    let handle = toolkit_db::outbox::Outbox::builder(db.clone())
        .queue(
            DLQ_QUEUE,
            toolkit_db::outbox::Partitions::of(DLQ_PARTITIONS as u16),
        )
        .leased(NoopProcessor)
        .start()
        .await
        .expect("outbox starts");
    (handle, db)
}

async fn capturing_outbox_handle() -> (
    toolkit_db::outbox::OutboxHandle,
    toolkit_db::Db,
    CapturedPayloads,
) {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dsn = format!("sqlite:file:evbk_dlq_outbox_cap_{seq}?mode=memory&cache=shared");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("connect toolkit db");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations(),
    )
    .await
    .expect("outbox migrations");

    let seen: CapturedPayloads = Arc::new(Mutex::new(Vec::new()));
    let handle = toolkit_db::outbox::Outbox::builder(db.clone())
        .queue(
            DLQ_QUEUE,
            toolkit_db::outbox::Partitions::of(DLQ_PARTITIONS as u16),
        )
        .leased(CapturingProcessor {
            seen: Arc::clone(&seen),
        })
        .start()
        .await
        .expect("outbox starts");
    (handle, db, seen)
}

#[tokio::test]
async fn consumer_dlq_outbox_builder_keeps_queue_and_partitions() {
    let (handle, _db) = outbox_handle().await;
    let helper = ConsumerDlqOutbox::builder(Arc::clone(handle.outbox()))
        .queue(DLQ_QUEUE)
        .partitions(DLQ_PARTITIONS)
        .build();

    assert_eq!(helper.queue(), DLQ_QUEUE);
    assert_eq!(helper.partitions(), DLQ_PARTITIONS);
    handle.stop().await;
}

#[tokio::test]
async fn consumer_dlq_outbox_maps_consumed_event_partition_to_dlq_partition_count() {
    let (handle, _db) = outbox_handle().await;
    let helper = ConsumerDlqOutbox::builder(Arc::clone(handle.outbox()))
        .queue(DLQ_QUEUE)
        .partitions(DLQ_PARTITIONS)
        .build();
    let record = dead_letter_record(10);

    let actual = helper.partition_for_record(&record);

    assert_eq!(actual, record.partition % DLQ_PARTITIONS);
    assert!(actual < DLQ_PARTITIONS);
    handle.stop().await;
}

#[tokio::test]
async fn consumer_dlq_outbox_enqueues_dead_letter_envelope() {
    let (handle, db, seen) = capturing_outbox_handle().await;
    let conn = db.conn().expect("db conn");
    let helper = ConsumerDlqOutbox::builder(Arc::clone(handle.outbox()))
        .queue(DLQ_QUEUE)
        .partitions(DLQ_PARTITIONS)
        .build();

    let record = dead_letter_record(11);
    // What `enqueue` should serialize, built from the same record.
    let expected = DeadLetterEnvelope::from_record(record.clone());

    helper
        .enqueue(&conn, record)
        .await
        .expect("enqueue succeeds")
        .fire();

    // Drive delivery and read back the bytes that were actually written.
    let payload = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(payload) = seen.lock().expect("lock").first().cloned() {
                break payload;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "dead-letter envelope never reached the processor"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };

    let mut delivered = DeadLetterEnvelope::from_slice(&payload).expect("decode envelope");
    // `parked_at` is stamped inside `from_record` at enqueue time, so it
    // legitimately differs from the expected value built here; everything else
    // must match the source event exactly.
    delivered.parked_at = expected.parked_at;
    assert_eq!(delivered, expected);

    handle.stop().await;
}

/// A queue the outbox does not hold fails the dead-letter write as a
/// dead-letter outbox error whose source is the outbox's own error, not as
/// the producer's queue mismatch.
#[tokio::test]
async fn a_dead_letter_enqueue_to_a_missing_queue_is_a_dead_letter_outbox_error() {
    let (handle, db) = outbox_handle().await;
    let conn = db.conn().expect("db conn");
    let helper = ConsumerDlqOutbox::builder(Arc::clone(handle.outbox()))
        .queue("missing-queue")
        .partitions(DLQ_PARTITIONS)
        .build();

    let err = helper
        .enqueue(&conn, dead_letter_record(12))
        .await
        .expect_err("missing queue should fail");

    assert_eq!(
        (err.kind(), err.to_string()),
        (
            "Outbox",
            "dead-letter outbox: queue 'missing-queue' is not registered".to_owned()
        )
    );
    let Some(outbox) = first_in_chain::<OutboxError>(&err) else {
        panic!("expected an OutboxError in the chain of {err:?}");
    };
    assert!(
        matches!(outbox, OutboxError::QueueNotRegistered(queue) if queue == "missing-queue"),
        "{outbox:?}"
    );
    handle.stop().await;
}

/// A dead-letter record whose envelope is over the outbox's payload limit is
/// refused before the write, as a dead-letter outbox error whose source is
/// the outbox's size error.
#[tokio::test]
async fn a_dead_letter_record_over_the_payload_limit_is_a_dead_letter_outbox_error() {
    let (handle, db) = outbox_handle().await;
    let conn = db.conn().expect("db conn");
    let helper = ConsumerDlqOutbox::builder(Arc::clone(handle.outbox()))
        .queue(DLQ_QUEUE)
        .partitions(DLQ_PARTITIONS)
        .build();
    let mut record = dead_letter_record(14);
    record.payload = serde_json::json!({ "blob": "x".repeat(70_000) });
    let size = DeadLetterEnvelope::from_record(record.clone())
        .to_vec()
        .expect("envelope serializes")
        .len();

    let err = helper
        .enqueue(&conn, record)
        .await
        .expect_err("an oversized envelope should fail");

    assert_eq!(
        (err.kind(), err.to_string()),
        (
            "Outbox",
            format!("dead-letter outbox: payload size {size} exceeds maximum 65536")
        )
    );
    let Some(outbox) = first_in_chain::<OutboxError>(&err) else {
        panic!("expected an OutboxError in the chain of {err:?}");
    };
    assert!(
        matches!(outbox, OutboxError::PayloadTooLarge { size: s, max: 65_536 } if *s == size),
        "{outbox:?}"
    );
    handle.stop().await;
}

/// A database failure of the dead-letter write keeps the database error in
/// the error's source chain, so a caller can classify it (for example as
/// contention to retry), and the error names the dead-letter outbox.
#[tokio::test]
async fn a_dead_letter_enqueue_database_error_keeps_the_database_error_as_its_source() {
    let (handle, _db) = outbox_handle().await;
    // A database without the outbox's tables: the queue resolves in the
    // outbox's registry, and the write itself fails in the database.
    let bare = toolkit_db::connect_db(
        "sqlite:file:evbk_dlq_outbox_bare?mode=memory&cache=shared",
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("connect bare db");
    let conn = bare.conn().expect("db conn");
    let helper = ConsumerDlqOutbox::builder(Arc::clone(handle.outbox()))
        .queue(DLQ_QUEUE)
        .partitions(DLQ_PARTITIONS)
        .build();

    let err = helper
        .enqueue(&conn, dead_letter_record(13))
        .await
        .expect_err("a database without the outbox tables should fail");

    let Some(database) = first_in_chain::<sea_orm::DbErr>(&err) else {
        panic!("expected a DbErr in the chain of {err:?}");
    };
    assert!(
        database.to_string().contains("no such table"),
        "{database:?}"
    );
    assert_eq!(
        (err.kind(), err.to_string()),
        ("Outbox", format!("dead-letter outbox: {database}"))
    );
    handle.stop().await;
}

/// The first `T` in `err`'s source chain. An SDK error keeps its source in an
/// `Arc`, whose own `source` skips the wrapped error, so the walk unwraps it.
fn first_in_chain<'a, T: std::error::Error + 'static>(
    err: &'a (dyn std::error::Error + 'static),
) -> Option<&'a T> {
    let mut cause = Some(err);
    while let Some(error) = cause {
        if let Some(found) = error.downcast_ref::<T>() {
            return Some(found);
        }
        cause = error
            .downcast_ref::<Arc<dyn std::error::Error + Send + Sync>>()
            .map_or_else(|| error.source(), |shared| Some(shared.as_ref()));
    }
    None
}

#[test]
fn dead_letter_envelope_payload_type_is_used_by_processors() {
    assert_eq!(
        DeadLetterEnvelope::PAYLOAD_TYPE,
        "application/vnd.constructorfabric.event-broker.dlq+json"
    );
}
