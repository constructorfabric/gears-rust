#[cfg(test)]
use super::helpers::*;

// Rebalance: Scenarios A+B, C3–C6, v1 deterministic round-robin.
use super::helpers::{broker_with_topic, ctx, ctx2, join_group, make_group, wire_event};
use crate::api::EventBroker;
use std::collections::HashMap;

#[tokio::test]
async fn two_members_split_partitions() {
    // Scenario B: 4 partitions, 2 members → 2+2 split.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub_first = join_group(&c, &broker, &gid, TOPIC).await;
    let sub_second = join_group(&c, &broker, &gid, TOPIC).await;

    let sub_first_slots = h.assignment(sub_first.subscription_id).await;
    let sub_second_slots = h.assignment(sub_second.subscription_id).await;

    // No overlap.
    for p in &sub_first_slots {
        assert!(
            !sub_second_slots.contains(p),
            "partition {p:?} must not be in both assignments"
        );
    }
    // Together they cover all 4 partitions.
    let mut all: Vec<_> = sub_first_slots
        .iter()
        .chain(sub_second_slots.iter())
        .cloned()
        .collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 4, "2 members must cover all 4 partitions");
}

#[tokio::test]
async fn per_group_topology_version_increments() {
    let (broker, h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    assert_eq!(h.topology_version(&gid).await, 0);
    join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(h.topology_version(&gid).await, 1);
    join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(h.topology_version(&gid).await, 2);
}

#[tokio::test]
async fn leave_triggers_rebalance() {
    // C5: one of two members leaves; remaining member inherits all partitions.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub_first = join_group(&c, &broker, &gid, TOPIC).await;
    let sub_second = join_group(&c, &broker, &gid, TOPIC).await;
    let tv_before = h.topology_version(&gid).await;

    broker.leave(&c, sub_second.subscription_id).await.unwrap();

    let tv_after = h.topology_version(&gid).await;
    assert!(tv_after > tv_before, "leave must bump topology_version");
    let surviving_slots = h.assignment(sub_first.subscription_id).await;
    assert_eq!(
        surviving_slots.len(),
        4,
        "surviving member must own all 4 partitions"
    );
}

#[tokio::test]
async fn expire_subscription_triggers_rebalance() {
    // C6: crash (session_timeout) → rebalance.
    let (broker, h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub_first = join_group(&c, &broker, &gid, TOPIC).await;
    let sub_second = join_group(&c, &broker, &gid, TOPIC).await;

    h.expire_subscription(sub_first.subscription_id).await;

    let sub_second_slots = h.assignment(sub_second.subscription_id).await;
    assert_eq!(
        sub_second_slots.len(),
        2,
        "after sub_first expires, sub_second must own all partitions"
    );
}

#[tokio::test]
async fn sticky_cursor_on_handoff() {
    // Scenario A: cursor.acked survives partition handoff.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub_first = join_group(&c, &broker, &gid, TOPIC).await;

    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();
    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();

    // sub_first acks offset 0.
    let mut offsets: HashMap<String, HashMap<u32, i64>> = HashMap::new();
    offsets.entry(TOPIC.to_owned()).or_default().insert(0, 0);
    broker
        .ack(&c, sub_first.subscription_id, &offsets)
        .await
        .unwrap();
    assert_eq!(h.cursor(&gid, TOPIC, 0).await, Some(0));

    // sub_first crashes; sub_second joins and inherits cursor.
    h.expire_subscription(sub_first.subscription_id).await;
    let sub_second = join_group(&c, &broker, &gid, TOPIC).await;
    let slot = sub_second
        .assigned
        .iter()
        .find(|s| s.topic == TOPIC && s.partition == 0);
    if let Some(slot) = slot {
        assert_eq!(
            slot.offset, 0,
            "sub_second must start from sub_first's cursor.acked=0"
        );
    }
}
