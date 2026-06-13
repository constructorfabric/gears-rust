#[cfg(test)]
use super::helpers::*;

// C1 solo consumer: JOIN → stream → ack → leave. C2 crash re-JOIN from cursor.
use super::helpers::{broker_with_topic, ctx, join_group, make_group, wire_event};
use crate::api::EventBroker;
use crate::consumer::backend::WireFrame;
use futures_util::StreamExt;
use std::collections::HashMap;

#[tokio::test]
async fn join_returns_assignment() {
    let (broker, _h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let assignment = join_group(&c, &broker, &gid, TOPIC).await;
    assert!(
        !assignment.assigned.is_empty(),
        "should have at least one partition assigned"
    );
    assert_eq!(assignment.topology_version, 1);
}

#[tokio::test]
async fn publish_then_stream_delivers_event() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let assignment = join_group(&c, &broker, &gid, TOPIC).await;

    // Publish one event.
    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();

    // Open stream and read first few frames.
    let mut stream = broker.stream(&c, assignment.subscription_id).await.unwrap();
    // First frame is Topology.
    let frame1 = stream.next().await.unwrap().unwrap();
    assert!(
        matches!(frame1, WireFrame::Topology { .. }),
        "first frame must be Topology"
    );
    // Second frame is the event.
    let frame2 = stream.next().await.unwrap().unwrap();
    assert!(
        matches!(frame2, WireFrame::Event(_)),
        "second frame must be Event"
    );
}

#[tokio::test]
async fn ack_advances_cursor() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let assignment = join_group(&c, &broker, &gid, TOPIC).await;
    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();

    let mut offsets: HashMap<String, HashMap<u32, i64>> = HashMap::new();
    offsets.entry(TOPIC.to_owned()).or_default().insert(0, 0);
    broker
        .ack(&c, assignment.subscription_id, &offsets)
        .await
        .unwrap();

    assert_eq!(
        h.cursor(&gid, TOPIC, 0).await,
        Some(0),
        "cursor must advance to acked offset"
    );
}

#[tokio::test]
async fn leave_succeeds() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let assignment = join_group(&c, &broker, &gid, TOPIC).await;
    broker.leave(&c, assignment.subscription_id).await.unwrap();
    assert!(broker.list_subscriptions(&c).await.unwrap().is_empty());
}

#[tokio::test]
async fn rejoin_after_leave_resumes_from_cursor() {
    // C2: re-JOIN picks up from cursor.acked.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    // First consumer: publish 3 events, ack first 2.
    let sub_first = join_group(&c, &broker, &gid, TOPIC).await;
    for _ in 0..3 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let mut offsets: HashMap<String, HashMap<u32, i64>> = HashMap::new();
    offsets.entry(TOPIC.to_owned()).or_default().insert(0, 1); // ack offsets 0 and 1
    broker
        .ack(&c, sub_first.subscription_id, &offsets)
        .await
        .unwrap();
    broker.leave(&c, sub_first.subscription_id).await.unwrap();

    // Second consumer joins same group: assignment offset should be 1 (last acked).
    let sub_second = join_group(&c, &broker, &gid, TOPIC).await;
    let slot = sub_second
        .assigned
        .iter()
        .find(|s| s.topic == TOPIC && s.partition == 0);
    if let Some(slot) = slot {
        assert_eq!(slot.offset, 1, "re-JOIN should start from cursor.acked=1");
    }
}
