//! `handlers/ingest/events.rs` coverage (`eb-rest-handlers` task 11.1): happy path
//! and reachable Hard-Error Catalog codes for `POST /v1/events`/`:batch`,
//! driven through `EventBrokerHarness::api_v1()`.
//!
//! Every test inlines the exact request body sent and asserts the exact
//! response body received (not just the status code) - no shared
//! request/fixture builders, so nothing about what's sent or checked is
//! hidden outside the test itself. Topic/event-type fixtures go through
//! `StaticTypesRegistry::of(...)` (`test_support::type_registry`), which
//! validates every id as a real GTS instance id.

use std::sync::Arc;

use authz_resolver_sdk::{EvaluationRequest, PolicyEnforcer};
use chrono::Utc;
use serde_json::json;
use toolkit_security::pep_properties;
use uuid::Uuid;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::ingest::{ProducerMode, ProducerRegistrationInput, PublishRequest};
use crate::test_support::{
    DenyingAuthZ, EventBrokerHarness, Json, NeverStoringBackend, StaticTypesRegistry, mapping_lines,
};

/// Registers a producer via the real `IngestService::register_producer`
/// (not a bare `Uuid::new_v4()`) - `Storage`'s `check_and_enqueue` looks up
/// the producer row before accepting its first chained event
/// (eb-single-process-implementation D2 risk mitigation swapped the
/// permissive `InMemoryDomainRepo` stand-in, which never checked this, for
/// the real `Storage`), so a producer chain test needs a producer that
/// genuinely exists.
async fn register_test_producer(harness: &EventBrokerHarness) -> Uuid {
    harness
        .ingest()
        .register_producer(
            harness.security_context(),
            ProducerRegistrationInput {
                mode: ProducerMode::Chained,
                client_agent: "test-agent".to_owned(),
            },
        )
        .await
        .expect("producer registration must succeed")
        .id
}

#[tokio::test]
async fn publish_event_happy_path_returns_202_accepted() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_happy_path_returns_202_accepted",
            "subject": "s-publish_event_happy_path_returns_202_accepted",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
}

// Synchronous publish: `Prefer: wait=N` holds the response until the backend
// stores the event (`201`) or N elapses (`202`).
#[tokio::test]
async fn publish_event_prefer_wait_returns_201_once_stored() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    // The harness runs the real ingest outbox over a memory backend, so a
    // `Prefer: wait=5` publish is stored well within the wait and the broker
    // answers `201 Created` once the backend holds it.
    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_wait_returns_201",
            "subject": "s-publish_event_prefer_wait_returns_201",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(resp.text(), "", "201 Created must carry no body");
}

#[tokio::test]
async fn publish_event_prefer_wait_returns_202_when_the_wait_elapses() {
    // A backend that never completes an append: the event is admitted and
    // enqueued, but never stored, so the synchronous wait always elapses.
    let backend = Arc::new(NeverStoringBackend::new(Arc::new(
        memory_event_broker_plugin::MemoryBackend::new(),
    )));
    let harness = EventBrokerHarness::builder()
        .with_backend(backend)
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    // `wait=0`: the event is enqueued, but with nothing stored before the (zero)
    // wait elapses the broker answers `202 Accepted` - it stays enqueued and is
    // stored later.
    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=0"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_wait_returns_202",
            "subject": "s-publish_event_prefer_wait_returns_202",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
    assert_eq!(
        resp.headers()
            .get("preference-applied")
            .map(|v| v.to_str().unwrap()),
        Some("wait=0"),
        "a honoured wait preference is echoed even on a 202"
    );
}

// A nonzero `Prefer: wait` that runs out is a 202: the event is admitted and
// enqueued, the wait elapses before the backend stores it, and
// `Preference-Applied` still reports the wait the broker honoured. Distinct
// from the `wait=0` case above, this exercises a real (if short) hold.
#[tokio::test]
async fn publish_event_prefer_nonzero_wait_that_runs_out_returns_202() {
    let backend = Arc::new(NeverStoringBackend::new(Arc::new(
        memory_event_broker_plugin::MemoryBackend::new(),
    )));
    let harness = EventBrokerHarness::builder()
        .with_backend(backend)
        .with_type_registry(stream_registry())
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=1"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_nonzero_wait_that_runs_out",
            "subject": "s-publish_event_prefer_nonzero_wait_that_runs_out",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
    assert_eq!(
        resp.headers()
            .get("preference-applied")
            .map(|v| v.to_str().unwrap()),
        Some("wait=1"),
    );
}

// A `Prefer: wait` value too large for `u64` is still all digits, so it is not
// malformed - it saturates to the broker's ceiling exactly like any over-cap
// wait (events.rs: `parse::<u64>().map_or(Duration::MAX, ..)`). With a fast
// backend the event stores inside the cap, so the answer is `201` and
// `Preference-Applied` reports the cap. Pins the saturating branch, which a
// merely large value like `99999` (which parses) would not reach.
#[tokio::test]
async fn publish_event_prefer_wait_beyond_u64_saturates_to_the_cap() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(stream_registry())
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            // 30 nines, far beyond `u64::MAX` (~1.8e19, 20 digits).
            axum::http::HeaderValue::from_static("wait=999999999999999999999999999999"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_wait_beyond_u64",
            "subject": "s-publish_event_prefer_wait_beyond_u64",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(
        resp.headers()
            .get("preference-applied")
            .map(|v| v.to_str().unwrap()),
        Some("wait=20"),
        "a wait beyond u64 saturates and clamps to the cap"
    );
}

// A malformed `Prefer: wait` value is a bad request, not a silent fall-back to
// the cap or to async: the broker answers `400`.
#[tokio::test]
async fn publish_event_prefer_wait_with_a_malformed_value_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=soon"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_wait_malformed",
            "subject": "s-publish_event_prefer_wait_malformed",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
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
            "detail": "InvalidHeader: Prefer: wait expects a non-negative whole number of seconds",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "InvalidHeader: Prefer: wait expects a non-negative whole number of seconds",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// A fixture topic/event type, inlined so each test states its own.
fn stream_registry() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        {
            "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
        },
    ]))
}

// A `wait` above the broker's ceiling is not rejected - it clamps to the cap.
// With a fast backend the event stores well inside the cap, so the answer is
// `201`, and `Preference-Applied` reports the applied (capped) wait, not the
// requested one (RFC 7240 §3).
#[tokio::test]
async fn publish_event_prefer_wait_above_the_cap_clamps_and_reports_the_applied_wait() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(stream_registry())
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            // Far above the default 20s ceiling.
            axum::http::HeaderValue::from_static("wait=99999"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_wait_above_cap",
            "subject": "s-publish_event_prefer_wait_above_cap",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(
        resp.headers()
            .get("preference-applied")
            .map(|v| v.to_str().unwrap()),
        Some("wait=20"),
        "the applied wait is the cap, not the requested 99999"
    );
}

// A bare `Prefer: wait` (no value) asks to wait as long as the broker allows:
// it clamps to the cap exactly as a numeric over-cap wait does, so the answer
// is `201` and `Preference-Applied` reports the cap.
#[tokio::test]
async fn publish_event_prefer_bare_wait_uses_the_cap() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(stream_registry())
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_bare_wait",
            "subject": "s-publish_event_prefer_bare_wait",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(
        resp.headers()
            .get("preference-applied")
            .map(|v| v.to_str().unwrap()),
        Some("wait=20")
    );
}

// An async publish (no `Prefer: wait`) never carries `Preference-Applied`, so a
// client can tell a synchronous `202` (preference honoured) from one where the
// preference was absent or ignored.
#[tokio::test]
async fn an_async_publish_carries_no_preference_applied_header() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(stream_registry())
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-async-no-preference-applied",
            "subject": "s-async-no-preference-applied",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert!(
        resp.headers().get("preference-applied").is_none(),
        "an async publish applied no wait preference"
    );
}

// A malformed `Prefer: wait` is rejected before anything is enqueued: a valid
// resend of the same chain link is then newly accepted (`202`), not a duplicate
// (`200`) - proof the rejected request wrote no outbox row and moved no head.
#[tokio::test]
async fn publish_event_prefer_wait_malformed_enqueues_nothing() {
    let harness = run_rules_harness().await;
    let producer = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x4a11_defe);
    let event_id = Uuid::from_u128(0x4a11_0001);
    let meta = json!({ "version": 1, "producer": producer, "previous": 0, "sequence": 1 });

    // Malformed `Prefer: wait` on the first chain link: rejected `400`.
    harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=nope"),
        )
        .with_body(Json(&json!({
            "id": event_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-malformed-wait",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:13:00Z",
            "meta": meta,
        })))
        .send()
        .await
        .assert_status(400);

    // The same chain link, now valid: if the rejected request had enqueued its
    // row and moved the head to 1, this would be a duplicate (`200`). It is
    // newly accepted (`202`), so nothing was enqueued.
    let resend = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": event_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-malformed-wait",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:13:00Z",
            "meta": meta,
        })))
        .send()
        .await;
    resend.assert_status(202);
}

// The batch route honours `Prefer: wait=N` as a whole: once every admitted
// event is stored the broker answers `201 Created` (feature 0001 §5.1).
#[tokio::test]
async fn publish_batch_prefer_wait_returns_201_once_stored() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    // The real ingest outbox over a memory backend stores both events well
    // within the wait, so the batch answers `201` once every one is stored.
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::new_v4(),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::new_v4(),
                    "source": "test-publish_batch_prefer_wait_returns_201",
                    "subject": "s-publish_batch_prefer_wait_returns_201",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": Utc::now().to_rfc3339(),
                },
                {
                    "id": Uuid::new_v4(),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::new_v4(),
                    "source": "test-publish_batch_prefer_wait_returns_201",
                    "subject": "s-publish_batch_prefer_wait_returns_201",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": Utc::now().to_rfc3339(),
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(resp.text(), "", "201 Created must carry no body");
}

#[tokio::test]
async fn publish_batch_prefer_wait_returns_202_when_the_wait_elapses() {
    // A backend that never stores: every admitted event is enqueued but the
    // batch's wait always elapses, so the broker answers `202 Accepted`.
    let backend = Arc::new(NeverStoringBackend::new(Arc::new(
        memory_event_broker_plugin::MemoryBackend::new(),
    )));
    let harness = EventBrokerHarness::builder()
        .with_backend(backend)
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    // `wait=0`: nothing is stored before the (zero) wait elapses, so the batch
    // stays enqueued and the broker answers `202 Accepted`.
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=0"),
        )
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::new_v4(),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::new_v4(),
                    "source": "test-publish_batch_prefer_wait_returns_202",
                    "subject": "s-publish_batch_prefer_wait_returns_202",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": Utc::now().to_rfc3339(),
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
    assert_eq!(
        resp.headers()
            .get("preference-applied")
            .map(|v| v.to_str().unwrap()),
        Some("wait=0"),
        "a honoured wait preference is echoed even on a batch 202"
    );
}

// A synchronous publish of an idempotent duplicate is answered `200` at once:
// the event is already durable, so nothing is written and there is nothing to
// wait on (`DESIGN.md` "Sync vs Async Persist Modes").
#[tokio::test]
async fn publish_event_prefer_wait_duplicate_returns_200() {
    let harness = run_rules_harness().await;
    let producer = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1702);
    let event = json!({
        "id": Uuid::from_u128(0xe100_1701),
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": tenant_id,
        "source": "order-service",
        "subject": "order-sync-dup",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-05-29T10:12:00Z",
        "meta": { "version": 1, "producer": producer, "previous": 0, "sequence": 1 },
    });

    // The first publish is stored on the memory backend within the wait.
    harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
        .with_body(Json(&event))
        .send()
        .await
        .assert_status(201);

    // Lost-ack resend of the chain head: a duplicate, answered 200 at once.
    let resend = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
        .with_body(Json(&event))
        .send()
        .await;

    resend.assert_status(200);
    assert_eq!(resend.text(), "", "200 OK must carry no body");
}

// An all-duplicate batch admits nothing, so a synchronous publish answers `200`
// at once and must not wait: no traced batch is enqueued (an empty traced batch
// is refused), and awaiting a trace that nothing ever completes would wrongly
// hang to the timeout instead of answering `200`.
#[tokio::test]
async fn publish_batch_prefer_wait_all_duplicate_returns_200() {
    let harness = run_rules_harness().await;
    let producer = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1703);
    let batch = json!({
        "events": [
            {
                "id": Uuid::from_u128(0xe100_1801),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-sync-dup-batch",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:13:00Z",
                "meta": { "version": 1, "producer": producer, "previous": 0, "sequence": 1 },
            },
            {
                "id": Uuid::from_u128(0xe100_1802),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-sync-dup-batch",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:13:01Z",
                "meta": { "version": 1, "producer": producer, "previous": 1, "sequence": 2 },
            },
        ],
    });

    // The first batch is admitted and durably enqueued (async).
    harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&batch))
        .send()
        .await
        .assert_status(202);

    // Resend with `Prefer: wait`: every event is a duplicate, so nothing is
    // admitted and the broker answers 200 without waiting.
    let resend = harness
        .api_v1()
        .post_events_batch()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
        .with_body(Json(&batch))
        .send()
        .await;

    resend.assert_status(200);
    assert_eq!(resend.text(), "", "200 OK must carry no body");
}

