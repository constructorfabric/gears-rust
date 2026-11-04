//! Plugin-specific behaviour against a real server (`docs/TESTING.md` layer
//! 3), each test once on `MySQL` and once on `MariaDB`, each run on a server
//! of its own (or the one `EB_TEST_MYSQL_URL` / `EB_TEST_MARIADB_URL` names)
//! and a database of its own.

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "integration tests: a failed expectation is the test failure"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use common::Engine;
use event_broker_sdk::backend::support::{partition_for, topic_uuid};
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Fence, FenceKey, FenceSeq, Flush, KeyedEvent,
    LogClaim, LogId, Partition, Provisioning, Replacing, Retention, TopicPartition, TopicSpec,
    Wait,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, GtsTypeId, Position, Sequence};
use sqlx::{AssertSqlSafe, MySqlPool};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Runs `$name(engine)` as `$name_mysql` and `$name_mariadb`.
macro_rules! on_both {
    ($($name:ident),+ $(,)?) => {
        mod mysql {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(super::Engine::Mysql).await;
                }
            )+
        }
        mod mariadb {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(super::Engine::Mariadb).await;
                }
            )+
        }
    };
}

on_both!(
    provision_creates_tables_and_first_segments,
    a_replayed_fence_on_an_unprovisioned_topic_is_not_provisioned,
    a_hung_fence_row_times_out_the_retry,
    a_re_provision_does_not_wait_for_an_open_append,
    segments_are_kept_ahead_of_appends,
    the_sealer_weighs_full_segments,
    retention_by_age_drops_whole_sealed_segments,
    retention_by_bytes_keeps_the_newest_bytes_within_the_bound,
    a_ddl_waiting_on_a_metadata_lock_does_not_stop_appends,
    a_crash_between_ddl_and_catalog_is_settled,
    appended_events_survive_a_server_restart,
    a_server_that_does_not_flush_every_commit_is_refused_unless_allowed,
    a_clock_step_back_does_not_lower_seq_time,
    stop_closes_every_connection_even_past_its_deadline,
);

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new(&format!(
        "gts.cf.core.events.topic.v1~cf.mysqltest.x.t{}.v1",
        &Uuid::new_v4().simple().to_string()[..12]
    ))
    .unwrap()
}

/// An event of fixed size for any `n` of four digits.
fn keyed(key: &str, n: u64) -> KeyedEvent {
    keyed_padded(key, n, 0)
}

/// An event carrying `pad` bytes of padding besides its number.
fn keyed_padded(key: &str, n: u64, pad: usize) -> KeyedEvent {
    KeyedEvent {
        key: key.to_owned(),
        event: Event {
            id: Uuid::from_u128(0xe1),
            type_id: GtsTypeId::new("gts.cf.core.events.event.v1~cf.mysqltest.x.happened.v1~"),
            tenant_id: Uuid::from_u128(0x7e4a_0001),
            source: "mysql-test".to_owned(),
            subject: "s".to_owned(),
            subject_type: GtsTypeId::new("gts.cf.mysqltest.x.thing.v1~"),
            occurred_at: Utc.timestamp_opt(1_767_225_600, 0).single().unwrap(),
            trace_parent: None,
            data: Some(serde_json::json!({ "n": n, "pad": "x".repeat(pad) })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        },
    }
}

/// A key that lands in `partition` of `partitions`.
fn key_for(partition: u32, partitions: u32) -> String {
    (0..10_000)
        .map(|i| format!("k{i}"))
        .find(|key| partition_for(key, partitions).value() == partition)
        .unwrap()
}

const KEEP: Retention = Retention {
    max_age: None,
    max_bytes: None,
};

async fn provision(
    backend: &dyn Backend,
    partitions: u32,
    retention: Retention,
) -> (TopicSpec, LogClaim) {
    let spec = TopicSpec {
        topic: topic(),
        partitions,
        retention,
    };
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: LogId::new_v4(),
    };
    assert_eq!(
        backend.provision(&spec, &claim).await.unwrap(),
        Provisioning::Ready { partitions }
    );
    (spec, claim)
}

