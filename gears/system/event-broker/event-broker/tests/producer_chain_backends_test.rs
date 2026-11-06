//! A chained producer's resubmissions racing its own originals (strategy §5
//! items 3 and 4): concurrent copies of one chain step store the event once and
//! answer every other copy `200` duplicate, and the producer's head only ever
//! moves forward. On `PostgreSQL` and `MySQL` (read committed / repeatable
//! read) two copies that both read the old head would otherwise both be
//! accepted. Batches follow the run rules of feature 0001 §5.2 on every
//! engine: a rejected batch stores nothing, and batches locking the same heads
//! in opposite orders each store once.
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

use event_broker::test_support::{EventBrokerHarness, Json, PausePoint, StaticTypesRegistry};
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

/// Sends `body` and holds it at `pause`; sends the same `body` again, which
/// commits; releases the held copy. Returns both answers, the held one first.
/// The copy is bounded, so a copy that blocks on the held original's lock is
/// an `Err` instead of a hang.
async fn copy_commits_while_original_is_held(
    harness: &Arc<EventBrokerHarness>,
    pause: &PausePoint,
    body: &serde_json::Value,
) -> (
    (u16, String),
    Result<(u16, String), tokio::time::error::Elapsed>,
) {
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
        .expect("the original reaches the pause point");
    let copy = tokio::time::timeout(Duration::from_secs(10), async {
        let copy = harness
            .api_v1()
            .post_events()
            .with_body(Json(body))
            .send()
            .await;
        (copy.status().as_u16(), copy.text())
    })
    .await;
    pause.release();
    (held.await.expect("the held publish"), copy)
}

/// Deterministic form of the resubmission race: the original of a chain step
/// is about to read the head, and its copy commits first. The original then
/// reads the head its copy wrote - under a row lock on `PostgreSQL` and
/// `MySQL`, under the write lock on `SQLite` - and is a `200` duplicate, for
/// the first step (head inserted) and for the next one (head advanced).
async fn held_original_loses_to_its_copy_and_answers_duplicate(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "held-original").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let producer_id = register_chained(&harness).await;

    let first = copy_commits_while_original_is_held(
        &harness,
        harness.repo().before_head_read_pause(),
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
    assert_eq!(first, ((200, String::new()), Ok((202, String::new()))));

    let second = copy_commits_while_original_is_held(
        &harness,
        harness.repo().before_head_read_pause(),
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
    assert_eq!(second, ((200, String::new()), Ok((202, String::new()))));

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

/// Sends `body` to `POST /v1/events:batch`; the `(status, body)` answer.
async fn send_batch(harness: &EventBrokerHarness, body: &serde_json::Value) -> (u16, String) {
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(body))
        .send()
        .await;
    (resp.status().as_u16(), resp.text())
}

/// Rounds of the opposite-order batch race, each with fresh producers: the two
/// batches interleave their head locks on only some rounds, so a single round
/// rarely reaches the deadlock-and-retry path.
const OPPOSITE_ORDER_ROUNDS: u32 = 20;

/// Two batches carrying the same first events of two producers, `[P1, Q1]`
/// and `[Q1, P1]`, at once. They lock the two heads in opposite orders; where
/// that deadlocks, the database aborts one and its retry succeeds. One batch
/// stores both events and answers `202`; the other finds both a duplicate and
/// answers `200`. Each event is stored once and both heads end at 1.
async fn opposite_order_batches_each_store_once(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "opposite-order").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let mut expected_log = Vec::new();

    for round in 0..OPPOSITE_ORDER_ROUNDS {
        let p = register_chained(&harness).await;
        let q = register_chained(&harness).await;
        let p_id = format!("0190f1a2-0000-7000-8000-{:012x}", 0x1_0000 + 2 * round);
        let q_id = format!("0190f1a2-0000-7000-8000-{:012x}", 0x1_0001 + 2 * round);
        let p1 = json!({
            "id": p_id,
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000001001",
            "source": "chain-test",
            "subject": "order-p",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": p, "previous": 0, "sequence": 1 },
        });
        let q1 = json!({
            "id": q_id,
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000001001",
            "source": "chain-test",
            "subject": "order-q",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": q, "previous": 0, "sequence": 1 },
        });
        let forward = {
            let harness = Arc::clone(&harness);
            let body = json!({ "events": [p1.clone(), q1.clone()] });
            tokio::spawn(async move { send_batch(&harness, &body).await })
        };
        let backward = {
            let harness = Arc::clone(&harness);
            let body = json!({ "events": [q1, p1] });
            tokio::spawn(async move { send_batch(&harness, &body).await })
        };
        let mut answers = vec![
            forward.await.expect("the forward batch"),
            backward.await.expect("the backward batch"),
        ];
        answers.sort();
        assert_eq!(
            answers,
            vec![(200, String::new()), (202, String::new())],
            "round {round}: one batch stores both events, the other is a duplicate"
        );
        for producer_id in [&p, &q] {
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
                    "client_agent": "chain-test",
                    "topics": [{ "topic": TOPIC, "last_sequence": 1 }],
                }),
                "round {round}: the head is 1"
            );
        }
        expected_log.push(p_id);
        expected_log.push(q_id);
    }

    // Which batch won decides the order of a round's two events, so the log
    // is compared as a set of ids, each stored once.
    let mut stored = stored_ids(harness.backend().as_ref(), TOPIC, 1, expected_log.len()).await;
    stored.sort();
    expected_log.sort();
    assert_eq!(stored, expected_log, "every event is stored once");
}

