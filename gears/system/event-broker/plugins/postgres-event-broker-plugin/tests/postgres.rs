//! Behaviour only `PostgreSQL` has (`docs/TESTING.md` layer 3): catalog objects,
//! migrations, segment upkeep, the fence row's lock timeout, and the listener.

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code: a failed expectation is the test failure"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, BuildContext, Fence, FenceKey, FenceSeq, Flush,
    KeyedEvent, LogClaim, LogId, Partition, Replacing, Retention, Role, TopicPartition, TopicSpec,
    Wait,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, GtsTypeId, Position, Sequence};
use futures_util::StreamExt;
use sqlx::{AssertSqlSafe, PgPool};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const KEEP: Retention = Retention {
    max_age: Some(Duration::from_hours(24)),
    max_bytes: None,
};

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new(&format!(
        "gts.cf.core.events.topic.v1~cf.pgtest.x.t{}.v1",
        &Uuid::new_v4().simple().to_string()[..12]
    ))
    .unwrap()
}

fn topic_uuid(topic: &GtsInstanceId) -> String {
    event_broker_sdk::backend::support::topic_uuid(topic)
        .simple()
        .to_string()
}

fn keyed(n: u64) -> KeyedEvent {
    KeyedEvent {
        key: "k".to_owned(),
        event: Event {
            id: Uuid::new_v4(),
            type_id: GtsTypeId::new("gts.cf.core.events.event.v1~cf.pgtest.x.happened.v1~"),
            tenant_id: Uuid::from_u128(1),
            source: "pgtest".to_owned(),
            subject: format!("s-{n}"),
            subject_type: GtsTypeId::new("gts.cf.pgtest.x.thing.v1~"),
            occurred_at: chrono::Utc::now(),
            trace_parent: None,
            data: Some(serde_json::json!({ "n": n })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        },
    }
}

async fn provision(
    backend: &dyn Backend,
    topic: &GtsInstanceId,
    partitions: u32,
    retention: Retention,
) {
    backend
        .provision(
            &TopicSpec {
                topic: topic.clone(),
                partitions,
                retention,
            },
            &LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::new_v4(),
            },
        )
        .await
        .expect("provision");
}

async fn append(
    backend: &dyn Backend,
    topic: &GtsInstanceId,
    key: FenceKey,
    seq: i64,
    events: &[KeyedEvent],
) -> Result<Appended, BackendError> {
    backend
        .append(&AppendRequest {
            fence: Fence {
                key,
                seq: FenceSeq::new(seq),
            },
            topic,
            events,
            flush: Flush::Sync,
        })
        .await
}

async fn control(url: &str) -> PgPool {
    PgPool::connect(url).await.expect("control pool")
}

/// Every relation of `schema` whose name starts with `prefix`, sorted.
async fn relations(pool: &PgPool, schema: &str, prefix: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relkind IN ('r', 'p') AND starts_with(c.relname, $2) ORDER BY c.relname",
    )
    .bind(schema)
    .bind(prefix)
    .fetch_all(pool)
    .await
    .expect("catalog")
}

/// `(partition, first_seq, end_seq, table_name, state, counted_to)`.
type SegmentRow = (i32, i64, i64, String, String, i64);

type PartitionRow = (i32, String, i64, i64, i64, i64);

/// `(partition, first_seq, end_seq, table_name, state, counted_to)` of every
/// `evbk__segment` row, in order.
async fn segment_rows(pool: &PgPool, schema: &str) -> Vec<SegmentRow> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT partition, first_seq, end_seq, table_name, state::text, counted_to FROM {schema}.evbk__segment \
         ORDER BY partition, first_seq"
    )))
    .fetch_all(pool)
    .await
    .expect("segment rows")
}

/// Waits until `schema` has `count` segment rows.
async fn await_segment_rows(pool: &PgPool, schema: &str, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while segment_rows(pool, schema).await.len() != count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{count} segment rows did not appear within 30s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `segment_rows` of partition 0 of topic `t`, one per `(first_seq,
/// end_seq, state, counted_to)`.
fn partition_rows(t: &str, rows: &[Counted]) -> Vec<SegmentRow> {
    rows.iter()
        .map(|&(first, end, state, counted_to)| {
            (
                0,
                first,
                end,
                format!("evbk__{t}_p0_s{first:016x}"),
                state.to_owned(),
                counted_to,
            )
        })
        .collect()
}

fn batch(from: u64, count: u64) -> Vec<KeyedEvent> {
    (from..from + count).map(keyed).collect()
}

/// `(relation, attached bound)` of every segment table of `topic`, in order.
async fn segment_tables(pool: &PgPool, schema: &str, topic: &str) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT c.relname::text, pg_get_expr(c.relpartbound, c.oid) FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relkind = 'r' AND starts_with(c.relname, $2) ORDER BY c.relname",
    )
    .bind(schema)
    .bind(format!("evbk__{topic}_p0_s"))
    .fetch_all(pool)
    .await
    .expect("catalog")
}