/// Appends under a fresh fence, retrying `Unavailable` (an append that found
/// no segment) for up to 30 s, as the gear does.
async fn append(backend: &dyn Backend, topic: &GtsInstanceId, events: &[KeyedEvent]) -> Appended {
    let fence = Fence {
        key: FenceKey::new(Uuid::new_v4().as_u128()),
        seq: FenceSeq::new(1),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match backend
            .append(&AppendRequest {
                fence,
                topic,
                events,
                flush: Flush::Sync,
            })
            .await
        {
            Err(BackendError::Unavailable { .. }) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            outcome => return outcome.expect("append"),
        }
    }
}

async fn read_all(backend: &dyn Backend, at: &TopicPartition) -> Vec<i64> {
    let mut all = Vec::new();
    let mut offset = Sequence::NONE;
    loop {
        let batch = backend.read(at, offset, 500, Wait::No).await.expect("read");
        let Some(last) = batch.last() else {
            return all;
        };
        offset = last.sequence.unwrap();
        all.extend(batch.iter().map(|e| e.sequence.unwrap().as_i64()));
    }
}

fn table(spec: &TopicSpec, partition: u32) -> String {
    format!("evbk__{}_p{partition}", topic_uuid(&spec.topic).simple())
}

/// `(first, end, name, state)` of every segment row of a partition.
async fn rows(
    pool: &MySqlPool,
    spec: &TopicSpec,
    partition: u32,
) -> Vec<(i64, i64, String, String)> {
    sqlx::query_as(
        "SELECT first_seq, end_seq, segment_name, CAST(state AS CHAR) FROM evbk__segment \
         WHERE topic_uuid = ? AND partition_no = ? ORDER BY first_seq",
    )
    .bind(topic_uuid(&spec.topic))
    .bind(i32::try_from(partition).unwrap())
    .fetch_all(pool)
    .await
    .unwrap()
}

/// The range partitions the server holds of a partition's table, lowest
/// first.
async fn held(pool: &MySqlPool, spec: &TopicSpec, partition: u32) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT PARTITION_NAME FROM information_schema.PARTITIONS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? ORDER BY PARTITION_ORDINAL_POSITION",
    )
    .bind(table(spec, partition))
    .fetch_all(pool)
    .await
    .unwrap()
}