/// The first-head race, held open: the original of a first chain step reads
/// the head absent and is held there, before it writes; its copy reads the
/// same absent head and commits the first head. On `PostgreSQL` and `MySQL`
/// (read committed) an absent head locks nothing, so the copy does not wait.
/// The released original's `INSERT` of the head then fails on the primary
/// key, the attempt rolls back with its outbox row, and the retry reads the
/// head its copy wrote: a `200` duplicate. `SQLite` has no variant: there the
/// original holds the write lock from before its read, so the copy cannot
/// commit in this window (`held_original_*_on_sqlite` covers the copy that
/// commits first).
async fn an_original_that_read_an_absent_head_retries_after_its_copy_commits(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "absent-head-retry").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let producer_id = register_chained(&harness).await;

    let answers = copy_commits_while_original_is_held(
        &harness,
        harness.repo().after_head_read_pause(),
        &json!({
            "id": "0190f1a2-0000-7000-8000-000000001301",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
            "source": "chain-test",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1 },
        }),
    )
    .await;
    assert_eq!(answers, ((200, String::new()), Ok((202, String::new()))));

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
            "topics": [{ "topic": TOPIC, "last_sequence": 1 }],
        })
    );
    publish_sentinel(&harness, "0190f1a2-0000-7000-8000-0000000013ff").await;
    assert_eq!(
        stored_ids(harness.backend().as_ref(), TOPIC, 1, 2).await,
        vec![
            "0190f1a2-0000-7000-8000-000000001301".to_owned(),
            "0190f1a2-0000-7000-8000-0000000013ff".to_owned(),
        ],
        "the event is stored once"
    );
}