fn attached(topic: &str, ranges: &[(i64, i64)]) -> Vec<(String, Option<String>)> {
    ranges
        .iter()
        .map(|(first, end)| {
            (
                format!("evbk__{topic}_p0_s{first:016x}"),
                Some(format!("FOR VALUES FROM ('{first}') TO ('{end}')")),
            )
        })
        .collect()
}

/// `provision` creates one partitioned table per partition with two empty
/// segments of room, named per the design, and a counter row for each.
#[tokio::test(flavor = "multi_thread")]
async fn provision_creates_parents_and_first_segments() {
    let server = common::server("provision-creates-parents-and-first-segments").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({ "segment": { "events": 1000 } }),
    )
    .await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 2, KEEP).await;

    let t = topic_uuid(&topic);
    let pool = control(&server.url).await;
    assert_eq!(
        relations(&pool, &schema, &format!("evbk__{t}")).await,
        [
            format!("evbk__{t}_p0"),
            format!("evbk__{t}_p0_s0000000000000001"),
            format!("evbk__{t}_p0_s00000000000003e9"),
            format!("evbk__{t}_p1"),
            format!("evbk__{t}_p1_s0000000000000001"),
            format!("evbk__{t}_p1_s00000000000003e9"),
        ]
    );
    // (partition, table_name, hwm, appended_bytes, floor, removed_bytes)
    let counters: Vec<PartitionRow> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT partition, p.table_name, p.hwm, p.appended_bytes, r.floor, r.removed_bytes \
         FROM {schema}.evbk__partition p JOIN {schema}.evbk__retention r USING (topic_uuid, partition) \
         ORDER BY partition"
    )))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        counters,
        [
            (0, format!("evbk__{t}_p0"), 0, 0, 0, 0),
            (1, format!("evbk__{t}_p1"), 0, 0, 0, 0),
        ]
    );
    let open = |partition: i32, first: i64, end: i64| {
        (
            partition,
            first,
            end,
            format!("evbk__{t}_p{partition}_s{first:016x}"),
            "open".to_owned(),
            first - 1,
        )
    };
    assert_eq!(
        segment_rows(&pool, &schema).await,
        [
            open(0, 1, 1001),
            open(0, 1001, 2001),
            open(1, 1, 1001),
            open(1, 1001, 2001),
        ]
    );
}

/// More partitions than the table names can carry is refused.
#[tokio::test(flavor = "multi_thread")]
async fn too_many_partitions_is_refused() {
    let server = common::server("too-many-partitions-is-refused").await;
    let backend =
        common::backend(&server.url, &common::fresh_schema(), serde_json::json!({})).await;
    let refused = backend
        .provision(
            &TopicSpec {
                topic: topic(),
                partitions: 10_000,
                retention: KEEP,
            },
            &LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::new_v4(),
            },
        )
        .await;
    match refused {
        Err(BackendError::InvalidConfig { detail }) => {
            assert_eq!(
                detail,
                "a topic has between 1 and 9999 partitions on the postgres backend"
            );
        }
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

/// The plugin's migrations are recorded in its own schema; a recorded
/// migration that no longer matches keeps the backend from starting.
#[tokio::test(flavor = "multi_thread")]
async fn a_drifted_schema_fails_startup() {
    let server = common::server("a-drifted-schema-fails-startup").await;
    let schema = common::fresh_schema();
    common::backend(&server.url, &schema, serde_json::json!({})).await;
    let pool = control(&server.url).await;
    let applied: Vec<i64> = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT version FROM {schema}._sqlx_migrations"
    )))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(applied, [1]);

    sqlx::query(AssertSqlSafe(format!(
        "UPDATE {schema}._sqlx_migrations SET checksum = '\\x00'"
    )))
    .execute(&pool)
    .await
    .unwrap();
    let serde_json::Value::Object(settings) =
        serde_json::json!({ "url": server.url, "schema": schema })
    else {
        unreachable!("an object literal");
    };
    let drifted = postgres_event_broker_plugin::PostgresBackendProvider::default();
    let backend = event_broker_sdk::backend::Provider::build(
        &drifted,
        &BuildContext::new(&settings, Role::Standalone),
    )
    .await
    .unwrap();
    backend.start().await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let probe = TopicPartition::new(topic(), Partition::new(0));
    assert!(
        matches!(
            backend.resolve(&probe, Position::Latest).await,
            Err(BackendError::Unavailable { .. })
        ),
        "a drifted schema must keep the backend unavailable"
    );
}

