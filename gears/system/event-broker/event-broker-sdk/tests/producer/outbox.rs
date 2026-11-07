use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use event_broker::test_support::{
    DeliveredOutcome, EventBrokerHarness, FaultInjectingBroker, StaticTypesRegistry, TransientFault,
};
use event_broker_sdk::api::{Ack, EventBrokerApi};
use event_broker_sdk::backend::{Partition, TopicPartition, Wait};
use event_broker_sdk::error::EventBrokerError;
use event_broker_sdk::gts::EventMeta;
use event_broker_sdk::models::ResetScope;
use event_broker_sdk::producer::IngestOutcome;
use event_broker_sdk::producer::TEST_QUEUE;
use event_broker_sdk::{
    EventPayload, EventV1, GtsInstanceId, GtsTypeId, OutboxProducer, OutboxRegistration,
    OutboxSetup, ProducerId, ProducerMode, Sequence,
};
use gts::GtsSchema;
use toolkit_db::outbox::{Outbox, Partitions, Wake};

const QUEUE: &str = "event-broker-producer";
/// Partition count `fixture` registers its topics with.
const FIXTURE_BROKER_PARTITIONS: u32 = 4;

const TOPIC: &str = "gts.cf.core.events.topic.v1~example.sdk.outbox.orders.v1";
const TOPIC2: &str = "gts.cf.core.events.topic.v1~example.sdk.outbox.billing.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~example.sdk.outbox.created.v1~";
const EVENT_TYPE2: &str = "gts.cf.core.events.event.v1~example.sdk.outbox.charged.v1~";
/// A third event type on `TOPIC` whose declared `partition_key` points into the
/// payload (`/data/partition_key`) rather than at `/tenant_id`. The real broker
/// fixes a type's partition key at registration, so steering the partition by
/// payload is a *separate* catalog type - not a runtime repoint of `EVENT_TYPE`.
const EVENT_TYPE_KEYED: &str = "gts.cf.core.events.event.v1~example.sdk.outbox.keyed.v1~";
const SUBJECT_TYPE: &str = "gts.cf.core.events.subject.v1~example.sdk.outbox.order.v1~";
/// The payload member `EVENT_TYPE_KEYED` points its partition key at. Reaching
/// inside `data` is the interesting case: a bare field name could not.
const PAYLOAD_POINTER: &str = "/data/partition_key";

static DB_SEQ: AtomicU64 = AtomicU64::new(1);

/// The orders payload of [`order`], with a fresh order id per event.
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

/// Same payload shape as [`OrderCreated`] plus `partition_key`, but a distinct
/// event type (`EVENT_TYPE_KEYED`) whose catalog `partition_key` is
/// `PAYLOAD_POINTER`, so its broker partition is driven by
/// `data.partition_key` rather than the tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyedOrderCreated {
    order_id: Uuid,
    total_cents: i64,
    partition_key: String,
}

impl GtsSchema for KeyedOrderCreated {
    const TYPE_ID: &'static str = EVENT_TYPE_KEYED;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BillingCharged {
    charge_id: Uuid,
    total_cents: i64,
}

impl GtsSchema for BillingCharged {
    const TYPE_ID: &'static str = EVENT_TYPE2;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

struct NoopProcessor;

#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for NoopProcessor {
    async fn handle(
        &self,
        _msg: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        toolkit_db::outbox::MessageResult::Ok
    }
}

#[tokio::test]
async fn managed_client_agent_drift_fails_loudly() {
    let (db, broker, _harness) = fixture().await;
    let first = managed_producer(db.clone(), Arc::clone(&broker)).await;
    first
        .partition_producer_for_test(TEST_QUEUE, 0)
        .await
        .unwrap();

    let result = managed_setup(db, broker, "order-service/2.0").await;
    let err = match result {
        Ok(_) => panic!("managed client-agent drift must fail"),
        Err(err) => err,
    };

    assert!(matches!(
        err,
        EventBrokerError::InvalidProducerOptions { .. }
    ));
}

#[tokio::test]
async fn managed_registration_requires_producer_migrations() {
    let (_, broker, _harness) = fixture().await;
    let db = db_without_producer_migrations().await;

    let result = managed_setup(db, broker, "order-service/1.0").await;
    let err = match result {
        Ok(_) => panic!("managed registration must require producer migrations"),
        Err(err) => err,
    };

    assert!(
        err.to_string().contains("producer"),
        "unexpected migration error: {err}"
    );
}

#[tokio::test]
async fn an_interim_row_written_like_products_drains_through_the_processor() {
    let (db, broker, harness) = fixture().await;
    let faulty = FaultInjectingBroker::new(Arc::clone(&broker));
    let producer = OutboxSetup::builder()
        .broker(faulty.clone() as Arc<dyn EventBrokerApi>)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::managed(
            ProducerMode::Monotonic,
            "order-service",
        ))
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .queue(QUEUE, Partitions::of(4))
        .client_agent("order-service")
        .build()
        .await
        .unwrap();
    let event_id = Uuid::parse_str("0190a5b2-7c3d-7e4f-8a1b-2c3d4e5f6a7b").unwrap();
    let order_id = Uuid::parse_str("0190a5b2-7c3d-7e4f-8a1b-000000000042").unwrap();
    // The interim envelope bss products hand-writes in `enqueue_typed`
    // (`gears/bss/products/products/src/infra/events.rs`); an SDK envelope
    // change must keep this row draining.
    let payload = serde_json::json!({
        "version": 1,
        "event_id": event_id,
        "type": EVENT_TYPE,
        "topic": TOPIC,
        "tenant_id": test_tenant(),
        "source": "order-service",
        "subject": order_id.to_string(),
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-11-03T10:15:30.123456789Z",
        "trace_parent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "data": {"order_id": order_id, "total_cents": 1250},
        "producer_mode": "stateless",
        "diagnostic_metadata": {"sdk_client_agent": "order-service"}
    });

    let result = producer
        .process_outbox_payload_for_test(serde_json::to_vec(&payload).unwrap(), 1)
        .await;

    assert!(
        matches!(result, toolkit_db::outbox::MessageResult::Ok),
        "expected Ok, got {result:?}"
    );
    assert_eq!(producer.registrations_for_test().await.unwrap(), vec![]);
    let delivered = faulty.delivered_publishes();
    let [publish] = delivered.as_slice() else {
        panic!("expected one delivered publish, got {delivered:?}");
    };
    assert_eq!(
        (
            publish.event_id,
            publish.subject.as_str(),
            publish.meta.is_none(),
            publish.outcome.clone(),
            publish.ack_lost
        ),
        (
            event_id,
            order_id.to_string().as_str(),
            true,
            DeliveredOutcome::Accepted,
            false
        )
    );
    wait_for_stored(&harness, TOPIC, 1).await;
    let mut stored = Vec::new();
    for partition in 0..FIXTURE_BROKER_PARTITIONS {
        let at = TopicPartition::new(
            GtsInstanceId::try_new(TOPIC).unwrap(),
            Partition::new(partition),
        );
        stored.extend(
            harness
                .backend()
                .read(&at, Sequence::NONE, 16, Wait::No)
                .await
                .unwrap(),
        );
    }
    let [event] = stored.as_slice() else {
        panic!("expected one stored event, got {stored:?}");
    };
    assert_eq!(
        (
            event.id,
            event.type_id.as_ref(),
            event.tenant_id,
            event.source.as_str(),
            event.subject.clone(),
            event.subject_type.as_ref(),
            event
                .occurred_at
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            event.trace_parent.as_deref(),
            event.data.clone(),
            event.partition,
            event.sequence,
            (event.sequence_time.is_some(), event.meta.is_none()),
        ),
        (
            event_id,
            EVENT_TYPE,
            test_tenant(),
            "order-service",
            order_id.to_string(),
            SUBJECT_TYPE,
            "2026-11-03T10:15:30.123456789Z".to_owned(),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            Some(serde_json::json!({"order_id": order_id, "total_cents": 1250})),
            Some(2),
            Some(Sequence::assigned(1)),
            (true, true),
        )
    );
}