/// Two producers whose heads are both at 1, and two batches carrying their
/// next events, `[P2, Q2]` and `[Q2, P2]`, at once. Both heads exist, so each
/// batch row-locks them in its own order; where that deadlocks, the database
/// aborts one and its retry finds both events a duplicate. One batch answers
/// `202`, the other `200`; both heads end at 2 and each event is stored once.
async fn opposite_order_batches_over_existing_heads_store_once(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "opposite-order-existing").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let p = register_chained(&harness).await;
    let q = register_chained(&harness).await;

    let seed = send_batch(
        &harness,
        &json!({ "events": [
            {
                "id": "0190f1a2-0000-7000-8000-000000001401",
                "type": EVENT_TYPE,
                "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
                "source": "chain-test",
                "subject": "order-p",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-10-01T12:00:00Z",
                "meta": { "version": 1, "producer_id": p, "previous": 0, "sequence": 1 },
            },
            {
                "id": "0190f1a2-0000-7000-8000-000000001402",
                "type": EVENT_TYPE,
                "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
                "source": "chain-test",
                "subject": "order-q",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-10-01T12:00:00Z",
                "meta": { "version": 1, "producer_id": q, "previous": 0, "sequence": 1 },
            },
        ] }),
    )
    .await;
    assert_eq!(seed, (202, String::new()));

    let p2 = json!({
        "id": "0190f1a2-0000-7000-8000-000000001403",
        "type": EVENT_TYPE,
        "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
        "source": "chain-test",
        "subject": "order-p",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-10-01T12:00:01Z",
        "meta": { "version": 1, "producer_id": p, "previous": 1, "sequence": 2 },
    });
    let q2 = json!({
        "id": "0190f1a2-0000-7000-8000-000000001404",
        "type": EVENT_TYPE,
        "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
        "source": "chain-test",
        "subject": "order-q",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-10-01T12:00:01Z",
        "meta": { "version": 1, "producer_id": q, "previous": 1, "sequence": 2 },
    });
    let forward = {
        let harness = Arc::clone(&harness);
        let body = json!({ "events": [p2.clone(), q2.clone()] });
        tokio::spawn(async move { send_batch(&harness, &body).await })
    };
    let backward = {
        let harness = Arc::clone(&harness);
        let body = json!({ "events": [q2, p2] });
        tokio::spawn(async move { send_batch(&harness, &body).await })
    };
    let mut answers = vec![
        forward.await.expect("the forward batch"),
        backward.await.expect("the backward batch"),
    ];
    answers.sort();
    assert_eq!(answers, vec![(200, String::new()), (202, String::new())]);

    let p_cursors = harness.api_v1().get_producer_cursors(&p).send().await;
    p_cursors.assert_status(200);
    assert_eq!(
        p_cursors.json(),
        json!({
            "producer_id": p,
            "client_agent": "chain-test",
            "topics": [{ "topic": TOPIC, "last_sequence": 2 }],
        })
    );
    let q_cursors = harness.api_v1().get_producer_cursors(&q).send().await;
    q_cursors.assert_status(200);
    assert_eq!(
        q_cursors.json(),
        json!({
            "producer_id": q,
            "client_agent": "chain-test",
            "topics": [{ "topic": TOPIC, "last_sequence": 2 }],
        })
    );
    publish_sentinel(&harness, "0190f1a2-0000-7000-8000-0000000014ff").await;
    // Which batch won decides the order of P2 and Q2, so the two are compared
    // as a set; the seed comes first and the sentinel last.
    let stored = stored_ids(harness.backend().as_ref(), TOPIC, 1, 5).await;
    let [seed_p, seed_q, second_a, second_b, sentinel] = stored.as_slice() else {
        panic!("expected five stored events, got {stored:?}");
    };
    assert_eq!(
        [seed_p.as_str(), seed_q.as_str(), sentinel.as_str()],
        [
            "0190f1a2-0000-7000-8000-000000001401",
            "0190f1a2-0000-7000-8000-000000001402",
            "0190f1a2-0000-7000-8000-0000000014ff",
        ]
    );
    let mut second = [second_a.as_str(), second_b.as_str()];
    second.sort_unstable();
    assert_eq!(
        second,
        [
            "0190f1a2-0000-7000-8000-000000001403",
            "0190f1a2-0000-7000-8000-000000001404",
        ],
        "each event is stored once"
    );
}

/// Publishes one stateless event with the same tenant - so the same partition
/// key - as the test's other events: anything a rejected batch left behind
/// drains ahead of it.
async fn publish_sentinel(harness: &EventBrokerHarness, id: &str) {
    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": id,
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
            "source": "chain-test",
            "subject": "order-sentinel",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:09Z",
        })))
        .send()
        .await;
    assert_eq!((resp.status().as_u16(), resp.text()), (202, String::new()));
}

