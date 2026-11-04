//! Content negotiation on `:stream` and `:sse` (scenarios consumer/stream/1.07
//! and 1.08): `:stream` serves `multipart/mixed` only, `:sse` serves
//! `text/event-stream` only. An incompatible `Accept` is refused with `406`
//! before any subscription lookup, so no subscription needs to exist; a
//! wildcard `Accept` is not refused. In-process: real services and router,
//! `SQLite` gear database, memory backend.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use event_broker::test_support::EventBrokerHarness;
use http::HeaderValue;
use serde_json::json;

// scenario: consumer/stream/1.07-guardrail-stream-accept-json-rejected.md
#[tokio::test]
async fn stream_refuses_an_application_json_accept_with_406() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .get_events_stream("0190f1a2-0000-7000-8000-000000000701")
        .with_header(
            http::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        )
        .send()
        .await;

    resp.assert_status(406);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 406,
            "detail": "this endpoint serves multipart/mixed only",
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "format": "this endpoint serves multipart/mixed only",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

// scenario: consumer/stream/1.08-guardrail-sse-from-stream-endpoint.md
#[tokio::test]
async fn stream_refuses_a_text_event_stream_accept_and_points_at_sse() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .get_events_stream("0190f1a2-0000-7000-8000-000000000702")
        .with_header(
            http::header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        )
        .send()
        .await;

    resp.assert_status(406);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 406,
            "detail": "SSE is served only at /v1/events:sse; this endpoint serves multipart/mixed",
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "format": "SSE is served only at /v1/events:sse; this endpoint serves multipart/mixed",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// The direction 1.08 does not enumerate: `multipart/mixed` on `:sse` is
/// refused in favour of `:stream`.
#[tokio::test]
async fn sse_refuses_a_multipart_mixed_accept_and_points_at_stream() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .get_events_sse("0190f1a2-0000-7000-8000-000000000703")
        .with_header(
            http::header::ACCEPT,
            HeaderValue::from_static("multipart/mixed"),
        )
        .send()
        .await;

    resp.assert_status(406);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 406,
            "detail": "multipart/mixed is served only at /v1/events:stream; this endpoint serves text/event-stream",
            "instance": "/event-broker/v1/events:sse",
            "context": {
                "format": "multipart/mixed is served only at /v1/events:stream; this endpoint serves text/event-stream",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// A wildcard `Accept` means "anything", so the request passes negotiation
/// and the lookup of the unknown subscription answers `404`, never `406`.
#[tokio::test]
async fn stream_accepts_a_wildcard_accept_and_answers_404_for_an_unknown_subscription() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .get_events_stream("0190f1a2-0000-7000-8000-000000000704")
        .with_header(http::header::ACCEPT, HeaderValue::from_static("*/*"))
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "subscription '0190f1a2-0000-7000-8000-000000000704' does not exist",
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "resource_name": "0190f1a2-0000-7000-8000-000000000704",
            },
        })
    );
}