/// `(floor_seq, removed_bytes, bytes, hwm)` of a partition.
async fn figures(pool: &MySqlPool, spec: &TopicSpec, partition: u32) -> (i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT r.floor_seq, r.removed_bytes, p.bytes, p.hwm FROM evbk__partition p \
         JOIN evbk__retention r ON r.topic_uuid = p.topic_uuid AND r.partition_no = p.partition_no \
         WHERE p.topic_uuid = ? AND p.partition_no = ?",
    )
    .bind(topic_uuid(&spec.topic))
    .bind(i32::try_from(partition).unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Segments `[first, first + span)` from `from` up to `to`, all `state`.
fn segments(from: i64, to: i64, span: i64, state: &str) -> Vec<(i64, i64, String, String)> {
    (from..to)
        .step_by(usize::try_from(span).unwrap())
        .map(|first| {
            (
                first,
                first + span,
                format!("s{first:016x}"),
                state.to_owned(),
            )
        })
        .collect()
}

fn names_of(rows: &[(i64, i64, String, String)]) -> Vec<String> {
    rows.iter().map(|(_, _, name, _)| name.clone()).collect()
}

/// Polls `actual` until it returns `expected`, for up to 30 s, then asserts.
async fn settles<T, F, Fut>(expected: T, mut actual: F)
where
    T: PartialEq + std::fmt::Debug,
    F: FnMut() -> Fut,
    Fut: Future<Output = T>,
{
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let now = actual().await;
        if now == expected || Instant::now() >= deadline {
            assert_eq!(now, expected);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Segments of 1000 events, the smallest allowed: one append's worth.
fn thousands() -> serde_json::Value {
    serde_json::json!({ "segment": { "events": 1000 } })
}

/// Appends `events` in calls of at most a thousand.
async fn append_all(backend: &dyn Backend, topic: &GtsInstanceId, events: &[KeyedEvent]) {
    for chunk in events.chunks(1000) {
        append(backend, topic, chunk).await;
    }
}

/// Every operation that may wait on a lock is given this long; one that
/// waits for a holder that never lets go does not return within it.
const BOUND: Duration = Duration::from_secs(30);

/// `provision` creates each partition's table with the segments that give
/// room for one append, records them `open`, and names everything as stored.
async fn provision_creates_tables_and_first_segments(engine: Engine) {
    let server = common::server(engine, "provision").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, thousands()).await;
    let control = server.control(&database).await;

    let (spec, _) = provision(backend.as_ref(), 2, KEEP).await;

    let tables: Vec<(i32, String, i64, i64)> = sqlx::query_as(
        "SELECT partition_no, table_name, hwm, bytes FROM evbk__partition WHERE topic_uuid = ? ORDER BY partition_no",
    )
    .bind(topic_uuid(&spec.topic))
    .fetch_all(&control)
    .await
    .unwrap();
    assert_eq!(
        tables,
        [(0, table(&spec, 0), 0, 0), (1, table(&spec, 1), 0, 0)]
    );
    let expected = segments(1, 2001, 1000, "open");
    for partition in 0..2 {
        assert_eq!(rows(&control, &spec, partition).await, expected);
        assert_eq!(held(&control, &spec, partition).await, names_of(&expected));
    }
    backend.stop(CancellationToken::new()).await;
}

/// An append to a topic this backend does not hold is `NotProvisioned`, even
/// when its fence has passed the append's seq.
async fn a_replayed_fence_on_an_unprovisioned_topic_is_not_provisioned(engine: Engine) {
    let server = common::server(engine, "replay").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, serde_json::json!({})).await;
    let control = server.control(&database).await;
    let key = FenceKey::new(0xfe);
    sqlx::query("INSERT INTO evbk__fence (fence_key, last_seq) VALUES (?, 10)")
        .bind(key.uuid())
        .execute(&control)
        .await
        .unwrap();

    let outcome = backend
        .append(&AppendRequest {
            fence: Fence {
                key,
                seq: FenceSeq::new(5),
            },
            topic: &topic(),
            events: &[keyed("k", 1000)],
            flush: Flush::Sync,
        })
        .await;

    assert_eq!(
        format!("{outcome:?}"),
        r#"Err(NotProvisioned { detail: "append: the topic has no log on this backend" })"#
    );
    backend.stop(CancellationToken::new()).await;
}

/// A retry blocked behind a hung transaction holding its fence row fails on
/// the 5 s row lock wait, `Unavailable`, instead of waiting for the server's
/// default.
async fn a_hung_fence_row_times_out_the_retry(engine: Engine) {
    let server = common::server(engine, "hung-fence").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, serde_json::json!({})).await;
    let control = server.control(&database).await;
    let (spec, _) = provision(backend.as_ref(), 1, KEEP).await;
    let key = FenceKey::new(0xfe);
    let mut hung = control.begin().await.unwrap();
    sqlx::query("INSERT INTO evbk__fence (fence_key, last_seq) VALUES (?, 1)")
        .bind(key.uuid())
        .execute(&mut *hung)
        .await
        .unwrap();

    // The server's default row lock wait is 50 s: past the bound, the plugin's
    // own 5 s is not in force.
    let outcome = tokio::time::timeout(
        BOUND,
        backend.append(&AppendRequest {
            fence: Fence {
                key,
                seq: FenceSeq::new(2),
            },
            topic: &spec.topic,
            events: &[keyed("k", 1000)],
            flush: Flush::Sync,
        }),
    )
    .await
    .expect("the retry fails on its own row lock wait");
    hung.rollback().await.unwrap();

    assert_eq!(
        format!("{outcome:?}"),
        r#"Err(Unavailable { reason: "append: database error 1205 (SQLSTATE HY000)", detail: "" })"#
    );
    backend.stop(CancellationToken::new()).await;
}

