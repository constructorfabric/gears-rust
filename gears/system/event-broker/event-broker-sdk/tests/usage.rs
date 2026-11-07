//! Executable usage showcase for the event-broker SDK.
//!
//! Each test is a small service story driven against a real in-process broker
//! (`event_broker::test_support::EventBrokerHarness`) rather than a mock, so the
//! wiring reads as guidance and the assertions reflect the broker's real
//! behaviour. The important setup stays visible in the test body.
//!
//! | Area | Shows | Core APIs | Proof |
//! | --- | --- | --- | --- |
//! | Catalog | Register topic and event type | `EventBrokerHarness`, `StaticTypesRegistry` | Topic/type visible through `EventBrokerApi` |
//! | Direct producer | Publish typed event | `DirectProducer`, `EventV1`, `Ack` | Event is stored by the broker |
//! | Persisted/batch producer | Persisted (unimplemented until #5655) and batch send | `Ack::Persisted`, `publish_batch` | Outcomes and stored events match |
//! | Chained producer | Broker-issued producer id | `ProducerMode::Chained` | Broker cursor advances |
//! | Consumer | In-memory offset delivery | `ConsumerBuilder`, `InMemoryOffsetManager` | Handler records event data |
//! | Routing | Topic/type dispatch | consumer routes and default handler | Correct handler receives each event |
//! | End-to-end | Producer -> broker -> consumer | producer and consumer together | Handler sees the produced subject |
//! | Outbox producer | Validate and drain durable enqueue | `OutboxSetup`, `OutboxProducer`, toolkit-db outbox | Enqueued row reaches the broker |
//! | Multi-topic outbox | One queue for multiple topics | `OutboxSetup` topics + outbox queue | Both topics are delivered |

#[cfg(all(feature = "db", feature = "outbox"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker::test_support::{EventBrokerHarness, StaticTypesRegistry};
use event_broker_sdk::backend::{Partition, TopicPartition, Wait};
use event_broker_sdk::{
    Ack, ConsumerBuilder, ConsumerError, ConsumerGroupRef, Delivery, DirectProducer,
    EventBrokerError, EventHandler, EventPayload, EventV1, Fallback, GtsIdPattern, GtsInstanceId,
    GtsTypeId, HandlerOutcome, InMemoryOffsetManager, IngestOutcome, ProducerMode, Sequence,
    SubscriptionInterest, gts_id,
};
#[cfg(all(feature = "db", feature = "outbox"))]
use event_broker_sdk::{OutboxRegistration, OutboxSetup};
use gts::GtsSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::{Instant, sleep};
use toolkit_security::SecurityContext;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.usage.orders.v1");
#[cfg(all(feature = "db", feature = "outbox"))]
const BILLING_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.usage.billing.v1");
const ORDER_CREATED: &str = gts_id!("cf.core.events.event.v1~example.sdk.usage.created.v1~");
const ORDER_UPDATED: &str = gts_id!("cf.core.events.event.v1~example.sdk.usage.updated.v1~");
#[cfg(all(feature = "db", feature = "outbox"))]
const BILLING_CHARGED: &str = gts_id!("cf.core.events.event.v1~example.sdk.usage.charged.v1~");
// A `subject_type` names a *kind* of entity, so it is a GTS type id (trailing
// `~`); the real broker rejects a `subject_type` that is not.
const ORDER_SUBJECT: &str = gts_id!("cf.core.events.subject.v1~example.sdk.usage.order.v1~");
#[cfg(all(feature = "db", feature = "outbox"))]
const BILLING_SUBJECT: &str = gts_id!("cf.core.events.subject.v1~example.sdk.usage.charge.v1~");
#[cfg(all(feature = "db", feature = "outbox"))]
const PRODUCER_QUEUE: &str = "event-broker-producer";

#[cfg(all(feature = "db", feature = "outbox"))]
static USAGE_DB_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreated {
    order_id: Uuid,
    total_cents: i64,
}

impl GtsSchema for OrderCreated {
    const TYPE_ID: &'static str = ORDER_CREATED;

    fn gts_schema_with_refs() -> serde_json::Value {
        order_created_schema()
    }
}

#[cfg(all(feature = "db", feature = "outbox"))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InvalidOrderCreated {
    order_id: Uuid,
    total_cents: String,
}

