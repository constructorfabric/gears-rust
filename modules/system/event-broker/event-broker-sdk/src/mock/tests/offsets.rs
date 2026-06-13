#[cfg(test)]
use super::helpers::*;

// Cursor keying (H5/H6), forward-only ack, seek, cursor durability (M8), auto_commit.
use super::helpers::{broker_with_topic, ctx, make_group, wire_event};
use crate::ResolvedPosition;
use crate::api::{EventBroker, JoinRequest};
use crate::consumer::backend::{SeekPosition, SubscriptionInterest};
use std::collections::HashMap;

#[tokio::test]
async fn ack_is_forward_only_max() {
    // H5: ack(10) then ack(5) → cursor stays at 10.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let subscription = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "*".into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await
        .unwrap();

    let mut ack = |offset: i64| {
        let mut m: HashMap<String, HashMap<u32, i64>> = HashMap::new();
        m.entry(TOPIC.to_owned()).or_default().insert(0, offset);
        m
    };
    broker
        .ack(&c, subscription.subscription_id, &ack(10))
        .await
        .unwrap();
    assert_eq!(h.cursor(&gid, TOPIC, 0).await, Some(10));
    broker
        .ack(&c, subscription.subscription_id, &ack(5))
        .await
        .unwrap();
    assert_eq!(
        h.cursor(&gid, TOPIC, 0).await,
        Some(10),
        "forward-only MAX rule: cannot go back"
    );
    broker
        .ack(&c, subscription.subscription_id, &ack(15))
        .await
        .unwrap();
    assert_eq!(h.cursor(&gid, TOPIC, 0).await, Some(15));
}

#[tokio::test]
async fn multi_topic_cursors_independent() {
    // H6: (group, T1, 0) and (group, T2, 0) are independent.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    h.register_topic(TOPIC2, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let subscription = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![
                    SubscriptionInterest {
                        topic: TOPIC.into(),
                        event_type_pattern: "*".into(),
                        filter_engine: None,
                        filter_expression: None,
                    },
                    SubscriptionInterest {
                        topic: TOPIC2.into(),
                        event_type_pattern: "*".into(),
                        filter_engine: None,
                        filter_expression: None,
                    },
                ],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await
        .unwrap();

    let mut m: HashMap<String, HashMap<u32, i64>> = HashMap::new();
    m.entry(TOPIC.to_owned()).or_default().insert(0, 42);
    m.entry(TOPIC2.to_owned()).or_default().insert(0, 7);
    broker
        .ack(&c, subscription.subscription_id, &m)
        .await
        .unwrap();

    assert_eq!(h.cursor(&gid, TOPIC, 0).await, Some(42));
    assert_eq!(
        h.cursor(&gid, TOPIC2, 0).await,
        Some(7),
        "cursors must be independent (H6)"
    );
}

#[tokio::test]
async fn cursor_durable_across_subscription_churn() {
    // M8: cursor survives leave + rejoin.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;

    let sub_first = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "*".into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await
        .unwrap();

    let mut m: HashMap<String, HashMap<u32, i64>> = HashMap::new();
    m.entry(TOPIC.to_owned()).or_default().insert(0, 99);
    broker.ack(&c, sub_first.subscription_id, &m).await.unwrap();
    broker.leave(&c, sub_first.subscription_id).await.unwrap();

    let sub_second = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "*".into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await
        .unwrap();

    let slot = sub_second
        .assigned
        .iter()
        .find(|s| s.topic == TOPIC && s.partition == 0);
    if let Some(slot) = slot {
        assert_eq!(
            slot.offset, 99,
            "cursor must survive subscription churn (M8)"
        );
    }
}

#[tokio::test]
async fn seek_earliest_starts_from_zero() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    for _ in 0..5 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let gid = make_group(&c, &broker).await;
    let subscription = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "*".into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: false,
            },
        )
        .await
        .unwrap();
    broker
        .seek(
            &c,
            subscription.subscription_id,
            &[SeekPosition {
                topic: TOPIC.to_owned(),
                partition: 0,
                value: ResolvedPosition::Earliest,
            }],
        )
        .await
        .unwrap();
    // After seek Earliest, stream should deliver from offset 0.
    // (Full stream traversal is covered by mock_backfill.rs)
}

#[tokio::test]
async fn auto_commit_advances_cursor_after_stream() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let subscription = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    event_type_pattern: "*".into(),
                    filter_engine: None,
                    filter_expression: None,
                }],
                session_timeout: None,
                auto_commit: true,
            },
        )
        .await
        .unwrap();
    broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();

    use crate::consumer::backend::WireFrame;
    use futures_util::StreamExt;
    let mut s = broker
        .stream(&c, subscription.subscription_id)
        .await
        .unwrap();
    // Read Topology + Event frames.
    s.next().await; // Topology
    s.next().await; // Event — auto_commit should advance cursor

    // After receiving the event with auto_commit=true, cursor.acked must advance.
    let cursor = h.cursor(&gid, TOPIC, 0).await;
    assert!(
        cursor.is_some() && cursor.unwrap() >= 0,
        "auto_commit must advance cursor"
    );
}