/// A re-provision of a written topic, with an open append's row lock and
/// metadata lock held, finishes at once: it takes no lock and runs no DDL on
/// a partition whose table exists.
async fn a_re_provision_does_not_wait_for_an_open_append(engine: Engine) {
    let server = common::server(engine, "re-provision").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, serde_json::json!({})).await;
    let control = server.control(&database).await;
    let (spec, claim) = provision(backend.as_ref(), 1, KEEP).await;
    append(backend.as_ref(), &spec.topic, &[keyed("k", 1000)]).await;
    let mut open = control.begin().await.unwrap();
    sqlx::query("UPDATE evbk__partition SET hwm = hwm WHERE topic_uuid = ?")
        .bind(topic_uuid(&spec.topic))
        .execute(&mut *open)
        .await
        .unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {}",
        table(&spec, 0)
    )))
    .execute(&mut *open)
    .await
    .unwrap();

    // A DDL behind the holder would wait for it (its metadata lock wait is
    // the server's default, a year): returning while it is held is the
    // check.
    let outcome = tokio::time::timeout(BOUND, backend.provision(&spec, &claim))
        .await
        .expect("the re-provision returns while the append is open");
    open.rollback().await.unwrap();

    assert_eq!(outcome.unwrap(), Provisioning::Ready { partitions: 1 });
    backend.stop(CancellationToken::new()).await;
}

/// The maintenance keeps segments ahead of the head: appends of a thousand
/// events each, a segment's worth, all land, with no gap, and room for one
/// more append is left in segments the server holds and the catalog records.
async fn segments_are_kept_ahead_of_appends(engine: Engine) {
    let server = common::server(engine, "ahead").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, thousands()).await;
    let control = server.control(&database).await;
    let (spec, _) = provision(backend.as_ref(), 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    for round in 0..3 {
        let batch: Vec<_> = (0..1000).map(|n| keyed("k", 1000 + n)).collect();
        let appended = append(backend.as_ref(), &spec.topic, &batch).await;
        assert_eq!(
            format!("{appended:?}"),
            format!(
                "Heads([Head {{ at: {at:?}, sequence: Sequence({}) }}])",
                1000 * (round + 1)
            )
        );
    }

    assert_eq!(
        read_all(backend.as_ref(), &at).await,
        (1..=3000).collect::<Vec<_>>()
    );
    let mut expected = segments(1, 3001, 1000, "sealed");
    expected.extend(segments(3001, 5001, 1000, "open"));
    settles(expected.clone(), || rows(&control, &spec, 0)).await;
    assert_eq!(held(&control, &spec, 0).await, names_of(&expected));
    backend.stop(CancellationToken::new()).await;
}

/// `(first_seq, state, bytes, newest_at)` of a segment row.
type SealRow = (i64, String, i64, Option<DateTime<Utc>>);

/// A full segment is sealed with the weight of its events - each body and a
/// 48-byte row allowance - and its last event's storage time; the open one
/// is not.
async fn the_sealer_weighs_full_segments(engine: Engine) {
    let server = common::server(engine, "sealer").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, thousands()).await;
    let control = server.control(&database).await;
    let (spec, _) = provision(backend.as_ref(), 1, KEEP).await;
    let batch: Vec<_> = (0..2500)
        .map(|n| keyed_padded("k", 1000 + n, usize::try_from(n % 7).unwrap() * 10))
        .collect();
    append_all(backend.as_ref(), &spec.topic, &batch).await;

    let measured: Vec<(i64, i64, DateTime<Utc>)> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT (seq - 1) DIV 1000, CAST(SUM(LENGTH(body)) + 48 * COUNT(*) AS SIGNED), MAX(seq_time) \
         FROM {} GROUP BY (seq - 1) DIV 1000 ORDER BY 1",
        table(&spec, 0)
    )))
    .fetch_all(&control)
    .await
    .unwrap();
    let expected: Vec<SealRow> = vec![
        (1, "sealed".to_owned(), measured[0].1, Some(measured[0].2)),
        (
            1001,
            "sealed".to_owned(),
            measured[1].1,
            Some(measured[1].2),
        ),
        (2001, "open".to_owned(), 0, None),
    ];
    settles(expected, || async {
        sqlx::query_as::<_, SealRow>(
            "SELECT first_seq, CAST(state AS CHAR), bytes, newest_at FROM evbk__segment \
             WHERE topic_uuid = ? AND first_seq < 3000 ORDER BY first_seq",
        )
        .bind(topic_uuid(&spec.topic))
        .fetch_all(&control)
        .await
        .unwrap()
    })
    .await;
    // The running weight the sealer works from agrees with the bodies.
    assert_eq!(
        figures(&control, &spec, 0).await.2,
        measured.iter().map(|m| m.1).sum::<i64>()
    );
    backend.stop(CancellationToken::new()).await;
}