#[cfg(all(feature = "db", feature = "outbox"))]
impl GtsSchema for InvalidOrderCreated {
    const TYPE_ID: &'static str = ORDER_CREATED;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderUpdated {
    order_id: Uuid,
    status: String,
}

impl GtsSchema for OrderUpdated {
    const TYPE_ID: &'static str = ORDER_UPDATED;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

#[cfg(all(feature = "db", feature = "outbox"))]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BillingCharged {
    charge_id: Uuid,
    total_cents: i64,
}

#[cfg(all(feature = "db", feature = "outbox"))]
impl GtsSchema for BillingCharged {
    const TYPE_ID: &'static str = BILLING_CHARGED;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

struct RecordingHandler {
    subjects: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl EventHandler<serde_json::Value> for RecordingHandler {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.subjects
            .lock()
            .unwrap()
            .push(event.subject().to_owned());
        Ok(HandlerOutcome::Success)
    }
}

struct NamedHandler {
    name: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl EventHandler<serde_json::Value> for NamedHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(self.name);
        Ok(HandlerOutcome::Success)
    }
}

/// A service can set up the broker's catalog before wiring producers or consumers.
///
/// Preconditions: the harness is seeded with the orders topic and its created type.
/// Expected: the registered topic and event type are visible through `EventBrokerApi`.
#[tokio::test]
async fn broker_exposes_registered_order_topic_and_event_type() -> TestResult {
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();

    let topics = broker.list_topics(&ctx).await?;
    let event_type = broker.get_event_type(&ctx, ORDER_CREATED).await?;

    // A topic reports the instance document's own values; an event type's topic
    // binding is a resolved trait, and `data_schema` is the payload contract
    // composed out of the base event's `data` member and this type's narrowing.
    // The public SDK models carry no serde, so the shape is asserted field by
    // field; the JSON wire form is pinned in the rest wire-DTO tests.
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0].id.as_ref(), ORDERS_TOPIC);
    assert_eq!(topics[0].description, "a topic this test publishes to");
    assert_eq!(topics[0].retention, None);

    assert_eq!(event_type.id.as_ref(), ORDER_CREATED);
    assert_eq!(event_type.topic.as_ref(), ORDERS_TOPIC);
    assert_eq!(event_type.partition_key, "/tenant_id");
    assert_eq!(event_type.description, None);
    assert_eq!(
        event_type
            .allowed_subject_types
            .iter()
            .map(GtsIdPattern::pattern)
            .collect::<Vec<_>>(),
        vec![ORDER_SUBJECT]
    );
    // The composed contract is the base event's `data` branch narrowed by this
    // type's schema. The harness registers the base the SDK declares, so the
    // first branch is that base's own `data` member.
    assert_eq!(
        event_type.data_schema,
        json!({
            "allOf": [
                { "type": ["object", "null"] },
                order_created_schema(),
            ],
        })
    );

    Ok(())
}

