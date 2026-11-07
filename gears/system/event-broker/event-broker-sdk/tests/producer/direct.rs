use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use event_broker::test_support::{
    DeliveredOutcome, EventBrokerHarness, FaultInjectingBroker, StaticTypesRegistry,
};
use event_broker_sdk::backend::{Partition, TopicPartition, Wait};
use event_broker_sdk::{
    Ack, DirectProducer, EventBrokerApi, EventBrokerError, EventV1, GtsInstanceId, GtsTypeId,
    IngestOutcome, Sequence, gts_id,
};
use gts::GtsSchema;
use toolkit_security::SecurityContext;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.producer.orders.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.sdk.producer.created.v1~");
const SUBJECT_TYPE: &str = gts_id!("cf.core.events.subject.v1~example.sdk.producer.order.v1~");
const TENANT_PARTITION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreated {
    order_id: Uuid,
    total_cents: i64,
}

impl GtsSchema for OrderCreated {
    const TYPE_ID: &'static str = EVENT_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

/// Number of events the backend has stored on `(TOPIC, partition)`, read back
/// from the real broker.
async fn stored_count(harness: &EventBrokerHarness, partition: u32) -> usize {
    harness
        .backend()
        .read(
            &TopicPartition::new(
                GtsInstanceId::try_new(TOPIC).unwrap(),
                Partition::new(partition),
            ),
            Sequence::NONE,
            1024,
            Wait::No,
        )
        .await
        .expect("read stored events")
        .len()
}

/// Waits for the backend to have durably stored `expected` events on the partition.
/// The real broker admits a publish (`Accepted`) and persists asynchronously through
/// its ingest outbox, so the read-back must poll rather than assert immediately.
async fn wait_for_stored(harness: &EventBrokerHarness, partition: u32, expected: usize) {
    for _ in 0..1000 {
        if stored_count(harness, partition).await >= expected {
            assert_eq!(stored_count(harness, partition).await, expected);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!(
        "expected {expected} events stored on partition {partition}, saw {}",
        stored_count(harness, partition).await
    );
}

/// Every publish advances the producer's cursor on the broker to the sequence
/// it carried.
#[tokio::test]
async fn every_publish_advances_the_producers_cursor() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = producer_over(Arc::clone(&broker)).await;

    let outcome = producer.publish(order(), Ack::Accepted).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    let cursors = broker
        .get_producer_cursors(&SecurityContext::anonymous(), producer.producer_id())
        .await
        .unwrap();
    assert_eq!(cursors.last_sequence(&topic_instance()), Some(0));
    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

/// The event type declares no partition key of its own, so the base's default
/// applies and the event partitions by tenant.
#[tokio::test]
async fn a_publish_partitions_by_tenant_under_the_default_pointer() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = producer_over(Arc::clone(&broker)).await;
    let event = order_event(OrderCreated {
        order_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
        total_cents: 10,
    });
    producer.publish(event, Ack::Accepted).await.unwrap();

    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

#[tokio::test]
async fn a_chained_producer_registers_an_id_and_publishes_the_first_sequence() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = producer_over(Arc::clone(&broker)).await;

    let producer_id = producer.producer_id();
    let outcome = producer.publish(order(), Ack::Accepted).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    let cursors = broker
        .get_producer_cursors(&SecurityContext::anonymous(), producer_id)
        .await
        .unwrap();
    assert_eq!(cursors.last_sequence(&topic_instance()), Some(0));
    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

/// The in-process broker checks a registration's `client_agent` the way the
/// REST surface does: an over-long one is refused before anything is written,
/// so the producer does not build.
#[tokio::test]
async fn in_process_registration_rejects_an_over_long_client_agent() {
    let harness = harness().await;
    let broker = harness.broker();

    let err = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .client_agent("a".repeat(257))
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .build()
        .await
        .unwrap_err();

    // The answer crosses the canonical error, which carries the reason but
    // not the field name or the rule's wording.
    match err {
        EventBrokerError::InvalidTextField {
            field,
            detail,
            reason,
        } => assert_eq!(
            (field, detail.as_str(), reason),
            ("", "Request validation failed", "field_too_long")
        ),
        other => panic!("expected InvalidTextField, got {other:?}"),
    }
}

/// A producer always registers a fresh id at build, so a rebuilt producer
/// starts a chain of its own instead of continuing the first one.
#[tokio::test]
async fn a_rebuilt_producer_registers_a_new_id_and_starts_its_own_chain() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let first = producer_over(Arc::clone(&broker)).await;
    let first_outcome = first.publish(order(), Ack::Accepted).await.unwrap();

    let rebuilt = producer_over(Arc::clone(&broker)).await;
    let rebuilt_outcome = rebuilt.publish(order(), Ack::Accepted).await.unwrap();

    assert_ne!(first.producer_id(), rebuilt.producer_id());
    assert_eq!(
        (first_outcome, rebuilt_outcome),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    for producer_id in [first.producer_id(), rebuilt.producer_id()] {
        let cursors = broker
            .get_producer_cursors(&ctx, producer_id)
            .await
            .unwrap();
        assert_eq!(cursors.last_sequence(&topic_instance()), Some(0));
    }
    wait_for_stored(&harness, TENANT_PARTITION, 2).await;
}

/// A persisted publish is not served until #5655; it admits nothing, and the
/// next publish re-reads the head.
// #5655: the in-process broker serves no persisted publish until synchronous publish lands.
#[tokio::test]
async fn a_persisted_publish_is_unimplemented_until_the_broker_serves_it() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = producer_over(Arc::clone(&broker)).await;

    let persisted = producer
        .publish(
            order(),
            Ack::Persisted {
                within: Duration::from_secs(5),
            },
        )
        .await;
    let next = producer.publish(order(), Ack::Accepted).await.unwrap();

    let Err(EventBrokerError::Unimplemented(detail)) = persisted else {
        panic!("expected `Unimplemented`, got {persisted:?}");
    };
    assert_eq!(
        detail,
        "synchronous publish (Prefer: wait) is not implemented yet"
    );
    assert_eq!(next, IngestOutcome::Accepted);
    let cursors = broker
        .get_producer_cursors(&SecurityContext::anonymous(), producer.producer_id())
        .await
        .unwrap();
    assert_eq!(cursors.last_sequence(&topic_instance()), Some(0));
    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

#[tokio::test]
async fn batch_publish_routes_through_event_broker() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = producer_over(Arc::clone(&broker)).await;

    let outcome = producer
        .publish_batch(vec![order(), order()], Ack::Accepted)
        .await
        .unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    wait_for_stored(&harness, TENANT_PARTITION, 2).await;
}

/// An order whose `Serialize` fails with a message that carries a value, as a
/// gear's custom serializer can.
#[derive(Debug, Deserialize)]
struct PoisonedOrder {
    order_id: Uuid,
}

impl Serialize for PoisonedOrder {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("PLANTED-VALUE-7f3a"))
    }
}

