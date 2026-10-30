//! The storage invariants (`tests/sql/invariants.sql`): they hold once a
//! workload has taken segments through every state, and each live invariant
//! is reported when broken by hand.

#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code: a failed expectation is the test failure"
)]

mod common;

use std::time::Duration;

use common::invariants::{Invariants, Mode, Violation};
use event_broker_sdk::backend::{
    AppendRequest, Backend, Fence, FenceKey, FenceSeq, Flush, KeyedEvent, LogClaim, LogId,
    Replacing, Retention, TopicSpec,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, GtsTypeId};
use sqlx::{AssertSqlSafe, PgPool};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const KEEP: Retention = Retention {
    max_age: None,
    max_bytes: None,
};

/// Segments of 50, and a rescan and cross-check twice a second.
fn small_segments() -> serde_json::Value {
    serde_json::json!({
        "segment": { "events": 50 },
        "maintenance": { "rescan_interval_ms": 500, "crosscheck_every": 1 },
    })
}

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new(&format!(
        "gts.cf.core.events.topic.v1~cf.pgtest.x.t{}.v1",
        &Uuid::new_v4().simple().to_string()[..12]
    ))
    .unwrap()
}

fn topic_uuid(topic: &GtsInstanceId) -> Uuid {
    event_broker_sdk::backend::support::topic_uuid(topic)
}

/// Event `n`, the same bytes every run: the figures a violation reports are
/// exact.
fn keyed(n: u64) -> KeyedEvent {
    KeyedEvent {
        key: format!("k{}", n % 7),
        event: Event {
            id: Uuid::from_u128(u128::from(n) + 1),
            type_id: GtsTypeId::new("gts.cf.core.events.event.v1~cf.pgtest.x.happened.v1~"),
            tenant_id: Uuid::from_u128(1),
            source: "pgtest".to_owned(),
            subject: format!("s-{n:06}"),
            subject_type: GtsTypeId::new("gts.cf.pgtest.x.thing.v1~"),
            occurred_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            trace_parent: None,
            data: Some(serde_json::json!({ "n": format!("{n:06}") })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        },
    }
}

async fn provision(backend: &dyn Backend, topic: &GtsInstanceId, retention: Retention) {
    backend
        .provision(
            &TopicSpec {
                topic: topic.clone(),
                partitions: 1,
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

/// Appends events `from..from + count` under fence `(key, seq)`: one fence key
/// spans every topic, so each topic a test writes takes its own.
async fn append(
    backend: &dyn Backend,
    topic: &GtsInstanceId,
    (key, seq): (u128, i64),
    from: u64,
    count: u64,
) {
    let events: Vec<KeyedEvent> = (from..from + count).map(keyed).collect();
    backend
        .append(&AppendRequest {
            fence: Fence {
                key: FenceKey::new(key),
                seq: FenceSeq::new(seq),
            },
            topic,
            events: &events,
            flush: Flush::Sync,
        })
        .await
        .expect("append");
}

/// Checks `schema` until it reports `expected`, for up to 30 s, then asserts
/// it.
async fn check_settles(
    invariants: &mut Invariants,
    schema: &str,
    mode: Mode,
    expected: &[Violation],
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let found = invariants.check(schema, mode).await.expect("check");
        if found == expected || tokio::time::Instant::now() >= deadline {
            assert_eq!(found, expected);
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `floor` of partition 0 of `topic`.
async fn floor(pool: &PgPool, schema: &str, topic: Uuid) -> i64 {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT floor FROM {schema}.evbk__retention WHERE topic_uuid = $1 AND partition = 0"
    )))
    .bind(topic)
    .fetch_one(pool)
    .await
    .expect("retention row")
}

/// `hwm` of every partition of `schema`.
async fn heads(pool: &PgPool, schema: &str) -> Vec<i64> {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT hwm FROM {schema}.evbk__partition ORDER BY hwm"
    )))
    .fetch_all(pool)
    .await
    .expect("partition rows")
}

/// Every relation of `schema` and its kind, sorted.
async fn objects(pool: &PgPool, schema: &str) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT c.relname::text, c.relkind::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 ORDER BY 1",
    )
    .bind(schema)
    .fetch_all(pool)
    .await
    .expect("catalog")
}