/// A retry blocked on a hung transaction's fence row gives up on
/// `lock_timeout` (5 s) instead of waiting forever, and the outbox retries.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_fence_row_times_out_the_retry() {
    let server = common::server("a-hung-fence-row-times-out-the-retry").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, serde_json::json!({})).await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let key = FenceKey::new(11);
    append(backend.as_ref(), &topic, key, 1, &[keyed(1)])
        .await
        .unwrap();

    let pool = control(&server.url).await;
    let mut hung = pool.begin().await.unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "SELECT * FROM {schema}.evbk__fence WHERE fence_key = '{}' FOR UPDATE",
        key.uuid()
    )))
    .execute(&mut *hung)
    .await
    .unwrap();

    let started = tokio::time::Instant::now();
    let retry = append(backend.as_ref(), &topic, key, 2, &[keyed(2)]).await;
    let waited = started.elapsed();
    hung.rollback().await.unwrap();

    assert!(
        matches!(retry, Err(BackendError::Unavailable { .. })),
        "{retry:?}"
    );
    assert!(
        waited >= Duration::from_secs(4) && waited < Duration::from_secs(15),
        "waited {waited:?}"
    );
}

/// After the listener's connection is killed, a head committed meanwhile by
/// another instance still reaches a watch on this one.
#[tokio::test(flavor = "multi_thread")]
async fn listener_reconnect_re_emits_heads() {
    let server = common::server("listener-reconnect-re-emits-heads").await;
    let schema = common::fresh_schema();
    let watcher: Arc<dyn Backend> =
        common::backend(&server.url, &schema, serde_json::json!({})).await;
    let writer = common::backend(&server.url, &schema, serde_json::json!({})).await;
    let topic = topic();
    provision(writer.as_ref(), &topic, 1, KEEP).await;
    let at = TopicPartition::new(topic.clone(), Partition::new(0));
    let mut heads = match watcher.watch() {
        event_broker_sdk::backend::Watch::Native(watch) => watch.watch(std::slice::from_ref(&at)),
        event_broker_sdk::backend::Watch::Poll(_) => {
            panic!("the postgres backend watches natively")
        }
    };
    let first = heads.next().await.unwrap().unwrap();
    assert_eq!(first.sequence, Sequence::NONE);

    let pool = control(&server.url).await;
    sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE query LIKE 'LISTEN%'",
    )
    .execute(&pool)
    .await
    .unwrap();
    append(writer.as_ref(), &topic, FenceKey::new(13), 1, &[keyed(1)])
        .await
        .unwrap();

    let next = tokio::time::timeout(Duration::from_secs(40), heads.next())
        .await
        .expect("a head within 40s of the reconnect")
        .unwrap()
        .unwrap();
    assert_eq!((next.at, next.sequence), (at, Sequence::assigned(1)));
}

/// Full appends keep finding segments: after each, the extender attaches
/// segments ahead of the head and records them `open`.
#[tokio::test(flavor = "multi_thread")]
async fn segments_are_kept_ahead_of_appends() {
    let server = common::server("segments-are-kept-ahead-of-appends").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({ "segment": { "events": 500 } }),
    )
    .await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let pool = control(&server.url).await;
    await_segment_rows(&pool, &schema, 2).await;

    append(
        backend.as_ref(),
        &topic,
        FenceKey::new(21),
        1,
        &batch(0, 1000),
    )
    .await
    .unwrap();
    await_segment_rows(&pool, &schema, 4).await;
    append(
        backend.as_ref(),
        &topic,
        FenceKey::new(21),
        2,
        &batch(1000, 1000),
    )
    .await
    .unwrap();
    // Full segments are counted and sealed meanwhile.
    let t = topic_uuid(&topic);
    settles(
        partition_rows(
            &t,
            &[
                (1, 501, "sealed", 500),
                (501, 1001, "sealed", 1000),
                (1001, 1501, "sealed", 1500),
                (1501, 2001, "sealed", 2000),
                (2001, 2501, "open", 2000),
                (2501, 3001, "open", 2500),
            ],
        ),
        || segment_rows(&pool, &schema),
    )
    .await;
    let ranges = [
        (1, 501),
        (501, 1001),
        (1001, 1501),
        (1501, 2001),
        (2001, 2501),
        (2501, 3001),
    ];
    assert_eq!(
        segment_tables(&pool, &schema, &t).await,
        attached(&t, &ranges)
    );
}

