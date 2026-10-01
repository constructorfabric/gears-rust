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
    AppendRequest, Appended, Backend, BackendError, Fence, FenceKey, FenceSeq, Flush, KeyedEvent,
    LogClaim, LogId, Partition, Replacing, Retention, TopicPartition, TopicSpec,
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

/// `provision` creates one partitioned table and one first segment per
/// partition, named per the design, and a counter row for each.
#[tokio::test(flavor = "multi_thread")]
async fn provision_creates_parents_and_first_segments() {
    let server = common::server("provision-creates-parents-and-first-segments").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({ "segment": { "events": 100 } }),
        CancellationToken::new(),
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
            format!("evbk__{t}_p1"),
            format!("evbk__{t}_p1_s0000000000000001"),
        ]
    );
    let counters: Vec<(i32, i64, i64, i64, i64)> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT partition, p.last_sequence, p.appended_bytes, r.floor, r.removed_bytes \
         FROM {schema}.evbk__partition p JOIN {schema}.evbk__retention r USING (topic_uuid, partition) \
         ORDER BY partition"
    )))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(counters, [(0, 0, 0, 0, 0), (1, 0, 0, 0, 0)]);
}

/// More partitions than the table names can carry is refused.
#[tokio::test(flavor = "multi_thread")]
async fn too_many_partitions_is_refused() {
    let server = common::server("too-many-partitions-is-refused").await;
    let backend = common::backend(
        &server.url,
        &common::fresh_schema(),
        serde_json::json!({}),
        CancellationToken::new(),
    )
    .await;
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
    common::backend(
        &server.url,
        &schema,
        serde_json::json!({}),
        CancellationToken::new(),
    )
    .await;
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
    let backend =
        event_broker_sdk::backend::Provider::build(&drifted, &settings, CancellationToken::new())
            .await
            .unwrap();
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

/// Once half of a segment is used, the next one is attached ahead of the
/// head, and appends run on through it: with 10-sequence segments, 11..20 is
/// due from sequence 6 and 21..30 from 16.
#[tokio::test(flavor = "multi_thread")]
async fn segments_are_created_ahead() {
    let server = common::server("segments-are-created-ahead").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({ "segment": { "events": 10 } }),
        CancellationToken::new(),
    )
    .await;
    let topic = topic();
    provision(backend.as_ref(), &topic, 1, KEEP).await;
    let key = FenceKey::new(7);
    let t = topic_uuid(&topic);
    let pool = control(&server.url).await;

    for seq in 1..=25 {
        let appended = append(
            backend.as_ref(),
            &topic,
            key,
            seq,
            &[keyed(seq.cast_unsigned())],
        )
        .await;
        if appended.is_err() {
            // Behind for a moment: the outbox would retry.
            tokio::time::sleep(Duration::from_millis(300)).await;
            append(
                backend.as_ref(),
                &topic,
                key,
                seq,
                &[keyed(seq.cast_unsigned())],
            )
            .await
            .expect("an append finds its segment after one maintenance pass");
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    assert_eq!(
        relations(&pool, &schema, &format!("evbk__{t}_p0_s")).await,
        [
            format!("evbk__{t}_p0_s0000000000000001"),
            format!("evbk__{t}_p0_s000000000000000b"),
            // 31..40 is due only from sequence 26, half of 21..30.
            format!("evbk__{t}_p0_s0000000000000015"),
        ]
    );
    let at = TopicPartition::new(topic, Partition::new(0));
    assert_eq!(
        backend.resolve(&at, Position::Latest).await.unwrap(),
        Sequence::assigned(25)
    );
}

/// An expired segment is detached and dropped and the rest of the expired
/// prefix deleted; the counters follow, and `Latest` stays.
#[tokio::test(flavor = "multi_thread")]
async fn expired_segments_are_dropped() {
    let server = common::server("expired-segments-are-dropped").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({ "segment": { "events": 4 } }),
        CancellationToken::new(),
    )
    .await;
    let topic = topic();
    let short = Retention {
        max_age: Some(Duration::from_secs(1)),
        max_bytes: None,
    };
    provision(backend.as_ref(), &topic, 1, short).await;
    let t = topic_uuid(&topic);
    let pool = control(&server.url).await;
    let key = FenceKey::new(9);
    for seq in 1..=6 {
        loop {
            if append(
                backend.as_ref(),
                &topic,
                key,
                seq,
                &[keyed(seq.cast_unsigned())],
            )
            .await
            .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let segments = relations(&pool, &schema, &format!("evbk__{t}_p0_s")).await;
        // (stored events, stored bytes), derived as the plugin derives them.
        let counters: (i64, i64) = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT p.last_sequence - r.floor, p.appended_bytes - r.removed_bytes \
             FROM {schema}.evbk__partition p JOIN {schema}.evbk__retention r USING (topic_uuid, partition)"
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        if !segments.contains(&format!("evbk__{t}_p0_s0000000000000001")) && counters == (0, 0) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "still {segments:?} with counters {counters:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let at = TopicPartition::new(topic, Partition::new(0));
    assert_eq!(
        backend.resolve(&at, Position::Latest).await.unwrap(),
        Sequence::assigned(6)
    );
    assert_eq!(
        backend.resolve(&at, Position::Earliest).await.unwrap(),
        Sequence::assigned(6)
    );
}

/// A retry blocked on a hung transaction's fence row gives up on
/// `lock_timeout` (5 s) instead of waiting forever, and the outbox retries.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_fence_row_times_out_the_retry() {
    let server = common::server("a-hung-fence-row-times-out-the-retry").await;
    let schema = common::fresh_schema();
    let backend = common::backend(
        &server.url,
        &schema,
        serde_json::json!({}),
        CancellationToken::new(),
    )
    .await;
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
    let watcher: Arc<dyn Backend> = common::backend(
        &server.url,
        &schema,
        serde_json::json!({}),
        CancellationToken::new(),
    )
    .await;
    let writer = common::backend(
        &server.url,
        &schema,
        serde_json::json!({}),
        CancellationToken::new(),
    )
    .await;
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