// A batch mixing a duplicate and a newly admitted event under `Prefer: wait`
// answers `201` once the admitted event is stored. The wait is on the admitted
// rows only: were it on the batch's full size (the duplicate counted in), the
// trace would carry one more entity than is ever enqueued and never complete,
// so this would wrongly time out to `202`.
#[tokio::test]
async fn publish_batch_prefer_wait_mixed_new_and_duplicate_returns_201() {
    let harness = run_rules_harness().await;
    let producer = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x3e7a_2206);

    // Advance the chain to head 2 with two async publishes.
    for (previous, sequence) in [(0, 1), (1, 2)] {
        harness
            .api_v1()
            .post_events()
            .with_body(Json(&json!({
                "id": Uuid::new_v4(),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-mixed",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:14:00Z",
                "meta": { "version": 1, "producer": producer, "previous": previous, "sequence": sequence },
            })))
            .send()
            .await
            .assert_status(202);
    }

    // Head is 2. In the batch the first link (sequence 2) is a duplicate prefix
    // and the second (sequence 3) is newly admitted - one admitted row.
    let batch = json!({
        "events": [
            {
                "id": Uuid::from_u128(0x3e7a_2201),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-mixed",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:14:01Z",
                "meta": { "version": 1, "producer": producer, "previous": 1, "sequence": 2 },
            },
            {
                "id": Uuid::from_u128(0x3e7a_2202),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-mixed",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:14:02Z",
                "meta": { "version": 1, "producer": producer, "previous": 2, "sequence": 3 },
            },
        ],
    });

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
        .with_body(Json(&batch))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(resp.text(), "", "201 Created must carry no body");
}

#[tokio::test]
async fn publish_event_prefer_respond_async_is_the_default_returns_202() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    // `Prefer: respond-async` names the default (asynchronous) behaviour, so it
    // is accepted as a no-op and the publish still returns `202`.
    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("respond-async"),
        )
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_prefer_respond_async_is_the_default_returns_202",
            "subject": "s-publish_event_prefer_respond_async_is_the_default_returns_202",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
}

/// A registered event type whose `topic` names a stream nobody registered is
/// the only route left to `TopicNotFound` on publish: an event names no topic
/// of its own, so the producer cannot ask for a missing one.
#[tokio::test]
async fn publish_event_whose_type_names_an_unregistered_topic_returns_404_topic_not_found() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.missing.topic.v1",
                "topic_registered": false,
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_whose_type_names_an_unregistered_topic",
            "subject": "s-publish_event_whose_type_names_an_unregistered_topic",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "topic 'gts.cf.core.events.topic.v1~x.eb.missing.topic.v1' is not registered",
            "instance": "/event-broker/v1/events",
            "context": {
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.missing.topic.v1",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
}

#[tokio::test]
async fn publish_event_readonly_field_rejected_via_deny_unknown_fields() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;

    // `sequence` is one of the schema's `readOnly` fields - a client that
    // supplies it is rejected by `#[serde(deny_unknown_fields)]`. The handler
    // parses the body itself and reports the deserialize failure as a `400`
    // `InvalidBody` (a producer request to fix) rather than the `422` axum's
    // `Json` extractor would emit - `422` is reserved for payload-schema
    // violations decided later against the event type's `data_schema`.
    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_readonly_field_rejected_via_deny_unknown_fields",
            "subject": "s-publish_event_readonly_field_rejected_via_deny_unknown_fields",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
            "sequence": 1,
        })))
        .send()
        .await;

    resp.assert_status(400);
    let body = resp.json();
    assert_eq!(
        body["type"],
        json!("gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
    );
    assert!(
        body["detail"].as_str().unwrap().starts_with("InvalidBody:"),
        "detail must start with 'InvalidBody:', got: {}",
        body["detail"]
    );
}

/// A `meta` naming `producer_id`, a key `meta` does not declare (it names the
/// producer `producer`), is refused as an unknown field, and nothing is
/// enqueued. The body is sent as written, so the reported column is the byte
/// offset just past the key's closing quote.
#[tokio::test]
async fn a_publish_whose_meta_names_producer_id_is_refused() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )
        .with_body(
            r#"{"id":"00000000-0000-0000-0000-00000000b901","type":"gts.cf.core.events.event.v1~x.eb.t1.foo.v1~","subject_tenant":"00000000-0000-0000-0000-00000000b902","source":"test-old-producer-id-key","subject":"s-old-producer-id-key","subject_type":"gts.x.eb.t1.subject.v1~","occurred_at":"2026-11-13T00:00:00Z","meta":{"version":1,"producer_id":"00000000-0000-0000-0000-00000000b903"}}"#,
        )
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidBody: invalid JSON body: unknown field `producer_id`, expected one \
                       of `version`, `producer`, `previous`, `sequence` at line 1 column 335",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "InvalidBody: invalid JSON body: unknown field `producer_id`, expected \
                           one of `version`, `producer`, `previous`, `sequence` at line 1 column \
                           335",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(harness.outbox_bodies().await, 0);
}

#[tokio::test]
async fn publish_event_schema_violation_returns_422() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "data_schema": {
                    "type": "object",
                    "required": ["required_field"],
                    "additionalProperties": false,
                },
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_schema_violation_returns_422",
            "subject": "s-publish_event_schema_violation_returns_422",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
            "data": {},
        })))
        .send()
        .await;

    // A payload that fails its type's `data_schema` is `422`, and the body names
    // the target stream (topic) in `resource_name` while the `jsonschema`
    // message (which can quote the submitted value) is deliberately not carried.
    resp.assert_status(422);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 422,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "(payload)",
                    "description": "event data does not satisfy the event type's data schema",
                    "reason": "schema_validation",
                }],
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            },
        })
    );
}

#[tokio::test]
async fn a_single_publish_412_names_index_zero_its_producer_and_the_committed_head() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let producer_id = register_test_producer(&harness).await;

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_sequence_violation_returns_412_with_expected_previous",
            "subject": "s-publish_event_sequence_violation_returns_412_with_expected_previous",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
            "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
        })))
        .send()
        .await
        .assert_status(202);

    // A genuine chain break: `previous` (5) does not link to the current head
    // (1). Sequence gaps are allowed - what a violation catches is a `previous`
    // that has diverged from the broker's head, so `previous: 5` (not `1`) is the
    // break, regardless of the sequence value.
    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_sequence_violation_returns_412_with_expected_previous",
            "subject": "s-publish_event_sequence_violation_returns_412_with_expected_previous",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
            "meta": { "version": 1, "producer": producer_id, "previous": 5, "sequence": 6 },
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
            "instance": "/event-broker/v1/events",
            "context": {
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "violations": [{
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": format!(
                        "index=0 producer={producer_id} \
                         topic=gts.cf.core.events.topic.v1~x.eb.t1.topic.v1 last_sequence=1"
                    ),
                }],
            },
        })
    );
}

/// A topic its backend does not hold yet cannot take events: the publish is
/// answered `503`, retryable, and the same publish succeeds once the topic is
/// provisioned (feature 0006 §4.2).
#[tokio::test]
#[tracing_test::traced_test]
async fn publish_to_a_topic_not_provisioned_returns_503_then_202_once_provisioned() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .without_provisioning()
        .build()
        .await;
    let body = json!({
        "id": "0190f1a2-0000-7000-8000-000000000001",
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": "0190f1a2-0000-7000-8000-000000000002",
        "source": "test-publish-before-provisioning",
        "subject": "s-1",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-10-07T12:00:00Z",
    });

    let refused = harness
        .api_v1()
        .post_events()
        .with_body(Json(&body))
        .send()
        .await;
    refused.assert_status(503);
    assert_eq!(
        refused.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "instance": "/event-broker/v1/events",
            "context": {},
        })
    );
    let logged = std::cell::RefCell::new(Vec::new());
    logs_assert(|lines| {
        *logged.borrow_mut() = mapping_lines(lines);
        Ok(())
    });
    assert_eq!(
        logged.into_inner(),
        vec![
            "WARN answering 503 with the generic body error=storage backend unavailable: \
             topic 'gts.cf.core.events.topic.v1~x.eb.t1.topic.v1' is not provisioned on its \
             storage backend yet"
                .to_owned()
        ]
    );

    harness.provision_topics().await;
    harness
        .api_v1()
        .post_events()
        .with_body(Json(&body))
        .send()
        .await
        .assert_status(202);
}

/// An unreadable spec cache says nothing about whether the type is registered,
/// so the publish is answered `503`, retryable, not `404 EventTypeNotFound`.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_publish_during_a_spec_cache_fault_answers_503() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    harness.drop_metadata_table("event_broker_spec_cache").await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000000011",
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": "0190f1a2-0000-7000-8000-000000000012",
            "source": "test-publish-during-a-spec-cache-fault",
            "subject": "s-1",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-10-07T12:00:00Z",
        })))
        .send()
        .await;

    resp.assert_status(503);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "instance": "/event-broker/v1/events",
            "context": {},
        })
    );
    let logged = std::cell::RefCell::new(Vec::new());
    logs_assert(|lines| {
        *logged.borrow_mut() = mapping_lines(lines);
        Ok(())
    });
    assert_eq!(
        logged.into_inner(),
        vec![
            "WARN answering 503 with the generic body error=storage backend unavailable: \
             database scope resolution failed: database error: Query Error: error returned \
             from database: (code: 1) no such table: event_broker_spec_cache"
                .to_owned()
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0);
    assert_eq!(
        stored_ids(&harness, "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1").await,
        Vec::<Uuid>::new()
    );
}

/// An unreadable topic binding says nothing about whether the topic is
/// provisioned, so the publish is answered `503`, retryable, and nothing is
/// enqueued.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_publish_during_a_topic_binding_fault_answers_503() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    harness
        .drop_metadata_table("event_broker__topic_binding")
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000000031",
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": "0190f1a2-0000-7000-8000-000000000032",
            "source": "test-publish-during-a-topic-binding-fault",
            "subject": "s-1",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-10-07T12:00:00Z",
        })))
        .send()
        .await;

    resp.assert_status(503);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "instance": "/event-broker/v1/events",
            "context": {},
        })
    );
    let logged = std::cell::RefCell::new(Vec::new());
    logs_assert(|lines| {
        *logged.borrow_mut() = mapping_lines(lines);
        Ok(())
    });
    assert_eq!(
        logged.into_inner(),
        vec![
            "WARN answering 503 with the generic body error=storage backend unavailable: \
             database scope resolution failed: database error: Query Error: error returned \
             from database: (code: 1) no such table: event_broker__topic_binding"
                .to_owned()
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0);
    assert_eq!(
        stored_ids(&harness, "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1").await,
        Vec::<Uuid>::new()
    );
}

/// A batch resolves each event's topic through its type, so the same fault
/// answers `503` there too.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_batch_publish_during_a_spec_cache_fault_answers_503() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    harness.drop_metadata_table("event_broker_spec_cache").await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": "0190f1a2-0000-7000-8000-000000000021",
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": "0190f1a2-0000-7000-8000-000000000023",
                    "source": "test-batch-during-a-spec-cache-fault",
                    "subject": "s-1",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-10-07T12:00:00Z",
                },
                {
                    "id": "0190f1a2-0000-7000-8000-000000000022",
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": "0190f1a2-0000-7000-8000-000000000023",
                    "source": "test-batch-during-a-spec-cache-fault",
                    "subject": "s-2",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-10-07T12:00:01Z",
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(503);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "instance": "/event-broker/v1/events:batch",
            "context": {},
        })
    );
    let logged = std::cell::RefCell::new(Vec::new());
    logs_assert(|lines| {
        *logged.borrow_mut() = mapping_lines(lines);
        Ok(())
    });
    assert_eq!(
        logged.into_inner(),
        vec![
            "WARN answering 503 with the generic body error=storage backend unavailable: \
             database scope resolution failed: database error: Query Error: error returned \
             from database: (code: 1) no such table: event_broker_spec_cache"
                .to_owned()
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0);
    assert_eq!(
        stored_ids(&harness, "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1").await,
        Vec::<Uuid>::new()
    );
}