/// An append that finds no segment is `Unavailable` and calls the extender
/// at once; the retry succeeds once it has run.
#[tokio::test(flavor = "multi_thread")]
async fn an_append_with_no_segment_recovers() {
    let server = common::server("an-append-with-no-segment-recovers").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({ "segment": { "events": 500 } }),
    )
    .await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    append(
        backend.as_ref(),
        &topic,
        FenceKey::new(22),
        1,
        &batch(0, 1000),
    )
    .await
    .unwrap();
    let pool = control(&server.url).await;
    let t = topic_uuid(&topic);
    // Settled: room made and both full segments sealed, so no count is due
    // once the room is back.
    settles(
        partition_rows(
            &t,
            &[
                (1, 501, "sealed", 500),
                (501, 1001, "sealed", 1000),
                (1001, 1501, "open", 1000),
                (1501, 2001, "open", 1500),
            ],
        ),
        || segment_rows(&pool, &schema),
    )
    .await;

    // Take the room away behind the instance's back: its registry still
    // believes the segments are there.
    sqlx::raw_sql(AssertSqlSafe(format!(
        "BEGIN; \
         DROP TABLE {schema}.evbk__{t}_p0_s{:016x}, {schema}.evbk__{t}_p0_s{:016x}; \
         DELETE FROM {schema}.evbk__segment WHERE first_seq > 1000; \
         COMMIT",
        1001, 1501
    )))
    .execute(&pool)
    .await
    .unwrap();

    let refused = append(
        backend.as_ref(),
        &topic,
        FenceKey::new(22),
        2,
        &batch(1000, 1),
    )
    .await;
    assert!(
        matches!(refused, Err(BackendError::Unavailable { .. })),
        "{refused:?}"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match append(
            backend.as_ref(),
            &topic,
            FenceKey::new(22),
            2,
            &batch(1000, 1),
        )
        .await
        {
            Ok(appended) => {
                assert_eq!(
                    appended,
                    Appended::Heads(vec![event_broker_sdk::backend::Head {
                        at: TopicPartition::new(topic.clone(), Partition::new(0)),
                        sequence: Sequence::assigned(1001),
                    }])
                );
                break;
            }
            Err(BackendError::Unavailable { .. }) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the append did not recover within 30s"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(other) => panic!("{other:?}"),
        }
    }

    // Recreated for the head at 1000, then one more for the head at 1001.
    settles(
        partition_rows(
            &t,
            &[
                (1, 501, "sealed", 500),
                (501, 1001, "sealed", 1000),
                (1001, 1501, "open", 1000),
                (1501, 2001, "open", 1500),
                (2001, 2501, "open", 2000),
            ],
        ),
        || segment_rows(&pool, &schema),
    )
    .await;
    let ranges = [
        (1, 501),
        (501, 1001),
        (1001, 1501),
        (1501, 2001),
        (2001, 2501),
    ];
    assert_eq!(
        segment_tables(&pool, &schema, &t).await,
        attached(&t, &ranges)
    );
}

/// Segments of 500, and a rescan and cross-check five times a second.
fn quick() -> serde_json::Value {
    serde_json::json!({
        "segment": { "events": 500 },
        "maintenance": { "rescan_interval_ms": 200, "crosscheck_every": 1 },
    })
}

/// Polls `actual` until it equals `expected`, for up to 30 s, then asserts
/// it.
async fn settles<T, F, Fut>(expected: T, actual: F)
where
    T: PartialEq + std::fmt::Debug,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let now = actual().await;
        if now == expected || tokio::time::Instant::now() >= deadline {
            assert_eq!(now, expected);
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `(first_seq, end_seq, state, counted_to)`.
type Counted = (i64, i64, &'static str, i64);

type Weighed = (
    i64,
    i64,
    String,
    i64,
    i64,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// `(first_seq, end_seq, state, counted_to, bytes, newest_at)` of every
/// segment row of partition 0 of topic `t`, in order.
async fn weighed(pool: &PgPool, schema: &str, t: &str) -> Vec<Weighed> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT first_seq, end_seq, state::text, counted_to, bytes, newest_at FROM {schema}.evbk__segment \
         WHERE topic_uuid = $1::uuid AND partition = 0 ORDER BY first_seq"
    )))
    .bind(t)
    .fetch_all(pool)
    .await
    .expect("segment rows")
}

/// `(SUM(stored_bytes), MAX(sequence_time))` of partition 0 of topic `t`
/// over `(after, upto]`, by a full scan.
async fn scanned(
    pool: &PgPool,
    schema: &str,
    t: &str,
    (after, upto): (i64, i64),
) -> (i64, Option<chrono::DateTime<chrono::Utc>>) {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT COALESCE(SUM(stored_bytes), 0)::bigint, MAX(sequence_time) FROM {schema}.evbk__{t}_p0 \
         WHERE sequence > $1 AND sequence <= $2"
    )))
    .bind(after)
    .bind(upto)
    .fetch_one(pool)
    .await
    .expect("scan")
}

/// `rows` of `(first_seq, end_seq, state, counted_to)`, each weighed by a
/// full scan of what it counts.
async fn weighed_by_scan(pool: &PgPool, schema: &str, t: &str, rows: &[Counted]) -> Vec<Weighed> {
    let mut weighed = Vec::new();
    for &(first, end, state, counted_to) in rows {
        let (bytes, newest_at) = scanned(pool, schema, t, (first - 1, counted_to)).await;
        weighed.push((first, end, state.to_owned(), counted_to, bytes, newest_at));
    }
    weighed
}

