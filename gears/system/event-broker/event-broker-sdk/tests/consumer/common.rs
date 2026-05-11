use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::mock::stubs::test_ctx_for_tenant;
use event_broker_sdk::mock::{MockBroker, MockBrokerHandle};
use event_broker_sdk::{Event, EventBroker};
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub const TENANT: &str = "00000000-0000-0000-0000-000000000001";

pub struct TopicFixture {
    pub broker: Arc<dyn EventBroker>,
    pub control: MockBrokerHandle,
    pub ctx: SecurityContext,
}

pub async fn topic_fixture(topic: &str, event_type: &str, partitions: u32) -> TopicFixture {
    let mock = MockBroker::new();
    let control = MockBrokerHandle::from_broker(&mock);
    control.register_topic(topic, partitions).await;
    control
        .register_event_type(
            topic,
            event_type,
            serde_json::json!({ "type": "object" }),
            &[],
        )
        .await;
    control
        .set_heartbeat_interval(Duration::from_millis(10))
        .await;

    TopicFixture {
        broker: Arc::new(mock),
        control,
        ctx: test_ctx_for_tenant(Uuid::parse_str(TENANT).expect("tenant uuid")),
    }
}

pub async fn publish_json(
    broker: &Arc<dyn EventBroker>,
    ctx: &SecurityContext,
    topic: &str,
    event_type: &str,
    subject: &str,
    partition: Option<u32>,
    data: serde_json::Value,
) {
    publish_json_with_partition_key(
        broker, ctx, topic, event_type, subject, None, partition, data,
    )
    .await;
}

pub async fn publish_json_with_partition_key(
    broker: &Arc<dyn EventBroker>,
    ctx: &SecurityContext,
    topic: &str,
    event_type: &str,
    subject: &str,
    partition_key: Option<&str>,
    partition: Option<u32>,
    data: serde_json::Value,
) {
    broker
        .publish(
            ctx,
            &Event {
                id: Uuid::new_v4(),
                type_id: event_type.to_owned(),
                topic: topic.to_owned(),
                tenant_id: ctx.subject_tenant_id(),
                source: "event-broker-sdk.consumer.showcase".to_owned(),
                subject: subject.to_owned(),
                subject_type: "showcase".to_owned(),
                partition_key: partition_key.map(str::to_owned),
                occurred_at: chrono::Utc::now(),
                trace_parent: None,
                data: Some(data),
                partition,
                sequence: None,
                sequence_time: None,
                offset: None,
                offset_time: None,
                meta: None,
            },
        )
        .await
        .expect("event published");
}

pub async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition was not observed before timeout");
}
