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

pub struct PublishJson<'a> {
    pub broker: &'a Arc<dyn EventBroker>,
    pub ctx: &'a SecurityContext,
    pub topic: &'a str,
    pub event_type: &'a str,
    pub subject: &'a str,
    pub partition_key: Option<&'a str>,
    pub partition: Option<u32>,
    pub data: serde_json::Value,
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
    publish_json_with_partition_key(PublishJson {
        broker,
        ctx,
        topic,
        event_type,
        subject,
        partition_key: None,
        partition,
        data,
    })
    .await;
}

pub async fn publish_json_with_partition_key(request: PublishJson<'_>) {
    let PublishJson {
        broker,
        ctx,
        topic,
        event_type,
        subject,
        partition_key,
        partition,
        data,
    } = request;
    let resolved_partition_key = partition_key
        .map(str::to_owned)
        .or_else(|| partition.map(partition_key_for_two_partition_fixture));
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
                partition_key: resolved_partition_key,
                occurred_at: chrono::Utc::now(),
                trace_parent: None,
                data: Some(data),
                partition: None,
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

fn partition_key_for_two_partition_fixture(target: u32) -> String {
    match target {
        0 => "fixture-partition-key-0-0",
        1 => "fixture-partition-key-1-0",
        _ => panic!("two-partition fixture cannot target partition {target}"),
    }
    .to_owned()
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