/// An order service can use the broker and publish a typed event.
///
/// Preconditions: the orders topic and schema are registered before `prepare_all`.
/// Expected: the event is accepted and appears in broker storage.
#[tokio::test]
async fn producer_publish_sends_typed_event() -> TestResult {
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();

    let producer = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .client_agent("order-service/1.0")
        .topics([ORDERS_TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.usage.*"])
        .build()
        .await?;

    let outcome = producer
        .publish(order_event(order_created())?, Ack::Accepted)
        .await?;

    assert_eq!(outcome, IngestOutcome::Accepted);
    wait_for_stored(&harness, ORDERS_TOPIC, 1).await;

    Ok(())
}

/// A persisted publish is refused as unimplemented while the in-process
/// broker serves no synchronous publish, and the event is not stored.
///
/// Preconditions: the producer uses the same direct setup as normal publishing.
/// Expected: `Ack::Persisted` returns `Unimplemented`; the next `Ack::Accepted`
/// publish is stored alone.
#[tokio::test]
async fn a_persisted_publish_is_unimplemented_until_sync_publish_lands() -> TestResult {
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();

    let producer = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([ORDERS_TOPIC])
        .event_type_patterns([ORDER_CREATED])
        .build()
        .await?;

    let persisted = producer
        .publish(
            order_event(order_created())?,
            Ack::Persisted {
                within: Duration::from_secs(5),
            },
        )
        .await;
    let next = producer
        .publish(order_event(order_created())?, Ack::Accepted)
        .await?;

    // #5655: the in-process broker serves no persisted publish until synchronous publish lands.
    let Err(EventBrokerError::Unimplemented(detail)) = persisted else {
        panic!("expected `Unimplemented`, got {persisted:?}");
    };
    assert_eq!(
        detail,
        "synchronous publish (Prefer: wait) is not implemented yet"
    );
    assert_eq!(next, IngestOutcome::Accepted);
    wait_for_stored(&harness, ORDERS_TOPIC, 1).await;
    assert_eq!(stored_count(&harness, ORDERS_TOPIC).await, 1);

    Ok(())
}

/// A producer can publish multiple typed events in one batch call.
///
/// Preconditions: all events satisfy the prepared schema and configured topic.
/// Expected: each event is accepted and stored.
#[tokio::test]
async fn producer_batch_publish_sends_multiple_events() -> TestResult {
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();

    let producer = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([ORDERS_TOPIC])
        .event_type_patterns([ORDER_CREATED])
        .build()
        .await?;

    let outcome = producer
        .publish_batch(
            vec![order_event(order_created())?, order_event(order_created())?],
            Ack::Accepted,
        )
        .await?;

    assert_eq!(outcome, IngestOutcome::Accepted);
    wait_for_stored(&harness, ORDERS_TOPIC, 2).await;

    Ok(())
}

/// Chained mode obtains a broker-issued producer id and advances broker cursor state.
///
/// Preconditions: the producer opts into broker registration at startup.
/// Expected: the producer id comes from Event Broker and publishing advances the cursor.
#[tokio::test]
async fn producer_chained_mode_uses_broker_issued_id_and_sequences_events() -> TestResult {
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();
    let ctx = SecurityContext::anonymous();

    let producer = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(ctx.clone())
        .source("order-service")
        .client_agent("order-service/1.0")
        .mode(ProducerMode::Chained)
        .topics([ORDERS_TOPIC])
        .event_type_patterns([ORDER_CREATED])
        .build()
        .await?;

    let producer_id = producer.producer_id();
    producer
        .publish(order_event(order_created())?, Ack::Accepted)
        .await?;
    wait_for_stored(&harness, ORDERS_TOPIC, 1).await;

    let cursors = broker.get_producer_cursors(&ctx, producer_id).await?;
    assert_eq!(cursors.last_sequence(&ginst(ORDERS_TOPIC)), Some(0));

    Ok(())
}

/// A consumer can read matching events from the broker with in-memory offsets.
///
/// Preconditions: the consumer subscribes to the orders topic and created event type.
/// Expected: the handler records the subject of the produced order.
#[tokio::test]
async fn consumer_with_in_memory_offsets_receives_event() -> TestResult {
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();

    let subjects = Arc::new(Mutex::new(Vec::new()));
    let consumer = ConsumerBuilder::new(Arc::clone(&broker))
        .group(ConsumerGroupRef::auto_anonymous("usage-in-memory"))
        .subscription_interests([SubscriptionInterest::builder()
            .topic(ginst(ORDERS_TOPIC))
            .types([gpat(ORDER_CREATED)])
            .build()?])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(RecordingHandler {
            subjects: Arc::clone(&subjects),
        })
        .start()
        .await?;

    let producer = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([ORDERS_TOPIC])
        .event_type_patterns([ORDER_CREATED])
        .build()
        .await?;

    let order = order_created();
    let subject = order.order_id.to_string();
    producer.publish(order_event(order)?, Ack::Accepted).await?;

    wait_until(|| subjects.lock().unwrap().contains(&subject)).await;
    consumer.stop().await?;

    assert_eq!(subjects.lock().unwrap().as_slice(), [subject]);

    Ok(())
}

/// A consumer can route one event type to a specific handler and let another use default handling.
///
/// Preconditions: the consumer subscribes to the orders topic and registers a route for created events.
/// Expected: created and updated events reach different handlers.
#[tokio::test]
async fn consumer_routes_events_by_topic_and_type() -> TestResult {
    let harness = harness_with(orders_with_updated_catalog()).await;
    let broker = harness.broker();

    let calls = Arc::new(Mutex::new(Vec::new()));
    let consumer = ConsumerBuilder::new(Arc::clone(&broker))
        .group(ConsumerGroupRef::auto_anonymous("usage-routed"))
        .topics([ginst(ORDERS_TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NamedHandler {
            name: "default",
            calls: Arc::clone(&calls),
        })
        .route()
        .topic(ginst(ORDERS_TOPIC))
        .event_type(gpat(ORDER_CREATED))
        .handler(NamedHandler {
            name: "created",
            calls: Arc::clone(&calls),
        })
        .start()
        .await?;

    let producer = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([ORDERS_TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.usage.*"])
        .build()
        .await?;

    let order = order_created();
    let updated = OrderUpdated {
        order_id: order.order_id,
        status: "paid".to_owned(),
    };
    producer.publish(order_event(order)?, Ack::Accepted).await?;
    producer
        .publish(update_event(updated)?, Ack::Accepted)
        .await?;

    wait_until(|| calls.lock().unwrap().len() == 2).await;
    consumer.stop().await?;

    assert_eq!(calls.lock().unwrap().as_slice(), ["created", "default"]);

    Ok(())
}

/// A produced event is visible to a consumer reading from the same broker.
///
/// Preconditions: producer and consumer share the same `Arc<dyn EventBrokerApi>`.
/// Expected: the consumer observes the exact subject produced by the order service.
#[tokio::test]
async fn producer_and_consumer_round_trip_order_created() -> TestResult {
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();

    let received = Arc::new(Mutex::new(Vec::new()));
    let consumer = ConsumerBuilder::new(Arc::clone(&broker))
        .group(ConsumerGroupRef::auto_anonymous("usage-round-trip"))
        .subscription_interests([SubscriptionInterest::builder()
            .topic(ginst(ORDERS_TOPIC))
            .types([gpat(ORDER_CREATED)])
            .build()?])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(RecordingHandler {
            subjects: Arc::clone(&received),
        })
        .start()
        .await?;

    let producer = DirectProducer::builder()
        .broker(Arc::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([ORDERS_TOPIC])
        .event_type_patterns([ORDER_CREATED])
        .build()
        .await?;

    let order = order_created();
    let subject = order.order_id.to_string();
    producer.publish(order_event(order)?, Ack::Accepted).await?;

    wait_until(|| received.lock().unwrap().contains(&subject)).await;
    consumer.stop().await?;

    assert_eq!(received.lock().unwrap().as_slice(), [subject]);

    Ok(())
}

/// An outbox producer rejects invalid payloads before a durable outbox row is written.
///
/// Preconditions: the producer prepared the registered JSON schema before enqueue.
/// Expected: enqueue fails locally and the broker stays empty.
#[cfg(all(feature = "db", feature = "outbox"))]
#[tokio::test]
async fn outbox_producer_validates_before_enqueue() -> TestResult {
    let db = sqlite_db_with_outbox_and_producer_migrations().await?;
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();

    let setup = OutboxSetup::builder()
        .broker(Arc::clone(&broker))
        .db(db.clone())
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::Stateless)
        .topics([ORDERS_TOPIC])
        .event_type_patterns([ORDER_CREATED])
        .queue(PRODUCER_QUEUE, toolkit_db::outbox::Partitions::of(4))
        .client_agent("order-service/1.0")
        .build()
        .await?;

    let producer_handle = setup
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await?;
    let conn = db.conn()?;

    let err = producer_handle
        .producer()
        .enqueue(&conn, invalid_order_event()?)
        .await
        .expect_err("invalid event must fail before enqueue");

    assert!(matches!(err, EventBrokerError::EventDataInvalid { .. }));
    assert_eq!(stored_count(&harness, ORDERS_TOPIC).await, 0);
    producer_handle.stop().await;

    Ok(())
}

/// A running producer outbox processor drains a durable row into the broker.
///
/// Preconditions: toolkit-db owns the outbox lifecycle; the producer binding supplies the queue.
/// Expected: the enqueued order is eventually stored by the broker.
#[cfg(all(feature = "db", feature = "outbox"))]
#[tokio::test]
async fn outbox_producer_drains_queue_to_broker() -> TestResult {
    let db = sqlite_db_with_outbox_and_producer_migrations().await?;
    let harness = harness_with(orders_catalog()).await;
    let broker = harness.broker();

    let setup = OutboxSetup::builder()
        .broker(Arc::clone(&broker))
        .db(db.clone())
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::Stateless)
        .topics([ORDERS_TOPIC])
        .event_type_patterns([ORDER_CREATED])
        .queue(PRODUCER_QUEUE, toolkit_db::outbox::Partitions::of(4))
        .client_agent("order-service/1.0")
        .build()
        .await?;

    let producer_handle = setup
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await?;
    let conn = db.conn()?;

    producer_handle
        .producer()
        .enqueue(&conn, order_event(order_created())?)
        .await?
        .fire();

    wait_for_stored(&harness, ORDERS_TOPIC, 1).await;
    producer_handle.stop().await;

    Ok(())
}

/// One producer outbox queue can carry all topics configured on the `OutboxSetup`.
///
/// Preconditions: orders and billing topics are both configured on the producer.
/// Expected: both enqueued events are delivered under their own topics.
#[cfg(all(feature = "db", feature = "outbox"))]
#[tokio::test]
async fn single_outbox_queue_can_carry_multiple_topics() -> TestResult {
    let db = sqlite_db_with_outbox_and_producer_migrations().await?;
    let harness = harness_with(orders_and_billing_catalog()).await;
    let broker = harness.broker();

    let setup = OutboxSetup::builder()
        .broker(Arc::clone(&broker))
        .db(db.clone())
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::Stateless)
        .topics([ORDERS_TOPIC, BILLING_TOPIC])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.usage.*"])
        .queue(PRODUCER_QUEUE, toolkit_db::outbox::Partitions::of(4))
        .build()
        .await?;

    let producer_handle = setup
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await?;
    let conn = db.conn()?;

    producer_handle
        .producer()
        .enqueue(&conn, order_event(order_created())?)
        .await?
        .fire();
    producer_handle
        .producer()
        .enqueue(&conn, charge_event(billing_charged())?)
        .await?
        .fire();

    wait_for_stored(&harness, ORDERS_TOPIC, 1).await;
    wait_for_stored(&harness, BILLING_TOPIC, 1).await;
    producer_handle.stop().await;

    Ok(())
}

// --- harness helpers ---------------------------------------------------------

/// Partition count every fixture topic is configured with.
const ORDERS_PARTITIONS: u32 = 4;

fn ginst(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("valid GTS instance id")
}

fn gpat(id: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(id).expect("valid GTS pattern")
}

async fn harness_with(catalog: StaticTypesRegistry) -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(catalog)
        .build()
        .await
}

fn orders_catalog() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": ORDERS_TOPIC, "partitions": ORDERS_PARTITIONS },
        {
            "id": ORDER_CREATED,
            "topic": ORDERS_TOPIC,
            "data_schema": order_created_schema(),
            "allowed_subject_types": [ORDER_SUBJECT],
        },
    ]))
}

