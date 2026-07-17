use std::borrow::Cow;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use event_broker_sdk::api::EventBroker;
use event_broker_sdk::mock::MockBroker;
use event_broker_sdk::{
    DirectDeduplication, IngestOutcome, Producer, ProducerIdentity, ProducerMode, TypedEvent,
};

const TOPIC: &str = "gts.cf.core.events.topic.v1~example.sdk.producer.orders.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event_type.v1~example.sdk.producer.created.v1";
const SUBJECT_TYPE: &str = "gts.cf.core.events.subject.v1~example.sdk.producer.order.v1";
const TENANT_PARTITION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreated {
    order_id: Uuid,
    total_cents: i64,
}

impl TypedEvent for OrderCreated {
    const TYPE_ID: &'static str = EVENT_TYPE;
    const TOPIC: &'static str = TOPIC;
    const SUBJECT_TYPE: &'static str = SUBJECT_TYPE;
    const SOURCE: &'static str = "order-service";

    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.order_id.to_string())
    }

    fn tenant_id(&self) -> Option<Uuid> {
        Some(test_tenant())
    }
}

#[tokio::test]
async fn stateless_publish_omits_producer_cursor_state() {
    let (broker, handle) = broker().await;
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event_type.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();

    let outcome = producer.publish(order()).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    assert_eq!(handle.stored(TOPIC, TENANT_PARTITION).await.len(), 1);
}

#[tokio::test]
async fn stateless_publish_uses_tenant_when_partition_key_is_absent() {
    let (broker, handle) = broker().await;
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event_type.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();
    let subject_routed = OrderCreated {
        order_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
        total_cents: 10,
    };
    producer.publish(subject_routed).await.unwrap();

    assert_eq!(handle.stored(TOPIC, TENANT_PARTITION).await.len(), 1);
}

#[tokio::test]
async fn register_on_start_chained_mints_id_and_publishes_first_sequence() {
    let (broker, handle) = broker().await;
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::register_on_start(
            ProducerMode::Chained,
        ))
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event_type.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();

    let producer_id = producer.producer_id().expect("producer id is minted");
    let outcome = producer.publish(order()).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Accepted);
    let cursors = broker
        .get_producer_cursors(&toolkit_security::SecurityContext::anonymous(), producer_id)
        .await
        .unwrap();
    assert_eq!(cursors[0].last_sequence, 0);
    assert_eq!(handle.stored(TOPIC, TENANT_PARTITION).await.len(), 1);
}

#[tokio::test]
async fn reuse_monotonic_primes_from_broker_cursor() {
    let (broker, _handle) = broker().await;
    let ctx = toolkit_security::SecurityContext::anonymous();
    let producer_id = broker
        .register_producer(&ctx, ProducerMode::Monotonic, "test/1.0")
        .await
        .unwrap();
    let first = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::reuse(
            ProducerMode::Monotonic,
            producer_id,
        ))
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event_type.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();
    first.publish(order()).await.unwrap();

    let restarted = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::reuse(
            ProducerMode::Monotonic,
            producer_id,
        ))
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event_type.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();
    restarted.publish(order()).await.unwrap();

    let cursors = broker
        .get_producer_cursors(&ctx, producer_id)
        .await
        .unwrap();
    assert_eq!(cursors[0].last_sequence, 1);
}

#[tokio::test]
async fn persisted_publish_returns_persisted() {
    let (broker, _handle) = broker().await;
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event_type.v1~example.sdk.producer.*"])
        .prepare_all()
        .await
        .unwrap();

    let outcome = producer.publish_persisted(order()).await.unwrap();

    assert_eq!(outcome, IngestOutcome::Persisted);
}

#[tokio::test]
async fn batch_publish_routes_through_event_broker() {
    let (broker, handle) = broker().await;
    let producer = Producer::builder()
        .broker(Arc::clone(&broker))
        .security_context(toolkit_security::SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::stateless())
        .topics([TOPIC])
        .event_type_patterns(["gts.cf.core.events.event_type.v1~example.sdk.producer.*"])
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
    assert_eq!(handle.stored(TOPIC, TENANT_PARTITION).await.len(), 2);
}

async fn broker() -> (
    Arc<dyn EventBroker>,
    event_broker_sdk::mock::MockBrokerHandle,
) {
    let mock = Arc::new(MockBroker::new());
    let handle = mock.handle();
    handle.register_topic(TOPIC, 4).await;
    handle
        .register_event_type(
            TOPIC,
            EVENT_TYPE,
            serde_json::json!({
                "type": "object",
                "required": ["order_id", "total_cents"],
                "properties": {
                    "order_id": { "type": "string" },
                    "total_cents": { "type": "integer" }
                }
            }),
            &[SUBJECT_TYPE],
        )
        .await;
    (mock, handle)
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
