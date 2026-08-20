//! `handlers/delivery/streaming.rs` coverage (`eb-rest-handlers` task 11.5):
//! `:stream`/`:sse` frame shapes and streaming-in-progress lifecycle,
//! split out from `subscriptions_tests.rs` (item #12 of `REVIEW-TODO.md`
//! - this file was originally just a section of that one).
//!
//! These tests bypass `RequestCase::send()` (which eagerly collects the
//! *entire* response body via `axum::body::to_bytes` - fine for ordinary
//! JSON responses, but `:stream`/`:sse` bodies are intentionally long-lived
//! and never naturally end, so collecting them in full would hang forever).
//! Instead they drive `harness.router()` directly with `tower::ServiceExt`,
//! matching `api/rest/routes/routes_tests.rs`'s existing streaming-test
//! pattern (`idle_timeout_closes_connection_after_silence_but_not_between_heartbeats`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tokio_stream::StreamExt;
use tower::ServiceExt;
use uuid::Uuid;

use crate::test_support::{EventBrokerHarness, Json, StaticTypesRegistry};

#[tokio::test]
async fn stream_events_returns_409_positions_not_set_when_unseeded() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "tenant_id": Uuid::new_v4(),
                "types": ["gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = harness.api_v1().get_events_stream(&id).send().await;

    resp.assert_status(409);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
            "title": "Aborted",
            "status": 409,
            "detail": "unseeded assigned partitions: gts.cf.core.events.topic.v1~x.eb.t1.topic.v1:0",
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "reason": "PositionsNotSet",
                "resource_name": id,
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

#[tokio::test]
async fn stream_events_returns_404_for_unknown_subscription() {
    let harness = EventBrokerHarness::builder().build().await;
    let missing_id = Uuid::new_v4();

    let resp = harness
        .api_v1()
        .get_events_stream(&missing_id.to_string())
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{missing_id}' does not exist"),
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "resource_name": missing_id.to_string(),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

#[tokio::test]
async fn stream_events_happy_path_emits_topology_frame_first() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "tenant_id": Uuid::new_v4(),
                "types": ["gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "partition_positions": [{ "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0, "value": "earliest" }]
        })))
        .send()
        .await
        .assert_status(200);

    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/event-broker/v1/events:stream?subscription_id={id}"
        ))
        .body(Body::empty())
        .expect("request must build");
    let response = harness
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(response.status(), StatusCode::OK);

    let mut stream = response.into_body().into_data_stream();
    let first_chunk = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("must receive the first frame within 2s")
        .expect("stream must not end before the first frame")
        .expect("first chunk must not be an error");
    let text = String::from_utf8_lossy(&first_chunk).into_owned();
    assert_eq!(
        text,
        format!(
            "--event-broker-frame-boundary\r\nContent-Type: application/json\r\n\r\n\
             {{\"kind\":\"topology\",\"topology_version\":0,\"assigned\":\
             [{{\"topic\":\"gts.cf.core.events.topic.v1~x.eb.t1.topic.v1\",\"partition\":0,\"offset\":0,\
             \"last_examined\":0}}]}}\r\n"
        )
    );
}

#[tokio::test]
async fn stream_events_second_stream_on_same_subscription_returns_409_streaming_in_progress() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "tenant_id": Uuid::new_v4(),
                "types": ["gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "partition_positions": [{ "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0, "value": "earliest" }]
        })))
        .send()
        .await
        .assert_status(200);

    let request1 = Request::builder()
        .method("GET")
        .uri(format!(
            "/event-broker/v1/events:stream?subscription_id={id}"
        ))
        .body(Body::empty())
        .expect("request must build");
    // Held alive (not dropped) for the rest of the test - dropping the
    // response body would clear the active-stream marker immediately.
    let first_response = harness
        .router()
        .clone()
        .oneshot(request1)
        .await
        .expect("the service itself must not error");
    assert_eq!(first_response.status(), StatusCode::OK);

    let resp = harness.api_v1().get_events_stream(&id).send().await;

    resp.assert_status(409);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
            "title": "Aborted",
            "status": 409,
            "detail": format!("subscription '{id}' already has an open stream"),
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "reason": "StreamingInProgress",
                "resource_name": id,
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );

    drop(first_response);
}

#[tokio::test]
async fn seek_while_streaming_returns_409_streaming_in_progress() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "tenant_id": Uuid::new_v4(),
                "types": ["gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "partition_positions": [{ "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0, "value": "earliest" }]
        })))
        .send()
        .await
        .assert_status(200);

    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/event-broker/v1/events:stream?subscription_id={id}"
        ))
        .body(Body::empty())
        .expect("request must build");
    let stream_response = harness
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(stream_response.status(), StatusCode::OK);

    let resp = harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "partition_positions": [{ "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0, "value": "latest" }]
        })))
        .send()
        .await;

    resp.assert_status(409);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.aborted.v1~",
            "title": "Aborted",
            "status": 409,
            "detail": format!("subscription '{id}' has an open stream"),
            "instance": format!("/event-broker/v1/subscriptions/{id}:seek"),
            "context": {
                "reason": "StreamingInProgress",
                "resource_name": id,
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );

    drop(stream_response);
}