/// Segments go open, sealed, expired, detaching and dropped on a topic
/// bounded by age and one bounded by bytes; once appends stop and
/// maintenance settles, no invariant is broken, live or settled, and the
/// check leaves the schema as it found it.
#[tokio::test(flavor = "multi_thread")]
async fn invariants_hold_after_every_segment_state() {
    let server = common::server("invariants-hold").await;
    let schema = common::fresh_schema();
    let backend = common::backend(&server.url, &schema, small_segments()).await;
    let by_age = topic();
    let by_bytes = topic();
    let kept = topic();
    provision(
        backend.as_ref(),
        &by_age,
        Retention {
            max_age: Some(Duration::from_secs(2)),
            max_bytes: None,
        },
    )
    .await;
    provision(
        backend.as_ref(),
        &by_bytes,
        Retention {
            max_age: None,
            max_bytes: Some(40_000),
        },
    )
    .await;
    provision(backend.as_ref(), &kept, KEEP).await;
    let mut invariants = Invariants::connect(&server.url)
        .await
        .expect("check session");

    // A live finding counts once two checks in a row report it (design D2).
    let mut previous = Vec::new();
    for (seq, from) in (1..).zip((0..600).step_by(60)) {
        for (key, topic) in (1..).zip([&by_age, &by_bytes, &kept]) {
            append(backend.as_ref(), topic, (key, seq), from, 60).await;
        }
        let found = invariants.check(&schema, Mode::Live).await.expect("check");
        let confirmed: Vec<&Violation> = found.iter().filter(|v| previous.contains(*v)).collect();
        assert_eq!(confirmed, Vec::<&Violation>::new());
        previous = found;
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    check_settles(&mut invariants, &schema, Mode::Settled, &[]).await;
    let pool = PgPool::connect(&server.url).await.expect("control pool");
    // Proof the workload crossed every state: both bounded topics removed
    // segments, and S3 above found none still expired or detaching.
    assert_eq!(heads(&pool, &schema).await, vec![600, 600, 600]);
    assert!(floor(&pool, &schema, topic_uuid(&by_age)).await > 0);
    assert!(floor(&pool, &schema, topic_uuid(&by_bytes)).await > 0);

    // A fresh session defines the check again, finds nothing, and leaves the
    // schema as it found it. The backend stops first: its maintenance keeps
    // creating segments ahead of the head, which the schema comparison would
    // take for the check's. Live, not settled: the settled state is proven
    // above, and with no rescan left to expire them, segments keep ageing out
    // past the age bound.
    backend.stop(CancellationToken::new()).await;
    let before = objects(&pool, &schema).await;
    drop(invariants);
    let mut again = Invariants::connect(&server.url)
        .await
        .expect("check session");
    assert_eq!(
        again.check(&schema, Mode::Live).await.expect("check"),
        vec![]
    );
    drop(again);
    assert_eq!(objects(&pool, &schema).await, before);
}

/// A settled partition with sealed, open and empty segments, its backend
/// stopped so maintenance repairs nothing a test breaks.
struct Broken {
    _server: common::Server,
    schema: String,
    t: Uuid,
    parent: String,
    pool: PgPool,
    invariants: Invariants,
}

impl Broken {
    /// 120 events in segments of 50: `[1,51)` and `[51,101)` sealed,
    /// `[101,151)` open and partly written, then empty open segments.
    async fn new(purpose: &str) -> Self {
        let server = common::server(purpose).await;
        let schema = common::fresh_schema();
        let backend = common::backend(&server.url, &schema, small_segments()).await;
        let topic = topic();
        provision(backend.as_ref(), &topic, KEEP).await;
        append(backend.as_ref(), &topic, (1, 1), 0, 120).await;
        let mut invariants = Invariants::connect(&server.url)
            .await
            .expect("check session");
        check_settles(&mut invariants, &schema, Mode::Settled, &[]).await;
        backend.stop(CancellationToken::new()).await;
        let t = topic_uuid(&topic);
        let pool = PgPool::connect(&server.url).await.expect("control pool");
        Self {
            _server: server,
            schema,
            t,
            parent: format!("evbk__{}_p0", t.simple()),
            pool,
            invariants,
        }
    }

    /// The table name of the segment starting at `first`.
    fn segment(&self, first: i64) -> String {
        format!("{}_s{first:016x}", self.parent)
    }

    /// The first sequence of the partition's last segment row.
    async fn last_first_seq(&self) -> i64 {
        sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT max(first_seq) FROM {}.evbk__segment WHERE topic_uuid = $1",
            self.schema
        )))
        .bind(self.t)
        .fetch_one(&self.pool)
        .await
        .expect("segment rows")
    }

    async fn run(&self, sql: String) {
        sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&self.pool)
            .await
            .expect("break");
    }

    async fn live(&mut self) -> Vec<Violation> {
        self.invariants
            .check(&self.schema, Mode::Live)
            .await
            .expect("check")
    }

    fn violation(&self, invariant: &str, first_seq: Option<i64>, detail: &str) -> Violation {
        Violation {
            invariant: invariant.to_owned(),
            topic_uuid: Some(self.t),
            partition: Some(0),
            first_seq,
            detail: detail.to_owned(),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn l1_floor_below_zero() {
    let mut b = Broken::new("invariant-l1").await;
    b.run(format!(
        "UPDATE {}.evbk__retention SET floor = -1",
        b.schema
    ))
    .await;
    assert_eq!(
        b.live().await,
        vec![
            b.violation("L1", None, "floor=-1 hwm=120"),
            b.violation("L2", None, "count=120 expected=121 above_hwm=0"),
            b.violation("L3", Some(1), "range=[1,51) expected_first=0"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn l2_an_event_above_the_head() {
    let mut b = Broken::new("invariant-l2").await;
    b.run(format!(
        "INSERT INTO {s}.{p} SELECT 130, sequence_time, tenant_id, body FROM {s}.{p} WHERE sequence = 120",
        s = b.schema,
        p = b.parent
    ))
    .await;
    assert_eq!(
        b.live().await,
        vec![b.violation("L2", None, "count=120 expected=120 above_hwm=1")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn l3_a_segment_row_missing_between_two() {
    let mut b = Broken::new("invariant-l3").await;
    b.run(format!(
        "DELETE FROM {}.evbk__segment WHERE first_seq = 51",
        b.schema
    ))
    .await;
    let orphan = b.segment(51);
    assert_eq!(
        b.live().await,
        vec![
            b.violation("L3", Some(101), "range=[101,151) expected_first=51"),
            b.violation("L8", Some(51), &format!("table {orphan} has no row")),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn l4_an_expired_row_above_the_floor() {
    let mut b = Broken::new("invariant-l4").await;
    let last = b.last_first_seq().await;
    b.run(format!(
        "UPDATE {}.evbk__segment SET state = 'expired' WHERE first_seq = {last}",
        b.schema
    ))
    .await;
    assert_eq!(
        b.live().await,
        vec![b.violation(
            "L4",
            Some(last),
            &format!("expired end_seq={} floor=0", last + 50)
        )]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn l5_a_sealed_segment_with_another_time() {
    let mut b = Broken::new("invariant-l5").await;
    let (bytes, newest_at, later): (i64, String, String) = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT bytes, newest_at::text, (newest_at + interval '1 second')::text \
         FROM {}.evbk__segment WHERE first_seq = 1",
        b.schema
    )))
    .fetch_one(&b.pool)
    .await
    .unwrap();
    b.run(format!(
        "UPDATE {}.evbk__segment SET newest_at = newest_at + interval '1 second' WHERE first_seq = 1",
        b.schema
    ))
    .await;
    assert_eq!(
        b.live().await,
        vec![b.violation(
            "L5",
            Some(1),
            &format!(
                "end_seq=51 hwm=120 bytes={bytes} newest_at={later} last_event_at={newest_at}"
            )
        )]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn l6_an_open_segment_measured() {
    let mut b = Broken::new("invariant-l6").await;
    b.run(format!(
        "UPDATE {}.evbk__segment SET bytes = 1 WHERE first_seq = 101",
        b.schema
    ))
    .await;
    assert_eq!(
        b.live().await,
        vec![b.violation("L6", Some(101), "bytes=1 newest_at=null")]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn l7_a_segment_table_detached() {
    let mut b = Broken::new("invariant-l7").await;
    let last = b.last_first_seq().await;
    let table = b.segment(last);
    b.run(format!(
        "ALTER TABLE {s}.{p} DETACH PARTITION {s}.{table}",
        s = b.schema,
        p = b.parent
    ))
    .await;
    assert_eq!(
        b.live().await,
        vec![b.violation(
            "L7",
            Some(last),
            &format!("open table {table} not attached")
        )]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn l8_a_stray_segment_table() {
    let mut b = Broken::new("invariant-l8").await;
    let stray = b.segment(0x3e8);
    b.run(format!(
        "CREATE TABLE {s}.{stray} (LIKE {s}.{p})",
        s = b.schema,
        p = b.parent
    ))
    .await;
    assert_eq!(
        b.live().await,
        vec![b.violation("L8", Some(0x3e8), &format!("table {stray} has no row"))]
    );
}
