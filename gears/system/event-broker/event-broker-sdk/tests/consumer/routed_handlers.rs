use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, EventTypeRef, Fallback, HandlerOutcome,
    InMemoryOffsetManager, RawEvent, SingleEventHandler, TopicRef,
};

use super::common::{publish_json, topic_fixture, wait_until};

const TOPIC: &str = "gts.cf.core.events.topic.v1~example.mock.showcase.routed.v1";
const CREATED: &str = "gts.cf.core.events.event_type.v1~example.mock.routed.created.v1";
const UPDATED: &str = "gts.cf.core.events.event_type.v1~example.mock.routed.updated.v1";

struct NamedHandler {
    name: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl SingleEventHandler for NamedHandler {
    async fn handle(
        &self,
        _event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(self.name);
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn if_i_need_topic_type_routing_i_can_register_specific_and_default_handlers() {
    let fixture = topic_fixture(TOPIC, CREATED, 1).await;
    fixture
        .control
        .register_event_type(TOPIC, UPDATED, serde_json::json!({ "type": "object" }), &[])
        .await;

    let calls = Arc::new(Mutex::new(Vec::new()));
    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-routed"))
        .topics([TOPIC])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NamedHandler {
            name: "default",
            calls: calls.clone(),
        })
        .route()
        .topic(TopicRef::gts(TOPIC))
        .event_type(EventTypeRef::gts(CREATED))
        .handler(NamedHandler {
            name: "created",
            calls: calls.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        TOPIC,
        CREATED,
        "created-1",
        None,
        serde_json::json!({ "route": "created" }),
    )
    .await;
    publish_json(
        &fixture.broker,
        &fixture.ctx,
        TOPIC,
        UPDATED,
        "updated-1",
        None,
        serde_json::json!({ "route": "default" }),
    )
    .await;

    wait_until(|| calls.lock().unwrap().len() == 2).await;
    handle.stop().await.expect("consumer stops");

    assert_eq!(calls.lock().unwrap().as_slice(), ["created", "default"]);
}
