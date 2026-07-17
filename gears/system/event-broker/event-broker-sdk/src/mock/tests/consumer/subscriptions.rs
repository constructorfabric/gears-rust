//! Mirrors scenarios/consumer/subscriptions/. Tests migrated per mock-reference-alignment.
use super::super::helpers::*;
#[cfg(test)]
use toolkit_gts::gts_id;

use crate::api::{BarrierMode, Filter, SubscriptionInterest};
use crate::api::{EventBroker, JoinRequest};
use crate::error::EventBrokerError;
use crate::ids::SubscriptionId;
use uuid::Uuid;

/// Scenario: consumer/subscriptions/1.01-positive-cold-join-fresh-group.md
#[tokio::test]
async fn s1_01_positive_cold_join_fresh_group() {
    // 4-partition topic, fresh single-member group → all 4 partitions, topology_version 1.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    assert_eq!(h.topology_version(&gid).await, 0, "fresh group starts at 0");

    let assignment = join_group(&c, &broker, &gid, TOPIC).await;

    assert_eq!(
        assignment.assigned.len(),
        4,
        "sole member of a 4-partition topic owns all partitions"
    );
    assert_eq!(assignment.topology_version, 1);
    // Side effect: the group's topology_version advances 0 → 1.
    assert_eq!(h.topology_version(&gid).await, 1);
}