fn orders_with_updated_catalog() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": ORDERS_TOPIC, "partitions": ORDERS_PARTITIONS },
        {
            "id": ORDER_CREATED,
            "topic": ORDERS_TOPIC,
            "data_schema": order_created_schema(),
            "allowed_subject_types": [ORDER_SUBJECT],
        },
        {
            "id": ORDER_UPDATED,
            "topic": ORDERS_TOPIC,
            "data_schema": order_updated_schema(),
            "allowed_subject_types": [ORDER_SUBJECT],
        },
    ]))
}

#[cfg(all(feature = "db", feature = "outbox"))]
fn orders_and_billing_catalog() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": ORDERS_TOPIC, "partitions": ORDERS_PARTITIONS },
        {
            "id": ORDER_CREATED,
            "topic": ORDERS_TOPIC,
            "data_schema": order_created_schema(),
            "allowed_subject_types": [ORDER_SUBJECT],
        },
        { "id": BILLING_TOPIC, "partitions": ORDERS_PARTITIONS },
        {
            "id": BILLING_CHARGED,
            "topic": BILLING_TOPIC,
            "data_schema": billing_charged_schema(),
            "allowed_subject_types": [BILLING_SUBJECT],
        },
    ]))
}

/// Events published by these tests partition by `/tenant_id` and so all land on
/// the one partition the publishing tenant hashes to; counting the whole topic
/// means a test never has to know which. `read` is exclusive of the position it
/// names, and `Sequence::NONE` reads from the start of each partition.
async fn stored_count(harness: &EventBrokerHarness, topic: &str) -> usize {
    let mut total = 0;
    for partition in 0..ORDERS_PARTITIONS {
        let at = TopicPartition::new(
            GtsInstanceId::try_new(topic).unwrap(),
            Partition::new(partition),
        );
        total += harness
            .backend()
            .read(&at, Sequence::NONE, 1024, Wait::No)
            .await
            .expect("read stored events")
            .len();
    }
    total
}