/// A producer's chain is kept per `(producer_id, topic)`, not per partition:
/// two events of one topic with different partition keys continue one chain.
#[tokio::test]
async fn a_producers_chain_spans_every_key_of_a_topic() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 4 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let producer_id = register_test_producer(&harness).await;

    for (tenant, previous, sequence) in [
        ("0190f1a2-0000-7000-8000-00000000000a", 0, 1),
        ("0190f1a2-0000-7000-8000-00000000000b", 1, 2),
    ] {
        harness
            .api_v1()
            .post_events()
            .with_body(Json(&json!({
                "id": Uuid::new_v4(),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant,
                "source": "test-a_producers_chain_spans_every_key_of_a_topic",
                "subject": "s",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": Utc::now().to_rfc3339(),
                "meta": { "version": 1, "producer": producer_id, "previous": previous, "sequence": sequence },
            })))
            .send()
            .await
            .assert_status(202);
    }
}

#[tokio::test]
async fn publish_event_duplicate_resubmission_returns_200_not_an_error() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let producer_id = register_test_producer(&harness).await;
    let body = json!({
        "id": Uuid::new_v4(),
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": Uuid::new_v4(),
        "source": "test-publish_event_duplicate_resubmission_is_ignored_not_an_error",
        "subject": "s-publish_event_duplicate_resubmission_is_ignored_not_an_error",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": Utc::now().to_rfc3339(),
        "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
    });

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&body))
        .send()
        .await
        .assert_status(202);

    // Exact resubmission of the current chain head (a lost-ack retry) - the
    // event is already durable, so it is recognised as a duplicate and answered
    // `200` with no body, not re-admitted and not a sequence violation.
    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&body))
        .send()
        .await;
    resp.assert_status(200);
    assert_eq!(resp.text(), "", "200 duplicate response must carry no body");
}

#[tokio::test]
async fn publish_batch_happy_path_returns_202() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::new_v4(),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::new_v4(),
                    "source": "test-publish_batch_happy_path_returns_202",
                    "subject": "s-publish_batch_happy_path_returns_202",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": Utc::now().to_rfc3339(),
                },
                {
                    "id": Uuid::new_v4(),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::new_v4(),
                    "source": "test-publish_batch_happy_path_returns_202",
                    "subject": "s-publish_batch_happy_path_returns_202",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": Utc::now().to_rfc3339(),
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
}

#[tokio::test]
/// Two event types on two different topics: nothing in the request bodies says
/// so, and the rejection comes from resolving each `type` to the topic its
/// `topic` trait names.
async fn publish_batch_mixed_topics_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            { "id": "gts.cf.core.events.topic.v1~x.eb.t2.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t2.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t2.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::new_v4(),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::new_v4(),
                    "source": "test-publish_batch_mixed_topics_returns_400",
                    "subject": "s-publish_batch_mixed_topics_returns_400",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": Utc::now().to_rfc3339(),
                },
                {
                    "id": Uuid::new_v4(),
                    "type": "gts.cf.core.events.event.v1~x.eb.t2.foo.v1~",
                    "subject_tenant": Uuid::new_v4(),
                    "source": "test-publish_batch_mixed_topics_returns_400",
                    "subject": "s-publish_batch_mixed_topics_returns_400",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": Utc::now().to_rfc3339(),
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
            "detail": "MixedTopics: a batch must target a single topic",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "format": "MixedTopics: a batch must target a single topic",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn publish_batch_too_large_returns_413() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;

    let events: Vec<_> = (0..101)
        .map(|_| {
            json!({
                "id": Uuid::new_v4(),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": Uuid::new_v4(),
                "source": "test-publish_batch_too_large_returns_413",
                "subject": "s-publish_batch_too_large_returns_413",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": Utc::now().to_rfc3339(),
            })
        })
        .collect();
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({ "events": events })))
        .send()
        .await;

    resp.assert_status(413);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 413,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "field_violations": [{
                    "field": "batch.count",
                    "description": "batch too large: 101 events (max 100)",
                    "reason": "batch_too_large",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn a_chained_run_whose_second_event_does_not_link_is_refused_as_malformed_run() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_1001),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7e7a_1001),
                    "source": "order-service",
                    "subject": "order-unlinked",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:00Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
                },
                {
                    "id": Uuid::from_u128(0xe000_1002),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7e7a_1001),
                    "source": "order-service",
                    "subject": "order-unlinked",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    // `previous` (5) does not link to the first event's
                    // `sequence` (1): the run is malformed on its own (feature
                    // 0001 §5.2 R3), before any chain state is read.
                    "meta": { "version": 1, "producer": producer_id, "previous": 5, "sequence": 6 },
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
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(
        &harness,
        Uuid::from_u128(0x5e7_1001),
        Uuid::from_u128(0x7e7a_1001),
    )
    .await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a run that does not link stores nothing"
    );
}

/// The ids the backend holds on partition 0 of `topic` right now. Read after
/// [`EventBrokerHarness::outbox_bodies`], so an event still on its way through
/// the outbox is counted by one of the two.
async fn stored_ids(harness: &EventBrokerHarness, topic: &str) -> Vec<Uuid> {
    use event_broker_sdk::backend::{Partition, TopicPartition, Wait};

    let at = TopicPartition::new(
        toolkit_gts::GtsInstanceId::try_new(topic).expect("a valid topic id"),
        Partition::new(0),
    );
    harness
        .backend()
        .read(&at, crate::domain::model::Sequence::NONE, 1024, Wait::No)
        .await
        .expect("read the stored events")
        .iter()
        .map(|event| event.id)
        .collect()
}

/// The ids the backend holds on partition 0 of `topic`, in sequence order,
/// once `last` is among them. Everything published before `last` on the same
/// key drains ahead of it, so an event a rejected batch left behind shows up
/// in this list too.
async fn stored_ids_through(harness: &EventBrokerHarness, topic: &str, last: Uuid) -> Vec<Uuid> {
    use event_broker_sdk::backend::{Partition, TopicPartition, Wait};

    let at = TopicPartition::new(
        toolkit_gts::GtsInstanceId::try_new(topic).expect("a valid topic id"),
        Partition::new(0),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let ids: Vec<Uuid> = harness
            .backend()
            .read(&at, crate::domain::model::Sequence::NONE, 1024, Wait::No)
            .await
            .expect("read the stored events")
            .iter()
            .map(|event| event.id)
            .collect();
        if ids.contains(&last) || tokio::time::Instant::now() >= deadline {
            return ids;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_batch_with_an_invalid_second_event_stores_nothing() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "data_schema": {
                    "type": "object",
                    "required": ["order_id", "total_cents"],
                    "properties": {
                        "order_id": { "type": "string" },
                        "total_cents": { "type": "integer" },
                    },
                },
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let tenant_id = Uuid::from_u128(0x7e7a_0001);
    let sentinel_id = Uuid::from_u128(0x5e7_0001);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_0101),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-atomic",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:00Z",
                    "data": { "order_id": "order-atomic", "total_cents": 100 },
                },
                {
                    "id": Uuid::from_u128(0xe000_0102),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-atomic",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    "data": { "order_id": "order-atomic" },
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(422);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 422,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "field_violations": [{
                    "field": "(payload)",
                    "description": "event data does not satisfy the event type's data schema",
                    "reason": "schema_validation",
                }],
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            },
        })
    );

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": sentinel_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-atomic",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:12:00Z",
            "data": { "order_id": "order-sentinel", "total_cents": 1 },
        })))
        .send()
        .await
        .assert_status(202);
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel_id
        )
        .await,
        vec![sentinel_id],
        "the valid first event of the rejected batch must not be stored"
    );
}

#[tokio::test]
async fn a_batch_whose_second_run_is_stale_stores_nothing_and_keeps_both_chains() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let fresh_producer = register_test_producer(&harness).await;
    let stale_producer = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_0002);
    let seed_id = Uuid::from_u128(0xe000_0200);
    let sentinel_id = Uuid::from_u128(0x5e7_0002);

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": seed_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-chain",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "meta": { "version": 1, "producer": stale_producer, "previous": 0, "sequence": 1 },
        })))
        .send()
        .await
        .assert_status(202);

    // The first run would seed its chain; the second run's `previous` (2)
    // does not link to its head (1). The whole batch is refused.
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_0201),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-chain",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:00Z",
                    "meta": { "version": 1, "producer": fresh_producer, "previous": 0, "sequence": 1 },
                },
                {
                    "id": Uuid::from_u128(0xe000_0202),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-chain",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    "meta": { "version": 1, "producer": stale_producer, "previous": 2, "sequence": 3 },
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
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "violations": [{
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": format!(
                        "index=1 producer={stale_producer} \
                         topic=gts.cf.core.events.topic.v1~x.eb.t1.topic.v1 last_sequence=1"
                    ),
                }],
            },
        })
    );

    // The fresh run's head was never written, and the stale one stays at 1.
    let fresh = harness
        .api_v1()
        .get_producer_cursors(&fresh_producer.to_string())
        .send()
        .await;
    fresh.assert_status(200);
    assert_eq!(
        fresh.json(),
        json!({ "id": fresh_producer, "client_agent": "test-agent", "topics": [] })
    );
    let stale = harness
        .api_v1()
        .get_producer_cursors(&stale_producer.to_string())
        .send()
        .await;
    stale.assert_status(200);
    assert_eq!(
        stale.json(),
        json!({
            "id": stale_producer,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 1,
            }],
        })
    );

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": sentinel_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-chain",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:12:00Z",
        })))
        .send()
        .await
        .assert_status(202);
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel_id
        )
        .await,
        vec![seed_id, sentinel_id],
        "neither run of the rejected batch is stored"
    );
}

#[tokio::test]
async fn a_batch_skips_a_duplicate_and_stores_the_rest() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_0003);
    let first_id = Uuid::from_u128(0xe000_0301);
    let second_id = Uuid::from_u128(0xe000_0302);
    let first = json!({
        "id": first_id,
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": tenant_id,
        "source": "order-service",
        "subject": "order-dup",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-05-29T10:11:00Z",
        "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
    });

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&first))
        .send()
        .await
        .assert_status(202);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                first,
                {
                    "id": second_id,
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-dup",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 1, "sequence": 2 },
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");

    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 2,
            }],
        })
    );
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            second_id
        )
        .await,
        vec![first_id, second_id],
        "the duplicate is stored once, the rest of the batch after it"
    );
}

// -- batch run rules (feature 0001 §5.2) --

/// A harness with one single-partition topic and one event type on it.
async fn run_rules_harness() -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await
}

/// Publishes one stateless event as `tenant_id` and returns its id. The type
/// partitions on the tenant, so the sentinel shares the ingest outbox
/// partition of every earlier event of that tenant and drains behind them:
/// reading through it shows what the backend holds.
async fn publish_sentinel(harness: &EventBrokerHarness, id: Uuid, tenant_id: Uuid) -> Uuid {
    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-sentinel",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:30:00Z",
        })))
        .send()
        .await
        .assert_status(202);
    id
}

#[tokio::test]
async fn an_empty_batch_is_refused_as_empty_batch() {
    let harness = run_rules_harness().await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({ "events": [] })))
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
                    "field": "events",
                    "description": "a batch carries at least one event",
                    "reason": "empty_batch",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let sentinel = publish_sentinel(
        &harness,
        Uuid::from_u128(0x5e7_0101),
        Uuid::from_u128(0x7e7a_0101),
    )
    .await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "an empty batch stores nothing"
    );
}

#[tokio::test]
async fn a_repeated_event_id_is_refused_naming_the_repeat() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let repeated_id = Uuid::from_u128(0xe000_1101);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": repeated_id,
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7e7a_1101),
                    "source": "order-service",
                    "subject": "order-repeat",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:00Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
                },
                {
                    "id": repeated_id,
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7e7a_1101),
                    "source": "order-service",
                    "subject": "order-repeat",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 1, "sequence": 2 },
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
                    "field": "events[1].id",
                    "description": "repeats the id of an earlier event of the batch",
                    "reason": "duplicate_event_id",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(
        &harness,
        Uuid::from_u128(0x5e7_1101),
        Uuid::from_u128(0x7e7a_1101),
    )
    .await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a batch repeating an event id stores nothing"
    );
}

#[tokio::test]
async fn a_gap_inside_a_chained_run_is_refused_as_malformed_run() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;

    // The second event's `previous` (2) does not link to the first event's
    // `sequence` (1): the run is malformed on its own, whatever the head is.
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_1201),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7e7a_1201),
                    "source": "order-service",
                    "subject": "order-gap",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:00Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
                },
                {
                    "id": Uuid::from_u128(0xe000_1202),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7e7a_1201),
                    "source": "order-service",
                    "subject": "order-gap",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 2, "sequence": 3 },
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
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(
        &harness,
        Uuid::from_u128(0x5e7_1201),
        Uuid::from_u128(0x7e7a_1201),
    )
    .await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a malformed run stores nothing"
    );
}

