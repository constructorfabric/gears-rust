//! A chained producer's resubmissions racing its own originals (strategy §5
//! items 3 and 4): concurrent copies of one chain step store the event once and
//! answer every other copy `200` duplicate, and the producer's head only ever
//! moves forward. On `PostgreSQL` and `MySQL` (read committed / repeatable
//! read) two copies that both read the old head would otherwise both be
//! accepted.
//!
//! Each engine runs the gear database and the storage backend on one server
//! (`SQLite`: a temp file and the memory backend). Retries are off for this
//! binary (`.config/nextest.toml`), so a race that shows once fails the run.
//!
//! ```text
//! cargo test -p cf-gears-event-broker --features integration --test producer_chain_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use event_broker::test_support::{EventBrokerHarness, Json, StaticTypesRegistry};
use serde_json::json;

use support::matrix::{Engine, Matrix};
use support::stored::stored_ids;

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.chain.orders.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.chain.placed.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.chain.order.v1~";

/// Concurrent copies of one chain step.
const COPIES: usize = 8;
/// Chain steps in the head-monotonicity run.
const STEPS: u32 = 200;

async fn harness(matrix: &Matrix) -> Arc<EventBrokerHarness> {
    Arc::new(
        EventBrokerHarness::builder()
            .with_type_registry(StaticTypesRegistry::of(json!([
                { "id": TOPIC, "partitions": 1 },
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

/// Sends `body` from `copies` concurrent tasks; the `(status, body)` answers,
/// sorted.
async fn send_concurrently(
    harness: &Arc<EventBrokerHarness>,
    body: &serde_json::Value,
    copies: usize,
) -> Vec<(u16, String)> {
    let tasks: Vec<_> = (0..copies)
        .map(|_| {
            let harness = Arc::clone(harness);
            let body = body.clone();
            tokio::spawn(async move {
                let resp = harness
                    .api_v1()
                    .post_events()
                    .with_body(Json(&body))
                    .send()
                    .await;
                (resp.status().as_u16(), resp.text())
            })
        })
        .collect();
    let mut answers = Vec::new();
    for task in tasks {
        answers.push(task.await.expect("a publish task"));
    }
    answers.sort();
    answers
}

async fn register_chained(harness: &EventBrokerHarness) -> String {
    let registration = harness
        .api_v1()
        .post_producers()
        .with_body(Json(
            &json!({ "mode": "chained", "client_agent": "chain-test" }),
        ))
        .send()
        .await;
    registration.assert_status(201);
    let producer_id = registration.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(
        registration.json(),
        json!({ "id": producer_id, "mode": "chained", "client_agent": "chain-test" })
    );
    producer_id
}

/// Strategy §5 item 3: eight copies of `(0, 1)` at once, then eight copies of
/// `(1, 2)`. One of each is stored and answered `202`; the other seven are
/// `200` duplicates; nothing is `412` or `5xx`; the head is 2.
async fn concurrent_resubmission_stores_once(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "resubmission").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let producer_id = register_chained(&harness).await;

    let first = send_concurrently(
        &harness,
        &json!({
            "id": "0190f1a2-0000-7000-8000-000000000d01",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000000d01",
            "source": "chain-test",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1 },
        }),
        COPIES,
    )
    .await;
    assert_eq!(
        first,
        vec![
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (202, String::new()),
        ]
    );

    let second = send_concurrently(
        &harness,
        &json!({
            "id": "0190f1a2-0000-7000-8000-000000000d02",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000000d01",
            "source": "chain-test",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:01Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 1, "sequence": 2 },
        }),
        COPIES,
    )
    .await;
    assert_eq!(
        second,
        vec![
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (200, String::new()),
            (202, String::new()),
        ]
    );

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
            "client_agent": "chain-test",
            "topics": [{ "topic": TOPIC, "last_sequence": 2 }],
        })
    );
    assert_eq!(
        stored_ids(harness.backend().as_ref(), TOPIC, 1, 2).await,
        vec![
            "0190f1a2-0000-7000-8000-000000000d01".to_owned(),
            "0190f1a2-0000-7000-8000-000000000d02".to_owned(),
        ]
    );
}