/// `(floor, removed_bytes)` of partition 0 of topic `t`.
async fn retained(pool: &PgPool, schema: &str, t: &str) -> (i64, i64) {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT floor, removed_bytes FROM {schema}.evbk__retention WHERE topic_uuid = $1::uuid AND partition = 0"
    )))
    .bind(t)
    .fetch_one(pool)
    .await
    .expect("retention row")
}

/// `appended_bytes` of partition 0 of topic `t`.
async fn appended_bytes(pool: &PgPool, schema: &str, t: &str) -> i64 {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT appended_bytes FROM {schema}.evbk__partition WHERE topic_uuid = $1::uuid AND partition = 0"
    )))
    .bind(t)
    .fetch_one(pool)
    .await
    .expect("partition row")
}

/// `(first_seq, end_seq, table_name, state)` of every segment row of
/// partition 0 of topic `t`, in order.
async fn topic_rows(pool: &PgPool, schema: &str, t: &str) -> Vec<(i64, i64, String, String)> {
    sqlx::query_as(AssertSqlSafe(format!(
        "SELECT first_seq, end_seq, table_name, state::text FROM {schema}.evbk__segment \
         WHERE topic_uuid = $1::uuid AND partition = 0 ORDER BY first_seq"
    )))
    .bind(t)
    .fetch_all(pool)
    .await
    .expect("segment rows")
}

fn rows_of(t: &str, rows: &[(i64, i64, &str)]) -> Vec<(i64, i64, String, String)> {
    rows.iter()
        .map(|&(first, end, state)| {
            (
                first,
                end,
                format!("evbk__{t}_p0_s{first:016x}"),
                state.to_owned(),
            )
        })
        .collect()
}

/// The counter weighs each open segment up to the head, in step with a full
/// scan, and seals a segment exactly when it is counted to its end.
#[tokio::test(flavor = "multi_thread")]
async fn the_counter_weighs_segments_and_seals_full_ones() {
    let server = common::server("the-counter-weighs-segments").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, quick()).await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let pool = control(&server.url).await;
    let t = topic_uuid(&topic);

    let steps: [(u64, u64, &[Counted]); 3] = [
        (
            0,
            300,
            &[
                (1, 501, "open", 300),
                (501, 1001, "open", 500),
                (1001, 1501, "open", 1000),
            ],
        ),
        (
            300,
            300,
            &[
                (1, 501, "sealed", 500),
                (501, 1001, "open", 600),
                (1001, 1501, "open", 1000),
                (1501, 2001, "open", 1500),
            ],
        ),
        (
            600,
            400,
            &[
                (1, 501, "sealed", 500),
                (501, 1001, "sealed", 1000),
                (1001, 1501, "open", 1000),
                (1501, 2001, "open", 1500),
            ],
        ),
    ];
    for (seq, (from, count, rows)) in (1..).zip(steps) {
        append(
            backend.as_ref(),
            &topic,
            FenceKey::new(31),
            seq,
            &batch(from, count),
        )
        .await
        .unwrap();
        let expected = weighed_by_scan(&pool, &schema, &t, rows).await;
        settles(expected, || weighed(&pool, &schema, &t)).await;
    }
}

/// Segments whose newest event is past `max_age` expire whole, oldest first;
/// the open one stays, however old; the floor and `removed_bytes` move with
/// them, and the detacher then drops their tables and rows.
#[tokio::test(flavor = "multi_thread")]
async fn retention_by_age_expires_whole_sealed_segments() {
    let server = common::server("retention-by-age").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, quick()).await;
    let topic = topic();
    provision(
        backend.as_ref(),
        &topic,
        1,
        Retention {
            max_age: Some(Duration::from_secs(1)),
            max_bytes: None,
        },
    )
    .await;
    append(
        backend.as_ref(),
        &topic,
        FenceKey::new(41),
        1,
        &batch(0, 1000),
    )
    .await
    .unwrap();
    // Room for the next append is made after this one commits.
    let pool = control(&server.url).await;
    await_segment_rows(&pool, &schema, 4).await;
    append(
        backend.as_ref(),
        &topic,
        FenceKey::new(41),
        2,
        &batch(1000, 100),
    )
    .await
    .unwrap();
    let t = topic_uuid(&topic);
    let (kept, _) = scanned(&pool, &schema, &t, (1000, 1100)).await;
    let appended = appended_bytes(&pool, &schema, &t).await;

    settles((1000, appended - kept), || retained(&pool, &schema, &t)).await;
    let ranges = [(1001, 1501), (1501, 2001), (2001, 2501)];
    settles(
        rows_of(
            &t,
            &[
                (1001, 1501, "open"),
                (1501, 2001, "open"),
                (2001, 2501, "open"),
            ],
        ),
        || topic_rows(&pool, &schema, &t),
    )
    .await;
    settles(attached(&t, &ranges), || segment_tables(&pool, &schema, &t)).await;
    let at = TopicPartition::new(topic.clone(), Partition::new(0));
    assert_eq!(
        (
            backend.resolve(&at, Position::Earliest).await.unwrap(),
            backend.resolve(&at, Position::Latest).await.unwrap(),
        ),
        (Sequence::assigned(1000), Sequence::assigned(1100))
    );
}

