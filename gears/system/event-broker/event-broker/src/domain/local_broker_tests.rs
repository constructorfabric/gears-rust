//! `LocalBroker` coverage over the gear's real ingest path, through the
//! in-process client the test harness registers, and of its stream-frame
//! conversion.

use serde_json::json;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use event_broker_sdk::api::{
    Ack, EventBrokerApi, IngestOutcome, JoinRequest, ProducerMode, SubscriptionAssignment,
    SubscriptionInterest, WireFrame,
};
use event_broker_sdk::models::{
    ConsumerGroup, ConsumerGroupKind, ConsumerGroupQuery, CreateConsumerGroupRequest,
};
use event_broker_sdk::{
    ConsumerGroupId, EventBrokerError, EventV1, GtsIdPattern, GtsInstanceId, ProducerId, Sequence,
    SubscriptionId, UnmatchedTypes,
};
use toolkit_canonical_errors::Problem;

use crate::domain::streaming::frames::Frame;
use crate::test_support::{EventBrokerHarness, StaticTypesRegistry};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.eb.local_broker.orders.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.eb.local_broker.created.v1~");
const SUBJECT_TYPE: &str = gts_id!("example.eb.local_broker.order.v1~");
const TENANT: Uuid = Uuid::from_u128(1);
const SUBJECT: Uuid = Uuid::from_u128(2);
/// Bounds a persisted publish asked to wait `Duration::ZERO`: far under the broker's
/// default 20 s cap, so a publish that holds for the cap instead of `within` overruns it.
const ZERO_WAIT_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// The caller of the tests that pass their own context: a fixed subject in
/// `TENANT`. The broker refuses a caller with a nil subject or tenant.
fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(TENANT)
        .build()
        .expect("valid security context")
}

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
        "subject_tenant": TENANT,
        "source": "order-service",
        "subject": format!("order-{n}"),
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-01-01T00:00:00Z",
        "trace_parent": null,
        "data": { "order": n.to_string() },
        "meta": {
            "version": 1,
            "producer": producer_id.0,
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
    let ctx = caller();
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
    let ctx = caller();
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
    let ctx = caller();
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
    let ctx = caller();
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
    let ctx = caller();
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
    let ctx = caller();
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

/// An in-process get of a group that was never created reads back as
/// `ConsumerGroupNotFound` naming the group's GTS id, as the broker reports it.
#[tokio::test]
async fn a_local_get_of_an_unknown_group_names_the_group_it_was_asked_for() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = caller();

    let result = broker
        .get_consumer_group(&ctx, &ConsumerGroupId::new(Uuid::from_u128(0xabc)))
        .await;

    let Err(EventBrokerError::ConsumerGroupNotFound {
        consumer_group,
        detail,
    }) = result
    else {
        panic!("expected ConsumerGroupNotFound, got {result:?}");
    };
    assert_eq!(
        consumer_group,
        "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000abc"
    );
    assert_eq!(
        detail,
        concat!(
            "consumer group 'gts.cf.core.events.consumer_group.v1~",
            "00000000-0000-0000-0000-000000000abc' is not registered"
        )
    );
}