#[tokio::test]
async fn an_interim_row_written_like_pricing_drains_through_the_processor() {
    let (db, broker, harness) = fixture().await;
    let faulty = FaultInjectingBroker::new(Arc::clone(&broker));
    let producer = OutboxSetup::builder()
        .broker(faulty.clone() as Arc<dyn EventBrokerApi>)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("bss-pricing")
        .registration(OutboxRegistration::managed(
            ProducerMode::Monotonic,
            "bss-pricing",
        ))
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .queue(QUEUE, Partitions::of(4))
        .client_agent("bss-pricing")
        .build()
        .await
        .unwrap();
    let event_id = Uuid::parse_str("0190a5b2-7c3d-7e4f-8a1b-2c3d4e5f6a7c").unwrap();
    let order_id = Uuid::parse_str("0190a5b2-7c3d-7e4f-8a1b-000000000043").unwrap();
    // The interim envelope bss pricing hand-writes in `interim_envelope`
    // (`gears/bss/pricing/pricing/src/infra/events.rs`); an SDK envelope
    // change must keep this row draining.
    let payload = serde_json::json!({
        "version": 1,
        "event_id": event_id,
        "type": EVENT_TYPE,
        "topic": TOPIC,
        "tenant_id": test_tenant(),
        "source": "bss-pricing",
        "subject": order_id.to_string(),
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-11-03T10:15:30.123456789Z",
        "trace_parent": null,
        "data": {"order_id": order_id, "total_cents": 1250},
        "producer_mode": "stateless",
        "diagnostic_metadata": {"sdk_client_agent": "bss-pricing"},
    });

    let result = producer
        .process_outbox_payload_for_test(serde_json::to_vec(&payload).unwrap(), 1)
        .await;

    assert!(
        matches!(result, toolkit_db::outbox::MessageResult::Ok),
        "expected Ok, got {result:?}"
    );
    assert_eq!(producer.registrations_for_test().await.unwrap(), vec![]);
    let delivered = faulty.delivered_publishes();
    let [publish] = delivered.as_slice() else {
        panic!("expected one delivered publish, got {delivered:?}");
    };
    assert_eq!(
        (
            publish.event_id,
            publish.subject.as_str(),
            publish.meta.is_none(),
            publish.outcome.clone(),
            publish.ack_lost
        ),
        (
            event_id,
            order_id.to_string().as_str(),
            true,
            DeliveredOutcome::Accepted,
            false
        )
    );
    wait_for_stored(&harness, TOPIC, 1).await;
    let mut stored = Vec::new();
    for partition in 0..FIXTURE_BROKER_PARTITIONS {
        let at = TopicPartition::new(
            GtsInstanceId::try_new(TOPIC).unwrap(),
            Partition::new(partition),
        );
        stored.extend(
            harness
                .backend()
                .read(&at, Sequence::NONE, 16, Wait::No)
                .await
                .unwrap(),
        );
    }
    let [event] = stored.as_slice() else {
        panic!("expected one stored event, got {stored:?}");
    };
    assert_eq!(
        (
            event.id,
            event.type_id.as_ref(),
            event.tenant_id,
            event.source.as_str(),
            event.subject.clone(),
            event.subject_type.as_ref(),
            event
                .occurred_at
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            event.trace_parent.as_deref(),
            event.data.clone(),
            event.partition,
            event.sequence,
            (event.sequence_time.is_some(), event.meta.is_none()),
        ),
        (
            event_id,
            EVENT_TYPE,
            test_tenant(),
            "bss-pricing",
            order_id.to_string(),
            SUBJECT_TYPE,
            "2026-11-03T10:15:30.123456789Z".to_owned(),
            None,
            Some(serde_json::json!({"order_id": order_id, "total_cents": 1250})),
            Some(2),
            Some(Sequence::assigned(1)),
            (true, true),
        )
    );
}

/// A managed producer registers nothing until a partition first drains; that
/// drain registers the partition's producer id and publishes under it.
#[tokio::test]
async fn a_partition_id_is_registered_on_its_first_drain() {
    let (db, broker, harness) = fixture().await;
    let producer = managed_producer(db, broker).await;
    assert_eq!(producer.registrations_for_test().await.unwrap(), vec![]);

    let result = producer
        .process_outbox_payload_for_test(managed_envelope_payload(&producer).await, 1)
        .await;

    assert_message_ok(result);
    let registrations = producer.registrations_for_test().await.unwrap();
    let id = registrations[0].2;
    assert_eq!(registrations, vec![(TEST_QUEUE.to_owned(), 0, id, 1)]);
    wait_for_stored(&harness, TOPIC, 1).await;
}

/// The broker forgets a partition's producer id (its registration is reaped):
/// the drain that sees `404 ProducerNotFound` registers a replacement for that
/// partition (generation 2) and asks for a retry, and the retry publishes under
/// it. Nothing is rejected. Drives the real gear: `harness.forget_producer`
/// deletes the `event_broker_producer` row.
#[tokio::test]
async fn an_unknown_producer_id_is_replaced_and_the_message_retried() {
    let (db, broker, harness) = fixture().await;
    let producer = managed_producer(db, broker).await;
    assert_message_ok(
        producer
            .process_outbox_payload_for_test(managed_envelope_payload(&producer).await, 1)
            .await,
    );
    let forgotten = producer.registrations_for_test().await.unwrap()[0].2;
    harness.forget_producer(forgotten.0).await;

    let envelope = managed_envelope_payload(&producer).await;
    let first = producer
        .process_outbox_payload_for_test(envelope.clone(), 2)
        .await;
    let registrations = producer.registrations_for_test().await.unwrap();
    let replacement = registrations[0].2;
    let retried = producer.process_outbox_payload_for_test(envelope, 2).await;

    assert_message_retry(first);
    assert_ne!(replacement, forgotten);
    assert_eq!(
        registrations,
        vec![(TEST_QUEUE.to_owned(), 0, replacement, 2)]
    );
    assert_message_ok(retried);
    wait_for_stored(&harness, TOPIC, 2).await;
}

/// The event type decides which member is the partition key, so a tenant-keyed
/// type and a payload-keyed type place the same publish call in different
/// producer outbox partitions - and a producer cannot change the pointer per
/// message. Placement is `murmur3("{topic}:{key}") % 4`.
#[tokio::test]
async fn the_event_types_declared_pointer_drives_the_outbox_partition() {
    let (db, broker, _harness) = fixture().await;
    let by_tenant = stateless_setup(db.clone(), Arc::clone(&broker)).await;

    let (tenant_partition, tenant_envelope) =
        by_tenant.outbox_envelope_for_test(order()).await.unwrap();
    assert_eq!(tenant_partition, 3);
    assert!(tenant_envelope.get("broker_partition").is_none());

    let by_payload = keyed_producer(db, broker).await;
    let (keyed_partition, _) = by_payload
        .outbox_envelope_for_test(keyed_order("payload-key"))
        .await
        .unwrap();
    assert_eq!(keyed_partition, 0);
}

#[tokio::test]
async fn same_topic_partition_maps_to_same_outbox_partition() {
    let (db, broker, _harness) = fixture().await;
    let producer = OutboxSetup::builder()
        .broker(broker)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::Stateless)
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .queue(QUEUE, Partitions::of(8))
        .build()
        .await
        .unwrap();

    let (left, _) = producer.outbox_envelope_for_test(order()).await.unwrap();
    let (right, _) = producer.outbox_envelope_for_test(order()).await.unwrap();

    assert_eq!(left, right);
}