/// A segment goes by bytes only while what stays still holds `max_bytes`;
/// while another session holds the retention row, retention leaves the
/// partition to it and nothing moves.
#[tokio::test(flavor = "multi_thread")]
async fn retention_by_bytes_never_goes_under_the_bound_and_yields_to_a_holder() {
    let server = common::server("retention-by-bytes").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, quick()).await;
    let topic = topic();
    provision(
        backend.as_ref(),
        &topic,
        1,
        Retention {
            max_age: None,
            max_bytes: Some(1),
        },
    )
    .await;
    let pool = control(&server.url).await;
    let t = topic_uuid(&topic);

    // Another instance's detacher, holding the partition: expired segments
    // stay attached until it lets go.
    let mut detacher = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(format!("evbk:det:{}:0", Uuid::parse_str(&t).unwrap()))
        .execute(&mut *detacher)
        .await
        .unwrap();
    // Another instance's retention, holding the row.
    let mut holder = pool.begin().await.unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "SELECT floor FROM {schema}.evbk__retention WHERE topic_uuid = $1::uuid FOR UPDATE"
    )))
    .bind(&t)
    .execute(&mut *holder)
    .await
    .unwrap();
    append(
        backend.as_ref(),
        &topic,
        FenceKey::new(42),
        1,
        &batch(0, 1000),
    )
    .await
    .unwrap();
    let both_sealed = weighed_by_scan(
        &pool,
        &schema,
        &t,
        &[
            (1, 501, "sealed", 500),
            (501, 1001, "sealed", 1000),
            (1001, 1501, "open", 1000),
            (1501, 2001, "open", 1500),
        ],
    )
    .await;
    settles(both_sealed, || weighed(&pool, &schema, &t)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(retained(&pool, &schema, &t).await, (0, 0));
    holder.commit().await.unwrap();

    let (kept, _) = scanned(&pool, &schema, &t, (500, 1000)).await;
    let appended = appended_bytes(&pool, &schema, &t).await;
    settles((500, appended - kept), || retained(&pool, &schema, &t)).await;
    // Expired and still attached: a read from the start begins past the floor.
    assert_eq!(
        topic_rows(&pool, &schema, &t).await[0],
        rows_of(&t, &[(1, 501, "expired")])[0]
    );
    let at = TopicPartition::new(topic.clone(), Partition::new(0));
    let read = backend
        .read(&at, Sequence::NONE, 3, Wait::No)
        .await
        .unwrap();
    assert_eq!(
        read.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        [501, 502, 503].map(|n| Some(Sequence::assigned(n)))
    );
    // A pooled connection keeps its session, and with it the lock.
    sqlx::query("SELECT pg_advisory_unlock_all()")
        .execute(&mut *detacher)
        .await
        .unwrap();
    settles(
        rows_of(
            &t,
            &[
                (501, 1001, "sealed"),
                (1001, 1501, "open"),
                (1501, 2001, "open"),
            ],
        ),
        || topic_rows(&pool, &schema, &t),
    )
    .await;
}

/// A detach left pending - its first transaction committed, its wait cut
/// short - is finished with `FINALIZE`; the table is dropped and its row
/// deleted.
#[tokio::test(flavor = "multi_thread")]
async fn a_pending_detach_is_finalized() {
    let server = common::server("a-pending-detach-is-finalized").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, quick()).await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let pool = control(&server.url).await;
    let t = topic_uuid(&topic);
    let parent = format!("{schema}.evbk__{t}_p0");
    let first = format!("{schema}.evbk__{t}_p0_s{:016x}", 1);

    // A reader the detach waits for, and a detach that stops waiting.
    let mut reader = pool.begin().await.unwrap();
    sqlx::query(AssertSqlSafe(format!("SELECT count(*) FROM {parent}")))
        .execute(&mut *reader)
        .await
        .unwrap();
    let mut detacher = pool.acquire().await.unwrap();
    sqlx::raw_sql("SET statement_timeout = '500ms'")
        .execute(&mut *detacher)
        .await
        .unwrap();
    let cut = sqlx::raw_sql(AssertSqlSafe(format!(
        "ALTER TABLE {parent} DETACH PARTITION {first} CONCURRENTLY"
    )))
    .execute(&mut *detacher)
    .await;
    assert!(cut.is_err(), "{cut:?}");
    drop(detacher);
    reader.commit().await.unwrap();
    let pending: bool = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT inhdetachpending FROM pg_inherits WHERE inhrelid = '{first}'::regclass"
    )))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(pending);
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE {schema}.evbk__segment SET state = 'detaching' WHERE topic_uuid = $1::uuid AND first_seq = 1"
    )))
    .bind(&t)
    .execute(&pool)
    .await
    .unwrap();

    settles(
        rows_of(&t, &[(501, 1001, "open"), (1001, 1501, "open")]),
        || topic_rows(&pool, &schema, &t),
    )
    .await;
    settles(attached(&t, &[(501, 1001), (1001, 1501)]), || {
        segment_tables(&pool, &schema, &t)
    })
    .await;
    settles(0, || advisory_locks(&pool)).await;
}