/// By age: the sealed segments past `max_age` are expired with the floor and
/// their range partitions dropped; the open one stays, `Earliest` and
/// `Latest` follow, and a read from the start begins past the floor.
async fn retention_by_age_drops_whole_sealed_segments(engine: Engine) {
    let server = common::server(engine, "by-age").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, thousands()).await;
    let control = server.control(&database).await;
    let second = Retention {
        max_age: Some(Duration::from_secs(1)),
        max_bytes: None,
    };
    let (spec, _) = provision(backend.as_ref(), 1, second).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..2500).map(|n| keyed("k", 1000 + n)).collect();
    append_all(backend.as_ref(), &spec.topic, &batch).await;

    let expected = segments(2001, 5001, 1000, "open");
    settles(expected.clone(), || rows(&control, &spec, 0)).await;
    assert_eq!(held(&control, &spec, 0).await, names_of(&expected));
    assert_eq!(figures(&control, &spec, 0).await.0, 2000);
    assert_eq!(
        (
            backend.resolve(&at, Position::Earliest).await.unwrap(),
            backend.resolve(&at, Position::Latest).await.unwrap()
        ),
        (Sequence::assigned(2000), Sequence::assigned(2500))
    );
    assert_eq!(
        read_all(backend.as_ref(), &at).await,
        (2001..=2500).collect::<Vec<_>>()
    );
    backend.stop(CancellationToken::new()).await;
}

/// By bytes: the oldest sealed segments go while what stays still holds
/// `max_bytes`, and no further: with three sealed segments and half of one
/// open, a bound of one and a half segments drops two.
async fn retention_by_bytes_keeps_the_newest_bytes_within_the_bound(engine: Engine) {
    let server = common::server(engine, "by-bytes").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, thousands()).await;
    let control = server.control(&database).await;
    // Every event of `keyed` weighs the same: measure one first.
    let (probe, _) = provision(backend.as_ref(), 1, KEEP).await;
    append(backend.as_ref(), &probe.topic, &[keyed("k", 1000)]).await;
    let weight = figures(&control, &probe, 0).await.2;
    let bound = Retention {
        max_age: None,
        max_bytes: Some(u64::try_from(1500 * weight).unwrap()),
    };
    let (spec, _) = provision(backend.as_ref(), 1, bound).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..3500).map(|n| keyed("k", 1000 + n)).collect();
    append_all(backend.as_ref(), &spec.topic, &batch).await;

    settles((2000, 2000 * weight, 3500 * weight, 3500), || {
        figures(&control, &spec, 0)
    })
    .await;
    let mut expected = segments(2001, 3001, 1000, "sealed");
    expected.extend(segments(3001, 6001, 1000, "open"));
    settles(expected.clone(), || rows(&control, &spec, 0)).await;
    assert_eq!(held(&control, &spec, 0).await, names_of(&expected));
    assert_eq!(
        read_all(backend.as_ref(), &at).await,
        (2001..=3500).collect::<Vec<_>>()
    );
    backend.stop(CancellationToken::new()).await;
}

