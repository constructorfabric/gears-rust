use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, Fallback, HandlerOutcome,
    InMemoryOffsetManager, RawEvent, SingleEventHandler,
};

use super::common::{PublishJson, publish_json, topic_fixture, wait_until};

const TOPIC: &str = "gts.cf.core.events.topic.v1~example.mock.showcase.single.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event_type.v1~example.mock.showcase.single.v1";

struct SingleEventProjector {
    offsets: Arc<Mutex<Vec<i64>>>,
    partition_keys: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl SingleEventHandler for SingleEventProjector {
    async fn handle(
        &self,
        event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.partition_keys
            .lock()
            .unwrap()
            .push(event.partition_key.clone());
        self.offsets.lock().unwrap().push(event.offset);
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn if_i_want_a_simple_single_event_handler() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let offsets = Arc::new(Mutex::new(Vec::new()));
    let partition_keys = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-single"))
        .topics([TOPIC])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(SingleEventProjector {
            offsets: offsets.clone(),
            partition_keys: partition_keys.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        TOPIC,
        EVENT_TYPE,
        "single-1",
        None,
        serde_json::json!({ "kind": "single" }),
    )
    .await;

    wait_until(|| offsets.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");

    assert_eq!(offsets.lock().unwrap().len(), 1);
    assert_eq!(partition_keys.lock().unwrap()[0], None);
}

#[tokio::test]
async fn if_i_publish_with_a_partition_key_the_handler_can_inspect_it() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let offsets = Arc::new(Mutex::new(Vec::new()));
    let partition_keys = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous(
            "showcase-single-partition-key",
        ))
        .topics([TOPIC])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(SingleEventProjector {
            offsets: offsets.clone(),
            partition_keys: partition_keys.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    super::common::publish_json_with_partition_key(PublishJson {
        broker: &fixture.broker,
        ctx: &fixture.ctx,
        topic: TOPIC,
        event_type: EVENT_TYPE,
        subject: "single-1",
        partition_key: Some("tenant-a/order-1"),
        partition: None,
        data: serde_json::json!({ "kind": "single" }),
    })
    .await;

    wait_until(|| offsets.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");

    assert_eq!(
        partition_keys.lock().unwrap()[0].as_deref(),
        Some("tenant-a/order-1")
    );
}