#[tokio::test]
async fn a_lost_ack_resend_of_a_whole_run_answers_200() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let seed_id = Uuid::from_u128(0xe000_1300);
    let tenant_id = Uuid::from_u128(0x7e7a_1301);

    // The chain's head is 9 before the run.
    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": seed_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-resend",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 8, "sequence": 9 },
        })))
        .send()
        .await
        .assert_status(202);

    let batch = json!({
        "events": [
            {
                "id": Uuid::from_u128(0xe000_1310),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-resend",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:11:00Z",
                "meta": { "version": 1, "producer": producer_id, "previous": 9, "sequence": 10 },
            },
            {
                "id": Uuid::from_u128(0xe000_1311),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-resend",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:11:01Z",
                "meta": { "version": 1, "producer": producer_id, "previous": 10, "sequence": 11 },
            },
            {
                "id": Uuid::from_u128(0xe000_1312),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-resend",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:11:02Z",
                "meta": { "version": 1, "producer": producer_id, "previous": 11, "sequence": 12 },
            },
        ],
    });
    let first = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&batch))
        .send()
        .await;
    first.assert_status(202);
    assert_eq!(first.text(), "", "202 Accepted must carry no body");

    // The ack was lost; the producer sends the same batch again.
    let resend = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&batch))
        .send()
        .await;

    resend.assert_status(200);
    assert_eq!(resend.text(), "", "200 OK must carry no body");
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 12,
            }],
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1301), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![
            seed_id,
            Uuid::from_u128(0xe000_1310),
            Uuid::from_u128(0xe000_1311),
            Uuid::from_u128(0xe000_1312),
            sentinel,
        ],
        "each event of the run is stored once"
    );
}

#[tokio::test]
async fn a_resend_overlapping_the_head_skips_the_duplicate_prefix() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1401);
    let seed_id = Uuid::from_u128(0xe000_1400);
    let tenth = json!({
        "id": Uuid::from_u128(0xe000_1410),
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": tenant_id,
        "source": "order-service",
        "subject": "order-overlap",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-05-29T10:11:00Z",
        "meta": { "version": 1, "producer": producer_id, "previous": 9, "sequence": 10 },
    });
    let eleventh = json!({
        "id": Uuid::from_u128(0xe000_1411),
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": tenant_id,
        "source": "order-service",
        "subject": "order-overlap",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-05-29T10:11:01Z",
        "meta": { "version": 1, "producer": producer_id, "previous": 10, "sequence": 11 },
    });

    // The head reaches 11 through single publishes: 9, then 10, then 11.
    for event in [
        json!({
            "id": seed_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-overlap",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 8, "sequence": 9 },
        }),
        tenth.clone(),
        eleventh.clone(),
    ] {
        harness
            .api_v1()
            .post_events()
            .with_body(Json(&event))
            .send()
            .await
            .assert_status(202);
    }

    // `e[1].sequence == last`: events 0 and 1 are a duplicate prefix, and
    // event 2 advances.
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                tenth,
                eleventh,
                {
                    "id": Uuid::from_u128(0xe000_1412),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-overlap",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:02Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 11, "sequence": 12 },
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 12,
            }],
        })
    );
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            Uuid::from_u128(0xe000_1412)
        )
        .await,
        vec![
            seed_id,
            Uuid::from_u128(0xe000_1410),
            Uuid::from_u128(0xe000_1411),
            Uuid::from_u128(0xe000_1412),
        ],
        "only the event past the head is newly stored"
    );
}

#[tokio::test]
async fn a_resend_at_the_head_with_another_previous_answers_200_and_stores_nothing() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1901);
    let first_id = Uuid::from_u128(0xe000_1908);

    // The chain's head is 8 before the resend.
    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": first_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-head-previous",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 7, "sequence": 8 },
        })))
        .send()
        .await
        .assert_status(202);

    // `sequence == last` makes it a duplicate; its `previous` is not read.
    let resend = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1918),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-head-previous",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:01Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 3, "sequence": 8 },
        })))
        .send()
        .await;

    resend.assert_status(200);
    assert_eq!(resend.text(), "", "200 OK must carry no body");
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 8,
            }],
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1901), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![first_id, sentinel],
        "the resend is not stored"
    );
}

#[tokio::test]
async fn a_batch_led_by_a_head_resend_with_another_previous_stores_only_the_rest() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_2001);
    let first_id = Uuid::from_u128(0xe000_2008);
    let ninth_id = Uuid::from_u128(0xe000_2009);

    // The chain's head is 8 before the batch.
    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": first_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-head-previous",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 7, "sequence": 8 },
        })))
        .send()
        .await
        .assert_status(202);

    // `e[0].sequence == last`: event 0 is a duplicate prefix whatever its
    // `previous`, and event 1 advances.
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_2018),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-head-previous",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:10:01Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 3, "sequence": 8 },
                },
                {
                    "id": ninth_id,
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-head-previous",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:10:02Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 8, "sequence": 9 },
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 9,
            }],
        })
    );
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            ninth_id
        )
        .await,
        vec![first_id, ninth_id],
        "only the event past the head is newly stored"
    );
}

#[tokio::test]
async fn a_monotonic_run_skips_its_leading_duplicates() {
    let harness = run_rules_harness().await;
    let producer_id = harness
        .ingest()
        .register_producer(
            harness.security_context(),
            ProducerRegistrationInput {
                mode: ProducerMode::Monotonic,
                client_agent: "test-agent".to_owned(),
            },
        )
        .await
        .expect("producer registration must succeed")
        .id;
    let tenant_id = Uuid::from_u128(0x7e7a_1501);
    let seed_id = Uuid::from_u128(0xe000_1505);

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": seed_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-monotonic",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "meta": { "version": 1, "producer": producer_id, "sequence": 5 },
        })))
        .send()
        .await
        .assert_status(202);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_1514),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-monotonic",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:00Z",
                    "meta": { "version": 1, "producer": producer_id, "sequence": 4 },
                },
                {
                    "id": Uuid::from_u128(0xe000_1515),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-monotonic",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    "meta": { "version": 1, "producer": producer_id, "sequence": 5 },
                },
                {
                    "id": Uuid::from_u128(0xe000_1516),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-monotonic",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:02Z",
                    "meta": { "version": 1, "producer": producer_id, "sequence": 6 },
                },
                {
                    "id": Uuid::from_u128(0xe000_1517),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-monotonic",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:03Z",
                    "meta": { "version": 1, "producer": producer_id, "sequence": 7 },
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 7,
            }],
        })
    );
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            Uuid::from_u128(0xe000_1517)
        )
        .await,
        vec![
            seed_id,
            Uuid::from_u128(0xe000_1516),
            Uuid::from_u128(0xe000_1517),
        ],
        "only the sequences past the head are stored"
    );
}

#[tokio::test]
async fn a_412_names_the_run_index_producer_and_committed_head() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1601);
    let seed_id = Uuid::from_u128(0xe000_1607);

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": seed_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-stale",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 6, "sequence": 7 },
        })))
        .send()
        .await
        .assert_status(202);

    // A stateless event first, so the run's first event sits at index 1. The
    // run links on its own but is behind the head (7).
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_1610),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-stale",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:00Z",
                },
                {
                    "id": Uuid::from_u128(0xe000_1611),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-stale",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 3, "sequence": 4 },
                },
                {
                    "id": Uuid::from_u128(0xe000_1612),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-stale",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:02Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 4, "sequence": 5 },
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
                "resource_name": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "violations": [{
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": format!(
                        "index=1 producer={producer_id} \
                         topic=gts.cf.core.events.topic.v1~x.eb.t1.topic.v1 last_sequence=7"
                    ),
                }],
            },
        })
    );

    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 7,
            }],
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1601), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![seed_id, sentinel],
        "a 412 batch stores nothing, its stateless event included"
    );
}

#[tokio::test]
async fn an_all_duplicate_batch_answers_200() {
    let harness = run_rules_harness().await;
    let first_producer = register_test_producer(&harness).await;
    let second_producer = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1701);
    let batch = json!({
        "events": [
            {
                "id": Uuid::from_u128(0xe000_1701),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-two-runs",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:11:00Z",
                "meta": { "version": 1, "producer": first_producer, "previous": 0, "sequence": 1 },
            },
            {
                "id": Uuid::from_u128(0xe000_1702),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-two-runs",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:11:01Z",
                "meta": { "version": 1, "producer": second_producer, "previous": 0, "sequence": 1 },
            },
            {
                "id": Uuid::from_u128(0xe000_1703),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": tenant_id,
                "source": "order-service",
                "subject": "order-two-runs",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:11:02Z",
                "meta": { "version": 1, "producer": first_producer, "previous": 1, "sequence": 2 },
            },
        ],
    });
    harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&batch))
        .send()
        .await
        .assert_status(202);

    let resend = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&batch))
        .send()
        .await;

    resend.assert_status(200);
    assert_eq!(resend.text(), "", "200 OK must carry no body");
    for (producer_id, last_sequence) in [(first_producer, 2), (second_producer, 1)] {
        let cursors = harness
            .api_v1()
            .get_producer_cursors(&producer_id.to_string())
            .send()
            .await;
        cursors.assert_status(200);
        assert_eq!(
            cursors.json(),
            json!({
                "id": producer_id,
                "client_agent": "test-agent",
                "topics": [{
                    "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                    "last_sequence": last_sequence,
                }],
            })
        );
    }
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1701), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![
            Uuid::from_u128(0xe000_1701),
            Uuid::from_u128(0xe000_1702),
            Uuid::from_u128(0xe000_1703),
            sentinel,
        ],
        "each event of both runs is stored once"
    );
}

#[tokio::test]
async fn a_batch_with_a_stateless_event_beside_duplicates_answers_202() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1801);
    let head = json!({
        "id": Uuid::from_u128(0xe000_1801),
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": tenant_id,
        "source": "order-service",
        "subject": "order-mixed",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-05-29T10:11:00Z",
        "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
    });
    harness
        .api_v1()
        .post_events()
        .with_body(Json(&head))
        .send()
        .await
        .assert_status(202);

    // A stateless event is never a duplicate, so the batch is not all
    // duplicates even though its only chained event is.
    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                head,
                {
                    "id": Uuid::from_u128(0xe000_1802),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-mixed",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:11:01Z",
                },
            ],
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 1,
            }],
        })
    );
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            Uuid::from_u128(0xe000_1802)
        )
        .await,
        vec![Uuid::from_u128(0xe000_1801), Uuid::from_u128(0xe000_1802)],
        "the duplicate is stored once and the stateless event is stored"
    );
}

// -- authz enforcement (`gears-rust#4516`, `eb-authz-enforcement`) --

#[tokio::test]
async fn publish_rejected_for_missing_event_type_produce_permission_returns_403_before_type_lookup()
{
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            // Denies only the type check: `produce` on the event type, not
            // the tenant check on the tenant-scope resource.
            deny_if: |req: &EvaluationRequest| {
                req.action.name == "produce"
                    && req.resource.resource_type != "gts.cf.core.events.request.v1~"
            },
        })))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            // Never seeded - proves the check runs before the event-type
            // lookup that resolves the topic (a 404 here instead of 403 would
            // mean the ordering regressed).
            "type": "gts.cf.core.events.event.v1~x.eb.missing.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_rejected_for_missing_event_type_produce_permission_returns_403_before_type_lookup",
            "subject": "s-publish_rejected_for_missing_event_type_produce_permission_returns_403_before_type_lookup",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/events",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn publish_rejected_for_unauthorized_tenant_id_returns_403() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            // Denies the tenant check (`produce` on the tenant-scope resource)
            // and the barrier right (`produce_ignore_barrier`), so the event's
            // tenant is unreachable; the type check would pass.
            deny_if: |req: &EvaluationRequest| {
                (req.action.name == "produce"
                    && req.resource.resource_type == "gts.cf.core.events.request.v1~")
                    || req.action.name == "produce_ignore_barrier"
            },
        })))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_rejected_for_unauthorized_tenant_id_returns_403",
            "subject": "s-publish_rejected_for_unauthorized_tenant_id_returns_403",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/events",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn publish_checks_produce_on_the_events_own_type_not_a_family_label() {
    let target_tenant = Uuid::new_v4();
    let asked = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let recorder = Arc::clone(&asked);
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: move |req: &EvaluationRequest| {
                if req.resource.properties.contains_key("event_type_id")
                    || req.resource.properties.contains_key("topic_id")
                {
                    assert_eq!(
                        req.resource.properties.get("owner_tenant_id"),
                        Some(&json!(target_tenant))
                    );
                }
                recorder
                    .lock()
                    .unwrap()
                    .push((req.action.name.clone(), req.resource.resource_type.clone()));
                false
            },
        })))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": target_tenant,
            "source": "test-publish_checks_produce_on_the_events_own_type",
            "subject": "s-publish_checks_produce_on_the_events_own_type",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
    let produce: Vec<String> = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(action, resource)| {
            action == "produce" && resource != "gts.cf.core.events.request.v1~"
        })
        .map(|(_, resource)| resource.clone())
        .collect();
    assert_eq!(produce, ["gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"]);
}