/// Feature 0001 §5.2 R3: a chained run whose second event does not link to
/// the first is refused `400 malformed_run` before any head is read, and
/// stores nothing.
async fn a_rejected_batch_stores_nothing(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "malformed-run").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let producer_id = register_chained(&harness).await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": "0190f1a2-0000-7000-8000-000000001101",
                    "type": EVENT_TYPE,
                    "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
                    "source": "chain-test",
                    "subject": "order-1",
                    "subject_type": SUBJECT_TYPE,
                    "occurred_at": "2026-10-01T12:00:00Z",
                    "meta": { "version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1 },
                },
                {
                    "id": "0190f1a2-0000-7000-8000-000000001102",
                    "type": EVENT_TYPE,
                    "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
                    "source": "chain-test",
                    "subject": "order-1",
                    "subject_type": SUBJECT_TYPE,
                    "occurred_at": "2026-10-01T12:00:01Z",
                    "meta": { "version": 1, "producer_id": producer_id, "previous": 2, "sequence": 3 },
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "field_violations": [{
                    "field": "events[1].meta",
                    "description": "does not follow the same producer's event before it in the batch",
                    "reason": "malformed_run",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id)
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "producer_id": producer_id, "client_agent": "chain-test", "topics": [] })
    );
    publish_sentinel(&harness, "0190f1a2-0000-7000-8000-0000000011ff").await;
    assert_eq!(
        stored_ids(harness.backend().as_ref(), TOPIC, 1, 1).await,
        vec!["0190f1a2-0000-7000-8000-0000000011ff".to_owned()],
        "the rejected batch stores nothing"
    );
}

/// Feature 0001 §5.2 "Outcome": run P would seed its chain, run Q's
/// `previous` does not link to its head. The whole batch is refused `412`,
/// naming Q's run and its committed head, and nothing of it is stored.
async fn a_stale_second_run_refuses_the_whole_batch(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "cross-run-412").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let p = register_chained(&harness).await;
    let q = register_chained(&harness).await;

    let seed = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000001200",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
            "source": "chain-test",
            "subject": "order-q",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": q, "previous": 0, "sequence": 1 },
        })))
        .send()
        .await;
    assert_eq!((seed.status().as_u16(), seed.text()), (202, String::new()));

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": "0190f1a2-0000-7000-8000-000000001201",
                    "type": EVENT_TYPE,
                    "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
                    "source": "chain-test",
                    "subject": "order-p",
                    "subject_type": SUBJECT_TYPE,
                    "occurred_at": "2026-10-01T12:00:01Z",
                    "meta": { "version": 1, "producer_id": p, "previous": 0, "sequence": 1 },
                },
                {
                    "id": "0190f1a2-0000-7000-8000-000000001202",
                    "type": EVENT_TYPE,
                    "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
                    "source": "chain-test",
                    "subject": "order-q",
                    "subject_type": SUBJECT_TYPE,
                    "occurred_at": "2026-10-01T12:00:02Z",
                    "meta": { "version": 1, "producer_id": q, "previous": 5, "sequence": 6 },
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(412);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 412,
            "detail": "Operation precondition not met",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": TOPIC,
                "violations": [{
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": format!("index=1 producer_id={q} topic={TOPIC} last_sequence=1"),
                }],
            },
        })
    );
    let p_cursors = harness.api_v1().get_producer_cursors(&p).send().await;
    p_cursors.assert_status(200);
    assert_eq!(
        p_cursors.json(),
        json!({ "producer_id": p, "client_agent": "chain-test", "topics": [] })
    );
    let q_cursors = harness.api_v1().get_producer_cursors(&q).send().await;
    q_cursors.assert_status(200);
    assert_eq!(
        q_cursors.json(),
        json!({
            "producer_id": q,
            "client_agent": "chain-test",
            "topics": [{ "topic": TOPIC, "last_sequence": 1 }],
        })
    );
    publish_sentinel(&harness, "0190f1a2-0000-7000-8000-0000000012ff").await;
    assert_eq!(
        stored_ids(harness.backend().as_ref(), TOPIC, 1, 2).await,
        vec![
            "0190f1a2-0000-7000-8000-000000001200".to_owned(),
            "0190f1a2-0000-7000-8000-0000000012ff".to_owned(),
        ],
        "neither run of the refused batch is stored"
    );
}