/// Scenario: consumer/subscriptions/1.02-positive-join-multi-topic-interests.md
#[tokio::test]
async fn s1_02_positive_join_multi_topic_interests() {
    // Two topics with 2 partitions each; sole member owns all 4 (topic, partition) pairs.
    let (broker, h) = broker_with_topic(TOPIC, 2).await;
    h.register_topic(TOPIC2, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let mk_interest = |topic: &str| SubscriptionInterest {
        topic: topic.to_owned(),
        tenant_id: Uuid::nil(),
        tenant_depth: crate::api::TenantTraversalDepth::CurrentTenant,
        barrier_mode: BarrierMode::Respect,
        types: vec!["*".to_owned()],
        filter: None,
    };

    let assignment = broker
        .join(
            &c,
            JoinRequest {
                group: gid,
                client_agent: "fulfilment-worker/2.0.0".to_owned(),
                interests: vec![mk_interest(TOPIC), mk_interest(TOPIC2)],
                session_timeout: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        assignment.assigned.len(),
        4,
        "assignment must span all partitions of both topics (2 + 2)"
    );
    let t1 = assignment
        .assigned
        .iter()
        .filter(|a| a.topic == TOPIC)
        .count();
    let t2 = assignment
        .assigned
        .iter()
        .filter(|a| a.topic == TOPIC2)
        .count();
    assert_eq!(t1, 2, "both partitions of TOPIC are assigned");
    assert_eq!(t2, 2, "both partitions of TOPIC2 are assigned");
    assert_eq!(assignment.topology_version, 1);
}

/// Scenario: consumer/subscriptions/1.03-positive-join-with-typed-filter.md
#[tokio::test]
async fn s1_03_positive_join_with_typed_filter() {
    // An interest carries a compiled filter; the JOIN is accepted and the member streams.
    let (broker, _h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let assignment = broker
        .join(
            &c,
            JoinRequest {
                group: gid,
                client_agent: "high-value-worker/1.0.0".to_owned(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.to_owned(),
                    tenant_id: Uuid::nil(),
                    tenant_depth: crate::api::TenantTraversalDepth::CurrentTenant,
                    barrier_mode: BarrierMode::Respect,
                    types: vec![EVT.to_owned()],
                    filter: Some(Filter {
                        engine: gts_id!("cf.core.events.filter.v1~cf.core.expression.cel.v1")
                            .to_owned(),
                        expression: "event.data.total_cents > 100000".to_owned(),
                    }),
                }],
                session_timeout: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        assignment.assigned.len(),
        2,
        "filtered member still owns all partitions (filter applies on delivery, not assignment)"
    );
    assert_eq!(assignment.topology_version, 1);
}

/// Scenario: consumer/subscriptions/1.04-positive-parallelism-multiple-subscriptions.md
#[tokio::test]
async fn s1_04_positive_parallelism_multiple_subscriptions() {
    // Two JOINs on a 4-partition topic split the partitions 2/2 (disjoint, full cover).
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let sub_a = join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(sub_a.assigned.len(), 4, "first member owns all 4");
    assert_eq!(h.topology_version(&gid).await, 1);

    let sub_b = join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(sub_b.topology_version, 2);
    assert_eq!(h.topology_version(&gid).await, 2, "topology advances 1 → 2");

    let a_slots = h.assignment(sub_a.subscription_id).await;
    let b_slots = h.assignment(sub_b.subscription_id).await;
    assert_eq!(a_slots.len(), 2, "sub_a reduced to 2 partitions");
    assert_eq!(b_slots.len(), 2, "sub_b assigned 2 partitions");
    // Disjoint and together covering all 4 exactly once.
    for p in &a_slots {
        assert!(!b_slots.contains(p), "partition {p:?} must not be in both");
    }
    let mut all: Vec<_> = a_slots.iter().chain(b_slots.iter()).cloned().collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 4, "the 2/2 split covers all 4 partitions");
}

/// Scenario: consumer/subscriptions/1.05-positive-leave-subscription.md
#[tokio::test]
async fn s1_05_positive_leave_subscription() {
    // LEAVE removes the subscription and releases its partitions back to the group.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub_a = join_group(&c, &broker, &gid, TOPIC).await;
    let sub_b = join_group(&c, &broker, &gid, TOPIC).await;
    let tv_before = h.topology_version(&gid).await;

    broker.leave(&c, sub_b.subscription_id).await.unwrap();

    // Side effects: sub_b is gone; surviving member inherits all partitions; topology advances.
    assert!(
        broker
            .get_subscription(&c, sub_b.subscription_id)
            .await
            .is_err(),
        "left subscription must be absent"
    );
    assert!(
        h.topology_version(&gid).await > tv_before,
        "LEAVE must bump topology_version for survivors"
    );
    assert_eq!(
        h.assignment(sub_a.subscription_id).await.len(),
        4,
        "released partitions return to the surviving member"
    );
}

/// Scenario: consumer/subscriptions/1.08-negative-leave-unknown-subscription.md
#[tokio::test]
async fn s1_08_negative_leave_unknown_subscription() {
    // Leaving an unknown/expired subscription is rejected with 404 SubscriptionNotFound.
    let broker = crate::mock::MockBroker::new();
    let c = ctx();
    let unknown = SubscriptionId(Uuid::parse_str("99999999-8888-7777-6666-555555555555").unwrap());

    let err = broker
        .leave(&c, unknown)
        .await
        .expect_err("leaving an unknown subscription must be rejected");
    assert!(
        matches!(
            err,
            crate::error::EventBrokerError::SubscriptionNotFound { .. }
        ),
        "expected SubscriptionNotFound, got {err:?}"
    );
}

/// Scenario: consumer/subscriptions/1.09-positive-list-subscriptions.md
#[tokio::test]
async fn s1_09_positive_list_subscriptions() {
    let (broker, _h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub = join_group(&c, &broker, &gid, TOPIC).await;

    let list = broker.list_subscriptions(&c).await.unwrap();
    assert_eq!(list.len(), 1, "exactly one active subscription is listed");
    let item = &list[0];
    assert_eq!(item.id, sub.subscription_id);
    assert_eq!(item.consumer_group, gid);
    assert_eq!(item.assigned.len(), 4);
    assert_eq!(item.topology_version, 1);
}

/// Scenario: consumer/subscriptions/1.10-positive-read-subscription.md
#[tokio::test]
async fn s1_10_positive_read_subscription() {
    let (broker, _h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub = join_group(&c, &broker, &gid, TOPIC).await;

    let record = broker
        .get_subscription(&c, sub.subscription_id)
        .await
        .unwrap();
    assert_eq!(record.id, sub.subscription_id);
    assert_eq!(record.consumer_group, gid);
    assert_eq!(
        record.assigned.len(),
        4,
        "sole member owns all 4 partitions"
    );
    assert_eq!(record.topology_version, 1);
}

/// Scenario: consumer/subscriptions/1.11-positive-second-join-triggers-rebalance.md
#[tokio::test]
async fn s1_11_positive_second_join_triggers_rebalance() {
    // Second JOIN rebalances a 4-partition topic to a 2+2 split; topology advances to 2.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let sub_a = join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(h.assignment(sub_a.subscription_id).await.len(), 4);

    let sub_b = join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(
        sub_b.assigned.len(),
        2,
        "new member gets half the partitions"
    );
    assert_eq!(sub_b.topology_version, 2);

    // Side effects: sub_a reduced to 2 at topology_version 2; sub_b holds the other 2; disjoint.
    assert_eq!(h.assignment(sub_a.subscription_id).await.len(), 2);
    assert_eq!(h.topology_version(&gid).await, 2);
    let a_slots = h.assignment(sub_a.subscription_id).await;
    let b_slots = h.assignment(sub_b.subscription_id).await;
    for p in &a_slots {
        assert!(
            !b_slots.contains(p),
            "single-consumer-per-partition invariant"
        );
    }
}

/// Scenario: consumer/subscriptions/1.12-positive-third-join-triggers-rebalance.md
#[tokio::test]
async fn s1_12_positive_third_join_triggers_rebalance() {
    // Third JOIN redistributes 4 partitions across 3 members; topology advances to 3.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let sub_a = join_group(&c, &broker, &gid, TOPIC).await;
    let sub_b = join_group(&c, &broker, &gid, TOPIC).await;
    let sub_c = join_group(&c, &broker, &gid, TOPIC).await;

    assert_eq!(sub_c.topology_version, 3);
    assert_eq!(h.topology_version(&gid).await, 3);

    let a_slots = h.assignment(sub_a.subscription_id).await;
    let b_slots = h.assignment(sub_b.subscription_id).await;
    let c_slots = h.assignment(sub_c.subscription_id).await;

    // Each member holds at least one partition; the single-consumer-per-partition
    // invariant holds and all 4 partitions are covered exactly once.
    assert!(!a_slots.is_empty() && !b_slots.is_empty() && !c_slots.is_empty());
    let mut all: Vec<_> = a_slots
        .iter()
        .chain(b_slots.iter())
        .chain(c_slots.iter())
        .cloned()
        .collect();
    let total = all.len();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 4, "all 4 partitions covered");
    assert_eq!(total, 4, "no partition owned by more than one member");
}

/// Scenario: consumer/subscriptions/1.13-negative-join-group-at-capacity.md
#[tokio::test]
async fn s1_13_negative_join_group_at_capacity() {
    // A 1-partition topic admits exactly one member; a second JOIN that would receive
    // zero partitions is refused with GroupAtCapacity (the 429 wire refusal).
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    // First member fills the only partition slot.
    let _sub_a = join_group(&c, &broker, &gid, TOPIC).await;

    // Second JOIN is refused - no partition slot available.
    let err = broker
        .join(
            &c,
            JoinRequest {
                group: gid,
                client_agent: "order-worker/1.4.0".to_owned(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.to_owned(),
                    tenant_id: Uuid::nil(),
                    tenant_depth: crate::api::TenantTraversalDepth::CurrentTenant,
                    barrier_mode: BarrierMode::Respect,
                    types: vec!["*".to_owned()],
                    filter: None,
                }],
                session_timeout: None,
            },
        )
        .await
        .unwrap_err();

    match err {
        EventBrokerError::GroupAtCapacity {
            active, partitions, ..
        } => {
            assert_eq!(active, 1, "one active member already holds the partition");
            assert_eq!(partitions, 1, "the topic has a single partition");
        }
        other => panic!("expected GroupAtCapacity, got {other:?}"),
    }

    // Side effect: no subscription is created (no zero-partition standby).
    assert_eq!(
        broker.list_subscriptions(&c).await.unwrap().len(),
        1,
        "the refused JOIN must not create a second subscription"
    );
}

/// Scenario: consumer/subscriptions/1.07-negative-join-too-many-interests.md
#[tokio::test]
async fn s1_07_negative_join_too_many_interests() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    // 65 interests exceeds the per-subscription cap of 64.
    let interests: Vec<SubscriptionInterest> = (0..65)
        .map(|_| SubscriptionInterest {
            topic: TOPIC.to_owned(),
            tenant_id: Uuid::nil(),
            tenant_depth: crate::api::TenantTraversalDepth::CurrentTenant,
            barrier_mode: BarrierMode::Respect,
            types: vec!["*".to_owned()],
            filter: None,
        })
        .collect();
    let err = broker
        .join(
            &c,
            JoinRequest {
                group: gid,
                client_agent: "test-consumer/1.0".to_owned(),
                interests,
                session_timeout: None,
            },
        )
        .await
        .expect_err("more than 64 interests must be rejected");
    assert!(
        matches!(err, EventBrokerError::InvalidEventField { field, .. } if field == "interests"),
        "expected InvalidEventField(interests), got {err:?}"
    );
}