/// The JSON body of a stateless event: no stamped member, no `meta`.
fn stateless_body(n: u128) -> serde_json::Value {
    json!({
        "id": Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0001_0000 + n),
        "type": EVENT_TYPE,
        "subject_tenant": TENANT,
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
        .publish(&caller(), sentinel, Ack::Accepted)
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
    let ctx = caller();

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

/// The ids stored on partition 0 right now, without waiting for the outbox.
async fn stored_ids_now(harness: &EventBrokerHarness) -> Vec<Uuid> {
    let at = event_broker_sdk::backend::TopicPartition::new(
        event_broker_sdk::GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
        event_broker_sdk::backend::Partition::new(0),
    );
    harness
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
        .collect()
}

/// In process a persisted publish holds until the ingest outbox has stored the event on
/// the backend: a single and a batch publish report `Persisted` and are readable when
/// they return, and a resend of the stored chain head is answered `Duplicate`.
#[tokio::test]
async fn a_local_persisted_publish_reports_persisted_once_stored() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = caller();
    let producer_id = broker
        .register_producer(&ctx, ProducerMode::Chained, "order-service/1.0")
        .await
        .expect("the producer registers");
    let persisted = Ack::Persisted {
        within: std::time::Duration::from_secs(30),
    };
    let first = chained_event(producer_id, 1, -1);
    let second = chained_event(producer_id, 2, 0);

    let single = broker
        .publish(&ctx, &first, persisted)
        .await
        .expect("the single publish succeeds");
    let batch = broker
        .publish_batch(&ctx, std::slice::from_ref(&second), persisted)
        .await
        .expect("the batch publish succeeds");
    let stored = stored_ids_now(&harness).await;
    let resent = broker
        .publish(&ctx, &second, persisted)
        .await
        .expect("the resend succeeds");

    assert_eq!(
        (single, batch, resent),
        (
            IngestOutcome::Persisted,
            IngestOutcome::Persisted,
            IngestOutcome::Duplicate
        )
    );
    assert_eq!(stored, vec![first.id(), second.id()]);
}

/// A persisted publish whose wait ends before the backend stores the event reports
/// `Accepted`, single and batch: the event stays admitted, and nothing is stored. The
/// wait is the caller's `within`, not the broker's cap.
#[tokio::test]
async fn a_local_persisted_publish_whose_wait_ends_first_reports_accepted() {
    let backend = std::sync::Arc::new(crate::test_support::NeverStoringBackend::new(
        std::sync::Arc::new(memory_event_broker_plugin::MemoryBackend::new()),
    ));
    let harness = EventBrokerHarness::builder()
        .with_backend(backend)
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
        .await;
    let broker = harness.broker();
    let ctx = caller();
    let persisted = Ack::Persisted {
        within: std::time::Duration::ZERO,
    };

    let single = tokio::time::timeout(
        ZERO_WAIT_BOUND,
        broker.publish(&ctx, &stateless_event(1), persisted),
    )
    .await
    .expect("the single publish waits `within`, not the cap")
    .expect("the single publish succeeds");
    let batch = tokio::time::timeout(
        ZERO_WAIT_BOUND,
        broker.publish_batch(&ctx, &[stateless_event(2)], persisted),
    )
    .await
    .expect("the batch publish waits `within`, not the cap")
    .expect("the batch publish succeeds");

    assert_eq!(
        (single, batch),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(stored_ids_now(&harness).await, Vec::<Uuid>::new());
}

/// The in-process path refuses what `EventV1::check_publishable` refuses, single and
/// batch, before any ingest call.
#[tokio::test]
async fn a_local_publish_of_a_stamped_or_sourceless_event_is_refused() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = caller();
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
    let ctx = caller();
    let mut body = stateless_body(1);
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

    let sentinel = stateless_event(2);
    let stored = stored_ids_through(&harness, broker.as_ref(), &sentinel).await;
    assert_eq!(
        stored,
        vec![sentinel.id()],
        "a refused publish stores nothing"
    );
}

/// An in-process publish hands the event's owner to ingest; an unowned event is
/// tenant-wide.
#[test]
fn an_in_process_publish_carries_the_events_owner() {
    let mut owned = stateless_body(1);
    owned["subject_owner"] = json!(Uuid::from_u128(0x0e));

    let owners = [event_from(owned), stateless_event(2)].map(|event| {
        crate::domain::ingest::PublishRequest::try_from(&event)
            .expect("the event converts")
            .subject_owner
    });

    assert_eq!(owners, [Some(Uuid::from_u128(0x0e)), None]);
}

/// A domain event as delivery hands it over: every member set, the position stamped.
fn delivered_domain_event() -> crate::domain::model::Event {
    crate::domain::model::Event {
        id: Uuid::from_u128(11),
        r#type: event_broker_sdk::GtsTypeId::new(EVENT_TYPE),
        topic: event_broker_sdk::GtsInstanceId::try_new(TOPIC).unwrap(),
        subject_tenant: TENANT,
        source: "order-service".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: event_broker_sdk::GtsTypeId::new(SUBJECT_TYPE),
        subject_owner: None,
        occurred_at: "2026-01-02T03:04:05Z".parse().unwrap(),
        trace_parent: Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_owned()),
        data: json!({ "amount": 100 }),
        meta: None,
        ingest: None,
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
    assert_eq!(event.subject_tenant(), TENANT);
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

/// An in-process delivery keeps the event's owner and never its ingest stamp: the
/// delivered `EventV1` writes every member, and `meta` stays `null`.
#[test]
fn a_delivered_domain_event_keeps_its_owner_and_drops_its_ingest_stamp() {
    let frame = delivered_frame(crate::domain::model::Event {
        subject_owner: Some(Uuid::from_u128(0x0e)),
        ingest: Some(crate::domain::model::IngestStamp {
            tenant: Uuid::from_u128(0x7e),
            actor: Uuid::from_u128(0xac),
        }),
        ..delivered_domain_event()
    })
    .unwrap();

    let WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": Uuid::from_u128(11),
            "type": EVENT_TYPE,
            "subject_tenant": TENANT,
            "source": "order-service",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "subject_owner": "00000000-0000-0000-0000-00000000000e",
            "occurred_at": "2026-01-02T03:04:05Z",
            "trace_parent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            "data": { "amount": 100 },
            "partition": 2,
            "sequence": 7,
            "sequence_time": "2026-01-02T03:04:06Z",
            "meta": null,
        })
    );
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
    assert_eq!(event.subject_tenant(), TENANT);
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