/// The same recovery through a running outbox: after the broker forgets the
/// partition's id, the next event drained from that partition is published
/// under a replacement, generation 2. `order()` keys by the test tenant,
/// which places it in outbox partition 3 of 4 - partition row id 4 in a fresh
/// database, the id toolkit-db delivers messages under.
#[tokio::test]
async fn outbox_processor_replaces_an_unknown_producer_while_draining() {
    let (db, broker, harness) = fixture().await;
    let producer_handle = managed_producer(db.clone(), Arc::clone(&broker))
        .await
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await
        .unwrap();
    // The started setup is consumed; a second setup of the same key reads the
    // stored registrations.
    let producer = managed_producer(db.clone(), broker).await;
    let conn = db.conn().unwrap();

    producer_handle
        .producer()
        .enqueue(&conn, order())
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;
    let forgotten = producer.registrations_for_test().await.unwrap()[0].2;
    harness.forget_producer(forgotten.0).await;
    producer_handle
        .producer()
        .enqueue(&conn, order())
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 2).await;

    let registrations = producer.registrations_for_test().await.unwrap();
    let replacement = registrations[0].2;
    assert_ne!(replacement, forgotten);
    assert_eq!(registrations, vec![(QUEUE.to_owned(), 4, replacement, 2)]);
    producer_handle.stop().await;
}

#[tokio::test]
async fn one_queue_can_carry_multiple_topics() {
    let (db, broker, _harness) = fixture().await;
    let producer = OutboxSetup::builder()
        .broker(broker)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::Stateless)
        .topics([TOPIC, TOPIC2])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.outbox.*"])
        .queue(QUEUE, Partitions::of(4))
        .build()
        .await
        .unwrap();

    let (orders_partition, orders) = producer.outbox_envelope_for_test(order()).await.unwrap();
    let charge_id = Uuid::new_v4();
    let (billing_partition, billing) = producer
        .outbox_envelope_for_test(
            EventV1::builder(BillingCharged {
                charge_id,
                total_cents: 30,
            })
            .tenant_id(test_tenant())
            .subject(charge_id.to_string())
            .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
            .build()
            .unwrap(),
        )
        .await
        .unwrap();

    assert!(orders_partition < 4);
    assert!(billing_partition < 4);
    assert_eq!(orders["topic"], TOPIC);
    assert_eq!(billing["topic"], TOPIC2);
}

/// The envelope records what was enqueued, not who publishes it: the producer
/// id and the chain fields are stamped when the message drains, under the id
/// of the partition it drains from.
#[tokio::test]
async fn managed_envelope_carries_no_producer_id_and_no_chain_fields() {
    let (db, broker, _harness) = fixture().await;
    let producer = managed_producer(db, broker).await;

    let (_, json) = producer.outbox_envelope_for_test(order()).await.unwrap();

    assert_eq!(json["version"], 1);
    assert_eq!(json["type"], EVENT_TYPE);
    assert_eq!(json["producer_mode"], "chained");
    assert!(json.get("producer_id").is_none());
    assert!(json.get("generation").is_none());
    assert!(json.get("previous").is_none());
    assert!(json.get("sequence").is_none());
    assert_eq!(
        json["diagnostic_metadata"]["sdk_client_agent"],
        "order-service/1.0"
    );
}

#[tokio::test]
async fn rotation_replaces_every_partition_id() {
    let (db, broker, _harness) = fixture().await;
    let producer = managed_producer(db.clone(), Arc::clone(&broker)).await;
    let zero = producer
        .partition_producer_for_test(TEST_QUEUE, 0)
        .await
        .unwrap();
    let one = producer
        .partition_producer_for_test(TEST_QUEUE, 1)
        .await
        .unwrap();
    let producer_handle = producer
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await
        .unwrap();

    producer_handle
        .producer()
        .rotate_registration()
        .await
        .unwrap();
    producer_handle.stop().await;

    // The started setup is consumed; a second setup of the same key reads the
    // stored registrations.
    let registrations = managed_producer(db, broker)
        .await
        .registrations_for_test()
        .await
        .unwrap();
    let (new_zero, new_one) = (registrations[0].2, registrations[1].2);
    assert_ne!(new_zero, zero);
    assert_ne!(new_one, one);
    assert_eq!(
        registrations,
        vec![
            (TEST_QUEUE.to_owned(), 0, new_zero, 2),
            (TEST_QUEUE.to_owned(), 1, new_one, 2),
        ]
    );
}

#[tokio::test]
async fn reset_chain_clears_the_chain_and_keeps_the_id() {
    let (db, broker, _harness) = fixture().await;
    let producer = managed_producer(db.clone(), Arc::clone(&broker)).await;
    assert_message_ok(
        producer
            .process_outbox_payload_for_test(managed_envelope_payload(&producer).await, 1)
            .await,
    );
    let id = producer.registrations_for_test().await.unwrap()[0].2;
    let producer_handle = producer
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await
        .unwrap();

    producer_handle
        .producer()
        .reset_chain(ResetScope::AllTopics)
        .await
        .unwrap();
    producer_handle.stop().await;
    // The started setup is consumed; a second setup of the same key reads the
    // stored registrations.
    let producer = managed_producer(db, Arc::clone(&broker)).await;

    let cursors = broker
        .get_producer_cursors(&toolkit_security::SecurityContext::anonymous(), id)
        .await
        .unwrap();
    assert!(cursors.is_empty(), "the chain must be cleared: {cursors:?}");
    assert_eq!(
        producer.registrations_for_test().await.unwrap(),
        vec![(TEST_QUEUE.to_owned(), 0, id, 1)]
    );
}

#[tokio::test]
async fn service_owned_lifecycle_registers_extra_queue_and_binds_producer_outbox() {
    let (db, broker, _harness) = fixture().await;
    let (queue_builder, binder) = stateless_setup(db.clone(), broker)
        .await
        .register(toolkit_db::outbox::Outbox::builder(db.clone()));
    let handle = queue_builder
        .queue("other-service-queue", toolkit_db::outbox::Partitions::of(2))
        .leased(NoopProcessor)
        .start()
        .await
        .unwrap();
    let producer_outbox = binder.bind(&handle);
    let conn = db.conn().unwrap();

    let id = producer_outbox.enqueue(&conn, order()).await.unwrap().ids()[0];

    assert!(id.0 > 0);
    handle.stop().await;
}

#[tokio::test]
async fn convenience_start_drains_enqueued_event_to_broker() {
    let (db, broker, harness) = fixture().await;
    let producer_handle = stateless_setup(db.clone(), broker)
        .await
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await
        .unwrap();
    let conn = db.conn().unwrap();

    producer_handle
        .producer()
        .enqueue(&conn, order())
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;

    producer_handle.stop().await;
}

#[tokio::test]
async fn enqueue_batch_drains_every_event_to_broker() {
    let (db, broker, harness) = fixture().await;
    let producer_handle = stateless_setup(db.clone(), broker)
        .await
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await
        .unwrap();
    let conn = db.conn().unwrap();

    // One batch, one flush: every event in it must reach the broker. All three
    // orders share a tenant, so they land in the one tenant partition.
    let events = vec![order(), order(), order()];
    let wake = producer_handle
        .producer()
        .enqueue_batch(&conn, events)
        .await
        .unwrap();
    assert_eq!(wake.ids().len(), 3);
    wake.fire();

    wait_for_stored(&harness, TOPIC, 3).await;

    producer_handle.stop().await;
}

#[tokio::test]
async fn outbox_processor_treats_duplicate_as_ok() {
    let (db, broker, harness) = fixture().await;
    let producer = managed_producer(db, Arc::clone(&broker)).await;
    let envelope = managed_envelope_payload(&producer).await;
    let producer_id = producer
        .partition_producer_for_test(TEST_QUEUE, 0)
        .await
        .unwrap();
    seed_chained_cursor(&broker, producer_id, 1).await;

    let result = producer.process_outbox_payload_for_test(envelope, 1).await;

    assert_message_ok(result);
    wait_for_stored(&harness, TOPIC, 1).await;
    assert_eq!(stored_count(&harness, TOPIC).await, 1);
}