/// A transaction holding the table's metadata lock keeps `ADD PARTITION`
/// waiting; the DDL gives up after its short `lock_wait_timeout` and is
/// retried, so appends behind it wait seconds at most and none fails. Once
/// the holder ends, the segments are added and every row settled.
async fn a_ddl_waiting_on_a_metadata_lock_does_not_stop_appends(engine: Engine) {
    let server = common::server(engine, "mdl").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, thousands()).await;
    let control = server.control(&database).await;
    let (spec, _) = provision(backend.as_ref(), 1, KEEP).await;
    let mut holder = control.begin().await.unwrap();
    sqlx::query(AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM {}",
        table(&spec, 0)
    )))
    .execute(&mut *holder)
    .await
    .unwrap();

    // The first fills a segment and calls for room the held lock refuses; the
    // other two fill the segment left, behind the waiting DDL.
    for (round, events) in [1000_u64, 500, 500].into_iter().enumerate() {
        let batch: Vec<_> = (0..events).map(|n| keyed("k", 1000 + n)).collect();
        let appended = tokio::time::timeout(
            BOUND,
            backend.append(&AppendRequest {
                fence: Fence {
                    key: FenceKey::new(0xa0),
                    seq: FenceSeq::new(i64::try_from(round).unwrap() + 1),
                },
                topic: &spec.topic,
                events: &batch,
                flush: Flush::Sync,
            }),
        )
        .await
        .expect("an append behind the waiting DDL returns while the holder lives");
        assert!(matches!(appended, Ok(Appended::Heads(_))), "{appended:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Nothing above the provisioned two segments while the holder lives.
    assert_eq!(
        held(&control, &spec, 0).await,
        names_of(&segments(1, 2001, 1000, "open"))
    );
    holder.commit().await.unwrap();

    let mut expected = segments(1, 2001, 1000, "sealed");
    expected.extend(segments(2001, 4001, 1000, "open"));
    settles(expected.clone(), || rows(&control, &spec, 0)).await;
    assert_eq!(held(&control, &spec, 0).await, names_of(&expected));
    backend.stop(CancellationToken::new()).await;
}

/// Every place a crash can cut a DDL from its catalog write is settled by
/// the next instance's maintenance:
/// - p0 `adding` row whose partition was added: opened;
/// - p1 `adding` row whose partition was never added: deleted;
/// - p2 `expired` row (the floor raised) whose partition was not dropped:
///   dropped, row deleted;
/// - p3 `expired` row whose partition was dropped: row deleted;
/// - p4 a range partition added with no row: adopted.
async fn a_crash_between_ddl_and_catalog_is_settled(engine: Engine) {
    let server = common::server(engine, "crash").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, thousands()).await;
    let control = server.control(&database).await;
    let (spec, _) = provision(backend.as_ref(), 5, KEEP).await;
    for partition in [2, 3] {
        let key = key_for(partition, 5);
        let batch: Vec<_> = (0..1000).map(|n| keyed(&key, 1000 + n)).collect();
        append(backend.as_ref(), &spec.topic, &batch).await;
    }
    let mut written = segments(1, 1001, 1000, "sealed");
    written.extend(segments(1001, 3001, 1000, "open"));
    for partition in [2, 3] {
        settles(written.clone(), || rows(&control, &spec, partition)).await;
    }
    backend.stop(CancellationToken::new()).await;

    crash_in_every_place(&control, &spec).await;

    let backend = common::backend(&server.url, &database, thousands()).await;

    let fresh = segments(1, 2001, 1000, "open");
    let mut grown = fresh.clone();
    grown.extend(segments(2001, 3001, 1000, "open"));
    let after_drop = segments(1001, 3001, 1000, "open");
    for (partition, expected) in [
        (0, grown.clone()),
        (1, fresh),
        (2, after_drop.clone()),
        (3, after_drop),
        (4, grown),
    ] {
        settles(expected.clone(), || rows(&control, &spec, partition)).await;
        assert_eq!(held(&control, &spec, partition).await, names_of(&expected));
    }
    backend.stop(CancellationToken::new()).await;
}

