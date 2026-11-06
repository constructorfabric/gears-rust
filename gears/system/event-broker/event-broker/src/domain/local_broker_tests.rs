//! `LocalBroker` coverage over the gear's real ingest path, through the
//! in-process client the test harness registers.

use chrono::{DateTime, Utc};
use serde_json::json;
use toolkit_gts::{GtsSchema, gts_id, gts_type_schema};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use event_broker_sdk::api::{IngestOutcome, ProducerMode};
use event_broker_sdk::gts::EventV1;
use event_broker_sdk::models::{Event, ProducerMeta};
use event_broker_sdk::{GtsTypeId, ProducerId};

use crate::test_support::{EventBrokerHarness, StaticTypesRegistry};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.eb.local_broker.orders.v1");
const EVENT_TYPE: &str = <CreatedV1 as GtsSchema>::TYPE_ID;
const SUBJECT_TYPE: &str = gts_id!("cf.core.events.subject.v1~example.eb.local_broker.order.v1~");
const TENANT: Uuid = Uuid::from_u128(1);

#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~example.eb.local_broker.created.v1~"),
    description = "An order was created.",
    properties = "",
    traits = serde_json::json!({
        "topic": TOPIC,
        "allowed_subject_types": [SUBJECT_TYPE],
    }),
)]
#[schemars(extend("additionalProperties" = true))]
struct CreatedV1;

async fn harness() -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(
            StaticTypesRegistry::of(json!([
                { "id": TOPIC, "partitions": 1 },
            ]))
            .event_type::<CreatedV1>(),
        )
        .build()
        .await
}

fn chained_event(producer_id: ProducerId, n: u128, previous: i64) -> Event {
    Event {
        id: Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0000 + n),
        type_id: GtsTypeId::new(EVENT_TYPE),
        tenant_id: TENANT,
        source: "order-service".to_owned(),
        subject: format!("order-{n}"),
        subject_type: GtsTypeId::new(SUBJECT_TYPE),
        occurred_at: DateTime::<Utc>::from_timestamp(1_767_225_600, 0).expect("a valid timestamp"),
        trace_parent: None,
        data: Some(json!({ "order": n.to_string() })),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: Some(ProducerMeta {
            version: 1,
            producer_id: Some(producer_id.0),
            previous: Some(previous),
            sequence: Some(previous + 1),
        }),
    }
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
        .publish_batch(&ctx, &batch)
        .await
        .expect("the batch publishes");
    let resent = broker
        .publish_batch(&ctx, &batch)
        .await
        .expect("the resent batch publishes");

    assert_eq!(
        (first, resent),
        (IngestOutcome::Accepted, IngestOutcome::Duplicate)
    );
}
