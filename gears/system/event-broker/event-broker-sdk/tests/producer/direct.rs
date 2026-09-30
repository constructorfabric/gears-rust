use std::borrow::Cow;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use event_broker::test_support::{EventBrokerHarness, StaticTypesRegistry};
use event_broker_sdk::{
    DirectDeduplication, IngestOutcome, Producer, ProducerIdentity, ProducerMode, Sequence,
    TypedEvent, gts_id,
};
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

impl TypedEvent for OrderCreated {
    const TYPE_ID: &'static str = EVENT_TYPE;
    const SUBJECT_TYPE: &'static str = SUBJECT_TYPE;
    const SOURCE: &'static str = "order-service";

    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.order_id.to_string())
    }

    fn tenant_id(&self) -> Option<Uuid> {
        Some(test_tenant())
    }
}

/// Number of events the backend has stored on `(TOPIC, partition)`, read back
/// from the real broker.
async fn stored_count(harness: &EventBrokerHarness, partition: u32) -> usize {
    harness
        .backend()
        .read(
            &SecurityContext::anonymous(),
            TOPIC,
            partition,
            Sequence::NONE,
            1024,
        )
        .await
        .expect("read stored events")
        .len()
}

/// Waits for the backend to have durably stored `expected` events on the partition.
/// The real broker admits a publish (`Accepted`) and persists asynchronously through
/// its ingest outbox, so the read-back must poll rather than assert immediately.
async fn wait_for_stored(harness: &EventBrokerHarness, partition: u32, expected: usize) {
    for _ in 0..200 {
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

#[tokio::test]
async fn stateless_publish_omits_producer_cursor_state() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();

    let outcome = producer.publish(order()).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

/// The event type declares no partition key of its own, so the base's default
/// applies and the event partitions by tenant.
#[tokio::test]
async fn stateless_publish_partitions_by_tenant_under_the_default_pointer() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();
    let event = OrderCreated {
        order_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
        total_cents: 10,
    };
    producer.publish(event).await.unwrap();

    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

#[tokio::test]
async fn register_on_start_chained_mints_id_and_publishes_first_sequence() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::register_on_start(
            ProducerMode::Chained,
        ))
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();

    let producer_id = producer.producer_id().expect("producer id is minted");
    let outcome = producer.publish(order()).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    let cursors = broker
        .get_producer_cursors(&SecurityContext::anonymous(), producer_id)
        .await
        .unwrap();
    assert_eq!(
        cursors.last_sequence(&topic_instance(), TENANT_PARTITION),
        Some(0)
    );
    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

#[tokio::test]
async fn reuse_monotonic_primes_from_broker_cursor() {
    let harness = harness().await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();
    let producer_id = broker
        .register_producer(&ctx, ProducerMode::Monotonic, "test/1.0")
        .await
        .unwrap();
    let first = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::reuse(
            ProducerMode::Monotonic,
            producer_id,
        ))
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();
    first.publish(order()).await.unwrap();

    let restarted = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::reuse(
            ProducerMode::Monotonic,
            producer_id,
        ))
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();
    restarted.publish(order()).await.unwrap();

    let cursors = broker
        .get_producer_cursors(&ctx, producer_id)
        .await
        .unwrap();
    assert_eq!(
        cursors.last_sequence(&topic_instance(), TENANT_PARTITION),
        Some(1)
    );
}

/// `publish_persisted` asks for a durable-write confirmation, but the in-process
/// broker enqueues to its async ingest outbox and exposes no persist-confirm hook
/// (see `local_broker.rs`: the REST layer answers `501` for `Prefer: wait`), so
/// the honest outcome is `Accepted`, not `Persisted`. The mock reported `Persisted`
/// unconditionally; the real broker does not assert a write it has not confirmed.
#[tokio::test]
async fn persisted_publish_is_accepted_without_a_persist_confirm_hook() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();

    let outcome = producer.publish_persisted(order()).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    wait_for_stored(&harness, TENANT_PARTITION, 1).await;
}

#[tokio::test]
async fn batch_publish_routes_through_event_broker() {
    let harness = harness().await;
    let broker = harness.broker();
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();

    let outcomes = producer
        .publish_batch(vec![order(), order()])
        .await
        .unwrap();

    assert_eq!(
        outcomes,
        vec![IngestOutcome::Accepted, IngestOutcome::Accepted]
    );
    wait_for_stored(&harness, TENANT_PARTITION, 2).await;
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

fn order() -> OrderCreated {
    OrderCreated {
        order_id: Uuid::new_v4(),
        total_cents: 10,
    }
}

fn test_tenant() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
}