/// The broker appends A but its ack is lost, so the queue redelivers A and the
/// broker answers that resend `Duplicate`. B must then chain straight onto A,
/// not go out with the chain head from before A and need a violation round
/// trip to recover.
#[tokio::test]
async fn an_outbox_duplicate_ack_advances_the_chain() {
    let (db, broker, harness) = fixture().await;
    let faulty = FaultInjectingBroker::new(Arc::clone(&broker));
    let producer_handle = OutboxSetup::builder()
        .broker(faulty.clone() as Arc<dyn EventBrokerApi>)
        .db(db.clone())
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::managed(ProducerMode::Chained, "orders"))
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .queue(QUEUE, Partitions::of(1))
        .client_agent("order-service/1.0")
        .build()
        .await
        .unwrap()
        .start(toolkit_db::outbox::Outbox::builder(db.clone()))
        .await
        .unwrap();
    let conn = db.conn().unwrap();
    faulty.lose_publish_acks(1);

    producer_handle
        .producer()
        .enqueue(&conn, order())
        .await
        .unwrap()
        .fire();
    let deadline = Instant::now() + Duration::from_secs(10);
    while faulty.delivered() < 2 {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the resend of A"
        );
        sleep(Duration::from_millis(10)).await;
    }
    producer_handle
        .producer()
        .enqueue(&conn, order())
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 2).await;
    producer_handle.stop().await;

    let delivered = faulty.delivered_publishes();
    let [lost, resent, b] = delivered.as_slice() else {
        panic!("expected A, its resend and B, got {delivered:?}");
    };
    let a_meta = lost.meta.as_ref().expect("a chained publish carries meta");
    let producer_id = a_meta.producer_id;
    let a_sequence = a_meta.sequence.expect("a chained publish has a sequence");
    let stamp = |meta: &Option<EventMeta>| {
        meta.as_ref()
            .map(|meta| (meta.version, meta.producer_id, meta.previous, meta.sequence))
    };
    assert_eq!(
        [
            (stamp(&lost.meta), lost.outcome.clone(), lost.ack_lost),
            (stamp(&resent.meta), resent.outcome.clone(), resent.ack_lost),
            (stamp(&b.meta), b.outcome.clone(), b.ack_lost),
        ],
        [
            (
                Some((1, producer_id, Some(-1), Some(a_sequence))),
                DeliveredOutcome::Accepted,
                true
            ),
            (
                Some((1, producer_id, Some(-1), Some(a_sequence))),
                DeliveredOutcome::Duplicate,
                false
            ),
            (
                Some((1, producer_id, Some(a_sequence), Some(a_sequence + 1))),
                DeliveredOutcome::Accepted,
                false
            ),
        ]
    );
}

/// A transient publish failure (here a rate limit, armed on the harness) makes
/// the outbox handler ask for a `Retry`, not a `Reject` - the event stays queued
/// and is redelivered rather than dropped. Drives the real gear: the harness
/// injects a genuine `RateLimited` on the publish path.
#[tokio::test]
async fn outbox_processor_retries_transient_rate_limit() {
    let (db, broker, harness) = fixture().await;
    let producer = stateless_setup(db, broker).await;
    let (_, envelope) = producer.outbox_envelope_for_test(order()).await.unwrap();
    harness.set_publish_rate_limited(true);

    let result = producer
        .process_outbox_payload_for_test(serde_json::to_vec(&envelope).unwrap(), 1)
        .await;

    assert_message_retry(result);
}

/// A chained producer's processor starts with an empty cursor cache, so its first message
/// reads the broker cursor before publishing. A transient transport or rate-limit failure of
/// that initial read must be a `Retry` - never a `Reject`, which would dead-letter the message
/// and advance the queue cursor - and nothing may reach the broker without a cursor. Once the
/// read succeeds, the same envelope publishes with its original event id. Dedicated regression
/// for the initial-cursor Retry branch (`c7de7b80ae`); the publish-path rate limit above does
/// not exercise this read.
#[tokio::test]
async fn outbox_processor_retries_transient_initial_cursor_read_with_empty_cache() {
    let (db, broker, harness) = fixture().await;
    let faulty = FaultInjectingBroker::new(Arc::clone(&broker));
    let producer = managed_producer(db, faulty.clone() as Arc<dyn EventBrokerApi>).await;
    let envelope = managed_envelope_payload(&producer).await;
    let event_id: Uuid =
        serde_json::from_value(envelope_json_from_bytes(&envelope)["event_id"].clone()).unwrap();

    for fault in [TransientFault::Transport, TransientFault::RateLimited] {
        faulty.fail_cursor_reads([fault]);
        let reads = faulty.cursor_reads();
        let result = producer
            .process_outbox_payload_for_test(envelope.clone(), 1)
            .await;
        assert_message_retry(result);
        assert_eq!(
            faulty.cursor_reads(),
            reads + 1,
            "the empty cache read the cursor"
        );
        assert_eq!(faulty.publishes(), 0, "nothing publishes without a cursor");
        assert_eq!(stored_count(&harness, TOPIC).await, 0);
    }

    let result = producer.process_outbox_payload_for_test(envelope, 1).await;
    assert_message_ok(result);
    wait_for_stored(&harness, TOPIC, 1).await;
    let mut stored = Vec::new();
    for partition in 0..FIXTURE_BROKER_PARTITIONS {
        let at = TopicPartition::new(
            GtsInstanceId::try_new(TOPIC).unwrap(),
            Partition::new(partition),
        );
        stored.extend(
            harness
                .backend()
                .read(&at, Sequence::NONE, 16, Wait::No)
                .await
                .unwrap(),
        );
    }
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].id, event_id,
        "recovery keeps the original event identity"
    );
}

#[tokio::test]
async fn outbox_processor_rejects_malformed_payload() {
    let (db, broker, harness) = fixture().await;
    let producer = stateless_setup(db, broker).await;

    let result = producer
        .process_outbox_payload_for_test(b"not-json".to_vec(), 1)
        .await;

    let toolkit_db::outbox::MessageResult::Reject(reason) = result else {
        panic!("expected reject, got {result:?}");
    };
    assert_eq!(reason, "undecodable producer envelope (syntax)");
    assert_eq!(stored_count(&harness, TOPIC).await, 0);
}

#[tokio::test]
async fn an_undecodable_envelope_is_dead_lettered_without_its_field_values() {
    let (db, broker, harness) = fixture().await;
    let producer = stateless_setup(db, broker).await;
    let (_, mut payload) = producer.outbox_envelope_for_test(order()).await.unwrap();
    payload["version"] = serde_json::json!("PLANTED-VALUE-7f3a");

    let result = producer
        .process_outbox_payload_for_test(serde_json::to_vec(&payload).unwrap(), 1)
        .await;

    let toolkit_db::outbox::MessageResult::Reject(reason) = result else {
        panic!("expected reject, got {result:?}");
    };
    assert_eq!(reason, "undecodable producer envelope (data)");
    assert!(!reason.contains("PLANTED-VALUE-7f3a"), "{reason}");
    assert_eq!(stored_count(&harness, TOPIC).await, 0);
}

#[tokio::test]
async fn a_truncated_envelope_is_dead_lettered_as_eof() {
    let (db, broker, harness) = fixture().await;
    let producer = stateless_setup(db, broker).await;

    let result = producer
        .process_outbox_payload_for_test(b"{\"version\":".to_vec(), 1)
        .await;

    let toolkit_db::outbox::MessageResult::Reject(reason) = result else {
        panic!("expected reject, got {result:?}");
    };
    assert_eq!(reason, "undecodable producer envelope (eof)");
    assert_eq!(stored_count(&harness, TOPIC).await, 0);
}

