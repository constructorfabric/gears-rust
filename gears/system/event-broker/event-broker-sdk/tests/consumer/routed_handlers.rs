use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, Fallback, HandlerOutcome,
    InMemoryOffsetManager, RawEvent, SingleEventHandler, gts_id,
};

use event_broker::test_support::StaticTypesRegistry;
use serde_json::json;

use super::common::{
    SHOWCASE_SUBJECT_TYPE, event_pattern, fixture_from_catalog, publish_json, topic, wait_until,
};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.routed.v1");
const CREATED: &str = gts_id!("cf.core.events.event.v1~example.mock.routed.created.v1~");
const UPDATED: &str = gts_id!("cf.core.events.event.v1~example.mock.routed.updated.v1~");

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
    // Both event types are declared on the one topic up front - a real broker
    // resolves types from its registered catalog, not a runtime registration.
    let fixture = fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": TOPIC, "partitions": 1 },
        { "id": CREATED, "topic": TOPIC, "data_schema": { "type": "object" }, "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE], "partition_key": "/source" },
        { "id": UPDATED, "topic": TOPIC, "data_schema": { "type": "object" }, "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE], "partition_key": "/source" },
    ])))
    .await;

    let calls = Arc::new(Mutex::new(Vec::new()));
    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-routed"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NamedHandler {
            name: "default",
            calls: calls.clone(),
        })
        .route()
        .topic(topic(TOPIC))
        .event_type(event_pattern(CREATED))
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
        CREATED,
        "created-1",
        None,
        serde_json::json!({ "route": "created" }),
    )
    .await;
    publish_json(
        &fixture.broker,
        &fixture.ctx,
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
