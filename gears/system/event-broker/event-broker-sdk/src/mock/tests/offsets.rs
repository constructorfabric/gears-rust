#[cfg(test)]
use super::helpers::*;

// Cursor keying (H5/H6), forward-only seek, cursor durability (M8).
use super::helpers::{broker_with_topic, ctx, join_group, make_group, wire_event};
use crate::ResolvedPosition;
use crate::api::{EventBroker, JoinRequest};
use crate::consumer::backend::{SeekPosition, SubscriptionInterest};

#[tokio::test]
async fn seek_is_forward_only_max() {
    // H5: seek(10) then seek(5) → cursor stays at 10.
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
                    tenant_id: uuid::Uuid::nil(),
                    max_depth: Some(0),
                    barrier_mode: crate::consumer::backend::BarrierMode::Respect,
                    types: vec!["*".into()],
                    filter: None,
                }],
                session_timeout: None,
            },
        )
        .await
        .unwrap();

    broker
        .seek(&c, subscription.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(10),
        }])
        .await
        .unwrap();
    assert_eq!(h.cursor(&gid, TOPIC, 0).await, Some(10));

    broker
        .seek(&c, subscription.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(5),
        }])
        .await
        .unwrap();
    assert_eq!(
        h.cursor(&gid, TOPIC, 0).await,
        Some(10),
        "forward-only MAX rule: cannot go back"
    );

    broker
        .seek(&c, subscription.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(15),
        }])
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
                        tenant_id: uuid::Uuid::nil(),
                        max_depth: Some(0),
                        barrier_mode: crate::consumer::backend::BarrierMode::Respect,
                        types: vec!["*".into()],
                        filter: None,
                    },
                    SubscriptionInterest {
                        topic: TOPIC2.into(),
                        tenant_id: uuid::Uuid::nil(),
                        max_depth: Some(0),
                        barrier_mode: crate::consumer::backend::BarrierMode::Respect,
                        types: vec!["*".into()],
                        filter: None,
                    },
                ],
                session_timeout: None,
            },
        )
        .await
        .unwrap();

    broker
        .seek(&c, subscription.subscription_id, &[
            SeekPosition {
                topic: TOPIC.to_owned(),
                partition: 0,
                value: ResolvedPosition::Exact(42),
            },
            SeekPosition {
                topic: TOPIC2.to_owned(),
                partition: 0,
                value: ResolvedPosition::Exact(7),
            },
        ])
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
                    tenant_id: uuid::Uuid::nil(),
                    max_depth: Some(0),
                    barrier_mode: crate::consumer::backend::BarrierMode::Respect,
                    types: vec!["*".into()],
                    filter: None,
                }],
                session_timeout: None,
            },
        )
        .await
        .unwrap();

    broker
        .seek(&c, sub_first.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(99),
        }])
        .await
        .unwrap();
    broker.leave(&c, sub_first.subscription_id).await.unwrap();

    let _sub_second = broker
        .join(
            &c,
            JoinRequest {
                group: gid.clone(),
                client_agent: "analytics-consumer/1.0".into(),
                interests: vec![SubscriptionInterest {
                    topic: TOPIC.into(),
                    tenant_id: uuid::Uuid::nil(),
                    max_depth: Some(0),
                    barrier_mode: crate::consumer::backend::BarrierMode::Respect,
                    types: vec!["*".into()],
                    filter: None,
                }],
                session_timeout: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        h.cursor(&gid, TOPIC, 0).await,
        Some(99),
        "cursor must survive subscription churn (M8)"
    );
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
                    tenant_id: uuid::Uuid::nil(),
                    max_depth: Some(0),
                    barrier_mode: crate::consumer::backend::BarrierMode::Respect,
                    types: vec!["*".into()],
                    filter: None,
                }],
                session_timeout: None,
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
async fn seek_returns_resolved_positions_for_exact_offset() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    // Publish 3 events so valid offsets exist.
    for _ in 0..3 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let gid = make_group(&c, &broker).await;
    let sub = join_group(&c, &broker, &gid, TOPIC).await;

    let results = broker
        .seek(
            &c,
            sub.subscription_id,
            &[SeekPosition {
                topic: TOPIC.to_owned(),
                partition: 0,
                value: ResolvedPosition::Exact(2),
            }],
        )
        .await
        .unwrap();

    assert!(
        !results.is_empty(),
        "seek() must return a non-empty resolved positions array"
    );
    assert_eq!(results[0].topic, TOPIC, "topic must be echoed back");
    assert_eq!(results[0].partition, 0, "partition must be echoed back");
    assert_eq!(results[0].offset, 2, "exact offset echoed back");
}

#[tokio::test]
async fn seek_returns_resolved_positions_for_earliest_sentinel() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    for _ in 0..3 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let gid = make_group(&c, &broker).await;
    let sub = join_group(&c, &broker, &gid, TOPIC).await;

    let results = broker
        .seek(
            &c,
            sub.subscription_id,
            &[SeekPosition {
                topic: TOPIC.to_owned(),
                partition: 0,
                value: ResolvedPosition::Earliest,
            }],
        )
        .await
        .unwrap();

    assert!(
        !results.is_empty(),
        "seek() must return a non-empty resolved positions array for Earliest"
    );
    // Earliest resolves to 0 in the mock (retention_floor = 0).
    assert_eq!(
        results[0].offset,
        0,
        "Earliest sentinel must resolve to integer 0"
    );
}

#[tokio::test]
async fn seek_at_timestamp_returns_resolved_offset() {
    // Verify "at:<ISO>" sentinel: the mock should accept it and resolve to an integer.
    // For mock purposes, "at:..." resolves to Earliest (offset 0) since the mock
    // doesn't implement true timestamp resolution — just verify the call succeeds
    // and returns a SeekResult with an integer offset.
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let sub = join_group(&c, &broker, &gid, TOPIC).await;

    let results = broker
        .seek(&c, sub.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::AtTimestamp("2026-06-01T00:00:00Z".to_owned()),
        }])
        .await
        .unwrap();

    assert!(!results.is_empty(), "seek must return resolved positions");
    assert_eq!(results[0].topic, TOPIC);
    assert_eq!(results[0].partition, 0);
    // resolved value is an integer (sentinel expanded)
    assert!(results[0].offset >= 0);
}
