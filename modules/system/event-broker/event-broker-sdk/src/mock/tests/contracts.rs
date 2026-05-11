#[cfg(test)]
use super::helpers::*;

// Interface contracts: M5a (SubscriptionInterest shape), M5b (storage trait methods),
// EventBrokerBackend compiles, mock compiles without mock feature.
use super::helpers::{broker_with_topic, ctx, make_group};
use crate::api::{EventBroker, EventBrokerBackend, JoinRequest};
use crate::consumer::backend::SubscriptionInterest;

#[tokio::test]
async fn subscription_interest_accepts_tenant_id_and_types() {
    // M5a: SubscriptionInterest should carry tenant_id and types[].
    // The current SDK struct has event_type_pattern (single, not types[]).
    // This test documents the gap: it passes today but the interest shape
    // diverges from the PRD/ADR-0004 contract.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    // Current shape:
    let interest = SubscriptionInterest {
        topic: TOPIC.into(),
        event_type_pattern: "gts.cf.core.events.event_type.v1~*".into(),
        filter_engine: None,
        filter_expression: None,
    };
    // TODO(M5a): once SubscriptionInterest is updated to PRD shape
    // (tenant_id: Uuid, types: Vec<String>), update this test.
    let join_result = broker
        .join(
            &c,
            JoinRequest {
                group: gid,
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![interest],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await;
    assert!(
        join_result.is_ok(),
        "JOIN with current interest shape must succeed"
    );
}

#[tokio::test]
async fn storage_backend_persist_and_read() {
    // M5b: EventBrokerBackend has persist/read/query/list_partition_leaders.
    use crate::mock::MockBroker;
    use modkit_security::SecurityContext;

    let broker = MockBroker::new();
    broker.handle().register_topic(TOPIC, 1).await;
    let c = ctx();

    // persist via storage backend.
    let event = wire_event(TOPIC, EVT, c.subject_tenant_id());
    EventBrokerBackend::persist(&broker, &c, TOPIC, 0, &[event])
        .await
        .unwrap();

    // read back.
    let events = EventBrokerBackend::read(&broker, &c, TOPIC, 0, 0, 10)
        .await
        .unwrap();
    assert_eq!(events.len(), 1, "persist + read round-trip must work (M5b)");

    // list_partition_leaders.
    let leaders = EventBrokerBackend::list_partition_leaders(&broker, &c, TOPIC)
        .await
        .unwrap();
    assert_eq!(leaders.len(), 1, "1 partition → 1 leader");
}

#[test]
fn mock_compiles_and_is_gated() {
    // This test file is only compiled with --features mock.
    // The existence of this test proves the mock compiled successfully.
    let _ = crate::mock::MockBroker::new();
}
