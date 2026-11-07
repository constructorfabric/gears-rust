//! Status-to-outcome mapping and the `Prefer` header of the single and batch publish
//! calls, and the refusals a publish makes before it sends anything, against a loopback
//! server.

use super::*;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use serde_json::json;

use crate::api::Ack;
use crate::gts::EventV1;

/// Serves `router` on an ephemeral loopback port; the client's base URL.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("resolve the bound address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    format!("http://{addr}")
}

/// The `Prefer` header of each publish request a [`publish_server`] received, in order.
type SeenPrefer = Arc<Mutex<Vec<Option<String>>>>;

/// Serves the single and batch publish routes. The n-th request is answered with
/// `statuses[n]`, the last status once they run out; each request's `Prefer` header is
/// recorded.
async fn publish_server(statuses: Vec<StatusCode>) -> (String, SeenPrefer) {
    let seen: SeenPrefer = Arc::default();
    let recorder = Arc::clone(&seen);
    let handler = move |headers: HeaderMap| {
        let recorder = Arc::clone(&recorder);
        let statuses = statuses.clone();
        async move {
            let mut seen = recorder.lock().expect("the recorder lock");
            let status = statuses[seen.len().min(statuses.len() - 1)];
            seen.push(
                headers
                    .get("prefer")
                    .map(|value| value.to_str().expect("an ASCII header").to_owned()),
            );
            status
        }
    };
    let base_url = serve(
        Router::new()
            .route("/event-broker/v1/events", post(handler.clone()))
            .route("/event-broker/v1/events:batch", post(handler)),
    )
    .await;
    (base_url, seen)
}

/// Serves the single and batch publish routes, answers `202` and records each request's JSON
/// body, in order.
async fn body_server() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let recorder = Arc::clone(&seen);
    let handler = move |axum::Json(body): axum::Json<serde_json::Value>| {
        let recorder = Arc::clone(&recorder);
        async move {
            recorder.lock().expect("the recorder lock").push(body);
            StatusCode::ACCEPTED
        }
    };
    let base_url = serve(
        Router::new()
            .route("/event-broker/v1/events", post(handler.clone()))
            .route("/event-broker/v1/events:batch", post(handler)),
    )
    .await;
    (base_url, seen)
}

fn seen(prefer: &SeenPrefer) -> Vec<Option<String>> {
    prefer.lock().expect("the recorder lock").clone()
}

/// A publishable event read from its JSON body: no stamped member, no `meta`.
fn published_body() -> serde_json::Value {
    json!({
        "id": "0190f1a2-0000-7000-8000-000000000c01",
        "type": "gts.cf.core.events.event.v1~x.eb.sdk.placed.v1~",
        "tenant_id": "0190f1a2-0000-7000-a000-000000000c01",
        "source": "sdk-test",
        "subject": "order-1",
        "subject_type": "gts.x.eb.sdk.order.v1~",
        "occurred_at": "2026-10-01T12:00:00Z",
        "data": null,
    })
}

fn event_from(body: serde_json::Value) -> EventV1 {
    serde_json::from_value(body).expect("a well-formed event body")
}

fn publishable() -> EventV1 {
    event_from(published_body())
}

#[tokio::test]
async fn a_publish_body_carries_every_member_of_the_event() {
    let (base_url, bodies) = body_server().await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let full_body = json!({
        "id": "0190f1a2-0000-7000-8000-000000000c11",
        "type": "gts.cf.core.events.event.v1~x.eb.sdk.placed.v1~",
        "tenant_id": "0190f1a2-0000-7000-a000-000000000c01",
        "source": "sdk-test",
        "subject": "order-1",
        "subject_type": "gts.x.eb.sdk.order.v1~",
        "occurred_at": "2026-10-01T12:00:00Z",
        "trace_parent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        "data": { "order_id": "o-1", "total_cents": 4299 },
        "meta": {
            "version": 1,
            "producer_id": "0190f1a2-0000-7000-8000-0000000000aa",
            "previous": 4,
            "sequence": 5
        }
    });
    let full = event_from(full_body.clone());
    let bare = publishable();

    broker
        .publish(&SecurityContext::anonymous(), &full, Ack::Accepted)
        .await
        .expect("a 202 is a success");
    broker
        .publish_batch(&SecurityContext::anonymous(), &[full, bare], Ack::Accepted)
        .await
        .expect("a 202 is a success");

    // `data`, `trace_parent` and `meta` are left out of the body when the event has none.
    let bare_body = json!({
        "id": "0190f1a2-0000-7000-8000-000000000c01",
        "type": "gts.cf.core.events.event.v1~x.eb.sdk.placed.v1~",
        "tenant_id": "0190f1a2-0000-7000-a000-000000000c01",
        "source": "sdk-test",
        "subject": "order-1",
        "subject_type": "gts.x.eb.sdk.order.v1~",
        "occurred_at": "2026-10-01T12:00:00Z",
    });
    let seen = bodies.lock().expect("the recorder lock").clone();
    assert_eq!(
        seen,
        vec![
            full_body.clone(),
            json!({ "events": [full_body, bare_body] }),
        ]
    );
}

