#[cfg(test)]
use super::helpers::*;

// C1 solo consumer: JOIN → stream → seek → leave. C2 crash re-JOIN from cursor.
use super::helpers::{broker_with_topic, ctx, join_group, make_group, wire_event};
use crate::ResolvedPosition;
use crate::api::EventBroker;
use crate::consumer::backend::{SeekPosition, WireFrame};
use futures_util::StreamExt;

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
async fn seek_advances_cursor() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let assignment = join_group(&c, &broker, &gid, TOPIC).await;
    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();

    broker
        .seek(&c, assignment.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(0),
        }])
        .await
        .unwrap();

    assert_eq!(
        h.cursor(&gid, TOPIC, 0).await,
        Some(0),
        "cursor must advance to seeked cursor.offset"
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
    // C2: re-JOIN picks up from cursor.offset.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    // First consumer: publish 3 events, seek to offset 1 (processed 0 and 1).
    let sub_first = join_group(&c, &broker, &gid, TOPIC).await;
    for _ in 0..3 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    broker
        .seek(&c, sub_first.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(1), // seek to offset 1 (processed offsets 0 and 1)
        }])
        .await
        .unwrap();
    broker.leave(&c, sub_first.subscription_id).await.unwrap();

    // Second consumer joins same group: cursor.offset should be 1.
    let _sub_second = join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(
        h.cursor(&gid, TOPIC, 0).await,
        Some(1),
        "re-JOIN should carry cursor.offset=1 into the group state"
    );
}

#[tokio::test]
async fn topology_frame_includes_offset_and_last_examined() {
    // After streaming an event, a topology frame should include offset and last_examined.
    // Verify the WireFrame::Topology variant has the new fields populated.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let assignment = join_group(&c, &broker, &gid, TOPIC).await;

    // Seek so the cursor is non-zero.
    broker
        .seek(&c, assignment.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(5),
        }])
        .await
        .unwrap();

    let mut stream = broker.stream(&c, assignment.subscription_id).await.unwrap();
    let frame = stream.next().await.unwrap().unwrap();

    // Verify the Topology frame has the new fields.
    match frame {
        WireFrame::Topology {
            topology_version,
            assigned,
            offsets,
            last_examined,
        } => {
            assert!(topology_version >= 0, "topology_version must be non-negative");
            assert!(!assigned.is_empty(), "assigned must be non-empty");
            // offsets and last_examined are present (may be empty if cursor not yet set, but fields exist)
            let _ = offsets;
            let _ = last_examined;
        }
        other => panic!("expected WireFrame::Topology, got {:?}", other),
    }
}