#[tokio::test]
async fn outbox_processor_recovers_chained_cursor_on_startup() {
    let (db, broker, _harness) = fixture().await;
    let producer = managed_producer(db, Arc::clone(&broker)).await;
    let envelope = managed_envelope_payload(&producer).await;
    let producer_id = producer
        .partition_producer_for_test(TEST_QUEUE, 0)
        .await
        .unwrap();
    seed_chained_cursor(&broker, producer_id, 2).await;

    let result = producer.process_outbox_payload_for_test(envelope, 4).await;
    let cursors = broker
        .get_producer_cursors(&toolkit_security::SecurityContext::anonymous(), producer_id)
        .await
        .unwrap();
    assert_message_ok(result);
    assert_eq!(
        cursors.last_sequence(&GtsInstanceId::try_new(TOPIC).unwrap()),
        Some(4)
    );
}

#[tokio::test]
async fn outbox_processor_recovers_chained_sequence_violation_by_refreshing_cursor() {
    let (db, broker, _harness) = fixture().await;
    let producer = managed_producer(db, Arc::clone(&broker)).await;
    let envelope = managed_envelope_payload(&producer).await;
    let producer_id = producer
        .partition_producer_for_test(TEST_QUEUE, 0)
        .await
        .unwrap();
    seed_chained_cursor(&broker, producer_id, 3).await;

    let result = producer
        .process_outbox_payload_with_cursor_for_test(envelope, 4, -1)
        .await;
    let cursors = broker
        .get_producer_cursors(&toolkit_security::SecurityContext::anonymous(), producer_id)
        .await
        .unwrap();
    assert_message_ok(result);
    assert_eq!(
        cursors.last_sequence(&GtsInstanceId::try_new(TOPIC).unwrap()),
        Some(4)
    );
}

/// A real in-process broker (the `EventBrokerHarness`) plus the producer-outbox
/// DB. The returned `EventBrokerHarness` must be held for the whole test: its
/// loader and ingest-outbox background tasks have to outlive every publish, so
/// callers bind it (often as `_harness`) rather than dropping it.
async fn fixture() -> (toolkit_db::Db, Arc<dyn EventBrokerApi>, EventBrokerHarness) {
    fixture_at(&next_dsn()).await
}

/// [`fixture`] over the producer-outbox database at `dsn`.
async fn fixture_at(dsn: &str) -> (toolkit_db::Db, Arc<dyn EventBrokerApi>, EventBrokerHarness) {
    let db = db_at(dsn).await;
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(serde_json::json!([
            { "id": TOPIC, "partitions": FIXTURE_BROKER_PARTITIONS },
            { "id": TOPIC2, "partitions": FIXTURE_BROKER_PARTITIONS },
            {
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "data_schema": order_schema(),
                "allowed_subject_types": [SUBJECT_TYPE],
                "partition_key": "/tenant_id",
            },
            {
                "id": EVENT_TYPE2,
                "topic": TOPIC2,
                "data_schema": billing_schema(),
                "allowed_subject_types": [SUBJECT_TYPE],
                "partition_key": "/tenant_id",
            },
            {
                "id": EVENT_TYPE_KEYED,
                "topic": TOPIC,
                "data_schema": order_schema(),
                "allowed_subject_types": [SUBJECT_TYPE],
                "partition_key": PAYLOAD_POINTER,
            },
        ])))
        .build()
        .await;
    let broker = harness.broker();
    (db, broker, harness)
}

/// Sums stored events across every partition of `topic` via the backend read
/// side.
async fn stored_count(harness: &EventBrokerHarness, topic: &str) -> usize {
    let mut total = 0;
    for partition in 0..FIXTURE_BROKER_PARTITIONS {
        let at = TopicPartition::new(
            GtsInstanceId::try_new(topic).unwrap(),
            Partition::new(partition),
        );
        total += harness
            .backend()
            .read(&at, Sequence::NONE, 1024, Wait::No)
            .await
            .unwrap()
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

/// A fresh shared-cache in-memory SQLite DSN, so a second connection of its
/// own (`outbox_bodies`) reads the same database.
fn next_dsn() -> String {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("sqlite:file:evbk_producer_outbox_{seq}?mode=memory&cache=shared")
}

/// The producer-outbox database at `dsn`, with the outbox and producer
/// registration migrations applied.
async fn db_at(dsn: &str) -> toolkit_db::Db {
    let db = toolkit_db::connect_db(
        dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    let mut migrations = toolkit_db::outbox::outbox_migrations();
    migrations.extend(event_broker_sdk::producer_registration_migrations());
    toolkit_db::migration_runner::run_migrations_for_testing(&db, migrations)
        .await
        .unwrap();
    db
}

async fn db_without_producer_migrations() -> toolkit_db::Db {
    let seq = DB_SEQ.fetch_add(1, Ordering::Relaxed);
    let dsn = format!("sqlite:file:evbk_producer_outbox_no_reg_{seq}?mode=memory&cache=shared");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations(),
    )
    .await
    .unwrap();
    db
}

/// A stateless setup bound to `EVENT_TYPE_KEYED` (payload-keyed). Its own
/// helper because `event_type_patterns` fixes which types a setup prepares,
/// and the keyed type is a different one.
async fn keyed_producer(db: toolkit_db::Db, broker: Arc<dyn EventBrokerApi>) -> OutboxSetup {
    OutboxSetup::builder()
        .broker(broker)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::Stateless)
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE_KEYED])
        .queue(QUEUE, Partitions::of(4))
        .build()
        .await
        .unwrap()
}

/// The managed chained setup of [`managed_setup`] under client agent
/// `order-service/1.0`.
async fn managed_producer(db: toolkit_db::Db, broker: Arc<dyn EventBrokerApi>) -> OutboxSetup {
    managed_setup(db, broker, "order-service/1.0")
        .await
        .unwrap()
}

fn order_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["order_id", "total_cents"],
        "properties": {
            "order_id": { "type": "string" },
            "total_cents": { "type": "integer" },
            "partition_key": { "type": "string" }
        }
    })
}

fn billing_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["charge_id", "total_cents"],
        "properties": {
            "charge_id": { "type": "string" },
            "total_cents": { "type": "integer" }
        }
    })
}

/// An order event of the test tenant, about a fresh order.
fn order() -> EventV1<OrderCreated> {
    let order_id = Uuid::new_v4();
    EventV1::builder(OrderCreated {
        order_id,
        total_cents: 10,
    })
    .tenant_id(test_tenant())
    .subject(order_id.to_string())
    .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
    .build()
    .unwrap()
}

/// As [`order`], of the payload-keyed type, keyed by `partition_key`.
fn keyed_order(partition_key: &str) -> EventV1<KeyedOrderCreated> {
    let order_id = Uuid::new_v4();
    EventV1::builder(KeyedOrderCreated {
        order_id,
        total_cents: 10,
        partition_key: partition_key.to_owned(),
    })
    .tenant_id(test_tenant())
    .subject(order_id.to_string())
    .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
    .build()
    .unwrap()
}

fn envelope_json_from_bytes(envelope: &[u8]) -> serde_json::Value {
    serde_json::from_slice(envelope).unwrap()
}

fn test_tenant() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
}

async fn managed_envelope_payload(producer: &OutboxSetup) -> Vec<u8> {
    let (_, envelope) = producer.outbox_envelope_for_test(order()).await.unwrap();
    serde_json::to_vec(&envelope).unwrap()
}

async fn seed_chained_cursor(
    broker: &Arc<dyn EventBrokerApi>,
    producer_id: ProducerId,
    last_sequence: i64,
) {
    let ctx = toolkit_security::SecurityContext::anonymous();
    for sequence in 1..=last_sequence {
        let previous = sequence - 1;
        let previous = if previous == 0 { -1 } else { previous };
        let outcome = broker
            .publish(
                &ctx,
                &chained_event_for_sequence(producer_id, sequence, previous),
                Ack::Accepted,
            )
            .await
            .unwrap();
        assert_eq!(outcome, IngestOutcome::Accepted);
    }
}