#[tokio::test]
async fn an_all_duplicate_batch_is_reported_as_duplicate() {
    let base_url = serve(Router::new().route(
        "/event-broker/v1/events:batch",
        post(|| async { StatusCode::OK }),
    ))
    .await;
    let broker = RestBroker::new(base_url).expect("build the client");

    let outcome = broker
        .publish_batch(
            &SecurityContext::anonymous(),
            &[publishable()],
            Ack::Accepted,
        )
        .await
        .expect("a 200 is a success");

    assert_eq!(outcome, IngestOutcome::Duplicate);
}

#[tokio::test]
async fn a_single_publish_answered_200_is_reported_as_duplicate() {
    let base_url =
        serve(Router::new().route("/event-broker/v1/events", post(|| async { StatusCode::OK })))
            .await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let event = publishable();

    let accepted = broker
        .publish(&SecurityContext::anonymous(), &event, Ack::Accepted)
        .await
        .expect("a 200 is a success");
    let persisted = broker
        .publish(
            &SecurityContext::anonymous(),
            &event,
            Ack::Persisted {
                within: Duration::from_secs(5),
            },
        )
        .await
        .expect("a 200 is a success");

    assert_eq!(
        (accepted, persisted),
        (IngestOutcome::Duplicate, IngestOutcome::Duplicate)
    );
}

#[tokio::test]
async fn a_persisted_publish_asks_for_whole_seconds_between_1_and_25() {
    let (base_url, prefer) = publish_server(vec![StatusCode::CREATED]).await;
    let broker = RestBroker::new(base_url).expect("build the client");

    let mut outcomes = Vec::new();
    for within in [
        Duration::from_millis(1_200),
        Duration::ZERO,
        Duration::from_secs(60),
        Duration::MAX,
    ] {
        outcomes.push(
            broker
                .publish(
                    &SecurityContext::anonymous(),
                    &publishable(),
                    Ack::Persisted { within },
                )
                .await
                .expect("a 201 is a success"),
        );
    }

    assert_eq!(
        seen(&prefer),
        vec![
            Some("wait=2".to_owned()),
            Some("wait=1".to_owned()),
            Some("wait=25".to_owned()),
            Some("wait=25".to_owned()),
        ]
    );
    assert_eq!(outcomes, vec![IngestOutcome::Persisted; 4]);
}

#[tokio::test]
async fn a_persisted_batch_asks_the_broker_to_wait_and_reports_its_status() {
    let (base_url, prefer) = publish_server(vec![
        StatusCode::CREATED,
        StatusCode::ACCEPTED,
        StatusCode::OK,
    ])
    .await;
    let broker = RestBroker::new(base_url).expect("build the client");

    let mut outcomes = Vec::new();
    for _ in 0..3 {
        outcomes.push(
            broker
                .publish_batch(
                    &SecurityContext::anonymous(),
                    &[publishable()],
                    Ack::Persisted {
                        within: Duration::from_secs(60),
                    },
                )
                .await
                .expect("a 2xx is a success"),
        );
    }

    assert_eq!(seen(&prefer), vec![Some("wait=25".to_owned()); 3]);
    assert_eq!(
        outcomes,
        vec![
            IngestOutcome::Persisted,
            IngestOutcome::Accepted,
            IngestOutcome::Duplicate,
        ]
    );
}

