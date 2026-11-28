//! Status-to-outcome mapping and the `Prefer` header of the single and batch publish
//! calls, and the refusals a publish makes before it sends anything, against a loopback
//! server. Also the mapping of a subscription interest to and from its wire body, of a
//! JOIN request's `session_timeout` to its wire value, and of a JOIN answer's
//! `unmatched_types` into the assignment.

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
        "subject_tenant": "0190f1a2-0000-7000-a000-000000000c01",
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
        "subject_tenant": "0190f1a2-0000-7000-a000-000000000c01",
        "source": "sdk-test",
        "subject": "order-1",
        "subject_type": "gts.x.eb.sdk.order.v1~",
        "occurred_at": "2026-10-01T12:00:00Z",
        "trace_parent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        "data": { "order_id": "o-1", "total_cents": 4299 },
        "meta": {
            "version": 1,
            "producer": "0190f1a2-0000-7000-8000-0000000000aa",
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
        "subject_tenant": "0190f1a2-0000-7000-a000-000000000c01",
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
async fn a_persisted_publish_asks_for_whole_seconds_between_1_and_the_client_sync_wait() {
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
            Some("wait=30".to_owned()),
            Some("wait=30".to_owned()),
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

    assert_eq!(seen(&prefer), vec![Some("wait=30".to_owned()); 3]);
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
        "producer": "0190f1a2-0000-7000-b000-000000000c01",
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

/// The typed consumer-group query renders the documented filterable fields
/// (`id` eq/in, `kind` eq) as one `$filter`, joined by `and`; an empty query
/// sends none. The query strings are compared decoded: the percent-encoding
/// is the form encoder's.
#[tokio::test]
async fn a_typed_consumer_group_query_renders_its_odata_filter() {
    use axum::extract::RawQuery;
    use axum::routing::get;
    use std::sync::{Arc, Mutex};

    type Pairs = Vec<(String, String)>;

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

/// `with_sync_wait` caps the wait a persisted publish asks for, so the client's request
/// timeout ([`request_timeout`]) always outlasts the broker's hold.
#[tokio::test]
async fn a_persisted_publish_never_asks_for_more_than_the_client_sync_wait() {
    let (base_url, prefer) = publish_server(vec![StatusCode::CREATED]).await;
    let broker = RestBroker::new(base_url)
        .expect("build the client")
        .with_sync_wait(Duration::from_secs(7))
        .expect("rebuild the client");
    let ctx = SecurityContext::anonymous();

    let mut outcomes = Vec::new();
    for within in [Duration::from_secs(60), Duration::from_secs(3)] {
        outcomes.push(
            broker
                .publish(&ctx, &publishable(), Ack::Persisted { within })
                .await
                .expect("a 201 is a success"),
        );
    }
    outcomes.push(
        broker
            .publish_batch(
                &ctx,
                &[publishable()],
                Ack::Persisted {
                    within: Duration::from_secs(60),
                },
            )
            .await
            .expect("a 201 is a success"),
    );

    assert_eq!(
        seen(&prefer),
        vec![
            Some("wait=7".to_owned()),
            Some("wait=3".to_owned()),
            Some("wait=7".to_owned()),
        ]
    );
    assert_eq!(outcomes, vec![IngestOutcome::Persisted; 3]);
}

/// A `202` to a persisted publish means the wait ended first: the event stays admitted.
#[tokio::test]
async fn a_persisted_publish_answered_202_is_reported_as_accepted() {
    let (base_url, prefer) = publish_server(vec![StatusCode::ACCEPTED]).await;
    let broker = RestBroker::new(base_url).expect("build the client");

    let outcome = broker
        .publish(
            &SecurityContext::anonymous(),
            &publishable(),
            Ack::Persisted {
                within: Duration::from_secs(5),
            },
        )
        .await
        .expect("a 202 is a success");

    assert_eq!(seen(&prefer), vec![Some("wait=5".to_owned())]);
    assert_eq!(outcome, IngestOutcome::Accepted);
}

// The client timeout is the caller's wait budget plus the processing margin, so
// the final `201`/`202` always outlasts the broker's hold. There is no ceiling:
// the timeout tracks sync_wait, so it adapts to whatever hold the server (and
// the per-route gateway deadline on the publish routes) allows.
#[test]
fn request_timeout_is_the_wait_plus_a_margin() {
    assert_eq!(
        request_timeout(Duration::from_secs(30)),
        Duration::from_secs(35)
    );
    // Zero wait still keeps the margin (the response head still has to arrive).
    assert_eq!(request_timeout(Duration::ZERO), Duration::from_secs(5));
    // A large wait is tracked, not clamped.
    assert_eq!(
        request_timeout(Duration::from_secs(600)),
        Duration::from_secs(605)
    );
    // A wait near the limit saturates instead of overflowing.
    assert_eq!(request_timeout(Duration::MAX), Duration::MAX);
}

#[test]
fn an_interest_sends_its_subject_type_selector() {
    let interest = crate::api::SubscriptionInterest::builder()
        .topic(
            gts::GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.x.v1").unwrap(),
        )
        .subject_tenant(uuid::Uuid::from_u128(0x1920))
        .types([gts::GtsIdPattern::try_new("gts.cf.core.events.event.v1~acme.orders.x.*").unwrap()])
        .subject_types([gts::GtsIdPattern::try_new("gts.acme.orders.x.customer.v1~").unwrap()])
        .build()
        .unwrap();

    assert_eq!(
        serde_json::to_value(interest_to_wire(&interest)).unwrap(),
        json!({
            "topic": "gts.cf.core.events.topic.v1~acme.orders.x.x.v1",
            "subject_tenant": "00000000-0000-0000-0000-000000001920",
            "ownership_filter": "only_mine",
            "barrier_mode": "respect",
            "types": ["gts.cf.core.events.event.v1~acme.orders.x.*"],
            "subject_types": ["gts.acme.orders.x.customer.v1~"],
        })
    );
}

#[test]
fn an_interest_echo_keeps_its_subject_type_selector() {
    let echo: wire::InterestResponseWire = serde_json::from_value(json!({
        "topic": "gts.cf.core.events.topic.v1~acme.orders.x.x.v1",
        "subject_tenant": "00000000-0000-0000-0000-000000001920",
        "ownership_filter": "only_mine",
        "barrier_mode": "respect",
        "types": ["gts.cf.core.events.event.v1~acme.orders.x.*"],
        "subject_types": ["gts.acme.orders.x.customer.v1~"],
        "filter": null,
    }))
    .unwrap();

    let interest = interest_from_wire(echo).unwrap();

    assert_eq!(
        interest.subject_types(),
        [gts::GtsIdPattern::try_new("gts.acme.orders.x.customer.v1~").unwrap()]
    );
}

/// An interest on the orders topic, built by `configure`.
fn orders_interest(
    configure: impl FnOnce(
        crate::api::SubscriptionInterestBuilder,
    ) -> crate::api::SubscriptionInterestBuilder,
) -> crate::api::SubscriptionInterest {
    configure(
        crate::api::SubscriptionInterest::builder()
            .topic(
                gts::GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.x.v1")
                    .unwrap(),
            )
            .types([
                gts::GtsIdPattern::try_new("gts.cf.core.events.event.v1~acme.orders.x.*").unwrap(),
            ]),
    )
    .build()
    .unwrap()
}

/// The broker resolves the caller's tenant, so an interest that names none
/// sends no `subject_tenant`; a named one is sent.
#[test]
fn an_interest_sends_its_tenant_only_when_it_names_one() {
    assert_eq!(
        serde_json::to_value(interest_to_wire(&orders_interest(|interest| interest))).unwrap(),
        json!({
            "topic": "gts.cf.core.events.topic.v1~acme.orders.x.x.v1",
            "ownership_filter": "only_mine",
            "barrier_mode": "respect",
            "types": ["gts.cf.core.events.event.v1~acme.orders.x.*"],
        })
    );
    assert_eq!(
        serde_json::to_value(interest_to_wire(&orders_interest(|interest| {
            interest
                .subject_tenant(uuid::Uuid::from_u128(0x1920))
                .ownership_filter(crate::api::OwnershipFilter::EntireTenant)
        })))
        .unwrap(),
        json!({
            "topic": "gts.cf.core.events.topic.v1~acme.orders.x.x.v1",
            "subject_tenant": "00000000-0000-0000-0000-000000001920",
            "ownership_filter": "entire_tenant",
            "barrier_mode": "respect",
            "types": ["gts.cf.core.events.event.v1~acme.orders.x.*"],
        })
    );
}

/// Every ownership filter goes out and comes back through the echo as itself.
#[test]
fn every_ownership_filter_round_trips_through_the_wire() {
    for (filter, wire) in [
        (crate::api::OwnershipFilter::OnlyMine, "only_mine"),
        (crate::api::OwnershipFilter::EntireTenant, "entire_tenant"),
        (
            crate::api::OwnershipFilter::TenantSubtreeIncluded,
            "tenant_subtree_included",
        ),
    ] {
        let sent = serde_json::to_value(interest_to_wire(&orders_interest(|interest| {
            interest.ownership_filter(filter)
        })))
        .unwrap();
        assert_eq!(sent["ownership_filter"], json!(wire), "{filter:?} sent");

        let echo: wire::InterestResponseWire = serde_json::from_value(json!({
            "topic": "gts.cf.core.events.topic.v1~acme.orders.x.x.v1",
            "subject_tenant": "00000000-0000-0000-0000-000000001920",
            "ownership_filter": wire,
            "barrier_mode": "respect",
            "types": ["gts.cf.core.events.event.v1~acme.orders.x.*"],
            "filter": null,
        }))
        .unwrap();
        let read = interest_from_wire(echo).unwrap();
        assert_eq!(
            (read.subject_tenant(), read.ownership_filter()),
            (
                crate::api::InterestTenant::Tenant(uuid::Uuid::from_u128(0x1920)),
                filter
            ),
            "{filter:?} echoed"
        );
    }
}

/// Serves the JOIN route, answering `201` with `body`.
async fn join_server(body: serde_json::Value) -> String {
    serve(Router::new().route(
        "/event-broker/v1/subscriptions",
        post(move || async move { (StatusCode::CREATED, axum::Json(body)) }),
    ))
    .await
}

/// A JOIN `201` body whose interests carry `unmatched` in order.
fn join_body(unmatched: [serde_json::Value; 3]) -> serde_json::Value {
    let interests: Vec<serde_json::Value> = unmatched
        .into_iter()
        .map(|unmatched_types| {
            json!({
                "topic": "gts.cf.core.events.topic.v1~acme.orders.x.x.v1",
                "subject_tenant": "00000000-0000-0000-0000-000000001920",
                "ownership_filter": "only_mine",
                "barrier_mode": "respect",
                "types": [
                    "gts.cf.core.events.event.v1~acme.orders.x.placed.v1~",
                    "gts.cf.core.events.event.v1~acme.orders.y.*",
                ],
                "filter": null,
                "unmatched_types": unmatched_types,
            })
        })
        .collect();
    json!({
        "id": "00000000-0000-0000-0000-000000001921",
        "consumer_group": "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000001922",
        "client_agent": "orders/1.0",
        "interests": interests,
        "assigned": [{ "topic": "gts.cf.core.events.topic.v1~acme.orders.x.x.v1", "partition": 0 }],
        "topology_version": 3,
        "created_at": "2026-01-02T03:04:05Z",
    })
}

fn join_request() -> JoinRequest {
    JoinRequest {
        consumer_group: ConsumerGroupId::new(uuid::Uuid::from_u128(0x1922)),
        client_agent: "orders/1.0".to_owned(),
        interests: Vec::new(),
        session_timeout: None,
    }
}

/// Serves the JOIN route, answers `201` with a JOIN body and records each request's JSON
/// body, in order.
async fn join_body_server() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let recorder = Arc::clone(&seen);
    let handler = move |axum::Json(body): axum::Json<serde_json::Value>| {
        let recorder = Arc::clone(&recorder);
        async move {
            recorder.lock().expect("the recorder lock").push(body);
            (
                StatusCode::CREATED,
                axum::Json(join_body([json!([]), json!([]), json!([])])),
            )
        }
    };
    let base_url =
        serve(Router::new().route("/event-broker/v1/subscriptions", post(handler))).await;
    (base_url, seen)
}

/// The broker reads `session_timeout` as an ISO 8601 duration with fractional
/// seconds, so a value under one second keeps its fraction on the wire.
#[tokio::test]
async fn a_join_sends_a_sub_second_session_timeout_as_fractional_seconds() {
    let (base_url, bodies) = join_body_server().await;
    let broker = RestBroker::new(base_url).expect("build the client");

    broker
        .join(
            &SecurityContext::anonymous(),
            JoinRequest {
                session_timeout: Some(Duration::from_millis(500)),
                ..join_request()
            },
        )
        .await
        .expect("the join succeeds");

    let bodies = bodies.lock().expect("the recorder lock").clone();
    let [body] = bodies.as_slice() else {
        panic!("expected one JOIN request, got {bodies:?}");
    };
    assert_eq!(
        body,
        &json!({
            "consumer_group": "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000001922",
            "client_agent": "orders/1.0",
            "interests": [],
            "session_timeout": "PT0.5S",
        })
    );
}

/// Whole minutes go out in the canonical `PT#H#M#S` form the broker parses back to
/// the same duration, and an unset `session_timeout` leaves the member out, so the
/// broker applies its default.
#[tokio::test]
async fn a_join_sends_whole_minutes_and_no_session_timeout_as_the_broker_reads_them() {
    let (base_url, bodies) = join_body_server().await;
    let broker = RestBroker::new(base_url).expect("build the client");

    for session_timeout in [
        Some(Duration::from_secs(60)),
        Some(Duration::from_millis(90_500)),
        None,
    ] {
        broker
            .join(
                &SecurityContext::anonymous(),
                JoinRequest {
                    session_timeout,
                    ..join_request()
                },
            )
            .await
            .expect("the join succeeds");
    }

    let bodies = bodies.lock().expect("the recorder lock").clone();
    let [minute, minute_and_a_half, unset] = bodies.as_slice() else {
        panic!("expected three JOIN requests, got {bodies:?}");
    };
    assert_eq!(
        (
            minute
                .get("session_timeout")
                .and_then(serde_json::Value::as_str),
            minute_and_a_half
                .get("session_timeout")
                .and_then(serde_json::Value::as_str),
            unset
                .get("session_timeout")
                .and_then(serde_json::Value::as_str),
        ),
        (Some("PT1M"), Some("PT1M30.5S"), None)
    );
}

/// Only the interests that report entries are listed, by their position.
#[tokio::test]
async fn a_join_keeps_each_interests_unmatched_types_by_its_position() {
    let base_url = join_server(join_body([
        json!([]),
        json!([
            "gts.cf.core.events.event.v1~acme.orders.x.placed.v1~",
            "gts.cf.core.events.event.v1~acme.orders.y.*",
        ]),
        json!([]),
    ]))
    .await;
    let broker = RestBroker::new(base_url).expect("build the client");

    let assignment = broker
        .join(&SecurityContext::anonymous(), join_request())
        .await
        .expect("the join succeeds");

    let SubscriptionAssignment {
        id,
        topology_version,
        assigned,
        unmatched_types,
    } = assignment;
    let [crate::api::AssignedPartition { topic, partition }] = assigned.as_slice() else {
        panic!("expected one assigned partition, got {assigned:?}");
    };
    assert_eq!(
        (
            id,
            topology_version,
            topic.as_ref(),
            *partition,
            unmatched_types
        ),
        (
            SubscriptionId(uuid::Uuid::from_u128(0x1921)),
            3,
            "gts.cf.core.events.topic.v1~acme.orders.x.x.v1",
            0,
            vec![crate::api::UnmatchedTypes {
                interest: 1,
                types: vec![
                    gts::GtsIdPattern::try_new(
                        "gts.cf.core.events.event.v1~acme.orders.x.placed.v1~"
                    )
                    .unwrap(),
                    gts::GtsIdPattern::try_new("gts.cf.core.events.event.v1~acme.orders.y.*")
                        .unwrap(),
                ],
            }],
        )
    );
}

/// An entry that is not a GTS pattern is a broken answer, refused like an
/// echoed type that does not parse.
#[tokio::test]
async fn a_join_answer_with_an_unparsable_unmatched_type_is_a_transport_error() {
    let base_url = join_server(join_body([json!([]), json!([]), json!(["not a pattern"])])).await;
    let broker = RestBroker::new(base_url).expect("build the client");

    let result = broker
        .join(&SecurityContext::anonymous(), join_request())
        .await;

    let Err(EventBrokerError::Transport(detail)) = result else {
        panic!("expected a transport error, got {result:?}");
    };
    assert_eq!(
        detail,
        format!(
            "subscription interest carried an invalid unmatched type: {}",
            gts::GtsIdPattern::try_new("not a pattern").unwrap_err()
        )
    );
}

/// The stream request names its subscription in the `subscription` query
/// parameter, on both transports. The loopback answers `404`, so each stream
/// ends with an error after its first request.
#[tokio::test]
async fn a_stream_request_names_its_subscription_in_the_subscription_query_parameter() {
    use axum::extract::RawQuery;
    use axum::routing::get;
    use futures_util::StreamExt;

    /// The route each request reached, with its raw query.
    type Seen = Arc<Mutex<Vec<(&'static str, Option<String>)>>>;
    let seen: Seen = Arc::default();
    let on_stream = Arc::clone(&seen);
    let on_sse = Arc::clone(&seen);
    let base_url = serve(
        Router::new()
            .route(
                "/event-broker/v1/events:stream",
                get(move |RawQuery(query): RawQuery| async move {
                    on_stream
                        .lock()
                        .expect("the recorder lock")
                        .push((":stream", query));
                    StatusCode::NOT_FOUND
                }),
            )
            .route(
                "/event-broker/v1/events:sse",
                get(move |RawQuery(query): RawQuery| async move {
                    on_sse
                        .lock()
                        .expect("the recorder lock")
                        .push((":sse", query));
                    StatusCode::NOT_FOUND
                }),
            ),
    )
    .await;
    let id = SubscriptionId(uuid::Uuid::from_u128(0x1923));

    for transport in [StreamTransport::Multipart, StreamTransport::Sse] {
        let broker = RestBroker::new(base_url.clone())
            .expect("build the client")
            .with_stream_transport(transport);
        let mut frames = broker
            .stream(&SecurityContext::anonymous(), id)
            .await
            .expect("the stream is built before any request");
        let first = frames.next().await;
        assert!(
            matches!(first, Some(Err(_))),
            "a 404 open ends the stream with an error, got {first:?}"
        );
    }

    assert_eq!(
        seen.lock().expect("the recorder lock").clone(),
        vec![
            (
                ":stream",
                Some("subscription=00000000-0000-0000-0000-000000001923".to_owned())
            ),
            (
                ":sse",
                Some("subscription=00000000-0000-0000-0000-000000001923".to_owned())
            ),
        ]
    );
}

/// A consumer-group answer reports its tenant and creator as `tenant` and
/// `owner_principal`, and every field reaches the decoded group.
#[tokio::test]
async fn a_consumer_group_answer_decodes_tenant_and_owner_principal() {
    use axum::routing::get;

    let base_url = serve(Router::new().route(
        "/event-broker/v1/consumer-groups/{id}",
        get(|| async {
            axum::Json(json!({
                "id": "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000001924",
                "kind": "anonymous",
                "tenant": "00000000-0000-0000-0000-000000001925",
                "owner_principal": "00000000-0000-0000-0000-000000001926",
                "description": "order-fulfilment workers",
                "created_at": "2026-01-02T03:04:05Z",
            }))
        }),
    ))
    .await;
    let broker = RestBroker::new(base_url).expect("build the client");
    let id = ConsumerGroupId::new(uuid::Uuid::from_u128(0x1924));

    let ConsumerGroup {
        id: group_id,
        tenant,
        owner_principal,
        kind,
        description,
        created_at,
    } = broker
        .get_consumer_group(&SecurityContext::anonymous(), &id)
        .await
        .expect("the group decodes");

    assert_eq!(
        (
            group_id,
            tenant,
            owner_principal,
            kind,
            description.as_deref(),
            created_at.to_rfc3339(),
        ),
        (
            id,
            uuid::Uuid::from_u128(0x1925),
            uuid::Uuid::from_u128(0x1926),
            crate::models::ConsumerGroupKind::Anonymous,
            Some("order-fulfilment workers"),
            "2026-01-02T03:04:05+00:00".to_owned(),
        )
    );
}