/// A chained event read from its JSON body: the builder sets no `meta`.
fn chained_event_for_sequence(producer_id: ProducerId, sequence: i64, previous: i64) -> EventV1 {
    serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(),
        "type": EVENT_TYPE,
        "tenant_id": test_tenant(),
        "source": "order-service",
        "subject": Uuid::new_v4().to_string(),
        "subject_type": SUBJECT_TYPE,
        "occurred_at": chrono::Utc::now(),
        "trace_parent": null,
        "data": {
            "order_id": Uuid::new_v4(),
            "total_cents": 10
        },
        "meta": {
            "version": 1,
            "producer_id": producer_id.0,
            "previous": previous,
            "sequence": sequence,
        },
    }))
    .expect("a well-formed event body")
}

fn assert_message_retry(result: toolkit_db::outbox::MessageResult) {
    assert!(matches!(result, toolkit_db::outbox::MessageResult::Retry));
}

fn assert_message_ok(result: toolkit_db::outbox::MessageResult) {
    assert!(matches!(result, toolkit_db::outbox::MessageResult::Ok));
}

// ---------------------------------------------------------------------------
// `OutboxSetup`: the outbox producer built from one setup, started or
// registered, enqueuing `EventV1<P>` inside the caller's transaction.
// ---------------------------------------------------------------------------

/// The orders payload as a typed event body.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreatedV1 {
    order_id: Uuid,
    total_cents: i64,
}

impl GtsSchema for OrderCreatedV1 {
    const TYPE_ID: &'static str = EVENT_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

/// An orders payload whose `total_cents` breaks the type's schema (a string
/// where an integer is required).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreatedWithTextTotalV1 {
    order_id: Uuid,
    total_cents: String,
}

impl GtsSchema for OrderCreatedWithTextTotalV1 {
    const TYPE_ID: &'static str = EVENT_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

/// Why a test transaction did not commit.
#[derive(Debug)]
enum TxError {
    Broker(EventBrokerError),
    #[expect(
        dead_code,
        reason = "read only through Debug, in a failed test's message"
    )]
    Db(toolkit_db::DbError),
    /// The test rolled the transaction back on purpose.
    RolledBack,
}

impl From<toolkit_db::DbError> for TxError {
    fn from(err: toolkit_db::DbError) -> Self {
        Self::Db(err)
    }
}

/// The `event` the started setup enqueued in a committed transaction is
/// drained to the broker once its `Wake` fires, stamped with the setup's
/// source.
#[tokio::test]
async fn a_started_setup_drains_an_event_enqueued_in_a_committed_transaction() {
    let (db, broker, harness) = fixture().await;
    let setup = stateless_setup(db.clone(), broker).await;
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();

    enqueue_in_tx(&db, handle.producer(), order_v1(1))
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;

    let stored = stored_events(&harness, TOPIC).await;
    let [event] = stored.as_slice() else {
        panic!("expected exactly one stored event, got {stored:?}");
    };
    assert_eq!(
        (
            event.id,
            event.type_id.as_ref(),
            event.tenant_id,
            event.source.as_str(),
            event.subject.as_str(),
            event.subject_type.as_ref(),
            event.occurred_at,
            event.trace_parent.as_deref(),
            event.data.clone(),
            event.partition,
            // Producer metadata is write-only: the broker strips it on read.
            event.meta.is_none(),
        ),
        (
            order_event_id(1),
            EVENT_TYPE,
            test_tenant(),
            "order-service",
            order_id(1).to_string().as_str(),
            SUBJECT_TYPE,
            order_occurred_at(),
            None,
            Some(serde_json::json!({ "order_id": order_id(1), "total_cents": 10 })),
            Some(TENANT_BROKER_PARTITION),
            true,
        )
    );
    handle.stop().await;
}

/// An enqueue whose transaction rolls back writes nothing, so nothing drains;
/// a sentinel of the same tenant, in the same outbox partition, committed
/// after it, is then the only stored event. Pins toolkit-db's transactional
/// write: no change in this crate can make it fail.
#[tokio::test]
async fn a_rolled_back_enqueue_is_never_drained() {
    let (db, broker, harness) = fixture().await;
    let setup = stateless_setup(db.clone(), broker).await;
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();
    let producer = handle.producer().clone();

    let rolled_back = db
        .transaction_ref_mapped(move |tx| {
            Box::pin(async move {
                producer
                    .enqueue(tx, order_v1(1))
                    .await
                    .map_err(TxError::Broker)?
                    .discard();
                Err::<(), _>(TxError::RolledBack)
            })
        })
        .await;
    assert!(
        matches!(rolled_back, Err(TxError::RolledBack)),
        "{rolled_back:?}"
    );
    enqueue_in_tx(&db, handle.producer(), order_v1(2))
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;

    let stored = stored_events(&harness, TOPIC).await;
    let [event] = stored.as_slice() else {
        panic!("expected only the sentinel stored, got {stored:?}");
    };
    assert_eq!(event.id, order_event_id(2));
    handle.stop().await;
}

/// The gear starts the outbox itself, with a queue of its own beside the
/// setup's; the producer bound to that handle drains through it.
#[tokio::test]
async fn a_registered_setup_drains_through_the_outbox_the_gear_starts() {
    let (db, broker, harness) = fixture().await;
    let setup = stateless_setup(db.clone(), broker).await;
    let (queue_builder, binder) = setup.register(Outbox::builder(db.clone()));
    let handle = queue_builder
        .queue("other-service-queue", Partitions::of(2))
        .leased(NoopProcessor)
        .start()
        .await
        .unwrap();
    let producer = binder.bind(&handle);

    enqueue_in_tx(&db, &producer, order_v1(1))
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;

    assert_eq!(stored_count(&harness, TOPIC).await, 1);
    handle.stop().await;
}

/// A binder bound to a handle whose outbox has no queue of its name refuses
/// the enqueue and writes nothing to either database.
#[tokio::test]
async fn a_binder_bound_to_an_outbox_without_its_queue_writes_nothing() {
    let dsn = next_dsn();
    let (db, broker, _harness) = fixture_at(&dsn).await;
    let setup = stateless_setup(db.clone(), broker).await;
    let (_queue_builder, binder) = setup.register(Outbox::builder(db.clone()));
    let foreign_dsn = next_dsn();
    let foreign_db = db_at(&foreign_dsn).await;
    let foreign = Outbox::builder(foreign_db.clone())
        .queue("other-service-queue", Partitions::of(2))
        .leased(NoopProcessor)
        .start()
        .await
        .unwrap();
    let producer = binder.bind(&foreign);

    let err = enqueue_in_tx(&foreign_db, &producer, order_v1(1))
        .await
        .unwrap_err();

    let TxError::Broker(EventBrokerError::OutboxQueueMismatch { queue }) = &err else {
        panic!("expected OutboxQueueMismatch, got {err:?}");
    };
    assert_eq!(queue, QUEUE);
    assert_eq!(
        (outbox_bodies(&dsn).await, outbox_bodies(&foreign_dsn).await),
        (0, 0)
    );
    foreign.stop().await;
}

/// A binder bound to a handle whose outbox has its queue with fewer
/// partitions than the event's (outbox partition 3 of the setup's 4) refuses
/// the enqueue and writes nothing to either database.
#[tokio::test]
async fn a_binder_bound_to_an_outbox_with_fewer_partitions_writes_nothing() {
    let dsn = next_dsn();
    let (db, broker, _harness) = fixture_at(&dsn).await;
    let setup = stateless_setup(db.clone(), broker).await;
    let (_queue_builder, binder) = setup.register(Outbox::builder(db.clone()));
    let foreign_dsn = next_dsn();
    let foreign_db = db_at(&foreign_dsn).await;
    let foreign = Outbox::builder(foreign_db.clone())
        .queue(QUEUE, Partitions::of(2))
        .leased(NoopProcessor)
        .start()
        .await
        .unwrap();
    let producer = binder.bind(&foreign);

    let err = enqueue_in_tx(&foreign_db, &producer, order_v1(1))
        .await
        .unwrap_err();

    let TxError::Broker(EventBrokerError::OutboxQueueMismatch { queue }) = &err else {
        panic!("expected OutboxQueueMismatch, got {err:?}");
    };
    assert_eq!(queue, QUEUE);
    assert_eq!(
        (outbox_bodies(&dsn).await, outbox_bodies(&foreign_dsn).await),
        (0, 0)
    );
    foreign.stop().await;
}

