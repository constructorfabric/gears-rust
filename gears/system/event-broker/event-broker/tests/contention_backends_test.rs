//! A chained producer's first publish while other writers commit on the gear
//! database (strategy §5 items 1 and 2): the publish reads the producer's
//! head, decides, and writes, and a commit by anyone else in between must not
//! turn an accept into a `503`. On `SQLite` that commit invalidates the
//! publish's read snapshot (`SQLITE_BUSY_SNAPSHOT`), the intermittent `503`
//! CI saw on a first publish.
//!
//! Each engine runs the gear database and the storage backend on one server
//! (`SQLite`: a temp file and the memory backend). Retries are off for this
//! binary (`.config/nextest.toml`), so a race that shows once fails the run.
//!
//! ```text
//! cargo test -p cf-gears-event-broker --features integration --test contention_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use event_broker::test_support::{EventBrokerHarness, Json, StaticTypesRegistry};
use serde_json::json;

use support::matrix::{Engine, Matrix};
use support::stored::stored_ids;

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.contention.orders.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.contention.placed.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.contention.order.v1~";
const PARTITIONS: u32 = 4;

/// Fresh producers whose first publishes race in one round.
const PRODUCERS_PER_ROUND: u32 = 32;
/// Stateless publishers keeping the outbox committing during a round.
const BACKGROUND_WRITERS: u32 = 4;
const ROUNDS: u32 = 20;
/// The rounds stop early once this much wall clock is spent.
const BUDGET: Duration = Duration::from_secs(60);

async fn harness(matrix: &Matrix) -> Arc<EventBrokerHarness> {
    Arc::new(
        EventBrokerHarness::builder()
            .with_type_registry(StaticTypesRegistry::of(json!([
                { "id": TOPIC, "partitions": PARTITIONS },
                {
                    "id": EVENT_TYPE,
                    "topic": TOPIC,
                    "allowed_subject_types": [SUBJECT_TYPE],
                },
            ])))
            .with_gear_db(matrix.gear_db())
            .with_backend(matrix.backend().await)
            .build()
            .await,
    )
}

/// Strategy §5 item 1: rounds of 32 fresh chained producers each publishing
/// `(previous 0, sequence 1)` at once, while four writers keep sending
/// stateless publishes so the outbox keeps committing. Every publish is
/// accepted, every head ends at 1, and every accepted event is stored exactly
/// once.
async fn first_publishes_race_the_outbox_drain(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "first-publish-race").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let deadline = tokio::time::Instant::now() + BUDGET;
    let mut accepted_ids = Vec::new();

    for round in 0..ROUNDS {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let background: Vec<_> = (0..BACKGROUND_WRITERS)
            .map(|writer| {
                let harness = Arc::clone(&harness);
                let stop = Arc::clone(&stop);
                tokio::spawn(async move {
                    let mut sent = Vec::new();
                    let mut n = 0_u32;
                    while !stop.load(Ordering::SeqCst) {
                        let id = format!("0190f1a2-0000-7000-9{writer:03x}-{round:04x}{n:08x}");
                        let resp = harness
                            .api_v1()
                            .post_events()
                            .with_body(Json(&json!({
                                "id": id,
                                "type": EVENT_TYPE,
                                "tenant_id": format!("0190f1a2-0000-7000-b000-{n:012x}"),
                                "source": "contention-test",
                                "subject": "order-stateless",
                                "subject_type": SUBJECT_TYPE,
                                "occurred_at": "2026-10-01T12:00:00Z",
                            })))
                            .send()
                            .await;
                        sent.push((id, resp.status().as_u16(), resp.text()));
                        n += 1;
                    }
                    sent
                })
            })
            .collect();

        let chained: Vec<_> = (0..PRODUCERS_PER_ROUND)
            .map(|producer| {
                let harness = Arc::clone(&harness);
                tokio::spawn(async move {
                    let registration = harness
                        .api_v1()
                        .post_producers()
                        .with_body(Json(
                            &json!({ "mode": "chained", "client_agent": "contention-test" }),
                        ))
                        .send()
                        .await;
                    registration.assert_status(201);
                    let producer_id = registration.json()["id"].as_str().unwrap().to_owned();
                    assert_eq!(
                        registration.json(),
                        json!({
                            "id": producer_id,
                            "mode": "chained",
                            "client_agent": "contention-test",
                        })
                    );
                    let id = format!("0190f1a2-0000-7000-8000-{round:04x}{producer:08x}");
                    let resp = harness
                        .api_v1()
                        .post_events()
                        .with_body(Json(&json!({
                            "id": id,
                            "type": EVENT_TYPE,
                            "tenant_id": format!("0190f1a2-0000-7000-a000-{producer:012x}"),
                            "source": "contention-test",
                            "subject": "order-1",
                            "subject_type": SUBJECT_TYPE,
                            "occurred_at": "2026-10-01T12:00:00Z",
                            "meta": {
                                "version": 1,
                                "producer_id": producer_id,
                                "previous": 0,
                                "sequence": 1,
                            },
                        })))
                        .send()
                        .await;
                    (producer_id, id, resp.status().as_u16(), resp.text())
                })
            })
            .collect();

        let mut first_publishes = Vec::new();
        for task in chained {
            first_publishes.push(task.await.expect("a first-publish task"));
        }
        stop.store(true, Ordering::SeqCst);
        let mut stateless = Vec::new();
        for task in background {
            stateless.extend(task.await.expect("a background writer"));
        }

        assert_eq!(
            first_publishes
                .iter()
                .map(|(_, id, status, body)| (id.clone(), *status, body.clone()))
                .collect::<Vec<_>>(),
            first_publishes
                .iter()
                .map(|(_, id, _, _)| (id.clone(), 202, String::new()))
                .collect::<Vec<_>>(),
            "round {round}: every first publish is accepted"
        );
        assert_eq!(
            stateless,
            stateless
                .iter()
                .map(|(id, _, _)| (id.clone(), 202, String::new()))
                .collect::<Vec<_>>(),
            "round {round}: every stateless publish is accepted"
        );

        for (producer_id, _, _, _) in &first_publishes {
            let cursors = harness
                .api_v1()
                .get_producer_cursors(producer_id)
                .send()
                .await;
            cursors.assert_status(200);
            assert_eq!(
                cursors.json(),
                json!({
                    "producer_id": producer_id,
                    "client_agent": "contention-test",
                    "topics": [{ "topic": TOPIC, "last_sequence": 1 }],
                })
            );
        }
        accepted_ids.extend(first_publishes.into_iter().map(|(_, id, _, _)| id));
        accepted_ids.extend(stateless.into_iter().map(|(id, _, _)| id));
    }

    let mut stored = stored_ids(
        harness.backend().as_ref(),
        TOPIC,
        PARTITIONS,
        accepted_ids.len(),
    )
    .await;
    stored.sort();
    accepted_ids.sort();
    assert_eq!(stored, accepted_ids, "every accepted event is stored once");
}