#[tokio::test]
async fn publish_succeeds_when_produce_permission_and_tenant_checks_both_pass() {
    let allowed_tenant_id = Uuid::new_v4();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            // Allows only the exact event type and tenant this test
            // publishes as - proves the real values get checked, not a
            // hardcoded pass.
            deny_if: move |req: &EvaluationRequest| {
                let allowed_event_type = req.resource.properties.get("event_type_id")
                    == Some(&json!("gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"));
                let allowed_tenant = req.resource.properties.get(pep_properties::OWNER_TENANT_ID)
                    == Some(&json!(allowed_tenant_id));
                !(allowed_event_type || allowed_tenant)
            },
        })))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": allowed_tenant_id,
            "source": "test-publish_succeeds_when_produce_permission_and_tenant_checks_both_pass",
            "subject": "s-publish_succeeds_when_produce_permission_and_tenant_checks_both_pass",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted must carry no body");
}

#[tokio::test]
async fn publish_batch_aborts_on_an_unauthorized_event() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
            // On the same topic as `foo.v1`, so the batch is homogeneous and
            // the denial below is the only thing that rejects an event.
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.bar.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: |req: &EvaluationRequest| {
                req.resource.properties.get("event_type_id")
                    == Some(&json!("gts.cf.core.events.event.v1~x.eb.t1.bar.v1~"))
            },
        })))
        .build()
        .await;
    let requests = vec![
        PublishRequest {
            id: Uuid::new_v4(),
            r#type: crate::test_support::event_type_id(
                "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            ),
            subject_tenant: Uuid::new_v4(),
            source: "test".to_owned(),
            subject: "s1".to_owned(),
            subject_type: gts::GtsTypeId::new("gts.x.eb.t1.subject.v1~"),
            subject_owner: None,
            occurred_at: Utc::now(),
            trace_parent: None,
            data: json!({}),
            meta: None,
        },
        PublishRequest {
            id: Uuid::new_v4(),
            r#type: crate::test_support::event_type_id(
                "gts.cf.core.events.event.v1~x.eb.t1.bar.v1~",
            ),
            subject_tenant: Uuid::new_v4(),
            source: "test".to_owned(),
            subject: "s1".to_owned(),
            subject_type: gts::GtsTypeId::new("gts.x.eb.t1.subject.v1~"),
            subject_owner: None,
            occurred_at: Utc::now(),
            trace_parent: None,
            data: json!({}),
            meta: None,
        },
    ];

    // A batch is all-or-nothing, so a denial on any one event, here the second,
    // rejects the whole request as a batch-level error; it is never reported as
    // a per-event failure behind a `202`.
    let err = harness
        .ingest()
        .publish_batch(harness.security_context(), requests)
        .await
        .expect_err("an unauthorized event must abort the whole batch");
    assert!(
        matches!(
            &err,
            DomainError::Forbidden {
                code: ErrorCode::NotAuthorizedToProduce,
                ..
            }
        ),
        "expected Forbidden/NotAuthorizedToProduce, got {err:?}"
    );
}

// -- event-type / subject-type enforcement (`eb-event-type-enforcement`) --

#[tokio::test]
async fn publish_event_unregistered_event_type_returns_404_event_type_not_found() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.unregistered.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_unregistered_event_type_returns_404_event_type_not_found",
            "subject": "s-publish_event_unregistered_event_type_returns_404_event_type_not_found",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "event type 'gts.cf.core.events.event.v1~x.eb.t1.unregistered.v1~' is not registered",
            "instance": "/event-broker/v1/events",
            "context": {
                "resource_name": "gts.cf.core.events.event.v1~x.eb.t1.unregistered.v1~",
                "resource_type": "gts.cf.core.events.event_type.v1~",
            },
        })
    );
}

#[tokio::test]
async fn publish_event_with_nothing_registered_returns_404_event_type_not_found() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.unregistered.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_with_nothing_registered",
            "subject": "s-publish_event_with_nothing_registered",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    // The event type resolves the topic, so with nothing registered the type
    // lookup is the one that fails - `EventTypeNotFound`, never
    // `TopicNotFound`: the request names no topic of its own to miss on.
    resp.assert_status(404);
    assert_eq!(
        resp.json()["context"]["resource_type"],
        json!("gts.cf.core.events.event_type.v1~")
    );
}

#[tokio::test]
async fn publish_event_malformed_subject_type_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_malformed_subject_type_returns_400",
            "subject": "s-publish_event_malformed_subject_type_returns_400",
            "subject_type": "not-a-gts-id",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(400);
    let body = resp.json();
    assert_eq!(
        body["type"],
        json!("gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
    );
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .starts_with("InvalidSubjectType:"),
        "detail must start with 'InvalidSubjectType:', got: {}",
        body["detail"]
    );
}

/// One producer cannot choose grouping per message: the field is gone from the
/// schema, and a body that carries it is refused rather than accepted and
/// ignored - which would leave the publisher believing it had chosen.
///
/// The refusal happens at deserialization, so the handler reports it as a `400`
/// `InvalidBody` (a producer request to fix) - the same shape a read-only field
/// on publish input produces (see
/// `publish_event_readonly_field_rejected_via_deny_unknown_fields`). What this
/// pins is the half that matters: refused, not ignored.
#[tokio::test]
async fn publish_event_carrying_a_partition_key_is_refused() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 4 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_carrying_a_partition_key_is_refused",
            "subject": "s-publish_event_carrying_a_partition_key_is_refused",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
            "partition_key": "chosen-by-the-publisher",
        })))
        .send()
        .await;

    resp.assert_status(400);
    let body = resp.json();
    assert_eq!(
        body["type"],
        json!("gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
    );
    assert!(
        body["detail"].as_str().unwrap().starts_with("InvalidBody:"),
        "detail must start with 'InvalidBody:', got: {}",
        body["detail"]
    );
}

/// The event's own type is a GTS **type** identifier: a concrete event type is
/// a derived type schema, so an identifier without the trailing `~` names
/// nothing that can exist. Rejected in the DTO, on the same terms as any other
/// malformed identifier, rather than reaching a lookup that would answer
/// "unregistered" and send the publisher looking for a registration problem.
#[tokio::test]
async fn publish_event_instance_shaped_type_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            // The same registered type, spelled as an instance would be.
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_instance_shaped_type_returns_400",
            "subject": "s-publish_event_instance_shaped_type_returns_400",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
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
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "type",
                    "description": "must be a valid GTS type identifier",
                    "reason": "invalid_event_field",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

// -- trace_parent format (scenario producer/single/1.07) --

#[tokio::test]
async fn a_publish_with_a_malformed_trace_parent_is_rejected_and_stores_nothing() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1201);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1201),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-42",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "trace_parent": "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01",
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
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "trace_parent",
                    "description": "must be a W3C traceparent: four lowercase hex groups of 2, 32, 16 and 2 digits joined by '-'",
                    "reason": "invalid_event_field",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1201), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a rejected publish stores nothing"
    );
}

#[tokio::test]
async fn a_batch_with_one_malformed_trace_parent_is_rejected_whole() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1202);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_1202),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-42",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:10:00Z",
                },
                {
                    "id": Uuid::from_u128(0xe000_1203),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-42",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:10:01Z",
                    "trace_parent": "00-0af7-b7ad6b7169203331-01",
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
                    "field": "trace_parent",
                    "description": "must be a W3C traceparent: four lowercase hex groups of 2, 32, 16 and 2 digits joined by '-'",
                    "reason": "invalid_event_field",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1202), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "the valid first event of the rejected batch must not be stored"
    );
}

/// The type is well-formed but unregistered, which alone answers `404`: the
/// envelope's format is checked before the type is looked up.
#[tokio::test]
async fn a_malformed_trace_parent_is_rejected_before_the_type_is_looked_up() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1203);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1204),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.unregistered.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-42",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "trace_parent": "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01",
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
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "trace_parent",
                    "description": "must be a W3C traceparent: four lowercase hex groups of 2, 32, 16 and 2 digits joined by '-'",
                    "reason": "invalid_event_field",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );

    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1203), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a rejected publish stores nothing"
    );
}

#[tokio::test]
async fn a_well_formed_trace_parent_is_accepted() {
    let harness = run_rules_harness().await;
    let event_id = Uuid::from_u128(0xe000_1205);
    let tenant_id = Uuid::from_u128(0x7e7a_1204);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": event_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-42",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:10:00Z",
            "trace_parent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "data": { "order_id": "order-42" },
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted carries no body");
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            event_id
        )
        .await,
        vec![event_id]
    );
    let stored = harness
        .backend()
        .read(
            &event_broker_sdk::backend::TopicPartition::new(
                event_broker_sdk::GtsInstanceId::try_new(
                    "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                )
                .expect("a valid topic id"),
                event_broker_sdk::backend::Partition::new(0),
            ),
            event_broker_sdk::Sequence::NONE,
            10,
            event_broker_sdk::backend::Wait::No,
        )
        .await
        .expect("read the stored events");
    let [event] = stored.as_slice() else {
        panic!("expected exactly one stored event, got {stored:?}");
    };
    // The SDK `Event` has no `PartialEq`, so the publisher-set fields compare
    // one by one; the broker stamps the rest.
    assert_eq!(
        (
            event.id,
            event.event_type.as_ref(),
            event.subject_tenant,
            event.source.as_str(),
            event.subject.as_str(),
            event.subject_type.as_ref(),
            event.occurred_at.to_rfc3339(),
            event.trace_parent.as_deref(),
            &event.data,
        ),
        (
            event_id,
            "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            tenant_id,
            "order-service",
            "order-42",
            "gts.x.eb.t1.subject.v1~",
            "2026-05-29T10:10:00+00:00".to_owned(),
            Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
            &Some(json!({ "order_id": "order-42" })),
        )
    );
}

/// The other side of the same contract: the identifier every committed scenario
/// writes - a derived event type, trailing `~` and all - is accepted.
#[tokio::test]
async fn publish_event_with_a_scenario_shaped_type_returns_202() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~acme.shop._.orders.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~acme.shop.orders.created.v1~",
                "topic": "gts.cf.core.events.topic.v1~acme.shop._.orders.v1",
                "allowed_subject_types": ["gts.acme.shop.orders.order.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~acme.shop.orders.created.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_with_a_scenario_shaped_type_returns_202",
            "subject": "order-1",
            "subject_type": "gts.acme.shop.orders.order.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
    assert_eq!(resp.text(), "", "202 Accepted carries no body");
}

#[tokio::test]
async fn publish_event_instance_shaped_subject_type_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_instance_shaped_subject_type_returns_400",
            "subject": "s-publish_event_instance_shaped_subject_type_returns_400",
            // Well-formed GTS id, but an Instance (no trailing '~') - proves
            // the type-vs-instance check, not just a raw parse failure.
            "subject_type": "gts.x.eb.t1.subject.v1~a.b.c.d.v1",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(400);
    let body = resp.json();
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .starts_with("InvalidSubjectType:"),
        "detail must start with 'InvalidSubjectType:', got: {}",
        body["detail"]
    );
}

#[tokio::test]
async fn publish_event_subject_type_exact_match_returns_202() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_subject_type_exact_match_returns_202",
            "subject": "s-publish_event_subject_type_exact_match_returns_202",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
}

#[tokio::test]
async fn publish_event_subject_type_wildcard_suffix_match_returns_202() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~acme.*"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_subject_type_wildcard_suffix_match_returns_202",
            "subject": "s-publish_event_subject_type_wildcard_suffix_match_returns_202",
            // Chained under the wildcard's "acme" prefix - a Type, still
            // ending in '~'.
            "subject_type": "gts.x.eb.t1.subject.v1~acme.corp.internal.vip_subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
}

#[tokio::test]
async fn publish_event_subject_type_wildcard_wrong_prefix_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~acme.*"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_subject_type_wildcard_wrong_prefix_returns_400",
            "subject": "s-publish_event_subject_type_wildcard_wrong_prefix_returns_400",
            // Same shape as the wildcard-match test, but chained under a
            // different vendor ("other", not "acme") - proves the
            // wildcard's fixed prefix is actually enforced, not a
            // match-everything fallback (`GlobalTypeSystem/gts-spec` §3.5).
            "subject_type": "gts.x.eb.t1.subject.v1~other.corp.internal.vip_subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(400);
    let body = resp.json();
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .starts_with("SubjectTypeNotAllowed:"),
        "detail must start with 'SubjectTypeNotAllowed:', got: {}",
        body["detail"]
    );
}