/// A `detaching` row whose table is already gone is just deleted.
#[tokio::test(flavor = "multi_thread")]
async fn a_detaching_row_with_no_table_is_deleted() {
    let server = common::server("a-detaching-row-with-no-table").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, quick()).await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let pool = control(&server.url).await;
    let t = topic_uuid(&topic);
    sqlx::raw_sql(AssertSqlSafe(format!(
        "BEGIN; \
         DROP TABLE {schema}.evbk__{t}_p0_s{:016x}; \
         UPDATE {schema}.evbk__segment SET state = 'detaching' WHERE topic_uuid = '{t}' AND first_seq = 1; \
         COMMIT",
        1
    )))
    .execute(&pool)
    .await
    .unwrap();

    settles(
        rows_of(&t, &[(501, 1001, "open"), (1001, 1501, "open")]),
        || topic_rows(&pool, &schema, &t),
    )
    .await;
}

/// The cross-check repairs through reconcile what it can: a missing table
/// above the head (its row deleted, then the extender makes both again), an
/// attached table with no row (adopted), a detached one with no row below
/// the floor (dropped); a missing table at or below the head is only
/// reported, and its row kept.
#[tokio::test(flavor = "multi_thread")]
async fn the_cross_check_repairs_drift_and_reports_lost_events() {
    let server = common::server("the-cross-check-repairs-drift").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, quick()).await;
    let [unbuilt, unrecorded, leftover, lost] = [topic(), topic(), topic(), topic()];
    for topic in [&unbuilt, &unrecorded, &leftover, &lost] {
        provision(backend.as_ref(), topic, 1, KEEP).await;
    }
    append(backend.as_ref(), &lost, FenceKey::new(61), 1, &batch(0, 10))
        .await
        .unwrap();
    let pool = control(&server.url).await;
    let [a, b, c, d] = [&unbuilt, &unrecorded, &leftover, &lost].map(topic_uuid);
    let segment = |t: &str, first: i64| format!("{schema}.evbk__{t}_p0_s{first:016x}");
    sqlx::raw_sql(AssertSqlSafe(format!(
        "DROP TABLE {}; \
         CREATE TABLE {b_new} (LIKE {schema}.evbk__{b}_p0 INCLUDING ALL); \
         ALTER TABLE {schema}.evbk__{b}_p0 ATTACH PARTITION {b_new} FOR VALUES FROM (1001) TO (1501); \
         CREATE TABLE {} (LIKE {schema}.evbk__{c}_p0 INCLUDING ALL); \
         DROP TABLE {}",
        segment(&a, 501),
        segment(&c, 0),
        segment(&d, 1),
        b_new = segment(&b, 1001),
    )))
    .execute(&pool)
    .await
    .unwrap();

    settles(attached(&a, &[(1, 501), (501, 1001)]), || {
        segment_tables(&pool, &schema, &a)
    })
    .await;
    settles(
        rows_of(&a, &[(1, 501, "open"), (501, 1001, "open")]),
        || topic_rows(&pool, &schema, &a),
    )
    .await;
    settles(
        rows_of(
            &b,
            &[(1, 501, "open"), (501, 1001, "open"), (1001, 1501, "open")],
        ),
        || topic_rows(&pool, &schema, &b),
    )
    .await;
    settles(attached(&c, &[(1, 501), (501, 1001)]), || {
        segment_tables(&pool, &schema, &c)
    })
    .await;
    // Several cross-checks later, the lost segment's row is still there.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        (
            topic_rows(&pool, &schema, &d).await,
            segment_tables(&pool, &schema, &d).await,
        ),
        (
            rows_of(
                &d,
                &[(1, 501, "open"), (501, 1001, "open"), (1001, 1501, "open")]
            ),
            attached(&d, &[(501, 1001), (1001, 1501)]),
        )
    );
    settles(0, || advisory_locks(&pool)).await;
}

/// Advisory locks held in the database, by any session.
async fn advisory_locks(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE locktype = 'advisory'")
        .fetch_one(pool)
        .await
        .expect("pg_locks")
}

