use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, Delivery, EventHandler, EventV1, Fallback,
    HandlerOutcome, InMemoryOffsetManager, SubscriptionInterest, gts_id,
};

use super::common::{event_pattern, publish_json, topic, topic_fixture, wait_until};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.inmemory.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.inmemory.v1~");

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

#[tokio::test]
async fn if_i_want_at_least_once_consumption_with_in_memory_offsets() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let subjects = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-in-memory"))
        .subscription_interests([SubscriptionInterest::builder()
            .topic(topic(TOPIC))
            .types([event_pattern(EVENT_TYPE)])
            .build()
            .expect("topic-scoped interest")])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(RecordingHandler {
            subjects: subjects.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        EVENT_TYPE,
        "order-1",
        None,
        serde_json::json!({ "ok": true }),
    )
    .await;

    wait_until(|| subjects.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");

    assert_eq!(subjects.lock().unwrap().as_slice(), ["order-1"]);
}