/// Acknowledged appends survive the server's restart: a backend built afresh
/// reads every one back, and numbering continues above them.
async fn appended_events_survive_a_server_restart(engine: Engine) {
    let mut server = common::server(engine, "restart").await;
    if std::env::var("EB_TEST_MYSQL_URL").is_ok() || std::env::var("EB_TEST_MARIADB_URL").is_ok() {
        return;
    }
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, serde_json::json!({})).await;
    let (spec, _) = provision(backend.as_ref(), 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    for n in 0..3 {
        append(
            backend.as_ref(),
            &spec.topic,
            &[keyed("k", 1000 + n), keyed("k", 2000 + n)],
        )
        .await;
    }
    backend.stop(CancellationToken::new()).await;
    drop(backend);

    server.restart().await;
    // The server may still be recovering: `started` waits it out.
    let backend: Arc<dyn Backend> =
        common::backend(&server.url, &database, serde_json::json!({})).await;

    assert_eq!(
        read_all(backend.as_ref(), &at).await,
        (1..=6).collect::<Vec<_>>()
    );
    append(backend.as_ref(), &spec.topic, &[keyed("k", 3000)]).await;
    assert_eq!(
        read_all(backend.as_ref(), &at).await,
        (1..=7).collect::<Vec<_>>()
    );
    backend.stop(CancellationToken::new()).await;
}

/// With `innodb_flush_log_at_trx_commit` at 2 the backend stays
/// `Unavailable` by default, and starts with `require_durable_server: false`.
async fn a_server_that_does_not_flush_every_commit_is_refused_unless_allowed(engine: Engine) {
    let server = common::server(engine, "durability").await;
    if std::env::var("EB_TEST_MYSQL_URL").is_ok() || std::env::var("EB_TEST_MARIADB_URL").is_ok() {
        return;
    }
    let database = common::fresh_database();
    let control = server.control(&database).await;
    sqlx::query("SET GLOBAL innodb_flush_log_at_trx_commit = 2")
        .execute(&control)
        .await
        .unwrap();

    let strict = common::built(&server.url, &database, serde_json::json!({})).await;
    strict.start().await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let refused = strict
        .resolve(
            &TopicPartition::new(topic(), Partition::new(0)),
            Position::Latest,
        )
        .await;
    let lenient = common::backend(
        &server.url,
        &database,
        serde_json::json!({ "require_durable_server": false }),
    )
    .await;
    let (spec, _) = provision(lenient.as_ref(), 1, KEEP).await;
    let appended = append(lenient.as_ref(), &spec.topic, &[keyed("k", 1000)]).await;
    sqlx::query("SET GLOBAL innodb_flush_log_at_trx_commit = 1")
        .execute(&control)
        .await
        .unwrap();

    assert_eq!(
        format!("{refused:?}"),
        format!(
            "Err(Unavailable {{ reason: \"the mysql backend has not finished starting: the server does not make every commit durable (innodb_flush_log_at_trx_commit is {}); fix the server or set require_durable_server to false\", detail: \"\" }})",
            match engine {
                Engine::Mysql => "not 1",
                Engine::Mariadb => "neither 1 nor 3",
            }
        )
    );
    assert!(matches!(appended, Appended::Heads(_)), "{appended:?}");
    strict.stop(CancellationToken::new()).await;
    lenient.stop(CancellationToken::new()).await;
}

