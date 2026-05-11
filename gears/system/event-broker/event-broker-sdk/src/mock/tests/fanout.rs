#[cfg(test)]
use super::helpers::*;

// Multi-group fan-out: every subscribed group receives every event.
use super::helpers::{broker_with_topic, ctx, join_group, make_group, wire_event};
use crate::ResolvedPosition;
use crate::api::EventBroker;
use crate::consumer::backend::{SeekPosition, WireFrame};
use futures_util::StreamExt;

#[tokio::test]
async fn two_groups_both_receive_event() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();

    let group1 = make_group(&c, &broker).await;
    let group2 = make_group(&c, &broker).await;
    let sub_group1 = join_group(&c, &broker, &group1, TOPIC).await;
    let sub_group2 = join_group(&c, &broker, &group2, TOPIC).await;

    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();

    async fn has_event(
        ctx: &toolkit_security::SecurityContext,
        broker: &impl EventBroker,
        sub: crate::ids::SubscriptionId,
    ) -> bool {
        let mut s = broker.stream(ctx, sub).await.unwrap();
        for _ in 0..5 {
            match s.next().await {
                Some(Ok(WireFrame::Event(_))) => return true,
                Some(Ok(WireFrame::Heartbeat)) | Some(Ok(WireFrame::Topology { .. })) => continue,
                _ => break,
            }
        }
        false
    }
    assert!(
        has_event(&c, &broker, sub_group1.subscription_id).await,
        "group1 must receive the event"
    );
    assert!(
        has_event(&c, &broker, sub_group2.subscription_id).await,
        "group2 must receive the event"
    );
}

#[tokio::test]
async fn group_cursors_are_independent() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let group1 = make_group(&c, &broker).await;
    let group2 = make_group(&c, &broker).await;
    let sub_group1 = join_group(&c, &broker, &group1, TOPIC).await;
    let _a2 = join_group(&c, &broker, &group2, TOPIC).await;

    broker
        .seek(&c, sub_group1.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(5),
        }])
        .await
        .unwrap();

    assert_eq!(h.cursor(&group1, TOPIC, 0).await, Some(5));
    let g2_cursor = h.cursor(&group2, TOPIC, 0).await;
    assert!(
        g2_cursor.unwrap_or(0) < 5,
        "g2 cursor must not be affected by g1 seek"
    );
}
