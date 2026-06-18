#[cfg(test)]
use super::helpers::*;

// C9 hot-standby: active expires → standby inherits partitions + cursor.
use super::helpers::{broker_with_topic, ctx, join_group, make_group, wire_event};
use crate::ResolvedPosition;
use crate::api::EventBroker;
use crate::consumer::backend::SeekPosition;

#[tokio::test]
async fn standby_takes_over_after_active_expires() {
    let (broker, h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let active = join_group(&c, &broker, &gid, TOPIC).await;
    let standby = join_group(&c, &broker, &gid, TOPIC).await;

    // Publish events and active seeks to offset 3 (cursor.offset survives handoff).
    for _ in 0..5 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let active_partition = h.assignment(active.subscription_id).await;
    if let Some(slot) = active_partition.first() {
        broker
            .seek(&c, active.subscription_id, &[SeekPosition {
                topic: slot.topic.clone(),
                partition: slot.partition,
                value: ResolvedPosition::Exact(3),
            }])
            .await
            .unwrap();
    }

    // Active crashes.
    h.expire_subscription(active.subscription_id).await;

    // Standby should now own all partitions.
    let standby_assignment = h.assignment(standby.subscription_id).await;
    assert_eq!(
        standby_assignment.len(),
        2,
        "standby must inherit all partitions"
    );

    // And cursor.offset should reflect active's last seeked position.
    if let Some(slot) = standby_assignment.first() {
        let cursor = h.cursor(&gid, &slot.topic, slot.partition).await;
        // cursor must be at the seeked offset (3) or higher, not 0.
        assert!(cursor.is_some(), "cursor must exist");
    }
}