#[tokio::test]
async fn publish_event_subject_type_bare_base_implicit_coverage_returns_202() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_subject_type_bare_base_implicit_coverage_returns_202",
            "subject": "s-publish_event_subject_type_bare_base_implicit_coverage_returns_202",
            // Chained under the bare-base pattern - matched via GTS's
            // "implicit derived-type coverage" (`gts-spec` §3.6), not exact
            // string equality.
            "subject_type": "gts.x.eb.t1.subject.v1~acme.corp.internal.vip_subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(202);
}

#[tokio::test]
async fn publish_event_subject_type_not_allowed_returns_400() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.other_subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_subject_type_not_allowed_returns_400",
            "subject": "s-publish_event_subject_type_not_allowed_returns_400",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
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
            "detail": "SubjectTypeNotAllowed: subject_type is not in \
                       event type 'gts.cf.core.events.event.v1~x.eb.t1.foo.v1~''s \
                       allowed_subject_types",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "SubjectTypeNotAllowed: subject_type is not \
                           in event type 'gts.cf.core.events.event.v1~x.eb.t1.foo.v1~''s \
                           allowed_subject_types",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

#[tokio::test]
async fn publish_event_with_a_nil_tenant_returns_400() {
    // A backend that never stores keeps an admitted event in the outbox, so
    // `outbox_bodies` counts it.
    let harness = EventBrokerHarness::builder()
        .with_backend(Arc::new(NeverStoringBackend::new(Arc::new(
            memory_event_broker_plugin::MemoryBackend::new(),
        ))))
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": "00000000-0000-0000-0000-000000000000",
            "source": "test-publish_event_with_a_nil_tenant_returns_400",
            "subject": "s-publish_event_with_a_nil_tenant_returns_400",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
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
            "detail": "InvalidTenant: event 0: the event's tenant must not be the nil UUID",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "InvalidTenant: event 0: the event's tenant must not be the nil UUID",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
/// The nil tenant is the second event, so the rejection names its index and the
/// whole batch is refused: the other two events are not enqueued either.
async fn publish_batch_with_a_nil_tenant_event_returns_400() {
    // A backend that never stores keeps an admitted event in the outbox, so
    // `outbox_bodies` counts it.
    let harness = EventBrokerHarness::builder()
        .with_backend(Arc::new(NeverStoringBackend::new(Arc::new(
            memory_event_broker_plugin::MemoryBackend::new(),
        ))))
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(145),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7a),
                    "source": "test-publish_batch_with_a_nil_tenant_event_returns_400",
                    "subject": "s-publish_batch_with_a_nil_tenant_event_returns_400",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:13:00Z",
                },
                {
                    "id": Uuid::from_u128(146),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": "00000000-0000-0000-0000-000000000000",
                    "source": "test-publish_batch_with_a_nil_tenant_event_returns_400",
                    "subject": "s-publish_batch_with_a_nil_tenant_event_returns_400",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:13:00Z",
                },
                {
                    "id": Uuid::from_u128(147),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": Uuid::from_u128(0x7b),
                    "source": "test-publish_batch_with_a_nil_tenant_event_returns_400",
                    "subject": "s-publish_batch_with_a_nil_tenant_event_returns_400",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:13:00Z",
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
            "detail": "InvalidTenant: event 1: the event's tenant must not be the nil UUID",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "format": "InvalidTenant: event 1: the event's tenant must not be the nil UUID",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
async fn publish_event_empty_allowed_subject_types_rejects_everything() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                // No `allowed_subject_types` key at all - defaults to `[]`.
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_empty_allowed_subject_types_rejects_everything",
            "subject": "s-publish_event_empty_allowed_subject_types_rejects_everything",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(400);
    let body = resp.json();
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .starts_with("SubjectTypeNotAllowed:"),
        "detail must start with 'SubjectTypeNotAllowed:', got: {}",
        body["detail"]
    );
}

#[tokio::test]
async fn publish_event_unregistered_event_type_short_circuits_subject_type_check() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.unregistered.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_unregistered_event_type_short_circuits_subject_type_check",
            "subject": "s-publish_event_unregistered_event_type_short_circuits_subject_type_check",
            // Malformed - would also fail the subject-type check if it were
            // ever reached. It must not be: event-type lookup (step 4) runs
            // before subject-type membership (step 5).
            "subject_type": "not-a-gts-id",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(404);
    let body = resp.json();
    assert_eq!(
        body["context"]["resource_type"],
        json!("gts.cf.core.events.event_type.v1~")
    );
}

// -- envelope field encoding and length -----------------------------------
//
// The validation pipeline puts schema-level field checks ahead of event-type
// lookup, so these run before the type is resolved.

fn envelope_violation(field: &str, description: &str, reason: &str) -> serde_json::Value {
    json!({
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "Request validation failed",
        "instance": "/event-broker/v1/events",
        "context": {
            "field_violations": [{
                "field": field,
                "description": description,
                "reason": reason,
            }],
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    })
}

#[tokio::test]
async fn publish_event_rejects_a_non_printable_source() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "orders\nservice",
            "subject": "s1",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        envelope_violation(
            "source",
            "must contain only printable ASCII (0x20-0x7E)",
            "ascii_only",
        )
    );
}

#[tokio::test]
async fn publish_event_rejects_an_over_length_subject() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "orders-service",
            "subject": "s".repeat(1025),
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        envelope_violation(
            "subject",
            "must be 1-1024 bytes, got 1025",
            "field_too_long"
        )
    );
}

#[tokio::test]
async fn publish_event_envelope_encoding_is_checked_before_event_type_lookup() {
    // No type registry, so the type below is unregistered either way.
    let harness = EventBrokerHarness::builder().build().await;

    let body = |source: &str| {
        json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.unregistered.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": source,
            "subject": "s1",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": Utc::now().to_rfc3339(),
        })
    };

    // Control: with a well-formed envelope the same publish fails on the
    // lookup, which is what makes the assertion below meaningful rather than
    // vacuous - the type really is unregistered.
    let control = harness
        .api_v1()
        .post_events()
        .with_body(Json(&body("orders-service")))
        .send()
        .await;
    assert_eq!(
        control.json()["status"],
        json!(404),
        "the control publish must fail on event-type lookup, got {}",
        control.json()
    );

    // A mis-encoded `source` on the same unregistered type reports the
    // encoding failure, because the envelope check precedes the lookup.
    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&body("orders\u{1}service")))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        envelope_violation(
            "source",
            "must contain only printable ASCII (0x20-0x7E)",
            "ascii_only",
        )
    );
}

/// A type grant in another tenant must not enqueue or advance producer state.
#[tokio::test]
async fn tenant_scoped_produce_denial_has_no_persistence_side_effects() {
    let target = Uuid::new_v4();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            { "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~", "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"] },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: move |req: &EvaluationRequest| {
                req.resource.resource_type == "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"
                    && req.resource.properties.get("owner_tenant_id") == Some(&json!(target))
            },
        })))
        .build().await;
    let producer = register_test_producer(&harness).await;
    let result = harness
        .ingest()
        .publish_event(
            harness.security_context(),
            PublishRequest {
                id: Uuid::new_v4(),
                r#type: crate::test_support::event_type_id(
                    "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                ),
                subject_tenant: target,
                source: "denied".to_owned(),
                subject: "s".to_owned(),
                subject_type: gts::GtsTypeId::new("gts.x.eb.t1.subject.v1~"),
                subject_owner: None,
                occurred_at: Utc::now(),
                trace_parent: None,
                data: json!({}),
                meta: Some(crate::domain::ingest::PublishMeta {
                    version: 1,
                    producer: Some(producer),
                    previous: Some(0),
                    sequence: Some(1),
                }),
            },
        )
        .await;
    assert!(matches!(
        result,
        Err(DomainError::Forbidden {
            code: ErrorCode::NotAuthorizedToProduce,
            ..
        })
    ));
    let cursors = harness
        .ingest()
        .get_producer_cursors(harness.security_context(), producer)
        .await
        .unwrap();
    assert!(
        cursors.topics.is_empty(),
        "denial must not advance producer state"
    );
    let events = harness
        .backend()
        .read(
            &event_broker_sdk::backend::TopicPartition::new(
                event_broker_sdk::GtsInstanceId::try_new(
                    "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                )
                .unwrap(),
                event_broker_sdk::backend::Partition::new(0),
            ),
            event_broker_sdk::Sequence::NONE,
            10,
            event_broker_sdk::backend::Wait::No,
        )
        .await
        .unwrap();
    assert!(events.is_empty(), "denial must not persist an event");
}

/// Two types on one topic: `foo` partitions on the tenant, `keyed` on `/data/k`.
/// One ingest outbox partition, so a sentinel published last drains behind every
/// event written before it.
async fn size_harness() -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_outbox_partitions(
            crate::domain::outbox::OutboxPartitions::new(1).expect("a power of 2 within 64"),
        )
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.keyed.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "data_schema": {
                    "type": "object",
                    "properties": { "k": { "type": "string" } },
                },
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
                "partition_key": "/data/k",
            },
        ])))
        .build()
        .await
}

/// A `foo` event whose `data.pad` is sized so the event encodes to exactly `bytes`.
fn foo_event_of(id: u128, bytes: usize) -> serde_json::Value {
    let mut body = json!({
        "id": Uuid::from_u128(id),
        "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
        "subject_tenant": Uuid::from_u128(0x7e7a_2001),
        "source": "order-service",
        "subject": "order-1",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-11-09T10:00:00Z",
        "data": { "pad": "" },
    });
    let event: event_broker_sdk::EventV1 =
        serde_json::from_value(body.clone()).expect("the SDK reads the body");
    let unpadded = event.encoded_len().expect("the event measures");
    body["data"]["pad"] = json!("x".repeat(bytes - unpadded));
    body
}

/// A `keyed` event whose partition key is `key`.
fn keyed_event(id: u128, key: &str) -> serde_json::Value {
    json!({
        "id": Uuid::from_u128(id),
        "type": "gts.cf.core.events.event.v1~x.eb.t1.keyed.v1~",
        "subject_tenant": Uuid::from_u128(0x7e7a_2001),
        "source": "order-service",
        "subject": "order-1",
        "subject_type": "gts.x.eb.t1.subject.v1~",
        "occurred_at": "2026-11-09T10:00:00Z",
        "data": { "k": key },
    })
}

/// How many events the topic holds, written or drained: an accepted event's body
/// stays in the outbox until the event is on the backend, so a count of 0 means
/// nothing was written. An event between its append and its body's removal counts
/// twice, so a positive count is read through a sentinel instead.
async fn written_events(harness: &EventBrokerHarness) -> usize {
    use event_broker_sdk::backend::{Partition, TopicPartition, Wait};

    let at = TopicPartition::new(
        toolkit_gts::GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.t1.topic.v1")
            .expect("a valid topic id"),
        Partition::new(0),
    );
    let bodies = usize::try_from(harness.outbox_bodies().await).expect("a count");
    let stored = harness
        .backend()
        .read(&at, crate::domain::model::Sequence::NONE, 1024, Wait::No)
        .await
        .expect("read the stored events")
        .len();
    bodies + stored
}

#[tokio::test]
async fn an_event_at_the_size_limit_is_accepted_and_one_byte_over_answers_413() {
    let harness = size_harness().await;

    let at_limit = harness
        .api_v1()
        .post_events()
        .with_body(Json(&foo_event_of(0xe000_2001, 61_440)))
        .send()
        .await;
    let over = harness
        .api_v1()
        .post_events()
        .with_body(Json(&foo_event_of(0xe000_2002, 61_441)))
        .send()
        .await;

    at_limit.assert_status(202);
    over.assert_status(413);
    assert_eq!(
        over.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 413,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "events[0]",
                    "description": "61441 bytes encoded, over the limit of 61440",
                    "reason": "event_too_large",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(
        &harness,
        Uuid::from_u128(0x5e7_2001),
        Uuid::from_u128(0x7e7a_2001),
    )
    .await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![Uuid::from_u128(0xe000_2001), sentinel],
        "only the event at the limit, then the sentinel"
    );
}

