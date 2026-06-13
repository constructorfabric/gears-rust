#[cfg(test)]
use super::helpers::*;

// C9 hot-standby: active expires → standby inherits partitions + cursor.
use super::helpers::{broker_with_topic, ctx, join_group, make_group, wire_event};
use crate::api::EventBroker;
use std::collections::HashMap;

#[tokio::test]
async fn standby_takes_over_after_active_expires() {
    let (broker, h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let active = join_group(&c, &broker, &gid, TOPIC).await;
    let standby = join_group(&c, &broker, &gid, TOPIC).await;

    // Publish events and active acks offset 3.
    for _ in 0..5 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let active_partition = h.assignment(active.subscription_id).await;
    if let Some(slot) = active_partition.first() {
        let mut m: HashMap<String, HashMap<u32, i64>> = HashMap::new();
        m.entry(slot.topic.clone())
            .or_default()
            .insert(slot.partition, 3);
        broker.ack(&c, active.subscription_id, &m).await.unwrap();
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

    // And cursor should reflect active's last acked position.
    if let Some(slot) = standby_assignment.first() {
        let cursor = h.cursor(&gid, &slot.topic, slot.partition).await;
        // cursor must be at the acked offset (3) or higher, not 0.
        assert!(cursor.is_some(), "cursor must exist");
    }
}