/// Feature 0001 §2.3: a chained event at the committed head is a duplicate by
/// its position, whatever its id. A resend of `(0, 1)` under another id is
/// answered `200`, leaves the head at 1, and stores nothing.
async fn a_different_id_resend_at_the_head_is_stored_once(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "different-id-resend").await else {
        return;
    };
    let harness = harness(&matrix).await;
    let producer_id = register_chained(&harness).await;
    let event = |id: &str| {
        json!({
            "id": id,
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-a000-000000001101",
            "source": "chain-test",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1 },
        })
    };

    let original = harness
        .api_v1()
        .post_events()
        .with_body(Json(&event("0190f1a2-0000-7000-8000-000000001301")))
        .send()
        .await;
    assert_eq!(
        (original.status().as_u16(), original.text()),
        (202, String::new())
    );
    let resend = harness
        .api_v1()
        .post_events()
        .with_body(Json(&event("0190f1a2-0000-7000-8000-000000001302")))
        .send()
        .await;
    assert_eq!(
        (resend.status().as_u16(), resend.text()),
        (200, String::new())
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
            "topics": [{ "topic": TOPIC, "last_sequence": 1 }],
        })
    );
    publish_sentinel(&harness, "0190f1a2-0000-7000-8000-0000000013ff").await;
    assert_eq!(
        stored_ids(harness.backend().as_ref(), TOPIC, 1, 2).await,
        vec![
            "0190f1a2-0000-7000-8000-000000001301".to_owned(),
            "0190f1a2-0000-7000-8000-0000000013ff".to_owned(),
        ],
        "only the original is stored"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opposite_order_batches_each_store_once_on_sqlite() {
    opposite_order_batches_each_store_once(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opposite_order_batches_each_store_once_on_postgres() {
    opposite_order_batches_each_store_once(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opposite_order_batches_each_store_once_on_mysql() {
    opposite_order_batches_each_store_once(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_batch_stores_nothing_on_sqlite() {
    a_rejected_batch_stores_nothing(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_batch_stores_nothing_on_postgres() {
    a_rejected_batch_stores_nothing(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_batch_stores_nothing_on_mysql() {
    a_rejected_batch_stores_nothing(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_second_run_refuses_the_whole_batch_on_sqlite() {
    a_stale_second_run_refuses_the_whole_batch(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_second_run_refuses_the_whole_batch_on_postgres() {
    a_stale_second_run_refuses_the_whole_batch(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_second_run_refuses_the_whole_batch_on_mysql() {
    a_stale_second_run_refuses_the_whole_batch(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_original_that_read_an_absent_head_retries_after_its_copy_commits_on_postgres() {
    an_original_that_read_an_absent_head_retries_after_its_copy_commits(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_original_that_read_an_absent_head_retries_after_its_copy_commits_on_mysql() {
    an_original_that_read_an_absent_head_retries_after_its_copy_commits(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opposite_order_batches_over_existing_heads_store_once_on_sqlite() {
    opposite_order_batches_over_existing_heads_store_once(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opposite_order_batches_over_existing_heads_store_once_on_postgres() {
    opposite_order_batches_over_existing_heads_store_once(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opposite_order_batches_over_existing_heads_store_once_on_mysql() {
    opposite_order_batches_over_existing_heads_store_once(Engine::Mysql).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_id_resend_at_the_head_is_stored_once_on_sqlite() {
    a_different_id_resend_at_the_head_is_stored_once(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_id_resend_at_the_head_is_stored_once_on_postgres() {
    a_different_id_resend_at_the_head_is_stored_once(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_id_resend_at_the_head_is_stored_once_on_mysql() {
    a_different_id_resend_at_the_head_is_stored_once(Engine::Mysql).await;
}