#[tokio::test]
async fn stream_events_delivers_published_event_via_notification_wakeup() {
    // Verifies the D6 wake-up mechanism itself (`domain::notify::DeliveryNotifier`,
    // `ClusterCacheV1::watch_prefix`) actually delivers a live event promptly -
    // not just that a pre-seeded position can be replayed. Group 7 removed the
    // old fixed-interval poll, so a regression here would only show up as a
    // stream that never wakes until its next heartbeat.
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
            {
                "id": "gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1",
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "allowed_subject_types": ["gts.x.eb.t1.subject.v1~"],
            },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tenant_id = Uuid::new_v4();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "tenant_id": tenant_id,
                "types": ["gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "partition_positions": [{ "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0, "value": "earliest" }]
        })))
        .send()
        .await
        .assert_status(200);

    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/event-broker/v1/events:stream?subscription_id={id}"
        ))
        .body(Body::empty())
        .expect("request must build");
    let response = harness
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("must receive the topology frame within 2s")
        .expect("stream must not end before the topology frame")
        .expect("topology chunk must not be an error");

    let event_id = Uuid::new_v4();
    harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": event_id,
            "type": "gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1",
            "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
            "tenant_id": tenant_id,
            "source": "test-stream_events_delivers_published_event_via_notification_wakeup",
            "subject": "s1",
            "subject_type": "gts.x.eb.t1.subject.v1~",
            "occurred_at": chrono::Utc::now().to_rfc3339(),
        })))
        .send()
        .await
        .assert_status(202);

    let event_chunk = tokio::time::timeout(std::time::Duration::from_secs(3), stream.next())
        .await
        .expect("the notification wake-up must deliver the event within 3s, not wait for a heartbeat")
        .expect("stream must not end before the event frame")
        .expect("event chunk must not be an error");
    let text = String::from_utf8_lossy(&event_chunk).into_owned();
    let json_start = text.find("\r\n\r\n").expect("frame must have a header/body separator") + 4;
    let json_body = text[json_start..].trim_end_matches("\r\n");
    let frame: serde_json::Value =
        serde_json::from_str(json_body).expect("frame body must be valid JSON");
    assert_eq!(frame["kind"], "event");
    assert_eq!(frame["payload"]["id"], event_id.to_string());
    assert_eq!(
        frame["payload"]["type"],
        "gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1"
    );
    assert_eq!(
        frame["payload"]["topic"],
        "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1"
    );
    assert_eq!(frame["payload"]["tenant_id"], tenant_id.to_string());
    assert_eq!(frame["payload"]["subject"], "s1");
}

#[tokio::test]
async fn sse_events_returns_404_for_unknown_subscription() {
    let harness = EventBrokerHarness::builder().build().await;
    let missing_id = Uuid::new_v4();

    let resp = harness
        .api_v1()
        .get_events_sse(&missing_id.to_string())
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{missing_id}' does not exist"),
            "instance": "/event-broker/v1/events:sse",
            "context": {
                "resource_name": missing_id.to_string(),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
}

#[tokio::test]
async fn sse_events_happy_path_opens_a_text_event_stream() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partitions": 1 },
        ])))
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "a",
            "interests": [{
                "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1",
                "tenant_id": Uuid::new_v4(),
                "types": ["gts.cf.core.events.event_type.v1~x.eb.t1.foo.v1"],
            }],
        })))
        .send()
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "partition_positions": [{ "topic": "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1", "partition": 0, "value": "earliest" }]
        })))
        .send()
        .await
        .assert_status(200);

    let request = Request::builder()
        .method("GET")
        .uri(format!("/event-broker/v1/events:sse?subscription_id={id}"))
        .body(Body::empty())
        .expect("request must build");
    let response = harness
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap()),
        Some("text/event-stream")
    );

    let mut stream = response.into_body().into_data_stream();
    let first_chunk = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("must receive the first event within 2s")
        .expect("stream must not end before the first event")
        .expect("first chunk must not be an error");
    let text = String::from_utf8_lossy(&first_chunk).into_owned();
    assert_eq!(
        text,
        "event: topology\ndata: {\"kind\":\"topology\",\"topology_version\":0,\"assigned\":\
         [{\"topic\":\"gts.cf.core.events.topic.v1~x.eb.t1.topic.v1\",\"partition\":0,\"offset\":0,\
         \"last_examined\":0}]}\n\n"
    );
}
