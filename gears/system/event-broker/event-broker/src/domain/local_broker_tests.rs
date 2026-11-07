//! `LocalBroker` coverage over the gear's real ingest path, through the
//! in-process client the test harness registers, and of its stream-frame
//! conversion.

use serde_json::json;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use event_broker_sdk::api::{Ack, EventBrokerApi, IngestOutcome, ProducerMode, WireFrame};
use event_broker_sdk::models::{
    ConsumerGroup, ConsumerGroupKind, ConsumerGroupQuery, CreateConsumerGroupRequest,
};
use event_broker_sdk::{EventBrokerError, EventV1, ProducerId, Sequence};
use toolkit_canonical_errors::Problem;

use crate::domain::streaming::frames::Frame;
use crate::test_support::{EventBrokerHarness, StaticTypesRegistry};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.eb.local_broker.orders.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.eb.local_broker.created.v1~");
const SUBJECT_TYPE: &str = gts_id!("cf.core.events.subject.v1~example.eb.local_broker.order.v1~");
const TENANT: Uuid = Uuid::from_u128(1);

async fn harness() -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            {
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "data_schema": { "type": "object" },
                "allowed_subject_types": [SUBJECT_TYPE],
            },
        ])))
        .build()
        .await
}

/// A chained event read from its JSON body: the builder sets no `meta`.
fn chained_event(producer_id: ProducerId, n: u128, previous: i64) -> EventV1 {
    serde_json::from_value(json!({
        "id": Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0000 + n),
        "type": EVENT_TYPE,
        "tenant_id": TENANT,
        "source": "order-service",
        "subject": format!("order-{n}"),
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-01T00:00:00Z",
        "trace_parent": null,
        "data": { "order": n.to_string() },
        "meta": {
            "version": 1,
            "producer_id": producer_id.0,
            "previous": previous,
            "sequence": previous + 1,
        },
    }))
    .expect("a well-formed event body")
}

/// A resent batch whose every event the chain already holds stores nothing
/// new, so the in-process client answers it the way REST answers `200`.
#[tokio::test]
async fn a_local_broker_all_duplicate_batch_is_reported_as_duplicate() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let producer_id = broker
        .register_producer(&ctx, ProducerMode::Chained, "order-service/1.0")
        .await
        .expect("the producer registers");
    let batch = [
        chained_event(producer_id, 1, -1),
        chained_event(producer_id, 2, 0),
    ];

    let first = broker
        .publish_batch(&ctx, &batch, Ack::Accepted)
        .await
        .expect("the batch publishes");
    let resent = broker
        .publish_batch(&ctx, &batch, Ack::Accepted)
        .await
        .expect("the resent batch publishes");

    assert_eq!(
        (first, resent),
        (IngestOutcome::Accepted, IngestOutcome::Duplicate)
    );
}

/// Creates three anonymous groups and returns them sorted by id, the listing's
/// one fixed order.
async fn three_groups(broker: &dyn EventBrokerApi, ctx: &SecurityContext) -> Vec<ConsumerGroup> {
    let mut groups = Vec::new();
    for _ in 0..3 {
        let group = broker
            .create_consumer_group(
                ctx,
                CreateConsumerGroupRequest {
                    client_agent: "order-service/1.0".to_owned(),
                    description: None,
                },
            )
            .await
            .expect("the group is created");
        groups.push(group);
    }
    groups.sort_by_key(|g| g.id.to_gts());
    groups
}

/// The wire body a rejection renders as, minus the per-request `instance`.
fn problem_of(
    result: Result<event_broker_sdk::models::Page<ConsumerGroup>, EventBrokerError>,
) -> serde_json::Value {
    let Err(EventBrokerError::Other { canonical }) = result else {
        panic!("expected an `Other` rejection, got {result:?}");
    };
    serde_json::to_value(Problem::from(canonical)).expect("the problem serializes")
}