/// Strategy §5 item 4: 200 chain steps, each `(n, n + 1)` sent together with
/// its own retry. Every step is accepted once and answered `200` once, the
/// head reads `n + 1` after the step, and the log holds every step once, in
/// chain order, with no gap.
async fn chained_head_never_moves_backwards(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "head-monotonic").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let producer_id = register_chained(&harness).await;
    let mut expected_log = Vec::new();

    for n in 0..STEPS {
        let id = format!("0190f1a2-0000-7000-8000-{:012x}", 0xe00 + n);
        let answers = send_concurrently(
            &harness,
            &json!({
                "id": id,
                "type": EVENT_TYPE,
                "tenant_id": "0190f1a2-0000-7000-a000-000000000e01",
                "source": "chain-test",
                "subject": "order-1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-10-01T12:00:00Z",
                "meta": { "version": 1, "producer_id": producer_id, "previous": n, "sequence": n + 1 },
            }),
            2,
        )
        .await;
        assert_eq!(
            answers,
            vec![(200, String::new()), (202, String::new())],
            "step {n}: the original is accepted once and its retry is a duplicate"
        );
        expected_log.push(id);

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
                "client_agent": "chain-test",
                "topics": [{ "topic": TOPIC, "last_sequence": n + 1 }],
            }),
            "step {n}: the head is the step just accepted"
        );
    }

    assert_eq!(
        stored_ids(harness.backend().as_ref(), TOPIC, 1, expected_log.len()).await,
        expected_log,
        "the log holds every step once, in chain order"
    );
}

/// Sends `body` and holds it right after it read the producer's head; sends
/// the same `body` again, which commits; releases the held copy. Returns both
/// answers, the held one first.
async fn copy_commits_while_original_is_held(
    harness: &Arc<EventBrokerHarness>,
    body: &serde_json::Value,
) -> ((u16, String), (u16, String)) {
    let pause = harness.repo().head_read_pause().clone();
    pause.arm();
    let held = {
        let harness = Arc::clone(harness);
        let body = body.clone();
        tokio::spawn(async move {
            let resp = harness
                .api_v1()
                .post_events()
                .with_body(Json(&body))
                .send()
                .await;
            (resp.status().as_u16(), resp.text())
        })
    };
    tokio::time::timeout(Duration::from_secs(10), pause.reached())
        .await
        .expect("the original reaches the point after its head read");
    let copy = harness
        .api_v1()
        .post_events()
        .with_body(Json(body))
        .send()
        .await;
    let copy = (copy.status().as_u16(), copy.text());
    pause.release();
    (held.await.expect("the held publish"), copy)
}

/// Deterministic form of the resubmission race: the original of a chain step
/// has read the head, and its copy commits before the original writes. The
/// original's head write then finds the head moved (on `SQLite` the stale
/// snapshot is refused instead), so it re-runs against the new head and is a
/// `200` duplicate - for the first step (head inserted) and for the next one
/// (head advanced).
async fn held_original_loses_to_its_copy_and_answers_duplicate(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "held-original").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let producer_id = register_chained(&harness).await;

    let first = copy_commits_while_original_is_held(
        &harness,
        &json!({
            "id": "0190f1a2-0000-7000-8000-000000000f01",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000000f01",
            "source": "chain-test",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1 },
        }),
    )
    .await;
    assert_eq!(first, ((200, String::new()), (202, String::new())));

    let second = copy_commits_while_original_is_held(
        &harness,
        &json!({
            "id": "0190f1a2-0000-7000-8000-000000000f02",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000000f01",
            "source": "chain-test",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:01Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 1, "sequence": 2 },
        }),
    )
    .await;
    assert_eq!(second, ((200, String::new()), (202, String::new())));

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
            "client_agent": "chain-test",
            "topics": [{ "topic": TOPIC, "last_sequence": 2 }],
        })
    );
    assert_eq!(
        stored_ids(harness.backend().as_ref(), TOPIC, 1, 2).await,
        vec![
            "0190f1a2-0000-7000-8000-000000000f01".to_owned(),
            "0190f1a2-0000-7000-8000-000000000f02".to_owned(),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_resubmission_stores_once_on_sqlite() {
    concurrent_resubmission_stores_once(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_resubmission_stores_once_on_postgres() {
    concurrent_resubmission_stores_once(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_resubmission_stores_once_on_mysql() {
    concurrent_resubmission_stores_once(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chained_head_never_moves_backwards_on_sqlite() {
    chained_head_never_moves_backwards(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chained_head_never_moves_backwards_on_postgres() {
    chained_head_never_moves_backwards(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chained_head_never_moves_backwards_on_mysql() {
    chained_head_never_moves_backwards(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_original_loses_to_its_copy_and_answers_duplicate_on_sqlite() {
    held_original_loses_to_its_copy_and_answers_duplicate(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_original_loses_to_its_copy_and_answers_duplicate_on_postgres() {
    held_original_loses_to_its_copy_and_answers_duplicate(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_original_loses_to_its_copy_and_answers_duplicate_on_mysql() {
    held_original_loses_to_its_copy_and_answers_duplicate(Engine::Mysql).await;
}