#[tokio::test]
async fn the_selector_survives_the_in_process_round_trip() {
    let harness = harness().await;
    let ctx = harness.security_context();
    let broker = harness.broker();
    let group = broker
        .create_consumer_group(
            ctx,
            CreateConsumerGroupRequest {
                client_agent: "order-service/1.0".to_owned(),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    let interest = SubscriptionInterest::builder()
        .topic(GtsInstanceId::try_new(TOPIC).unwrap())
        .subject_tenant(TENANT)
        .ownership_filter(event_broker_sdk::OwnershipFilter::EntireTenant)
        .types([GtsIdPattern::try_new(EVENT_TYPE).unwrap()])
        .subject_types([GtsIdPattern::try_new(SUBJECT_TYPE).unwrap()])
        .build()
        .expect("the interest builds");

    let assignment = broker
        .join(
            ctx,
            JoinRequest {
                consumer_group: group,
                client_agent: "order-service/1.0".to_owned(),
                interests: vec![interest],
                session_timeout: None,
            },
        )
        .await
        .expect("the join succeeds");
    let subscription = broker
        .get_subscription(ctx, assignment.id)
        .await
        .expect("the subscription is read back");

    let [interest] = subscription.interests.as_slice() else {
        panic!("expected one interest, got {:?}", subscription.interests);
    };
    assert_eq!(
        interest.subject_types(),
        [GtsIdPattern::try_new(SUBJECT_TYPE).unwrap()]
    );
}

/// An interest that names no tenant reads the caller's: the in-process JOIN
/// resolves it from the caller's context, as the REST JOIN does, and keeps the
/// default `only_mine`.
#[tokio::test]
async fn an_in_process_interest_without_a_tenant_reads_the_callers_tenant() {
    let harness = harness().await;
    let ctx = harness.security_context();
    let broker = harness.broker();
    let group = broker
        .create_consumer_group(
            ctx,
            CreateConsumerGroupRequest {
                client_agent: "order-service/1.0".to_owned(),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    let interest = SubscriptionInterest::builder()
        .topic(GtsInstanceId::try_new(TOPIC).unwrap())
        .types([GtsIdPattern::try_new(EVENT_TYPE).unwrap()])
        .build()
        .expect("the interest builds");

    let assignment = broker
        .join(
            ctx,
            JoinRequest {
                consumer_group: group,
                client_agent: "order-service/1.0".to_owned(),
                interests: vec![interest],
                session_timeout: None,
            },
        )
        .await
        .expect("the join succeeds");
    let subscription = broker
        .get_subscription(ctx, assignment.id)
        .await
        .expect("the subscription is read back");

    let [interest] = subscription.interests.as_slice() else {
        panic!("expected one interest, got {:?}", subscription.interests);
    };
    assert_eq!(
        (interest.subject_tenant(), interest.ownership_filter()),
        (
            event_broker_sdk::InterestTenant::Tenant(ctx.subject_tenant_id()),
            event_broker_sdk::OwnershipFilter::OnlyMine,
        )
    );
}

/// The in-process JOIN reports what the REST JOIN reports: the subscription,
/// its partitions and topology version as the coordinator holds them, and the
/// interests with entries that match no registered type, by position, in
/// interest order even when interests on two topics interleave.
#[tokio::test]
async fn the_in_process_join_carries_unmatched_types() {
    const OTHER_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.eb.local_broker.refunds.v1");
    const UNREGISTERED: &str =
        gts_id!("cf.core.events.event.v1~example.eb.local_broker.refunded.v1~");
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            { "id": OTHER_TOPIC, "partitions": 1 },
            { "id": EVENT_TYPE, "topic": TOPIC },
        ])))
        .build()
        .await;
    let ctx = harness.security_context();
    let broker = harness.broker();
    let group = broker
        .create_consumer_group(
            ctx,
            CreateConsumerGroupRequest {
                client_agent: "order-service/1.0".to_owned(),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    let interest = |topic: &str, types: &[&str]| {
        SubscriptionInterest::builder()
            .topic(GtsInstanceId::try_new(topic).unwrap())
            .subject_tenant(TENANT)
            .ownership_filter(event_broker_sdk::OwnershipFilter::EntireTenant)
            .types(types.iter().map(|t| GtsIdPattern::try_new(t).unwrap()))
            .build()
            .expect("the interest builds")
    };

    let assignment = broker
        .join(
            ctx,
            JoinRequest {
                consumer_group: group,
                client_agent: "order-service/1.0".to_owned(),
                interests: vec![
                    interest(TOPIC, &[EVENT_TYPE]),
                    interest(TOPIC, &[EVENT_TYPE, UNREGISTERED]),
                    interest(OTHER_TOPIC, &[EVENT_TYPE]),
                    interest(TOPIC, &[UNREGISTERED]),
                ],
                session_timeout: None,
            },
        )
        .await
        .expect("the join succeeds");

    let SubscriptionAssignment {
        id,
        topology_version,
        assigned,
        unmatched_types,
    } = assignment;
    let members = harness.groups().list();
    let [member] = members.as_slice() else {
        panic!("expected the one member the JOIN made, got {members:?}");
    };
    let [first, second] = assigned.as_slice() else {
        panic!("expected a partition of each topic, got {assigned:?}");
    };
    let [held_first, held_second] = member.assigned.as_slice() else {
        panic!("expected the member to hold a partition of each topic, got {member:?}");
    };
    assert_eq!(
        (
            id,
            topology_version,
            (first.topic.as_ref(), first.partition),
            (second.topic.as_ref(), second.partition),
        ),
        (
            SubscriptionId(member.id),
            member.topology_version,
            (
                held_first.topic.as_ref(),
                u32::try_from(held_first.partition).unwrap()
            ),
            (
                held_second.topic.as_ref(),
                u32::try_from(held_second.partition).unwrap()
            ),
        )
    );
    assert_eq!(
        unmatched_types,
        vec![
            UnmatchedTypes {
                interest: 1,
                types: vec![GtsIdPattern::try_new(UNREGISTERED).unwrap()],
            },
            UnmatchedTypes {
                interest: 2,
                types: vec![GtsIdPattern::try_new(EVENT_TYPE).unwrap()],
            },
            UnmatchedTypes {
                interest: 3,
                types: vec![GtsIdPattern::try_new(UNREGISTERED).unwrap()],
            },
        ]
    );
}

/// Only the joiner may LEAVE: another subject of the joiner's tenant gets the
/// permission-denied detail the SDK reads back for every `403`, and the
/// subscription stays.
#[tokio::test]
async fn a_local_leave_by_another_principal_of_the_tenant_is_unauthorized() {
    let harness = harness().await;
    let broker = harness.broker();
    let group = broker
        .create_consumer_group(
            &caller(),
            CreateConsumerGroupRequest {
                client_agent: "order-service/1.0".to_owned(),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    let assignment = broker
        .join(
            &caller(),
            JoinRequest {
                consumer_group: group,
                client_agent: "order-service/1.0".to_owned(),
                interests: vec![
                    SubscriptionInterest::builder()
                        .topic(GtsInstanceId::try_new(TOPIC).unwrap())
                        .subject_tenant(TENANT)
                        .ownership_filter(event_broker_sdk::OwnershipFilter::EntireTenant)
                        .types([GtsIdPattern::try_new(EVENT_TYPE).unwrap()])
                        .build()
                        .expect("the interest builds"),
                ],
                session_timeout: None,
            },
        )
        .await
        .expect("the join succeeds");
    let other = SecurityContext::builder()
        .subject_id(Uuid::from_u128(3))
        .subject_tenant_id(TENANT)
        .build()
        .expect("valid security context");

    let result = broker.leave(&other, assignment.id).await;

    let Err(EventBrokerError::Unauthorized { detail }) = result else {
        panic!("expected Unauthorized, got {result:?}");
    };
    assert_eq!(
        detail,
        "You do not have permission to perform this operation"
    );
    let subscription = broker
        .get_subscription(&caller(), assignment.id)
        .await
        .expect("the subscription stays");
    assert_eq!(subscription.id, assignment.id);
}

/// Only the creator may JOIN or delete an anonymous group: another subject of
/// the creator's tenant gets the permission-denied detail the SDK reads back
/// for every `403`, no subscription is created and the group stays.
#[tokio::test]
async fn a_local_join_or_delete_by_another_principal_of_the_tenant_is_unauthorized() {
    let harness = harness().await;
    let broker = harness.broker();
    let group = broker
        .create_consumer_group(
            &caller(),
            CreateConsumerGroupRequest {
                client_agent: "order-service/1.0".to_owned(),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    let other = SecurityContext::builder()
        .subject_id(Uuid::from_u128(3))
        .subject_tenant_id(TENANT)
        .build()
        .expect("valid security context");

    let joined = broker
        .join(
            &other,
            JoinRequest {
                consumer_group: group,
                client_agent: "order-service/1.0".to_owned(),
                interests: Vec::new(),
                session_timeout: None,
            },
        )
        .await;
    let deleted = broker.delete_consumer_group(&other, &group).await;

    let Err(EventBrokerError::Unauthorized { detail }) = joined else {
        panic!("expected Unauthorized, got {joined:?}");
    };
    assert_eq!(
        detail,
        "You do not have permission to perform this operation"
    );
    let Err(EventBrokerError::Unauthorized { detail }) = deleted else {
        panic!("expected Unauthorized, got {deleted:?}");
    };
    assert_eq!(
        detail,
        "You do not have permission to perform this operation"
    );
    assert!(
        harness.groups().list().is_empty(),
        "the refused JOIN created no subscription"
    );
    let kept = broker
        .get_consumer_group(&caller(), &group)
        .await
        .expect("the group stays");
    assert_eq!(kept.id, group);
}

/// A `meta` with a sequence and no producer is refused in process as over REST:
/// the canonical `400` with the `MetaWithoutProducerId` detail, recovered as
/// `EventBrokerError::Other`, and nothing is stored.
#[tokio::test]
async fn a_local_publish_with_a_sequence_and_no_producer_is_refused_as_meta_without_producer_id() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = caller();
    let mut body = stateless_body(1);
    body["meta"] = json!({ "version": 1, "sequence": 5 });
    let event = event_from(body);

    let single = broker.publish(&ctx, &event, Ack::Accepted).await;

    let Err(EventBrokerError::Other { canonical }) = single else {
        panic!("expected an `Other` rejection, got {single:?}");
    };
    assert_eq!(
        serde_json::to_value(Problem::from(canonical)).expect("the problem serializes"),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
            "context": {
                "format": "MetaWithoutProducerId: meta carries previous or sequence but no producer",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let sentinel = stateless_event(2);
    let stored = stored_ids_through(&harness, broker.as_ref(), &sentinel).await;
    assert_eq!(
        stored,
        vec![sentinel.id()],
        "a refused publish stores nothing"
    );
}

/// A monotonic producer's `meta` without a sequence is refused in process, and
/// its chain head is left as it was.
#[tokio::test]
async fn a_local_monotonic_publish_without_sequence_is_refused_as_monotonic_mode_fields_violation()
{
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = caller();
    let producer_id = broker
        .register_producer(&ctx, ProducerMode::Monotonic, "order-service/1.0")
        .await
        .expect("the producer registers");
    let mut body = stateless_body(1);
    body["meta"] = json!({ "version": 1, "producer": producer_id.0 });
    let event = event_from(body);

    let single = broker.publish(&ctx, &event, Ack::Accepted).await;

    let Err(EventBrokerError::Other { canonical }) = single else {
        panic!("expected an `Other` rejection, got {single:?}");
    };
    assert_eq!(
        serde_json::to_value(Problem::from(canonical)).expect("the problem serializes"),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "MonotonicModeFieldsViolation: a monotonic-mode publish needs meta.sequence and must omit meta.previous",
            "context": {
                "format": "MonotonicModeFieldsViolation: a monotonic-mode publish needs meta.sequence and must omit meta.previous",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    let cursors = broker
        .get_producer_cursors(&ctx, producer_id)
        .await
        .expect("the cursors read");
    assert_eq!(
        format!("{cursors:?}"),
        format!(
            "{:?}",
            event_broker_sdk::api::ProducerCursors {
                id: producer_id,
                client_agent: "order-service/1.0".to_owned(),
                topics: Vec::new(),
            }
        )
    );
    let sentinel = stateless_event(2);
    let stored = stored_ids_through(&harness, broker.as_ref(), &sentinel).await;
    assert_eq!(
        stored,
        vec![sentinel.id()],
        "a refused publish stores nothing"
    );
}