/// The in-process listing pages by id like `GET /v1/consumer-groups`: the
/// first page stops at `limit` with a cursor to the rest, and that cursor
/// reaches the last group. The SDK type has no `PartialEq`, so the full
/// structs compare through `Debug`.
#[tokio::test]
async fn a_local_consumer_group_listing_pages_past_its_limit() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let groups = three_groups(broker.as_ref(), &ctx).await;
    let [a, b, c] = groups.as_slice() else {
        panic!("expected three groups, got {groups:?}");
    };

    let first = broker
        .list_consumer_groups(&ctx, ConsumerGroupQuery::new().limit(2))
        .await
        .expect("the first page lists");
    assert_eq!(format!("{:?}", first.items), format!("{:?}", [a, b]));
    assert_eq!(first.next_cursor, Some(format!("fwd:{}", b.id.to_gts())));
    assert_eq!(first.prev_cursor, None);
    assert_eq!(first.limit, 2);

    let next_cursor = first.next_cursor.expect("checked above");
    let second = broker
        .list_consumer_groups(&ctx, ConsumerGroupQuery::new().limit(2).cursor(next_cursor))
        .await
        .expect("the second page lists");
    assert_eq!(format!("{:?}", second.items), format!("{:?}", [c]));
    assert_eq!(second.next_cursor, None);
    assert_eq!(second.prev_cursor, Some(format!("bwd:{}", c.id.to_gts())));
    assert_eq!(second.limit, 2);
}

/// An `id` narrows the in-process listing to the matching group.
#[tokio::test]
async fn a_local_consumer_group_listing_applies_its_filter() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let groups = three_groups(broker.as_ref(), &ctx).await;
    let [_, b, _] = groups.as_slice() else {
        panic!("expected three groups, got {groups:?}");
    };

    let page = broker
        .list_consumer_groups(&ctx, ConsumerGroupQuery::new().id(b.id))
        .await
        .expect("the filtered listing succeeds");

    assert_eq!(format!("{:?}", page.items), format!("{:?}", [b]));
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.prev_cursor, None);
    assert_eq!(page.limit, 25);
}

/// A `kind` keeps the groups of that kind: all three anonymous groups in id
/// order, and none for `Named`.
#[tokio::test]
async fn a_local_consumer_group_listing_filters_by_kind() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let groups = three_groups(broker.as_ref(), &ctx).await;
    let [a, b, c] = groups.as_slice() else {
        panic!("expected three groups, got {groups:?}");
    };

    let anonymous = broker
        .list_consumer_groups(
            &ctx,
            ConsumerGroupQuery::new().kind(ConsumerGroupKind::Anonymous),
        )
        .await
        .expect("the anonymous listing succeeds");
    let named = broker
        .list_consumer_groups(
            &ctx,
            ConsumerGroupQuery::new().kind(ConsumerGroupKind::Named),
        )
        .await
        .expect("the named listing succeeds");

    assert_eq!(format!("{:?}", anonymous.items), format!("{:?}", [a, b, c]));
    assert_eq!(anonymous.next_cursor, None);
    assert_eq!(anonymous.prev_cursor, None);
    assert_eq!(anonymous.limit, 25);
    assert_eq!(format!("{:?}", named.items), "[]");
    assert_eq!(named.next_cursor, None);
    assert_eq!(named.prev_cursor, None);
    assert_eq!(named.limit, 25);
}

