#[cfg(test)]
use super::helpers::*;

// Payload validation (M4): known-good accepted, known-bad rejected.
use super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::EventBroker;
use crate::producer::backend::IngestOutcome;

#[tokio::test]
async fn known_good_payload_accepted() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    h.register_event_type(
        TOPIC,
        EVT,
        serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": { "name": { "type": "string" } }
        }),
        &[],
    )
    .await;
    let c = ctx();
    let mut ev = wire_event(TOPIC, EVT, c.subject_tenant_id());
    ev.data = Some(serde_json::json!({"name": "hello"}));
    let out = broker.publish(&c, &ev).await.unwrap();
    assert_eq!(out, IngestOutcome::Accepted);
}

#[tokio::test]
async fn known_bad_payload_rejected() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    h.register_event_type(
        TOPIC,
        EVT,
        serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": { "name": { "type": "string" } }
        }),
        &[],
    )
    .await;
    let c = ctx();
    let mut ev = wire_event(TOPIC, EVT, c.subject_tenant_id());
    ev.data = Some(serde_json::json!({"name": 42})); // name must be string
    let err = broker.publish(&c, &ev).await.unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("EventDataInvalid") || msg.contains("validation") || msg.contains("invalid"),
        "bad payload must fail validation (M4): {msg}"
    );
}

#[tokio::test]
async fn unknown_topic_returns_error() {
    let broker = crate::mock::MockBroker::new();
    let c = ctx();
    let err = broker
        .publish(
            &c,
            &wire_event(
                "gts.cf.core.events.topic.v1~test.mock.broker.noexist.v1",
                EVT,
                c.subject_tenant_id(),
            ),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("gts.cf.core.events.topic.v1~test.mock.broker.noexist.v1")
            || format!("{err:?}").contains("TopicNotFound")
    );
}

#[tokio::test]
async fn unknown_event_type_returns_error() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    // Register one type so the topic is not permissive.
    _h.register_event_type(TOPIC, EVT, serde_json::json!({"type":"object"}), &[])
        .await;
    let c = ctx();
    let err = broker
        .publish(
            &c,
            &wire_event(
                TOPIC,
                "gts.cf.core.events.event_type.v1~test.mock.broker.ghost.v1",
                c.subject_tenant_id(),
            ),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("EventTypeUnknown")
            || format!("{err:?}")
                .contains("gts.cf.core.events.event_type.v1~test.mock.broker.ghost.v1")
    );
}
