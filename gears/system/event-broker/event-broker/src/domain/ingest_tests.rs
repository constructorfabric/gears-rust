//! Partition keys: what the pointer resolves to, and which ingest outbox
//! partition that places the event in. The topic partition is the backend's.
//!
//! Tested here rather than over HTTP because no publish response carries
//! either - the endpoint answers `202` with no body - so the observable
//! behaviour lives at this boundary. What an operator sees on the wire is the
//! ordering these values produce, which is the property each case below states.
//!
//! Event size: the broker counts it as the SDK does, and the ingest outbox row
//! of a largest event fits the outbox.

use event_broker_sdk::gts::EventV1Traits;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use super::PublishRequest;
use crate::domain::backend::partition_input;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::outbox::{OutboxPartitions, outbox_partition_for};

const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.part.foo.v1~";

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new("gts.cf.core.events.topic.v1~x.eb.part.topic.v1").expect("valid id")
}

fn request(tenant: Uuid, subject: &str, data: serde_json::Value) -> PublishRequest {
    PublishRequest {
        id: Uuid::new_v4(),
        r#type: crate::test_support::event_type_id(EVENT_TYPE),
        subject_tenant: tenant,
        source: "partition-test".to_owned(),
        subject: subject.to_owned(),
        subject_type: gts::GtsTypeId::new("gts.x.eb.part.subject.v1~"),
        subject_owner: None,
        occurred_at: chrono::Utc::now(),
        trace_parent: None,
        data,
        meta: None,
    }
}

/// The default the base declares: a tenant's events are co-located whatever
/// else differs about them, which is what gives a tenant total ordering on a
/// topic without anyone declaring anything.
#[test]
fn the_tenant_pointer_co_locates_a_tenants_events() {
    let tenant = Uuid::new_v4();
    let first = request(tenant, "order-1", serde_json::json!({ "order_id": "a" }));
    let second = request(tenant, "order-2", serde_json::json!({ "order_id": "b" }));

    let first_input =
        partition_input(&first, "/subject_tenant").expect("the tenant always resolves");
    let second_input =
        partition_input(&second, "/subject_tenant").expect("the tenant always resolves");

    assert_eq!(first_input, tenant.to_string());
    assert_eq!(first_input, second_input);
    assert_eq!(
        outbox_partition_for(&topic(), &first_input, OutboxPartitions::DEFAULT),
        outbox_partition_for(&topic(), &second_input, OutboxPartitions::DEFAULT),
        "same input, same partition"
    );
}

/// The base's default partition-key pointer is `/subject_tenant`, and it resolves
/// to the tenant's own string.
#[test]
fn the_default_partition_key_resolves_to_the_tenants_own_string() {
    let tenant = Uuid::from_u128(0x5b7_7e4a_0001);
    let traits = serde_json::to_value(
        EventV1Traits::builder()
            .with_topic(topic().as_ref())
            .with_allowed_subject_types(["gts.x.eb.part.subject.v1~"])
            .build(),
    )
    .expect("the traits serialize");
    let Some(default_pointer) = traits["partition_key"].as_str() else {
        panic!("the traits carry a partition key: {traits}");
    };
    assert_eq!(default_pointer, "/subject_tenant");

    let input = partition_input(
        &request(tenant, "order-1", serde_json::json!({ "order_id": "a" })),
        default_pointer,
    )
    .expect("the default pointer resolves");

    assert_eq!(input, tenant.to_string());
}

/// A pointer may reach into the payload, which is the case a bare field name
/// could not express.
#[test]
fn a_pointer_into_the_payload_groups_by_that_member() {
    let one = request(
        Uuid::new_v4(),
        "order-1",
        serde_json::json!({ "order_id": "shared" }),
    );
    let another = request(
        Uuid::new_v4(),
        "order-2",
        serde_json::json!({ "order_id": "shared" }),
    );

    let one_input = partition_input(&one, "/data/order_id").expect("the member is present");
    let another_input = partition_input(&another, "/data/order_id").expect("the member is present");

    assert_eq!(one_input, "shared");
    assert_eq!(
        outbox_partition_for(&topic(), &one_input, OutboxPartitions::DEFAULT),
        outbox_partition_for(&topic(), &another_input, OutboxPartitions::DEFAULT),
        "two different tenants sharing the pointed-at member land together - which is what \
         proves the key is the type's choice rather than the tenant's"
    );
}

/// A numeric member is hashable without a producer stringifying it first.
#[test]
fn a_numeric_member_resolves_to_its_json_form() {
    let event = request(
        Uuid::new_v4(),
        "order-1",
        serde_json::json!({ "order_id": 42 }),
    );

    assert_eq!(
        partition_input(&event, "/data/order_id").expect("a number is hashable"),
        "42"
    );
}