#[tokio::test]
async fn a_batch_with_an_event_over_the_size_limit_answers_413_naming_it_and_writes_nothing() {
    let harness = size_harness().await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                foo_event_of(0xe000_2011, 61_440),
                foo_event_of(0xe000_2012, 61_441),
            ],
        })))
        .send()
        .await;

    resp.assert_status(413);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 413,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "field_violations": [{
                    "field": "events[1]",
                    "description": "61441 bytes encoded, over the limit of 61440",
                    "reason": "event_too_large",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(written_events(&harness).await, 0);
}

#[tokio::test]
async fn a_partition_key_at_its_cap_is_accepted_and_one_byte_over_answers_400() {
    let harness = size_harness().await;

    let at_cap = harness
        .api_v1()
        .post_events()
        .with_body(Json(&keyed_event(0xe000_2021, &"k".repeat(1024))))
        .send()
        .await;
    // Each quote is escaped, so 513 of them are 1026 bytes as the row stores them.
    let over = harness
        .api_v1()
        .post_events()
        .with_body(Json(&keyed_event(0xe000_2022, &"\"".repeat(513))))
        .send()
        .await;

    at_cap.assert_status(202);
    over.assert_status(400);
    assert_eq!(
        over.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "events[0].partition_key",
                    "description": "1026 bytes, over the limit of 1024",
                    "reason": "partition_key_too_long",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(
        &harness,
        Uuid::from_u128(0x5e7_2021),
        Uuid::from_u128(0x7e7a_2001),
    )
    .await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![Uuid::from_u128(0xe000_2021), sentinel],
        "only the event at the cap, then the sentinel"
    );
}

#[tokio::test]
async fn a_batch_with_a_partition_key_over_its_cap_answers_400_naming_it_and_writes_nothing() {
    let harness = size_harness().await;

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                keyed_event(0xe000_2031, "order-1"),
                keyed_event(0xe000_2032, &"k".repeat(1025)),
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
                    "field": "events[1].partition_key",
                    "description": "1025 bytes, over the limit of 1024",
                    "reason": "partition_key_too_long",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(written_events(&harness).await, 0);
}

/// `Prefer: wait` takes the synchronous path, which holds an event to the same per-event
/// limit and partition-key cap as the default path, single and batch: each refusal is
/// answered before anything is written.
#[tokio::test]
async fn a_synchronous_publish_over_a_size_limit_is_refused_and_writes_nothing() {
    let harness = size_harness().await;
    let prefer = || {
        (
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
    };

    let (name, value) = prefer();
    let single = harness
        .api_v1()
        .post_events()
        .with_header(name, value)
        .with_body(Json(&foo_event_of(0xe000_2041, 61_441)))
        .send()
        .await;
    let (name, value) = prefer();
    let batch = harness
        .api_v1()
        .post_events_batch()
        .with_header(name, value)
        .with_body(Json(&json!({
            "events": [
                foo_event_of(0xe000_2042, 61_440),
                foo_event_of(0xe000_2043, 61_441),
            ],
        })))
        .send()
        .await;
    let (name, value) = prefer();
    let keyed = harness
        .api_v1()
        .post_events()
        .with_header(name, value)
        .with_body(Json(&keyed_event(0xe000_2044, &"k".repeat(1025))))
        .send()
        .await;

    single.assert_status(413);
    assert_eq!(
        single.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 413,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "events[0]",
                    "description": "61441 bytes encoded, over the limit of 61440",
                    "reason": "event_too_large",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    batch.assert_status(413);
    assert_eq!(
        batch.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 413,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "field_violations": [{
                    "field": "events[1]",
                    "description": "61441 bytes encoded, over the limit of 61440",
                    "reason": "event_too_large",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    keyed.assert_status(400);
    assert_eq!(
        keyed.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/events",
            "context": {
                "field_violations": [{
                    "field": "events[0].partition_key",
                    "description": "1025 bytes, over the limit of 1024",
                    "reason": "partition_key_too_long",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(written_events(&harness).await, 0);
}

// -- meta mode shape (ADR-0004 "Mode-Shape Enforcement at Publish Time") ----

/// Registers a monotonic-mode producer and returns its id.
async fn register_monotonic_producer(harness: &EventBrokerHarness) -> Uuid {
    harness
        .ingest()
        .register_producer(
            harness.security_context(),
            ProducerRegistrationInput {
                mode: ProducerMode::Monotonic,
                client_agent: "test-agent".to_owned(),
            },
        )
        .await
        .expect("producer registration must succeed")
        .id
}

#[tokio::test]
async fn a_meta_with_a_sequence_and_no_producer_is_refused_as_meta_without_producer_id() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1801);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1801),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "sequence": 5 },
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
            "detail": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1801), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_meta_with_a_previous_and_no_producer_is_refused_as_meta_without_producer_id() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1811);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1811),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "previous": 4 },
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
            "detail": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1811), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

/// A literal nil producer id names no producer, the same as an absent one.
#[tokio::test]
async fn a_meta_with_a_nil_producer_and_a_sequence_is_refused_as_meta_without_producer_id() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1821);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1821),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "producer": Uuid::nil(), "sequence": 3 },
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
            "detail": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1821), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

/// The synchronous path runs the same check before anything is enqueued.
#[tokio::test]
async fn a_sync_publish_with_a_sequence_and_no_producer_is_refused_as_meta_without_producer_id() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1831);

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::HeaderName::from_static("prefer"),
            axum::http::HeaderValue::from_static("wait=5"),
        )
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1831),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "sequence": 5 },
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
            "detail": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1831), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_chained_publish_without_previous_is_refused_as_chain_mode_fields_missing() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1841);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1841),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "producer": producer_id, "sequence": 1 },
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
            "detail": "ChainModeFieldsMissing: a chained-mode publish needs meta.previous and meta.sequence",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "ChainModeFieldsMissing: a chained-mode publish needs meta.previous and meta.sequence",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1841), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_chained_publish_without_sequence_is_refused_as_chain_mode_fields_missing() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1851);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1851),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 0 },
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
            "detail": "ChainModeFieldsMissing: a chained-mode publish needs meta.previous and meta.sequence",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "ChainModeFieldsMissing: a chained-mode publish needs meta.previous and meta.sequence",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1851), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_monotonic_publish_with_previous_is_refused_as_monotonic_mode_fields_violation() {
    let harness = run_rules_harness().await;
    let producer_id = register_monotonic_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1861);
    let seed_id = Uuid::from_u128(0xe000_1867);

    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": seed_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "producer": producer_id, "sequence": 7 },
        })))
        .send()
        .await
        .assert_status(202);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1868),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:01Z",
            "meta": { "version": 1, "producer": producer_id, "previous": 7, "sequence": 8 },
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
            "detail": "MonotonicModeFieldsViolation: a monotonic-mode publish needs meta.sequence and must omit meta.previous",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "MonotonicModeFieldsViolation: a monotonic-mode publish needs meta.sequence and must omit meta.previous",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({
            "id": producer_id,
            "client_agent": "test-agent",
            "topics": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "last_sequence": 7,
            }],
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1861), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![seed_id, sentinel],
        "the refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_monotonic_publish_without_sequence_is_refused_as_monotonic_mode_fields_violation() {
    let harness = run_rules_harness().await;
    let producer_id = register_monotonic_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1871);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1871),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "producer": producer_id },
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
            "detail": "MonotonicModeFieldsViolation: a monotonic-mode publish needs meta.sequence and must omit meta.previous",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "MonotonicModeFieldsViolation: a monotonic-mode publish needs meta.sequence and must omit meta.previous",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1871), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_meta_version_above_the_supported_one_is_refused_as_unknown_meta_version() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_1881);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_1881),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 2, "producer": producer_id, "previous": 0, "sequence": 1 },
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
            "detail": "UnknownMetaVersion: meta.version is newer than the broker supports (1)",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "UnknownMetaVersion: meta.version is newer than the broker supports (1)",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_1881), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}
/// The version check applies to every `meta` that is present (ADR-0003,
/// "Optional Versioned `meta` Block"): a block that names no producer and
/// carries no chain field is not stateless when its version is newer.
#[tokio::test]
async fn a_newer_meta_version_with_no_producer_is_refused_as_unknown_meta_version() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_18e1);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_18e1),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-stateless-meta",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 2 },
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
            "detail": "UnknownMetaVersion: meta.version is newer than the broker supports (1)",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "UnknownMetaVersion: meta.version is newer than the broker supports (1)",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_18e1), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_batch_with_a_newer_meta_version_with_no_producer_is_refused_as_unknown_meta_version() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_18f1);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_18f1),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-stateless-meta",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:40:00Z",
                },
                {
                    "id": Uuid::from_u128(0xe000_18f2),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-stateless-meta",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:40:01Z",
                    "meta": { "version": 2 },
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
            "detail": "UnknownMetaVersion: meta.version is newer than the broker supports (1)",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "format": "UnknownMetaVersion: meta.version is newer than the broker supports (1)",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_18f1), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused batch stores nothing"
    );
}

/// A block naming no producer is checked for chain fields before its version
/// (ADR-0004, "Mode-Shape Enforcement at Publish Time").
#[tokio::test]
async fn a_sequence_without_a_producer_at_a_newer_version_is_refused_as_meta_without_producer_id() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_18e9);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_18e9),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 2, "sequence": 1 },
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
            "detail": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_18e9), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

/// Two senders whose `meta` names no producer and carries no chain field are
/// each stateless: neither is checked against a chain, so neither is taken
/// for a duplicate of the other.
#[tokio::test]
async fn two_publishes_with_a_version_only_meta_are_both_stored() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_1891);
    let first_id = Uuid::from_u128(0xe000_1891);
    let second_id = Uuid::from_u128(0xe000_1892);

    let first = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": first_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-stateless-meta",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1 },
        })))
        .send()
        .await;
    let second = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": second_id,
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "billing-service",
            "subject": "invoice-stateless-meta",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:01Z",
            "meta": { "version": 1 },
        })))
        .send()
        .await;

    first.assert_status(202);
    second.assert_status(202);
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            second_id
        )
        .await,
        vec![first_id, second_id],
        "both stateless events are stored, in publish order"
    );
}

/// The event a version-only `meta` becomes carries no `meta`, as if the
/// producer had left it out.
#[tokio::test]
async fn a_version_only_meta_is_admitted_as_an_event_without_meta() {
    let harness = run_rules_harness().await;
    let caller = harness.security_context();
    let request = |id: Uuid, subject: &str| {
        let body: crate::api::rest::handlers::ingest::dto::PublishEventRequest =
            serde_json::from_value(json!({
                "id": id,
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "subject_tenant": Uuid::from_u128(0x7e7a_18a1),
                "source": "order-service",
                "subject": subject,
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:40:00Z",
                "meta": { "version": 1 },
            }))
            .expect("the REST handler reads the body");
        PublishRequest::try_from(body).expect("the REST handler takes it")
    };

    let first = harness
        .ingest()
        .publish_event(
            harness.security_context(),
            request(Uuid::from_u128(0xe000_18a1), "order-1"),
        )
        .await;
    let second = harness
        .ingest()
        .publish_event(
            harness.security_context(),
            request(Uuid::from_u128(0xe000_18a2), "order-2"),
        )
        .await;

    let Ok(crate::domain::ingest::PublishAck::Accepted(first)) = first else {
        panic!("the first event must be accepted, got {first:?}");
    };
    let Ok(crate::domain::ingest::PublishAck::Accepted(second)) = second else {
        panic!("the second event must be accepted, got {second:?}");
    };
    assert_eq!(
        (
            serde_json::to_value(first).expect("the event serializes"),
            serde_json::to_value(second).expect("the event serializes"),
        ),
        (
            json!({
                "id": Uuid::from_u128(0xe000_18a1),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::from_u128(0x7e7a_18a1),
                "source": "order-service",
                "subject": "order-1",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:40:00Z",
                "trace_parent": null,
                "data": null,
                "meta": null,
                "ingest": { "tenant": caller.subject_tenant_id(), "actor": caller.subject_id() },
                "partition": null,
                "sequence": null,
                "sequence_time": null,
            }),
            json!({
                "id": Uuid::from_u128(0xe000_18a2),
                "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "subject_tenant": Uuid::from_u128(0x7e7a_18a1),
                "source": "order-service",
                "subject": "order-2",
                "subject_type": "gts.x.eb.t1.subject.v1~",
                "occurred_at": "2026-05-29T10:40:00Z",
                "trace_parent": null,
                "data": null,
                "meta": null,
                "ingest": { "tenant": caller.subject_tenant_id(), "actor": caller.subject_id() },
                "partition": null,
                "sequence": null,
                "sequence_time": null,
            }),
        )
    );
}

