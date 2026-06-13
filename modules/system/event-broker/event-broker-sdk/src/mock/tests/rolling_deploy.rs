#[cfg(test)]
use super::helpers::*;

// C8/C8a: per-member filter isolation, rolling deploy with different filters.
use super::helpers::{broker_with_topic, ctx, make_group};
use crate::api::{EventBroker, JoinRequest};
use crate::consumer::backend::SubscriptionInterest;

#[tokio::test]
async fn two_members_different_filters_both_join() {
    let (broker, _h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    // Member 1: filter on vendor.foo.v1
    let sub_v1 = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "audit-service/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "gts.cf.core.events.event_type.v1~test.mock.broker.foo.v1"
                        .into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await;
    assert!(
        sub_v1.is_ok(),
        "member with filter foo must join successfully"
    );

    // Member 2: filter on vendor.bar.v1 (different — no GroupFilterMismatch)
    let sub_v2 = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "audit-service/2.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "gts.cf.core.events.event_type.v1~test.mock.broker.bar.v1"
                        .into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await;
    assert!(
        sub_v2.is_ok(),
        "member with different filter (bar) must also join — no GroupFilterMismatch"
    );
}

#[tokio::test]
async fn rolling_deploy_all_partitions_covered() {
    // C8a: during rolling deploy, together all partitions are assigned.
    let (broker, h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let sub_v1 = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "audit-service/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "gts.cf.core.events.event_type.v1~test.mock.broker.*.v1"
                        .into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await
        .unwrap();
    let sub_v2 = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "audit-service/2.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "gts.cf.core.events.event_type.v1~test.mock.broker.v2.*"
                        .into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await
        .unwrap();

    let v1_slots = h.assignment(sub_v1.subscription_id).await;
    let v2_slots = h.assignment(sub_v2.subscription_id).await;
    let all: std::collections::HashSet<_> = v1_slots.iter().chain(v2_slots.iter()).collect();
    assert_eq!(all.len(), 4, "together v1+v2 must cover all 4 partitions");
    // No overlap.
    for p in &v1_slots {
        assert!(
            !v2_slots.contains(p),
            "single-consumer-per-partition invariant violated"
        );
    }
}