#[tokio::test]
async fn an_accepted_publish_sends_no_prefer_header() {
    let (base_url, prefer) = publish_server(vec![StatusCode::ACCEPTED]).await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let ctx = SecurityContext::anonymous();

    let single = broker
        .publish(&ctx, &publishable(), Ack::Accepted)
        .await
        .expect("a 202 is a success");
    let batch = broker
        .publish_batch(&ctx, &[publishable()], Ack::Accepted)
        .await
        .expect("a 202 is a success");

    assert_eq!(seen(&prefer), vec![None, None]);
    assert_eq!(
        (single, batch),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
}

#[tokio::test]
async fn a_stamped_or_sourceless_event_is_refused_before_any_request() {
    let (base_url, prefer) = publish_server(vec![StatusCode::ACCEPTED]).await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let ctx = SecurityContext::anonymous();
    let with = |member: &str, value: serde_json::Value| {
        let mut body = published_body();
        body[member] = value;
        event_from(body)
    };
    let mut sourceless = published_body();
    sourceless
        .as_object_mut()
        .expect("an object body")
        .remove("source");
    let cases = [
        (
            with("partition", json!(3)),
            "partition",
            "broker-stamped and read-only on publish",
        ),
        (
            with("sequence", json!(42)),
            "sequence",
            "broker-stamped and read-only on publish",
        ),
        (
            with("sequence_time", json!("2026-10-01T12:00:01Z")),
            "sequence_time",
            "broker-stamped and read-only on publish",
        ),
        (event_from(sourceless), "source", "required"),
    ];

    for (event, expected_field, expected_detail) in cases {
        let single = broker.publish(&ctx, &event, Ack::Accepted).await;
        let Err(EventBrokerError::InvalidEventField { field, detail }) = single else {
            panic!("expected an `InvalidEventField` refusal, got {single:?}");
        };
        assert_eq!((field, detail.as_str()), (expected_field, expected_detail));

        let batch = broker
            .publish_batch(&ctx, &[publishable(), event], Ack::Accepted)
            .await;
        let Err(EventBrokerError::InvalidEventField { field, detail }) = batch else {
            panic!("expected an `InvalidEventField` refusal, got {batch:?}");
        };
        assert_eq!((field, detail.as_str()), (expected_field, expected_detail));
    }

    assert_eq!(
        seen(&prefer),
        Vec::<Option<String>>::new(),
        "no request was sent"
    );
}

#[tokio::test]
async fn a_meta_version_past_the_wire_range_is_refused_before_any_request() {
    let (base_url, prefer) = publish_server(vec![StatusCode::ACCEPTED]).await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let ctx = SecurityContext::anonymous();
    let mut body = published_body();
    body["meta"] = json!({
        "version": 3_000_000_000_u32,
        "producer_id": "0190f1a2-0000-7000-b000-000000000c01",
        "previous": 4,
        "sequence": 5,
    });
    let event = event_from(body);

    let single = broker.publish(&ctx, &event, Ack::Accepted).await;
    let Err(EventBrokerError::InvalidEventField { field, detail }) = single else {
        panic!("expected an `InvalidEventField` refusal, got {single:?}");
    };
    assert_eq!((field, detail.as_str()), ("meta", "version out of range"));

    let batch = broker.publish_batch(&ctx, &[event], Ack::Accepted).await;
    let Err(EventBrokerError::InvalidEventField { field, detail }) = batch else {
        panic!("expected an `InvalidEventField` refusal, got {batch:?}");
    };
    assert_eq!((field, detail.as_str()), ("meta", "version out of range"));

    assert_eq!(
        seen(&prefer),
        Vec::<Option<String>>::new(),
        "no request was sent"
    );
}

/// The cap keeps a persisted publish from timing out on the client: the broker answers
/// by the end of the longest wait the SDK asks for, which leaves 5 s of the HTTP
/// client's default request timeout for the round trip.
#[test]
fn the_rest_wait_cap_stays_below_the_client_request_timeout() {
    assert!(
        Duration::from_secs(REST_MAX_SYNC_WAIT_SECS) + Duration::from_secs(5)
            <= toolkit_http::HttpClientConfig::default().request_timeout
    );
}

/// The typed consumer-group query renders the documented filterable fields
/// (`id` eq/in, `kind` eq) as one `$filter`, joined by `and`; an empty query
/// sends none. The query strings are compared decoded: the percent-encoding
/// is the form encoder's.
#[tokio::test]
async fn a_typed_consumer_group_query_renders_its_odata_filter() {
    use axum::extract::RawQuery;
    use axum::routing::get;
    use std::sync::{Arc, Mutex};

    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let recorder = Arc::clone(&seen);
    let base_url = serve(Router::new().route(
        "/event-broker/v1/consumer-groups",
        get(move |RawQuery(query): RawQuery| async move {
            recorder.lock().expect("the recorder lock").push(query);
            axum::Json(serde_json::json!({
                "items": [],
                "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
            }))
        }),
    ))
    .await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let ctx = SecurityContext::anonymous();
    let a = ConsumerGroupId::new(uuid::Uuid::from_u128(0x0a));
    let b = ConsumerGroupId::new(uuid::Uuid::from_u128(0x0b));

    for query in [
        ConsumerGroupQuery::new().id(a),
        ConsumerGroupQuery::new()
            .id(a)
            .id(b)
            .kind(crate::models::ConsumerGroupKind::Anonymous),
        ConsumerGroupQuery::new().kind(crate::models::ConsumerGroupKind::Named),
        ConsumerGroupQuery::new(),
    ] {
        broker
            .list_consumer_groups(&ctx, query)
            .await
            .expect("the listing succeeds");
    }

    let seen = seen.lock().expect("the recorder lock").clone();
    type Pairs = Vec<(String, String)>;
    let decoded: Vec<Option<Pairs>> = seen
        .iter()
        .map(|query| {
            query
                .as_deref()
                .map(|q| serde_html_form::from_str(q).expect("the query string decodes as a form"))
        })
        .collect();
    let pair = |key: &str, value: &str| vec![(key.to_owned(), value.to_owned())];
    assert_eq!(
        decoded,
        vec![
            Some(pair(
                "$filter",
                "id eq 'gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-00000000000a'",
            )),
            Some(pair(
                "$filter",
                "id in ('gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-00000000000a',\
                 'gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-00000000000b') \
                 and kind eq 'anonymous'",
            )),
            Some(pair("$filter", "kind eq 'named'")),
            None,
        ]
    );
}