async fn wait_for_stored(harness: &EventBrokerHarness, topic: &str, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if stored_count(harness, topic).await >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} stored events on {topic}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

fn order_created_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["order_id", "total_cents"],
        "properties": {
            "order_id": { "type": "string" },
            "total_cents": { "type": "integer" }
        }
    })
}

fn order_updated_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["order_id", "status"],
        "properties": {
            "order_id": { "type": "string" },
            "status": { "type": "string" }
        }
    })
}

#[cfg(all(feature = "db", feature = "outbox"))]
fn billing_charged_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["charge_id", "total_cents"],
        "properties": {
            "charge_id": { "type": "string" },
            "total_cents": { "type": "integer" }
        }
    })
}

fn order_event(order: OrderCreated) -> Result<EventV1<OrderCreated>, EventBrokerError> {
    about_order(order.order_id, order)
}

fn update_event(update: OrderUpdated) -> Result<EventV1<OrderUpdated>, EventBrokerError> {
    about_order(update.order_id, update)
}

/// The charge as an event about itself, in the anonymous caller's tenant.
#[cfg(all(feature = "db", feature = "outbox"))]
fn charge_event(charge: BillingCharged) -> Result<EventV1<BillingCharged>, EventBrokerError> {
    EventV1::builder(charge.clone())
        .tenant_id(SecurityContext::anonymous().subject_tenant_id())
        .subject(charge.charge_id.to_string())
        .subject_type_id(GtsTypeId::new(BILLING_SUBJECT))
        .build()
}