/// Registration proves the member is *declared*, not that every event carries
/// it. An event omitting an optional member is refused rather than routed by
/// something else, because a silent fallback would split one key's events
/// across two partitions.
#[test]
fn a_pointer_resolving_to_nothing_is_refused() {
    let event = request(Uuid::new_v4(), "order-1", serde_json::json!({}));

    let err = partition_input(&event, "/data/order_id")
        .expect_err("an absent member cannot select a partition");
    assert!(
        matches!(
            err,
            DomainError::Validation {
                code: ErrorCode::PartitionKeyUnresolved,
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

#[test]
fn a_pointer_resolving_to_null_is_refused() {
    let event = request(
        Uuid::new_v4(),
        "order-1",
        serde_json::json!({ "order_id": null }),
    );

    let err = partition_input(&event, "/data/order_id")
        .expect_err("null is not a key, it is the absence of one");
    assert!(
        matches!(
            err,
            DomainError::Validation {
                code: ErrorCode::PartitionKeyUnresolved,
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

/// A container has no stable hash input: two objects that are equal as values
/// may serialize differently, so hashing one would make co-location depend on
/// key order.
#[test]
fn a_pointer_resolving_to_a_container_is_refused() {
    let event = request(
        Uuid::new_v4(),
        "order-1",
        serde_json::json!({ "order_id": { "nested": true } }),
    );

    let err =
        partition_input(&event, "/data/order_id").expect_err("an object cannot select a partition");
    assert!(
        matches!(
            err,
            DomainError::Validation {
                code: ErrorCode::PartitionKeyUnresolved,
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

/// Publish bodies covering every optional member absent, present and `null`, a
/// chained and a monotonic `meta`, and text that JSON escapes, each with how many
/// bytes fewer than the SDK the broker counts for it. Parsing makes a nil producer
/// id of an absent one and a sequence of 0 of an absent one, so the broker cannot
/// tell those apart; it counts them so it never measures more than the SDK.
fn parity_bodies() -> Vec<(serde_json::Value, usize)> {
    let base = serde_json::json!({
        "id": "0190f1a2-0000-7000-8000-00000000e101",
        "type": EVENT_TYPE,
        "subject_tenant": "0190f1a2-0000-7000-8000-000000007e01",
        "source": "order-service",
        "subject": "order-\"1\"\\x",
        "subject_type": "gts.x.eb.part.subject.v1~",
        "occurred_at": "2026-11-09T10:00:00.123456Z",
    });
    let with = |members: serde_json::Value| {
        let mut body = base.clone();
        for (name, value) in members.as_object().expect("an object") {
            body[name] = value.clone();
        }
        body
    };
    vec![
        (base.clone(), 0),
        (
            with(serde_json::json!({ "data": null, "trace_parent": null, "meta": null })),
            0,
        ),
        (
            with(serde_json::json!({
                "trace_parent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
                "data": { "order_id": "o-1", "note": "caf\u{e9} \"quoted\"\n", "total": 12.5 },
                "meta": {
                    "version": 1,
                    "producer": "0190f1a2-0000-7000-8000-000000000d01",
                    "previous": 4,
                    "sequence": 5,
                },
            })),
            0,
        ),
        (
            with(serde_json::json!({
                "data": [1, 2, 3],
                "meta": {
                    "version": 1,
                    "producer": "0190f1a2-0000-7000-8000-000000000d01",
                    "previous": null,
                    "sequence": 7,
                },
            })),
            0,
        ),
        // A sequence of 0, as a producer's fallback cursor stamps it.
        (
            with(serde_json::json!({
                "meta": {
                    "version": 1,
                    "producer": "0190f1a2-0000-7000-8000-000000000d01",
                    "previous": -1,
                    "sequence": 0,
                },
            })),
            0,
        ),
        // No producer id and no sequence: each counts as `null`, as the SDK writes it.
        (
            with(serde_json::json!({ "data": "text", "meta": { "version": 1 } })),
            0,
        ),
        // A literal nil producer id counts as `null`: 34 bytes fewer than its uuid.
        (
            with(serde_json::json!({
                "meta": {
                    "version": 1,
                    "producer": "00000000-0000-0000-0000-000000000000",
                    "sequence": 3,
                },
            })),
            34,
        ),
    ]
}

/// The broker counts an event's size on what it parsed, through either entry point,
/// as the SDK counts the same event, and never more: an event the SDK admits is
/// admitted.
#[test]
fn the_broker_counts_an_events_size_as_the_sdk_does() {
    for (body, fewer) in parity_bodies() {
        let event: event_broker_sdk::EventV1 =
            serde_json::from_value(body.clone()).expect("the SDK reads the body");
        let local = PublishRequest::try_from(&event).expect("the in-process broker takes it");
        let rest: crate::api::rest::handlers::ingest::dto::PublishEventRequest =
            serde_json::from_value(body.clone()).expect("the REST handler reads the body");
        let rest = PublishRequest::try_from(rest).expect("the REST handler takes it");

        let sdk = event.encoded_len().expect("the event measures");
        assert_eq!(
            (
                local.encoded_len().expect("the request measures"),
                rest.encoded_len().expect("the request measures"),
            ),
            (sdk - fewer, sdk - fewer),
            "{body}"
        );
    }
}

/// A topic id at the GTS length limit.
fn longest_topic() -> GtsInstanceId {
    let prefix = "gts.cf.core.events.topic.v1~x.eb.fit.";
    let suffix = ".v1";
    let name = "t".repeat(1024 - prefix.len() - suffix.len());
    GtsInstanceId::try_new(&format!("{prefix}{name}{suffix}")).expect("a valid topic id")
}

/// The ingest outbox row of an owned event at the per-event limit, with a topic at
/// the GTS length limit and a partition key at its cap, fits the outbox's row limit:
/// the row adds the topic twice, the key, the stored event's empty members, the
/// stored `meta`'s internal `producer_id` key, 3 bytes longer than the wire
/// `producer` the size counts, the ingest stamp, and the flag a synchronous
/// publish's row carries. A stored `meta` always names a registered producer, so the
/// size counts the same id the row holds. An unowned event's row is smaller: it
/// leaves out the `null` owner the size counts.
#[test]
fn an_ingest_row_of_a_largest_event_fits_the_outbox() {
    let topic = longest_topic();
    let key = "k".repeat(event_broker_sdk::EventV1::MAX_PARTITION_KEY_BYTES);
    let mut request = request(
        Uuid::from_u128(1),
        &"s".repeat(1024),
        serde_json::json!({ "pad": "" }),
    );
    request.source = "o".repeat(256);
    request.subject_owner = Some(Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0d02));
    let producer_id = Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0d01);
    request.meta = Some(super::PublishMeta {
        version: i32::MIN,
        producer: Some(producer_id),
        previous: Some(i64::MIN),
        sequence: Some(0),
    });
    let unpadded = request.encoded_len().expect("the request measures");
    request.data = serde_json::json!({
        "pad": "x".repeat(event_broker_sdk::EventV1::MAX_ENCODED_BYTES - unpadded),
    });
    assert_eq!(
        request.encoded_len().expect("the request measures"),
        event_broker_sdk::EventV1::MAX_ENCODED_BYTES
    );
    request.check_size(0).expect("an event at the limit passes");

    let payload = serde_json::to_vec(&crate::domain::outbox::IngestPayload {
        topic: topic.clone(),
        key,
        event: crate::domain::model::Event {
            id: request.id,
            r#type: request.r#type,
            topic,
            subject_tenant: request.subject_tenant,
            source: request.source,
            subject: request.subject,
            subject_type: request.subject_type,
            subject_owner: request.subject_owner,
            occurred_at: request.occurred_at,
            trace_parent: request.trace_parent,
            data: request.data,
            meta: request.meta.map(|meta| {
                meta.into_stored(producer_id, super::ProducerMode::Chained)
                    .expect("a chained producer's shape")
            }),
            ingest: Some(crate::domain::model::IngestStamp {
                tenant: Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0d03),
                actor: Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0d04),
            }),
            partition: None,
            sequence: None,
            sequence_time: None,
        },
        stored_wait: true,
    })
    .expect("the row serializes");

    assert_eq!(
        payload.len(),
        event_broker_sdk::EventV1::MAX_ENCODED_BYTES + 3_295
    );
    toolkit_db::outbox::Record::to(crate::domain::outbox::INGEST_QUEUE_NAME, 0)
        .payload(payload, crate::domain::outbox::INGEST_PAYLOAD_TYPE)
        .build()
        .expect("the row fits the outbox");
}

/// An event one byte over the limit is refused, naming its request position.
#[test]
fn an_event_one_byte_over_the_limit_is_refused() {
    let mut request = request(
        Uuid::from_u128(1),
        "order-1",
        serde_json::json!({ "pad": "" }),
    );
    let unpadded = request.encoded_len().expect("the request measures");
    request.data = serde_json::json!({
        "pad": "x".repeat(event_broker_sdk::EventV1::MAX_ENCODED_BYTES + 1 - unpadded),
    });

    let refused = request.check_size(4);

    let Err(DomainError::EventTooLarge { index, bytes, max }) = refused else {
        panic!("an event over the limit must be refused, got {refused:?}");
    };
    assert_eq!((index, bytes, max), (4, 61_441, 61_440));
}