/// A detach that waits on a long reader is cut off by its statement timeout
/// and left pending, so it never holds the parent's lock for long; once the
/// reader ends it is finished, and no advisory lock is left behind.
#[tokio::test(flavor = "multi_thread")]
async fn a_detach_cut_off_by_its_timeout_finishes_and_releases_its_lock() {
    let server = common::server("a-detach-cut-off-by-its-timeout").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, quick()).await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let pool = control(&server.url).await;
    let t = topic_uuid(&topic);
    let first = format!("{schema}.evbk__{t}_p0_s{:016x}", 1);

    let mut reader = pool.begin().await.unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "SELECT count(*) FROM {schema}.evbk__{t}_p0"
    )))
    .execute(&mut *reader)
    .await
    .unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE {schema}.evbk__segment SET state = 'expired' WHERE topic_uuid = $1::uuid AND first_seq = 1"
    )))
    .bind(&t)
    .execute(&pool)
    .await
    .unwrap();
    let pending = || async {
        sqlx::query_scalar::<_, bool>(
            "SELECT COALESCE((SELECT inhdetachpending FROM pg_inherits WHERE inhrelid = to_regclass($1)), false)",
        )
        .bind(&first)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    settles(true, pending).await;
    reader.commit().await.unwrap();

    settles(
        rows_of(&t, &[(501, 1001, "open"), (1001, 1501, "open")]),
        || topic_rows(&pool, &schema, &t),
    )
    .await;
    settles(0, || advisory_locks(&pool)).await;
}

/// An attached table with no row, at the range the extender needs next, is
/// adopted as soon as the extender runs into it, not after two cross-checks.
#[tokio::test(flavor = "multi_thread")]
async fn a_name_collision_is_adopted_at_once() {
    let server = common::server("a-name-collision-is-adopted-at-once").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({
            "segment": { "events": 500 },
            "maintenance": { "rescan_interval_ms": 200, "crosscheck_every": 1000 },
        }),
    )
    .await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let pool = control(&server.url).await;
    let t = topic_uuid(&topic);
    let parent = format!("{schema}.evbk__{t}_p0");
    sqlx::raw_sql(AssertSqlSafe(format!(
        "CREATE TABLE {parent}_s{first:016x} (LIKE {parent} INCLUDING ALL); \
         ALTER TABLE {parent} ATTACH PARTITION {parent}_s{first:016x} FOR VALUES FROM (1001) TO (1501)",
        first = 1001
    )))
    .execute(&pool)
    .await
    .unwrap();

    append(
        backend.as_ref(),
        &topic,
        FenceKey::new(71),
        1,
        &batch(0, 1000),
    )
    .await
    .unwrap();
    settles(
        rows_of(
            &t,
            &[
                (1, 501, "sealed"),
                (501, 1001, "sealed"),
                (1001, 1501, "open"),
                (1501, 2001, "open"),
            ],
        ),
        || topic_rows(&pool, &schema, &t),
    )
    .await;
    settles(
        attached(&t, &[(1, 501), (501, 1001), (1001, 1501), (1501, 2001)]),
        || segment_tables(&pool, &schema, &t),
    )
    .await;
}

/// The plugin's connection kinds open in the database, by `application_name`.
async fn connection_kinds(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT DISTINCT application_name::text FROM pg_stat_activity \
         WHERE application_name LIKE 'evbk-%' ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .expect("pg_stat_activity")
}

/// A delivery process holds a main pool and a `LISTEN` connection and runs
/// no maintenance; an ingest process adds the background pool, which goes
/// with it when it stops.
#[tokio::test(flavor = "multi_thread")]
async fn only_an_ingest_process_runs_the_maintenance() {
    let server = common::server("only-an-ingest-process-runs-the-maintenance").await;
    let schema = common::fresh_schema();
    let pool = control(&server.url).await;
    let delivery = common::backend_as(&server.url, &schema, quick(), Role::Delivery).await;
    let kinds = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    settles(kinds(&["evbk-listen", "evbk-main"]), || {
        connection_kinds(&pool)
    })
    .await;

    let ingest = common::backend_as(&server.url, &schema, quick(), Role::Ingest).await;
    let topic = topic();
    provision(ingest.as_ref(), &topic, 1, KEEP).await;
    append(ingest.as_ref(), &topic, FenceKey::new(81), 1, &batch(0, 3))
        .await
        .unwrap();
    settles(
        kinds(&["evbk-background", "evbk-listen", "evbk-main"]),
        || connection_kinds(&pool),
    )
    .await;

    ingest.stop(CancellationToken::new()).await;
    settles(kinds(&["evbk-listen", "evbk-main"]), || {
        connection_kinds(&pool)
    })
    .await;
    let at = TopicPartition::new(topic.clone(), Partition::new(0));
    let read = delivery
        .read(&at, Sequence::NONE, 10, Wait::No)
        .await
        .unwrap();
    assert_eq!(
        read.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        [1, 2, 3].map(|n| Some(Sequence::assigned(n)))
    );
}