/// The outbox producer refuses an event carrying what it stamps itself (a
/// source, a broker-stamped member) and a decoded event whose envelope breaks
/// a builder rule, alone and as the second event of a batch, and writes
/// nothing for any of them.
#[tokio::test]
async fn the_outbox_producer_refuses_input_it_stamps_or_an_envelope_rule_breaks() {
    let dsn = next_dsn();
    let (db, broker, _harness) = fixture_at(&dsn).await;
    let setup = stateless_setup(db.clone(), broker).await;
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();
    let producer = handle.producer();
    let with_source = EventV1::builder(order_payload(2))
        .id(order_event_id(2))
        .tenant_id(test_tenant())
        .source("other")
        .subject(order_id(2).to_string())
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .occurred_at(order_occurred_at())
        .build()
        .unwrap();

    let single = enqueue_in_tx(&db, producer, with_source.clone())
        .await
        .unwrap_err();
    let batched = enqueue_batch_in_tx(&db, producer, vec![order_v1(1), with_source])
        .await
        .unwrap_err();
    for err in [single, batched] {
        let TxError::Broker(EventBrokerError::InvalidEventField { field, detail }) = &err else {
            panic!("a set source must be refused, got {err:?}");
        };
        assert_eq!(
            (*field, detail.as_str()),
            ("source", "the producer stamps it")
        );
    }

    let partitioned = decoded_order_with("partition", serde_json::json!(1));
    let single = enqueue_in_tx(&db, producer, partitioned.clone())
        .await
        .unwrap_err();
    let batched = enqueue_batch_in_tx(&db, producer, vec![decoded_order(), partitioned])
        .await
        .unwrap_err();
    for err in [single, batched] {
        let TxError::Broker(EventBrokerError::InvalidEventField { field, detail }) = &err else {
            panic!("a set partition must be refused, got {err:?}");
        };
        assert_eq!(
            (*field, detail.as_str()),
            ("partition", "broker-stamped and read-only on publish")
        );
    }

    let non_ascii_subject = decoded_order_with("subject", serde_json::json!("PLANTED-\u{e9}"));
    let single = enqueue_in_tx(&db, producer, non_ascii_subject.clone())
        .await
        .unwrap_err();
    let batched = enqueue_batch_in_tx(&db, producer, vec![decoded_order(), non_ascii_subject])
        .await
        .unwrap_err();
    for err in [single, batched] {
        let TxError::Broker(EventBrokerError::InvalidTextField {
            field,
            detail,
            reason,
        }) = &err
        else {
            panic!("a non-ASCII subject must be refused, got {err:?}");
        };
        assert_eq!(
            (*field, detail.as_str(), *reason),
            (
                "subject",
                "must contain only printable ASCII (0x20-0x7E)",
                "ascii_only"
            )
        );
    }

    let malformed_trace = decoded_order_with("trace_parent", serde_json::json!("00-abc-def-01"));
    let single = enqueue_in_tx(&db, producer, malformed_trace.clone())
        .await
        .unwrap_err();
    let batched = enqueue_batch_in_tx(&db, producer, vec![decoded_order(), malformed_trace])
        .await
        .unwrap_err();
    for err in [single, batched] {
        let TxError::Broker(EventBrokerError::InvalidEventField { field, detail }) = &err else {
            panic!("a malformed trace parent must be refused, got {err:?}");
        };
        assert_eq!(
            (*field, detail.as_str()),
            (
                "trace_parent",
                "must be a W3C traceparent: four lowercase hex groups of 2, 32, 16 and 2 digits \
                 joined by '-'"
            )
        );
    }

    assert_eq!(outbox_bodies(&dsn).await, 0);
    handle.stop().await;
}

/// An empty batch is refused before anything is written; a batch of three
/// writes all three, and all three drain.
#[tokio::test]
async fn an_empty_batch_is_refused_and_a_batch_drains_every_event() {
    let dsn = next_dsn();
    let (db, broker, harness) = fixture_at(&dsn).await;
    let setup = stateless_setup(db.clone(), broker).await;
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();

    let err = enqueue_batch_in_tx(
        &db,
        handle.producer(),
        Vec::<EventV1<OrderCreatedV1>>::new(),
    )
    .await
    .unwrap_err();
    let TxError::Broker(EventBrokerError::InvalidEventField { field, detail }) = &err else {
        panic!("an empty batch must be refused, got {err:?}");
    };
    assert_eq!((*field, detail.as_str()), ("events", "empty batch"));
    assert_eq!(outbox_bodies(&dsn).await, 0);

    let wake = enqueue_batch_in_tx(
        &db,
        handle.producer(),
        vec![order_v1(1), order_v1(2), order_v1(3)],
    )
    .await
    .unwrap();
    assert_eq!(wake.ids().len(), 3);
    wake.fire();
    wait_for_stored(&harness, TOPIC, 3).await;

    assert_eq!(stored_count(&harness, TOPIC).await, 3);
    handle.stop().await;
}

/// A payload that breaks its type's schema is refused with the schema
/// error's location only, never the value, and nothing is written. The raw
/// validator message for the same data quotes the planted value, so the
/// refusal leaving it out is the SDK's doing.
#[tokio::test]
async fn a_schema_invalid_payload_is_refused_without_its_value() {
    let raw_messages = jsonschema::validator_for(&order_schema())
        .unwrap()
        .iter_errors(&serde_json::json!({
            "order_id": order_id(1),
            "total_cents": "PLANTED-VALUE-7f3a",
        }))
        .map(|err| err.to_string())
        .collect::<Vec<_>>();
    assert!(
        raw_messages
            .iter()
            .any(|message| message.contains("PLANTED-VALUE-7f3a")),
        "{raw_messages:?}"
    );
    let dsn = next_dsn();
    let (db, broker, _harness) = fixture_at(&dsn).await;
    let setup = stateless_setup(db.clone(), broker).await;
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();
    let event = EventV1::builder(OrderCreatedWithTextTotalV1 {
        order_id: order_id(1),
        total_cents: "PLANTED-VALUE-7f3a".to_owned(),
    })
    .id(order_event_id(1))
    .tenant_id(test_tenant())
    .subject(order_id(1).to_string())
    .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
    .occurred_at(order_occurred_at())
    .build()
    .unwrap();

    let err = enqueue_in_tx(&db, handle.producer(), event)
        .await
        .unwrap_err();

    let TxError::Broker(
        err @ EventBrokerError::EventDataInvalid {
            type_id,
            errors,
            detail,
        },
    ) = &err
    else {
        panic!("expected EventDataInvalid, got {err:?}");
    };
    assert_eq!(
        (type_id.as_str(), errors.as_slice(), detail.as_str()),
        (
            EVENT_TYPE,
            ["type at /allOf/1/properties/total_cents/type".to_owned()].as_slice(),
            "event payload failed schema validation"
        )
    );
    let display = err.to_string();
    assert!(!display.contains("PLANTED-VALUE-7f3a"), "{display}");
    assert_eq!(outbox_bodies(&dsn).await, 0);
    handle.stop().await;
}