/// Strategy §5 item 2, deterministic: the first publish is held right after it
/// read the producer's head; another publish commits on a second connection;
/// the held publish then writes. It is accepted, not refused with `503`.
async fn first_publish_with_a_committed_writer_between_read_and_write(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "committed-writer").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let registration = harness
        .api_v1()
        .post_producers()
        .with_body(Json(
            &json!({ "mode": "chained", "client_agent": "contention-test" }),
        ))
        .send()
        .await;
    registration.assert_status(201);
    let producer_id = registration.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(
        registration.json(),
        json!({ "id": producer_id, "mode": "chained", "client_agent": "contention-test" })
    );

    let pause = harness.repo().head_read_pause().clone();
    pause.arm();
    let held = {
        let harness = Arc::clone(&harness);
        let producer_id = producer_id.clone();
        tokio::spawn(async move {
            let resp = harness
                .api_v1()
                .post_events()
                .with_body(Json(&json!({
                    "id": "0190f1a2-0000-7000-8000-000000000c01",
                    "type": EVENT_TYPE,
                    "tenant_id": "0190f1a2-0000-7000-a000-000000000c01",
                    "source": "contention-test",
                    "subject": "order-1",
                    "subject_type": SUBJECT_TYPE,
                    "occurred_at": "2026-10-01T12:00:00Z",
                    "meta": { "version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1 },
                })))
                .send()
                .await;
            (resp.status().as_u16(), resp.text())
        })
    };
    tokio::time::timeout(Duration::from_secs(10), pause.reached())
        .await
        .expect("the first publish reaches the point after its head read");

    let writer = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000000c02",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000000c02",
            "source": "contention-test",
            "subject": "order-stateless",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
        })))
        .send()
        .await;
    assert_eq!(
        (writer.status().as_u16(), writer.text()),
        (202, String::new())
    );
    pause.release();

    assert_eq!(held.await.expect("the held publish"), (202, String::new()));
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id)
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "producer_id": producer_id,
            "client_agent": "contention-test",
            "topics": [{ "topic": TOPIC, "last_sequence": 1 }],
        })
    );
    let mut stored = stored_ids(harness.backend().as_ref(), TOPIC, PARTITIONS, 2).await;
    stored.sort();
    assert_eq!(
        stored,
        vec![
            "0190f1a2-0000-7000-8000-000000000c01".to_owned(),
            "0190f1a2-0000-7000-8000-000000000c02".to_owned(),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_publishes_race_the_outbox_drain_on_sqlite() {
    first_publishes_race_the_outbox_drain(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_publishes_race_the_outbox_drain_on_postgres() {
    first_publishes_race_the_outbox_drain(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_publishes_race_the_outbox_drain_on_mysql() {
    first_publishes_race_the_outbox_drain(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_publish_with_a_committed_writer_between_read_and_write_on_sqlite() {
    first_publish_with_a_committed_writer_between_read_and_write(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_publish_with_a_committed_writer_between_read_and_write_on_postgres() {
    first_publish_with_a_committed_writer_between_read_and_write(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_publish_with_a_committed_writer_between_read_and_write_on_mysql() {
    first_publish_with_a_committed_writer_between_read_and_write(Engine::Mysql).await;
}