/// The producer lookup comes before the shape check: an unknown producer is
/// answered `404` whatever its `meta` shape.
#[tokio::test]
async fn an_unknown_producer_with_a_bad_shape_is_answered_producer_not_found() {
    let harness = run_rules_harness().await;
    let unknown = Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_18b1);
    let tenant_id = Uuid::from_u128(0x7e7a_18b1);

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::from_u128(0xe000_18b1),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": tenant_id,
            "source": "order-service",
            "subject": "order-mode-shape",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-05-29T10:40:00Z",
            "meta": { "version": 1, "producer": unknown, "sequence": 1 },
        })))
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "the producer is not registered or its registration has expired",
            "instance": "/event-broker/v1/events",
            "context": {
                "resource_type": "gts.cf.core.events.producer.v1~",
                "resource_name": unknown,
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_18b1), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused publish stores nothing"
    );
}

#[tokio::test]
async fn a_batch_with_a_chained_event_missing_previous_is_refused_as_chain_mode_fields_missing() {
    let harness = run_rules_harness().await;
    let producer_id = register_test_producer(&harness).await;
    let tenant_id = Uuid::from_u128(0x7e7a_18c1);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_18c1),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-mode-shape",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:40:00Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 0, "sequence": 1 },
                },
                {
                    "id": Uuid::from_u128(0xe000_18c2),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-mode-shape",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:40:01Z",
                    "meta": { "version": 1, "producer": producer_id, "sequence": 2 },
                },
                {
                    "id": Uuid::from_u128(0xe000_18c3),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-mode-shape",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:40:02Z",
                    "meta": { "version": 1, "producer": producer_id, "previous": 2, "sequence": 3 },
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
            "detail": "ChainModeFieldsMissing: a chained-mode publish needs meta.previous and meta.sequence",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "format": "ChainModeFieldsMissing: a chained-mode publish needs meta.previous and meta.sequence",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer_id.to_string())
        .send()
        .await;
    cursors.assert_status(200);
    assert_eq!(
        cursors.json(),
        json!({ "id": producer_id, "client_agent": "test-agent", "topics": [] })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_18c1), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused batch stores nothing"
    );
}

#[tokio::test]
async fn a_batch_with_a_sequence_and_no_producer_is_refused_as_meta_without_producer_id() {
    let harness = run_rules_harness().await;
    let tenant_id = Uuid::from_u128(0x7e7a_18d1);

    let resp = harness
        .api_v1()
        .post_events_batch()
        .with_body(Json(&json!({
            "events": [
                {
                    "id": Uuid::from_u128(0xe000_18d1),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-mode-shape",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:40:00Z",
                },
                {
                    "id": Uuid::from_u128(0xe000_18d2),
                    "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                    "subject_tenant": tenant_id,
                    "source": "order-service",
                    "subject": "order-mode-shape",
                    "subject_type": "gts.x.eb.t1.subject.v1~",
                    "occurred_at": "2026-05-29T10:40:01Z",
                    "meta": { "version": 1, "sequence": 2 },
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
            "detail": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
            "instance": "/event-broker/v1/events:batch",
            "context": {
                "format": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = publish_sentinel(&harness, Uuid::from_u128(0x5e7_18d1), tenant_id).await;
    assert_eq!(
        stored_ids_through(
            &harness,
            "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            sentinel
        )
        .await,
        vec![sentinel],
        "a refused batch stores nothing"
    );
}

// -- subject_owner and the ingest stamp ------------------------------------------

/// The events the backend holds on partition 0 of `topic`, as
/// `(id, subject_owner, ingest stamp as (tenant, actor))`, in sequence order.
async fn stored_owners_and_stamps(
    harness: &EventBrokerHarness,
    topic: &str,
) -> Vec<(Uuid, Option<Uuid>, Option<(Uuid, Uuid)>)> {
    use event_broker_sdk::backend::{Partition, TopicPartition, Wait};

    let at = TopicPartition::new(
        toolkit_gts::GtsInstanceId::try_new(topic).expect("a valid topic id"),
        Partition::new(0),
    );
    harness
        .backend()
        .read(&at, crate::domain::model::Sequence::NONE, 1024, Wait::No)
        .await
        .expect("read the stored events")
        .iter()
        .map(|event| {
            (
                event.id,
                event.subject_owner,
                event.ingest.map(|stamp| (stamp.tenant, stamp.actor)),
            )
        })
        .collect()
}

/// An owned publish stores its owner; `"subject_owner": null` and an absent key
/// both store a tenant-wide event. Each stored event carries the caller's tenant
/// and principal as its ingest stamp.
#[tokio::test]
async fn a_publish_stores_its_owner_and_the_callers_ingest_stamp() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let owner = Uuid::from_u128(0x0e);
    let mut bodies = [
        json!({ "id": Uuid::from_u128(0x5a01), "subject_owner": owner }),
        json!({ "id": Uuid::from_u128(0x5a02), "subject_owner": null }),
        json!({ "id": Uuid::from_u128(0x5a03) }),
    ];
    for body in &mut bodies {
        let members = body.as_object_mut().expect("an object");
        for (key, value) in json!({
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::from_u128(0x7e7a_5a00),
            "source": "test-owner-and-stamp",
            "subject": "s-owner-and-stamp",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-11-15T10:00:00Z",
        })
        .as_object()
        .expect("an object")
        {
            members.insert(key.clone(), value.clone());
        }
    }

    for body in &bodies {
        harness
            .api_v1()
            .post_events()
            .with_header(
                axum::http::header::HeaderName::from_static("prefer"),
                axum::http::HeaderValue::from_static("wait=5"),
            )
            .with_body(Json(body))
            .send()
            .await
            .assert_status(201);
    }

    let caller = harness.security_context();
    let stamp = Some((caller.subject_tenant_id(), caller.subject_id()));
    assert_eq!(
        stored_owners_and_stamps(&harness, "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1").await,
        vec![
            (Uuid::from_u128(0x5a01), Some(owner), stamp),
            (Uuid::from_u128(0x5a02), None, stamp),
            (Uuid::from_u128(0x5a03), None, stamp),
        ]
    );
}

/// An anonymous publisher leaves no ingest stamp on the event it publishes.
#[tokio::test]
async fn an_anonymous_publish_carries_no_ingest_stamp() {
    let harness = run_rules_harness().await;
    let request = |id: u128| PublishRequest {
        id: Uuid::from_u128(id),
        r#type: crate::test_support::event_type_id("gts.cf.core.events.event.v1~x.eb.t1.foo.v1~"),
        subject_tenant: Uuid::from_u128(0x7e7a_5b00),
        source: "test-anonymous-stamp".to_owned(),
        subject: "s-anonymous-stamp".to_owned(),
        subject_type: gts::GtsTypeId::new("gts.x.eb.t1.subject.v1~"),
        subject_owner: None,
        occurred_at: Utc::now(),
        trace_parent: None,
        data: json!(null),
        meta: None,
    };

    let anonymous = harness
        .ingest()
        .publish_event(
            &toolkit_security::SecurityContext::anonymous(),
            request(0x5b01),
        )
        .await;
    let known = harness
        .ingest()
        .publish_event(harness.security_context(), request(0x5b02))
        .await;

    let (
        Ok(crate::domain::ingest::PublishAck::Accepted(anonymous)),
        Ok(crate::domain::ingest::PublishAck::Accepted(known)),
    ) = (anonymous, known)
    else {
        panic!("both publishes must be accepted");
    };
    let caller = harness.security_context();
    assert_eq!(
        (anonymous.ingest, known.ingest),
        (
            None,
            Some(crate::domain::model::IngestStamp {
                tenant: caller.subject_tenant_id(),
                actor: caller.subject_id(),
            })
        )
    );
}

#[tokio::test]
async fn publish_event_with_a_nil_owner_returns_400() {
    // A backend that never stores keeps an admitted event in the outbox, so
    // `outbox_bodies` counts it.
    let harness = EventBrokerHarness::builder()
        .with_backend(Arc::new(NeverStoringBackend::new(Arc::new(
            memory_event_broker_plugin::MemoryBackend::new(),
        ))))
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~",
            "subject_tenant": Uuid::new_v4(),
            "source": "test-publish_event_with_a_nil_owner_returns_400",
            "subject": "s-publish_event_with_a_nil_owner_returns_400",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "subject_owner": "00000000-0000-0000-0000-000000000000",
            "occurred_at": Utc::now().to_rfc3339(),
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
            "detail": "InvalidSubjectOwner: event 0: the event's owner must not be the nil UUID",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "InvalidSubjectOwner: event 0: the event's owner must not be the nil UUID",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

/// The ingest stamp is the broker's: a publish body carrying `meta.ingest_actor`
/// is refused as an unknown field, `400 InvalidBody` from the handler's own
/// parsing, and nothing is enqueued.
#[tokio::test]
async fn a_publish_whose_meta_carries_an_ingest_stamp_is_refused() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_header(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )
        .with_body(
            r#"{"id":"00000000-0000-0000-0000-00000000b911","type":"gts.cf.core.events.event.v1~x.eb.t1.foo.v1~","subject_tenant":"00000000-0000-0000-0000-00000000b912","source":"test-ingest-stamp-key","subject":"s-ingest-stamp-key","subject_type":"gts.x.eb.t1.subject.v1~","occurred_at":"2026-11-15T00:00:00Z","meta":{"version":1,"ingest_actor":"00000000-0000-0000-0000-00000000b913"}}"#,
        )
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidBody: invalid JSON body: unknown field `ingest_actor`, expected one \
                       of `version`, `producer`, `previous`, `sequence` at line 1 column 330",
            "instance": "/event-broker/v1/events",
            "context": {
                "format": "InvalidBody: invalid JSON body: unknown field `ingest_actor`, expected \
                           one of `version`, `producer`, `previous`, `sequence` at line 1 column \
                           330",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(harness.outbox_bodies().await, 0);
}

/// A type keyed on `/subject_owner` (which narrows the owner to required) takes an
/// owned event; an unowned one, absent or `null`, is `400 PartitionKeyUnresolved`;
/// and an unowned one with bad `data` gets the `data` refusal first.
#[tokio::test]
async fn a_type_keyed_on_the_owner_refuses_an_unowned_event_after_its_data_check() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event.v1~x.eb.t1.owned.v1~",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
                "partition_key": "/subject_owner",
                "subject_owner_required": true,
                "data_schema": { "type": "object", "required": ["order_id"] },
            },
        ])))
        .build()
        .await;
    let body = |data: serde_json::Value| {
        json!({
            "id": Uuid::new_v4(),
            "type": "gts.cf.core.events.event.v1~x.eb.t1.owned.v1~",
            "subject_tenant": Uuid::from_u128(0x7e7a_5c00),
            "source": "test-owner-keyed",
            "subject": "s-owner-keyed",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": "2026-11-15T10:00:00Z",
            "data": data,
        })
    };
    let mut owned = body(json!({ "order_id": "o-1" }));
    owned["subject_owner"] = json!(Uuid::from_u128(0x0e));
    let absent = body(json!({ "order_id": "o-2" }));
    let mut null_owner = body(json!({ "order_id": "o-3" }));
    null_owner["subject_owner"] = json!(null);
    let mut bad_data = body(json!({}));
    bad_data["subject_owner"] = json!(null);
    let unresolved = json!({
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "title": "Invalid Argument",
        "status": 400,
        "detail": "PartitionKeyUnresolved: event type \
                   'gts.cf.core.events.event.v1~x.eb.t1.owned.v1~' partitions on \
                   `/subject_owner`, which this event does not resolve: internal: \
                   partition-key pointer `/subject_owner` resolves to null",
        "instance": "/event-broker/v1/events",
        "context": {
            "format": "PartitionKeyUnresolved: event type \
                       'gts.cf.core.events.event.v1~x.eb.t1.owned.v1~' partitions on \
                       `/subject_owner`, which this event does not resolve: internal: \
                       partition-key pointer `/subject_owner` resolves to null",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    });

    let mut outcomes = Vec::new();
    for body in [owned, absent, null_owner, bad_data] {
        let resp = harness
            .api_v1()
            .post_events()
            .with_body(Json(&body))
            .send()
            .await;
        let status = resp.status().as_u16();
        let text = resp.text();
        outcomes.push((
            status,
            serde_json::from_str::<serde_json::Value>(&text).unwrap_or(json!(text)),
        ));
    }

    assert_eq!(
        outcomes,
        vec![
            (202, json!("")),
            (400, unresolved.clone()),
            (400, unresolved),
            (
                422,
                json!({
                    "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
                    "title": "Invalid Argument",
                    "status": 422,
                    "detail": "Request validation failed",
                    "instance": "/event-broker/v1/events",
                    "context": {
                        "field_violations": [{
                            "field": "(payload)",
                            "description": "event data does not satisfy the event type's data schema",
                            "reason": "schema_validation",
                        }],
                        "resource_type": "gts.cf.core.events.topic.v1~",
                        "resource_name": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                    },
                })
            ),
        ]
    );
}