/// A lazy setup prepares a type before it starts, and the producer it starts
/// shares that preparation: the type is enqueued without `SchemaNotPrepared`.
#[tokio::test]
async fn a_type_prepared_on_the_setup_before_start_is_enqueued() {
    let (db, broker, harness) = fixture().await;
    let setup = OutboxSetup::builder()
        .broker(broker)
        .db(db.clone())
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::managed(ProducerMode::Chained, "orders"))
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .queue(QUEUE, Partitions::of(4))
        .client_agent("order-service/1.0")
        .lazy_schema_loading()
        .build()
        .await
        .unwrap();

    setup
        .prepare_event_type(&GtsTypeId::new(EVENT_TYPE))
        .await
        .unwrap();
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();
    enqueue_in_tx(&db, handle.producer(), order_v1(1))
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;

    assert_eq!(stored_count(&harness, TOPIC).await, 1);
    handle.stop().await;
}

/// A managed setup whose client agent differs from the one its key's stored
/// registrations were made with is refused at build, and the refusal leaves
/// the stored registration as it was: a setup with the original client agent
/// still builds against it.
#[tokio::test]
async fn a_setup_whose_client_agent_drifted_from_its_stored_registration_is_refused() {
    let (db, broker, harness) = fixture().await;
    let first = managed_setup(db.clone(), Arc::clone(&broker), "order-service/1.0")
        .await
        .unwrap();
    let handle = first.start(Outbox::builder(db.clone())).await.unwrap();
    enqueue_in_tx(&db, handle.producer(), order_v1(1))
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;
    handle.stop().await;

    let err = managed_setup(db.clone(), Arc::clone(&broker), "order-service/2.0")
        .await
        .unwrap_err();

    let EventBrokerError::InvalidProducerOptions { detail } = &err else {
        panic!("expected InvalidProducerOptions, got {err:?}");
    };
    assert_eq!(
        detail,
        "managed producer registration 'orders' has client_agent 'order-service/1.0', \
         expected 'order-service/2.0'"
    );
    let rebuilt = managed_setup(db, broker, "order-service/1.0").await;
    assert!(rebuilt.is_ok(), "{rebuilt:?}");
}

/// A managed producer's registration rotates and its chain resets, and the
/// producer keeps draining.
#[tokio::test]
async fn a_managed_producer_rotates_and_resets_and_keeps_draining() {
    let (db, broker, harness) = fixture().await;
    let setup = managed_setup(db.clone(), broker, "order-service/1.0")
        .await
        .unwrap();
    let handle = setup.start(Outbox::builder(db.clone())).await.unwrap();
    enqueue_in_tx(&db, handle.producer(), order_v1(1))
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 1).await;

    handle.producer().rotate_registration().await.unwrap();
    handle
        .producer()
        .reset_chain(ResetScope::AllTopics)
        .await
        .unwrap();
    enqueue_in_tx(&db, handle.producer(), order_v1(2))
        .await
        .unwrap()
        .fire();
    wait_for_stored(&harness, TOPIC, 2).await;

    assert_eq!(stored_count(&harness, TOPIC).await, 2);
    handle.stop().await;
}

/// The broker partition the test tenant's events land in (`/tenant_id` keys
/// them, over `FIXTURE_BROKER_PARTITIONS`).
const TENANT_BROKER_PARTITION: u32 = 2;

/// A stateless setup of the orders topic, queue `QUEUE` over 4 outbox
/// partitions, eager: built with every type prepared.
async fn stateless_setup(db: toolkit_db::Db, broker: Arc<dyn EventBrokerApi>) -> OutboxSetup {
    OutboxSetup::builder()
        .broker(broker)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::Stateless)
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .queue(QUEUE, Partitions::of(4))
        .build()
        .await
        .unwrap()
}

/// A managed chained setup under key `"orders"`, otherwise as
/// [`stateless_setup`], registering with `client_agent`.
async fn managed_setup(
    db: toolkit_db::Db,
    broker: Arc<dyn EventBrokerApi>,
    client_agent: &str,
) -> Result<OutboxSetup, EventBrokerError> {
    OutboxSetup::builder()
        .broker(broker)
        .db(db)
        .security_context(toolkit_security::SecurityContext::anonymous())
        .source("order-service")
        .registration(OutboxRegistration::managed(ProducerMode::Chained, "orders"))
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .queue(QUEUE, Partitions::of(4))
        .client_agent(client_agent)
        .build()
        .await
}

/// Enqueues `event` through `producer` in a transaction on `db`, and returns
/// its `Wake` once the transaction committed.
async fn enqueue_in_tx<P: EventPayload>(
    db: &toolkit_db::Db,
    producer: &OutboxProducer,
    event: EventV1<P>,
) -> Result<Wake, TxError> {
    let producer = producer.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move { producer.enqueue(tx, event).await.map_err(TxError::Broker) })
    })
    .await
}

/// As [`enqueue_in_tx`], for a batch.
async fn enqueue_batch_in_tx<P: EventPayload>(
    db: &toolkit_db::Db,
    producer: &OutboxProducer,
    events: Vec<EventV1<P>>,
) -> Result<Wake, TxError> {
    let producer = producer.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            producer
                .enqueue_batch(tx, events)
                .await
                .map_err(TxError::Broker)
        })
    })
    .await
}

/// Every event stored on `topic`, across its partitions, read from the
/// backend.
async fn stored_events(
    harness: &EventBrokerHarness,
    topic: &str,
) -> Vec<event_broker_sdk::models::Event> {
    let mut events = Vec::new();
    for partition in 0..FIXTURE_BROKER_PARTITIONS {
        let at = TopicPartition::new(
            GtsInstanceId::try_new(topic).unwrap(),
            Partition::new(partition),
        );
        events.extend(
            harness
                .backend()
                .read(&at, Sequence::NONE, 1024, Wait::No)
                .await
                .unwrap(),
        );
    }
    events
}

/// How many message bodies the outbox in the database at `dsn` holds, read
/// over a connection of its own.
async fn outbox_bodies(dsn: &str) -> i64 {
    use sea_orm::ConnectionTrait;
    use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};

    let conn = sea_orm::Database::connect(dsn).await.expect("connect");
    conn.query_one(
        Query::select()
            .expr_as(Expr::col(Alias::new("id")).count(), "n")
            .from(Alias::new("toolkit_outbox_body")),
    )
    .await
    .expect("read the outbox bodies")
    .expect("a count row")
    .try_get::<i64>("", "n")
    .expect("a count")
}

/// Order `n`'s id.
fn order_id(n: u128) -> Uuid {
    Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0000 + n)
}

/// The id of order `n`'s created event.
fn order_event_id(n: u128) -> Uuid {
    Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_e000 + n)
}

fn order_occurred_at() -> chrono::DateTime<chrono::Utc> {
    "2026-11-07T10:00:00Z".parse().unwrap()
}

fn order_payload(n: u128) -> OrderCreatedV1 {
    OrderCreatedV1 {
        order_id: order_id(n),
        total_cents: 10,
    }
}

/// Order `n`'s created event of the test tenant, about its order, built.
fn order_v1(n: u128) -> EventV1<OrderCreatedV1> {
    EventV1::builder(order_payload(n))
        .id(order_event_id(n))
        .tenant_id(test_tenant())
        .subject(order_id(n).to_string())
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .occurred_at(order_occurred_at())
        .build()
        .unwrap()
}

/// Order 1's created event as a caller reads it with serde: no source, no
/// `meta`, no broker-stamped member.
fn decoded_order() -> EventV1 {
    decoded_order_with("subject", serde_json::json!(order_id(1).to_string()))
}

/// Order 1's created event body with `member` set to `value`, decoded.
fn decoded_order_with(member: &str, value: serde_json::Value) -> EventV1 {
    let mut body = serde_json::json!({
        "id": order_event_id(1),
        "type": EVENT_TYPE,
        "tenant_id": test_tenant(),
        "subject": order_id(1).to_string(),
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-11-07T10:00:00Z",
        "data": { "order_id": order_id(1), "total_cents": 10 },
    });
    body[member] = value;
    serde_json::from_value(body).expect("the order body decodes")
}
