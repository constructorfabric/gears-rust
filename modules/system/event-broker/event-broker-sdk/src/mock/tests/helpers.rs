use crate::api::{EventBroker, JoinRequest, SubscriptionAssignment};
use crate::consumer::backend::SubscriptionInterest;
use crate::ids::ConsumerGroupId;
use crate::internal::envelope::Event as WireEnvelope;
use crate::mock::stubs::test_ctx_for_tenant;
use crate::mock::{MockBroker, MockBrokerHandle};
use crate::models::CreateConsumerGroupRequest;
use modkit_security::SecurityContext;
use uuid::Uuid;

// ── GTS identifier constants used across all mock tests ──────────────────────

pub const TOPIC: &str = "gts.cf.core.events.topic.v1~test.mock.broker.audit.v1";
pub const TOPIC2: &str = "gts.cf.core.events.topic.v1~test.mock.broker.notify.v1";
pub const TOPIC3: &str = "gts.cf.core.events.topic.v1~test.mock.broker.analytics.v1";
pub const EVT: &str = "gts.cf.core.events.event_type.v1~test.mock.broker.event.v1";
pub const EVT2: &str = "gts.cf.core.events.event_type.v1~test.mock.broker.event2.v1";

// ── Helpers ───────────────────────────────────────────────────────────────────

pub async fn broker_with_topic(topic: &str, partitions: u32) -> (MockBroker, MockBrokerHandle) {
    let broker = MockBroker::new();
    let h = broker.handle();
    h.register_topic(topic, partitions).await;
    (broker, h)
}

pub fn ctx() -> SecurityContext {
    test_ctx_for_tenant(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
}

pub fn ctx2() -> SecurityContext {
    test_ctx_for_tenant(Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap())
}

pub fn wire_event(topic: &str, type_id: &str, tenant_id: Uuid) -> WireEnvelope {
    let json = serde_json::json!({
        "id": Uuid::new_v4().to_string(),
        "type": type_id,
        "topic": topic,
        "tenant_id": tenant_id.to_string(),
        "source": "test.mock",
        "subject": "test-subject",
        "subject_type": "test-type",
        "occurred_at": chrono::Utc::now().to_rfc3339(),
    });
    serde_json::from_value(json).expect("wire_event: serde round-trip failed")
}

pub async fn make_group(ctx: &SecurityContext, broker: &MockBroker) -> ConsumerGroupId {
    broker
        .create_consumer_group(
            ctx,
            CreateConsumerGroupRequest {
                client_agent: "test-agent/1.0".to_owned(),
                description: None,
            },
        )
        .await
        .unwrap()
        .id
}

pub async fn join_group(
    ctx: &SecurityContext,
    broker: &MockBroker,
    group: &ConsumerGroupId,
    topic: &str,
) -> SubscriptionAssignment {
    broker
        .join(
            ctx,
            JoinRequest {
                group: group.clone(),
                client_agent: "test-consumer/1.0".to_owned(),
                interests: vec![SubscriptionInterest {
                    topic: topic.to_owned(),
                    event_type_pattern: "*".to_owned(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: Some(std::time::Duration::from_secs(30)),
                auto_commit: false,
            },
        )
        .await
        .unwrap()
}