impl GtsSchema for PoisonedOrder {
    const TYPE_ID: &'static str = EVENT_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

#[tokio::test]
async fn a_payload_that_fails_to_serialize_is_refused_without_its_text() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = producer_over(Arc::clone(&broker)).await;

    let poisoned = PoisonedOrder {
        order_id: Uuid::new_v4(),
    };
    let subject = poisoned.order_id.to_string();
    let event = EventV1::builder(poisoned)
        .tenant_id(test_tenant())
        .subject(subject)
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .unwrap();

    let result = producer.publish(event, Ack::Accepted).await;

    let Err(EventBrokerError::Internal(detail)) = result else {
        panic!("expected an internal error, got {result:?}");
    };
    assert_eq!(detail, "serialize event data (data)");
    assert!(!detail.contains("PLANTED-VALUE-7f3a"), "{detail}");
    assert_eq!(stored_count(&harness, TENANT_PARTITION).await, 0);
}

/// The broker appends A but its ack is lost, so the caller sees a transport
/// fault. The next publish re-reads the producer's head from the broker before
/// it is stamped and chains onto A instead of reusing A's numbers.
#[tokio::test]
async fn a_publish_after_a_lost_ack_is_stamped_from_the_brokers_head() {
    let harness = harness().await;
    let broker = FaultInjectingBroker::new(harness.broker());
    let producer = producer_over(Arc::clone(&broker) as Arc<dyn EventBrokerApi>).await;
    let reads_at_build = broker.cursor_reads();
    let producer_id = producer.producer_id();
    broker.lose_publish_acks(1);

    let lost = producer.publish(order(), Ack::Accepted).await;
    let b = producer.publish(order(), Ack::Accepted).await;
    let c = producer.publish(order(), Ack::Accepted).await;

    assert!(
        matches!(lost, Err(EventBrokerError::Transport(_))),
        "the lost ack surfaces as a transport fault, got {lost:?}"
    );
    assert_eq!(
        (b.unwrap(), c.unwrap()),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    let delivered = broker
        .delivered_publishes()
        .into_iter()
        .map(|publish| {
            let meta = publish.meta.expect("a chained publish carries meta");
            (
                (meta.version, meta.producer_id, meta.previous, meta.sequence),
                publish.outcome,
                publish.ack_lost,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        delivered,
        vec![
            (
                (1, Some(producer_id.0), Some(-1), Some(0)),
                DeliveredOutcome::Accepted,
                true
            ),
            (
                (1, Some(producer_id.0), Some(0), Some(1)),
                DeliveredOutcome::Accepted,
                false
            ),
            (
                (1, Some(producer_id.0), Some(1), Some(2)),
                DeliveredOutcome::Accepted,
                false
            ),
        ]
    );
    assert_eq!(
        broker.cursor_reads(),
        reads_at_build + 1,
        "one re-read after the lost ack; c follows a seen outcome"
    );
    let cursors = harness
        .broker()
        .get_producer_cursors(&SecurityContext::anonymous(), producer_id)
        .await
        .unwrap();
    assert_eq!(cursors.last_sequence(&topic_instance()), Some(2));
    wait_for_stored(&harness, TENANT_PARTITION, 3).await;
}

/// The broker appends X but its ack is lost. Re-publishing X resends the
/// numbers X carried, so the broker answers `Duplicate` and stores X once.
#[tokio::test]
async fn a_republished_event_after_a_lost_ack_is_stored_once() {
    let harness = harness().await;
    let broker = FaultInjectingBroker::new(harness.broker());
    let producer = producer_over(Arc::clone(&broker) as Arc<dyn EventBrokerApi>).await;
    let reads_at_build = broker.cursor_reads();
    let producer_id = producer.producer_id();
    broker.lose_publish_acks(1);
    let x = order();
    let x_id = x.id();

    let lost = producer.publish(x.clone(), Ack::Accepted).await;
    let again = producer.publish(x, Ack::Accepted).await;

    assert!(
        matches!(lost, Err(EventBrokerError::Transport(_))),
        "the lost ack surfaces as a transport fault, got {lost:?}"
    );
    assert_eq!(again.unwrap(), IngestOutcome::Duplicate);
    let delivered = broker
        .delivered_publishes()
        .into_iter()
        .map(|publish| {
            let meta = publish.meta.expect("a chained publish carries meta");
            (
                publish.event_id,
                (meta.version, meta.producer_id, meta.previous, meta.sequence),
                publish.outcome,
                publish.ack_lost,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        delivered,
        vec![
            (
                x_id,
                (1, Some(producer_id.0), Some(-1), Some(0)),
                DeliveredOutcome::Accepted,
                true
            ),
            (
                x_id,
                (1, Some(producer_id.0), Some(-1), Some(0)),
                DeliveredOutcome::Duplicate,
                false
            ),
        ]
    );
    assert_eq!(
        broker.cursor_reads(),
        reads_at_build,
        "the re-publish reads no head"
    );
    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
    assert_eq!(stored_count(&harness, TENANT_PARTITION).await, 1);
}

/// A real in-process broker seeded with the orders topic and its event type. The
/// event type declares no `partition_key`, so the base default (`/tenant_id`)
/// applies and events partition by tenant.
async fn harness() -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(serde_json::json!([
            { "id": TOPIC, "partitions": 4 },
            {
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "data_schema": {
                    "type": "object",
                    "required": ["order_id", "total_cents"],
                    "properties": {
                        "order_id": { "type": "string" },
                        "total_cents": { "type": "integer" }
                    }
                },
                "allowed_subject_types": [SUBJECT_TYPE],
            },
        ])))
        .build()
        .await
}

fn topic_instance() -> event_broker_sdk::GtsInstanceId {
    event_broker_sdk::GtsInstanceId::try_new(TOPIC).unwrap()
}

/// A chained direct producer of the orders topic over `broker`.
async fn producer_over(broker: Arc<dyn EventBrokerApi>) -> DirectProducer {
    DirectProducer::builder()
        .broker(broker)
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .build()
        .await
        .unwrap()
}

fn order() -> EventV1<OrderCreated> {
    order_event(OrderCreated {
        order_id: Uuid::new_v4(),
        total_cents: 10,
    })
}

/// `payload` as an event of the test tenant, about its order.
fn order_event(payload: OrderCreated) -> EventV1<OrderCreated> {
    let subject = payload.order_id.to_string();
    EventV1::builder(payload)
        .tenant_id(test_tenant())
        .subject(subject)
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .unwrap()
}

fn test_tenant() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
}
