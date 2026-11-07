use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::gts_id;
use event_broker_sdk::{
    Ack, BatchHandler, BatchHandlerOutcome, ConsumerBatching, ConsumerBuilder, ConsumerError,
    ConsumerGroupRef, Delivery, EventBatch, EventV1, Fallback, GtsInstanceId, GtsTypeId,
    InMemoryOffsetManager,
};
use serde_json::json;
use uuid::Uuid;

use super::common::{
    SHOWCASE_SUBJECT_TYPE, publish_json, topic, topic_fixture, two_topic_fixture, wait_until,
};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.batch.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.batch.v1~");

type RecordedBatch = (u32, Vec<i64>);
type RecordedBatches = Arc<Mutex<Vec<RecordedBatch>>>;

struct BatchProjector {
    batches: RecordedBatches,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for BatchProjector {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.batches.lock().unwrap().push((
            delivery.partition(),
            chunk
                .iter()
                .map(|event| {
                    event
                        .sequence()
                        .expect("a delivered event is stamped")
                        .as_i64()
                })
                .collect(),
        ));
        Ok(BatchHandlerOutcome::AdvanceThrough {
            offset: chunk
                .last()
                .expect("showcase batch is not empty")
                .sequence()
                .expect("a delivered event is stamped"),
        })
    }
}

#[tokio::test]
async fn if_i_want_native_batches_they_stay_inside_one_partition() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let batches = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-batch"))
        .topics([topic(TOPIC)])
        .batching(ConsumerBatching {
            max_events: 8,
            max_wait: Duration::from_millis(10),
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(BatchProjector {
            batches: batches.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    for partition in [0, 1] {
        publish_json(
            &fixture.broker,
            &fixture.ctx,
            EVENT_TYPE,
            &format!("batch-{partition}"),
            Some(partition),
            serde_json::json!({ "partition": partition }),
        )
        .await;
    }

    wait_until(|| batches.lock().unwrap().len() >= 2).await;
    handle.stop().await.expect("consumer stops");

    assert!(
        batches
            .lock()
            .unwrap()
            .iter()
            .all(|(_, offsets)| !offsets.is_empty())
    );
}

const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.batch_orders.v1");
const ORDERS_EVENT: &str =
    gts_id!("cf.core.events.event.v1~example.mock.showcase.batch_orders.v1~");
const AUDIT_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.batch_audit.v1");
const AUDIT_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.batch_audit.v1~");
const ORDER_ID: Uuid = Uuid::from_u128(0x7160_0101);
const AUDIT_ID: Uuid = Uuid::from_u128(0x7160_0102);

type Placements = Arc<Mutex<Vec<(Uuid, GtsInstanceId, u32)>>>;

/// Records `(id, topic, partition)` of every event, the topic and partition taken
/// from the batch's delivery.
struct DeliveryRecorder {
    placements: Placements,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for DeliveryRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        self.placements.lock().unwrap().extend(
            batch
                .iter()
                .map(|event| (event.id(), delivery.topic().clone(), delivery.partition())),
        );
        Ok(BatchHandlerOutcome::Success)
    }
}

/// One batch is one topic-partition, so each batch's delivery names the topic its
/// events were published to. Both topics use partition 0: only the topic tells the
/// two deliveries apart.
#[tokio::test]
async fn a_batch_handler_sees_one_delivery_per_topic_partition_batch() {
    let fixture =
        two_topic_fixture((ORDERS_TOPIC, ORDERS_EVENT), (AUDIT_TOPIC, AUDIT_EVENT), 1).await;
    let placements = Placements::default();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-batch-delivery"))
        .topics([topic(ORDERS_TOPIC), topic(AUDIT_TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(DeliveryRecorder {
            placements: placements.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    for (id, event_type) in [(ORDER_ID, ORDERS_EVENT), (AUDIT_ID, AUDIT_EVENT)] {
        let event = EventV1::builder_for(GtsTypeId::new(event_type), json!({}))
            .id(id)
            .tenant_id(fixture.ctx.subject_tenant_id())
            .source("batch-delivery")
            .subject("subject-1")
            .subject_type_id(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
            .build()
            .expect("a valid event");
        fixture
            .broker
            .publish(&fixture.ctx, &event, Ack::Accepted)
            .await
            .expect("event published");
    }

    wait_until(|| placements.lock().unwrap().len() == 2).await;
    handle.stop().await.expect("consumer stops");

    let mut placed = placements.lock().unwrap().clone();
    placed.sort_by_key(|&(id, _, _)| id);
    assert_eq!(
        placed,
        vec![
            (ORDER_ID, topic(ORDERS_TOPIC), 0),
            (AUDIT_ID, topic(AUDIT_TOPIC), 0),
        ]
    );
}