/// What [`a_crash_between_ddl_and_catalog_is_settled`] leaves behind, made by
/// hand on a stopped backend's database.
async fn crash_in_every_place(control: &MySqlPool, spec: &TopicSpec) {
    let topic = topic_uuid(&spec.topic);
    let run = |sql: String| {
        let control = control.clone();
        async move {
            sqlx::query(AssertSqlSafe(sql))
                .execute(&control)
                .await
                .unwrap();
        }
    };
    let adding = |partition: u32| {
        format!(
            "INSERT INTO evbk__segment (topic_uuid, partition_no, first_seq, end_seq, segment_name, state, changed_at) \
             VALUES (UNHEX('{}'), {partition}, 2001, 3001, 's00000000000007d1', 'adding', UTC_TIMESTAMP(6))",
            topic.simple()
        )
    };
    let add = |partition: u32| {
        format!(
            "ALTER TABLE {} ADD PARTITION (PARTITION s00000000000007d1 VALUES LESS THAN (3001))",
            table(spec, partition)
        )
    };
    let expire = |partition: u32| {
        format!(
            "UPDATE evbk__segment SET state = 'expired' WHERE topic_uuid = UNHEX('{0}') AND partition_no = {partition} AND first_seq = 1; \
             UPDATE evbk__retention SET floor_seq = 1000 WHERE topic_uuid = UNHEX('{0}') AND partition_no = {partition}",
            topic.simple()
        )
    };
    run(adding(0)).await;
    run(add(0)).await;
    run(adding(1)).await;
    for statement in expire(2).split("; ").chain(expire(3).split("; ")) {
        run(statement.to_owned()).await;
    }
    run(format!(
        "ALTER TABLE {} DROP PARTITION s0000000000000001",
        table(spec, 3)
    ))
    .await;
    run(add(4)).await;
}

/// An event stored while the server's clock ran ahead - here a row an
/// earlier append left an hour in the future - is not followed by an
/// earlier `seq_time`: storage time never goes back within a partition, so
/// `resolve(At)` may still skip whole sealed segments by their newest time.
async fn a_clock_step_back_does_not_lower_seq_time(engine: Engine) {
    let server = common::server(engine, "clock").await;
    let database = common::fresh_database();
    let backend = common::backend(&server.url, &database, serde_json::json!({})).await;
    let control = server.control(&database).await;
    let (spec, _) = provision(backend.as_ref(), 1, KEEP).await;
    let ahead = Utc::now() + chrono::Duration::hours(1);
    let ahead =
        ahead - chrono::Duration::nanoseconds(i64::from(ahead.timestamp_subsec_nanos() % 1000));
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {} (seq, seq_time, tenant_id, cum_bytes, body) VALUES (1, ?, ?, 0, '')",
        table(&spec, 0)
    )))
    .bind(ahead)
    .bind(Uuid::nil())
    .execute(&control)
    .await
    .unwrap();
    sqlx::query("UPDATE evbk__partition SET hwm = 1 WHERE topic_uuid = ?")
        .bind(topic_uuid(&spec.topic))
        .execute(&control)
        .await
        .unwrap();

    append(backend.as_ref(), &spec.topic, &[keyed("k", 1000)]).await;

    let times: Vec<(i64, DateTime<Utc>)> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT seq, seq_time FROM {} ORDER BY seq",
        table(&spec, 0)
    )))
    .fetch_all(&control)
    .await
    .unwrap();
    assert_eq!(times, [(1, ahead), (2, ahead)]);
    backend.stop(CancellationToken::new()).await;
}

/// `stop` with its deadline already passed still closes the pools, and a
/// startup still connecting when `stop` came installs nothing afterwards:
/// no connection to the plugin's database is left open.
async fn stop_closes_every_connection_even_past_its_deadline(engine: Engine) {
    let server = common::server(engine, "stop").await;
    let database = common::fresh_database();
    let observer = server.control("test").await;
    let open = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE DB = ?",
        )
        .bind(&database)
        .fetch_one(&observer)
        .await
        .unwrap()
    };
    let passed = CancellationToken::new();
    passed.cancel();
    // Kept alive: a dropped instance's pools close with it, a stopped one's
    // must close on their own.
    let mut stopped = Vec::new();
    for _ in 0..3 {
        let started = common::backend(&server.url, &database, serde_json::json!({})).await;
        let (spec, _) = provision(started.as_ref(), 1, KEEP).await;
        append(started.as_ref(), &spec.topic, &[keyed("k", 1000)]).await;
        started.stop(passed.clone()).await;
        let starting = common::built(&server.url, &database, serde_json::json!({})).await;
        starting.start().await.unwrap();
        starting.stop(passed.clone()).await;
        stopped.extend([started, starting]);
    }

    settles(0, open).await;
    drop(stopped);
}