/// `payload` as an event about order `order_id`, in the anonymous caller's tenant.
fn about_order<P: EventPayload>(
    order_id: Uuid,
    payload: P,
) -> Result<EventV1<P>, EventBrokerError> {
    EventV1::builder(payload)
        .tenant_id(SecurityContext::anonymous().subject_tenant_id())
        .subject(order_id.to_string())
        .subject_type_id(GtsTypeId::new(ORDER_SUBJECT))
        .build()
}

fn order_created() -> OrderCreated {
    OrderCreated {
        order_id: Uuid::new_v4(),
        total_cents: 1200,
    }
}

/// An order event whose `total_cents` breaks the registered schema.
#[cfg(all(feature = "db", feature = "outbox"))]
fn invalid_order_event() -> Result<EventV1<InvalidOrderCreated>, EventBrokerError> {
    let order_id = Uuid::new_v4();
    about_order(
        order_id,
        InvalidOrderCreated {
            order_id,
            total_cents: "not-an-integer".to_owned(),
        },
    )
}

#[cfg(all(feature = "db", feature = "outbox"))]
fn billing_charged() -> BillingCharged {
    BillingCharged {
        charge_id: Uuid::new_v4(),
        total_cents: 1200,
    }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if predicate() {
            return;
        }
        assert!(Instant::now() < deadline, "condition was not observed");
        sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(all(feature = "db", feature = "outbox"))]
async fn sqlite_db_with_outbox_and_producer_migrations() -> TestResult<toolkit_db::Db> {
    let seq = USAGE_DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dsn = format!("sqlite:file:evbk_usage_showcase_{seq}?mode=memory&cache=shared");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await?;
    let mut migrations = toolkit_db::outbox::outbox_migrations();
    migrations.extend(event_broker_sdk::producer_registration_migrations());
    toolkit_db::migration_runner::run_migrations_for_testing(&db, migrations).await?;
    Ok(db)
}