/// A zero `limit` is rejected as an invalid query.
#[tokio::test]
async fn a_local_consumer_group_listing_rejects_a_zero_limit() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    three_groups(broker.as_ref(), &ctx).await;

    let result = broker
        .list_consumer_groups(&ctx, ConsumerGroupQuery::new().limit(0))
        .await;

    assert_eq!(
        problem_of(result),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidQuery: limit must be at least 1",
            "context": {
                "format": "InvalidQuery: limit must be at least 1",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// A `cursor` that is not a page cursor is rejected as an invalid query,
/// without quoting the token back.
#[tokio::test]
async fn a_local_consumer_group_listing_rejects_a_malformed_cursor() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    three_groups(broker.as_ref(), &ctx).await;

    let result = broker
        .list_consumer_groups(&ctx, ConsumerGroupQuery::new().cursor("not-a-cursor"))
        .await;

    assert_eq!(
        problem_of(result),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidQuery: cursor is not a valid page cursor",
            "context": {
                "format": "InvalidQuery: cursor is not a valid page cursor",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

/// The JSON body of a stateless event: no stamped member, no `meta`.
fn stateless_body(n: u128) -> serde_json::Value {
    json!({
        "id": Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0001_0000 + n),
        "type": EVENT_TYPE,
        "tenant_id": TENANT,
        "source": "order-service",
        "subject": format!("order-{n}"),
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-01T00:00:00Z",
        "trace_parent": null,
        "data": { "order": n.to_string() },
    })
}

fn event_from(body: serde_json::Value) -> EventV1 {
    serde_json::from_value(body).expect("a well-formed event body")
}

fn stateless_event(n: u128) -> EventV1 {
    event_from(stateless_body(n))
}

/// Publishes `sentinel` and returns the ids stored on partition 0 once it is among
/// them, or after 10 s. The sentinel shares the tenant and type of every event a test
/// publishes, so it drains behind anything those could have left in the outbox.
async fn stored_ids_through(
    harness: &EventBrokerHarness,
    broker: &dyn EventBrokerApi,
    sentinel: &EventV1,
) -> Vec<Uuid> {
    broker
        .publish(&SecurityContext::anonymous(), sentinel, Ack::Accepted)
        .await
        .expect("the sentinel publishes");
    let at = event_broker_sdk::backend::TopicPartition::new(
        event_broker_sdk::GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
        event_broker_sdk::backend::Partition::new(0),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let ids: Vec<Uuid> = harness
            .backend()
            .read(
                &at,
                event_broker_sdk::Sequence::NONE,
                1024,
                event_broker_sdk::backend::Wait::No,
            )
            .await
            .expect("read the stored events")
            .iter()
            .map(|event| event.id)
            .collect();
        if ids.contains(&sentinel.id()) || tokio::time::Instant::now() >= deadline {
            break ids;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The in-process path checks `trace_parent` as REST does. The SDK recovers the
/// field name empty (its `field` is `&'static str`) and the detail from the
/// problem's `detail`.
#[tokio::test]
async fn a_local_publish_with_a_malformed_trace_parent_is_rejected() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();

    let mut body = stateless_body(1);
    body["trace_parent"] = json!("00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01");
    let malformed = event_from(body);

    let result = broker.publish(&ctx, &malformed, Ack::Accepted).await;

    let Err(EventBrokerError::InvalidEventField { field, detail }) = result else {
        panic!("expected an `InvalidEventField` rejection, got {result:?}");
    };
    assert_eq!((field, detail.as_str()), ("", "Request validation failed"));

    let sentinel = stateless_event(2);
    let stored = stored_ids_through(&harness, broker.as_ref(), &sentinel).await;
    assert_eq!(
        stored,
        vec![sentinel.id()],
        "a rejected publish stores nothing"
    );
}

/// In process there is no synchronous ingest: a persisted publish, single or batch, is
/// refused with the detail the REST layer answers, and nothing is admitted.
#[tokio::test]
async fn a_local_persisted_publish_is_unimplemented_and_admits_nothing() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let persisted = Ack::Persisted {
        within: std::time::Duration::from_secs(5),
    };

    // #5655: the broker serves no persisted publish until synchronous publish lands.
    let single = broker.publish(&ctx, &stateless_event(1), persisted).await;
    let Err(EventBrokerError::Unimplemented(detail)) = single else {
        panic!("expected `Unimplemented`, got {single:?}");
    };
    assert_eq!(
        detail,
        "synchronous publish (Prefer: wait) is not implemented yet"
    );
    let batch = broker
        .publish_batch(&ctx, &[stateless_event(2)], persisted)
        .await;
    let Err(EventBrokerError::Unimplemented(detail)) = batch else {
        panic!("expected `Unimplemented`, got {batch:?}");
    };
    assert_eq!(
        detail,
        "synchronous publish (Prefer: wait) is not implemented yet"
    );

    let sentinel = stateless_event(3);
    let stored = stored_ids_through(&harness, broker.as_ref(), &sentinel).await;
    assert_eq!(
        stored,
        vec![sentinel.id()],
        "a refused publish stores nothing"
    );
}

/// The in-process path refuses what `EventV1::check_publishable` refuses, single and
/// batch, before any ingest call.
#[tokio::test]
async fn a_local_publish_of_a_stamped_or_sourceless_event_is_refused() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let with = |n: u128, member: &str, value: serde_json::Value| {
        let mut body = stateless_body(n);
        body[member] = value;
        event_from(body)
    };
    let mut sourceless = stateless_body(4);
    sourceless
        .as_object_mut()
        .expect("an object body")
        .remove("source");
    let cases = [
        (
            with(1, "partition", json!(3)),
            "partition",
            "broker-stamped and read-only on publish",
        ),
        (
            with(2, "sequence", json!(42)),
            "sequence",
            "broker-stamped and read-only on publish",
        ),
        (
            with(3, "sequence_time", json!("2026-01-01T00:00:01Z")),
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
            .publish_batch(&ctx, &[stateless_event(5), event], Ack::Accepted)
            .await;
        let Err(EventBrokerError::InvalidEventField { field, detail }) = batch else {
            panic!("expected an `InvalidEventField` refusal, got {batch:?}");
        };
        assert_eq!((field, detail.as_str()), (expected_field, expected_detail));
    }

    let sentinel = stateless_event(6);
    let stored = stored_ids_through(&harness, broker.as_ref(), &sentinel).await;
    assert_eq!(
        stored,
        vec![sentinel.id()],
        "a refused publish stores nothing"
    );
}

/// A `meta.version` past the domain's `i32` is refused before any ingest call.
#[tokio::test]
async fn a_local_publish_with_a_meta_version_past_the_domain_range_is_refused() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let mut body = stateless_body(1);
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

    let sentinel = stateless_event(2);
    let stored = stored_ids_through(&harness, broker.as_ref(), &sentinel).await;
    assert_eq!(
        stored,
        vec![sentinel.id()],
        "a refused publish stores nothing"
    );
}

/// A domain event as delivery hands it over: every member set, the position stamped.
fn delivered_domain_event() -> crate::domain::model::Event {
    crate::domain::model::Event {
        id: Uuid::from_u128(11),
        r#type: event_broker_sdk::GtsTypeId::new(EVENT_TYPE),
        topic: event_broker_sdk::GtsInstanceId::try_new(TOPIC).unwrap(),
        tenant_id: TENANT,
        source: "order-service".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: event_broker_sdk::GtsTypeId::new(SUBJECT_TYPE),
        occurred_at: "2026-01-02T03:04:05Z".parse().unwrap(),
        trace_parent: Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_owned()),
        data: json!({ "amount": 100 }),
        meta: None,
        partition: Some(2),
        sequence: Some(Sequence::assigned(7)),
        sequence_time: Some("2026-01-02T03:04:06Z".parse().unwrap()),
    }
}

fn delivered_frame(event: crate::domain::model::Event) -> Result<WireFrame, EventBrokerError> {
    WireFrame::try_from(Frame::Event(Box::new(event)))
}

#[test]
fn a_delivered_domain_event_keeps_its_source_and_position() {
    let frame = delivered_frame(delivered_domain_event()).unwrap();

    let WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.id(), Uuid::from_u128(11));
    assert_eq!(event.event_type(), EVENT_TYPE);
    assert_eq!(event.tenant_id(), TENANT);
    assert_eq!(event.source(), Some("order-service"));
    assert_eq!(event.subject(), "order-1");
    assert_eq!(event.subject_type(), SUBJECT_TYPE);
    assert_eq!(
        event.occurred_at(),
        "2026-01-02T03:04:05Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
    );
    assert_eq!(
        event.trace_parent(),
        Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
    );
    assert_eq!(event.data(), Some(&json!({ "amount": 100 })));
    assert_eq!(event.partition(), Some(2));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
    assert_eq!(
        event.sequence_time(),
        Some(
            "2026-01-02T03:04:06Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        )
    );
    assert!(event.meta().is_none());
}

#[test]
fn a_delivered_domain_event_with_null_data_has_no_data() {
    let frame = delivered_frame(crate::domain::model::Event {
        data: serde_json::Value::Null,
        ..delivered_domain_event()
    })
    .unwrap();

    let WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.data(), None);
    assert_eq!(event.source(), Some("order-service"));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
}

/// Delivery always stamps the position; a domain event without a usable one is
/// handed over unstamped, which the consumer skips, rather than at a made-up position.
#[test]
fn a_delivered_domain_event_without_a_position_is_left_unstamped() {
    let cases = [
        (
            "no partition",
            crate::domain::model::Event {
                partition: None,
                ..delivered_domain_event()
            },
        ),
        (
            "partition -1",
            crate::domain::model::Event {
                partition: Some(-1),
                ..delivered_domain_event()
            },
        ),
        (
            "no sequence",
            crate::domain::model::Event {
                sequence: None,
                ..delivered_domain_event()
            },
        ),
    ];
    for (case, domain_event) in cases {
        let frame = delivered_frame(domain_event).unwrap();

        let WireFrame::Event(event) = frame else {
            panic!("{case}: expected an Event frame, got {frame:?}");
        };
        assert_eq!(event.id(), Uuid::from_u128(11), "{case}");
        assert_eq!(event.partition(), None, "{case}");
        assert_eq!(event.sequence(), None, "{case}");
        assert_eq!(event.sequence_time(), None, "{case}");
    }
}

#[test]
fn a_delivered_domain_event_without_sequence_time_takes_occurred_at() {
    let frame = delivered_frame(crate::domain::model::Event {
        sequence_time: None,
        ..delivered_domain_event()
    })
    .unwrap();

    let WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.partition(), Some(2));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
    assert_eq!(
        event.sequence_time(),
        Some(
            "2026-01-02T03:04:05Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        )
    );
}

/// A stored event can predate ingest's `trace_parent` check, and the field is
/// optional telemetry: delivery drops a malformed one instead of failing the stream,
/// and logs that it did without the value.
#[test]
#[tracing_test::traced_test]
fn a_delivered_domain_event_with_a_malformed_trace_parent_is_delivered_without_it() {
    let frame = delivered_frame(crate::domain::model::Event {
        trace_parent: Some("00-abc-def-01".to_owned()),
        ..delivered_domain_event()
    })
    .unwrap();

    let WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.id(), Uuid::from_u128(11));
    assert_eq!(event.event_type(), EVENT_TYPE);
    assert_eq!(event.tenant_id(), TENANT);
    assert_eq!(event.source(), Some("order-service"));
    assert_eq!(event.subject(), "order-1");
    assert_eq!(event.subject_type(), SUBJECT_TYPE);
    assert_eq!(
        event.occurred_at(),
        "2026-01-02T03:04:05Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
    );
    assert_eq!(event.trace_parent(), None);
    assert_eq!(event.data(), Some(&json!({ "amount": 100 })));
    assert_eq!(event.partition(), Some(2));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
    assert_eq!(
        event.sequence_time(),
        Some(
            "2026-01-02T03:04:06Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        )
    );
    assert!(event.meta().is_none());
    assert!(logs_contain(
        "a delivered event carries a malformed trace_parent; delivering it without one"
    ));
    assert!(!logs_contain("00-abc-def-01"));
}

#[test]
fn a_delivered_domain_event_that_breaks_an_envelope_rule_is_a_transport_error() {
    const PLANTED: &str = "PLANTED-\u{e9}";
    let err = delivered_frame(crate::domain::model::Event {
        subject: PLANTED.to_owned(),
        ..delivered_domain_event()
    })
    .unwrap_err();

    let EventBrokerError::Transport(text) = err else {
        panic!("expected a Transport error, got {err:?}");
    };
    // Names the field and holds no part of the planted value.
    assert_eq!(
        text,
        "stream event frame broke an envelope rule: text field invalid: subject: must contain only printable ASCII (0x20-0x7E)"
    );
}
