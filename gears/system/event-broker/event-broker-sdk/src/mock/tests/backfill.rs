#[cfg(test)]
use super::helpers::*;

// Backfill (seek Earliest + catch-up) and C7 replay group.
use super::helpers::{broker_with_topic, ctx, make_group, wire_event};
use crate::ResolvedPosition;
use crate::api::EventBroker;
use crate::api::JoinRequest;
use crate::consumer::backend::{SeekPosition, SubscriptionInterest, WireFrame};
use futures_util::StreamExt;

#[tokio::test]
async fn seek_earliest_delivers_all_historical() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    // Pre-publish 3 events.
    for _ in 0..3 {
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

    let mut s = broker
        .stream(&c, subscription.subscription_id)
        .await
        .unwrap();
    let mut events = 0;
    for _ in 0..10 {
        match tokio::time::timeout(std::time::Duration::from_millis(50), s.next()).await {
            Ok(Some(Ok(WireFrame::Event(_)))) => {
                events += 1;
                if events == 3 {
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    assert_eq!(
        events, 3,
        "seek Earliest must deliver all 3 historical events"
    );
}

#[tokio::test]
async fn replay_group_does_not_perturb_primary() {
    // C7: dedicated replay group has its own cursor, primary group unaffected.
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    for _ in 0..5 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }

    let primary = make_group(&c, &broker).await;
    let replay = make_group(&c, &broker).await;

    let primary_sub = broker
        .join(
            &c,
            JoinRequest {
                group: primary.clone(),
                client_agent: "primary-consumer/1.0".into(),
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

    // Primary seeks to offset 4 (processed all 5 events).
    broker
        .seek(&c, primary_sub.subscription_id, &[SeekPosition {
            topic: TOPIC.to_owned(),
            partition: 0,
            value: ResolvedPosition::Exact(4),
        }])
        .await
        .unwrap();

    // Replay group seeks to Earliest — should not touch primary cursor.
    let replay_sub = broker
        .join(
            &c,
            JoinRequest {
                group: replay.clone(),
                client_agent: "replay-consumer/1.0".into(),
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
            replay_sub.subscription_id,
            &[SeekPosition {
                topic: TOPIC.to_owned(),
                partition: 0,
                value: ResolvedPosition::Earliest,
            }],
        )
        .await
        .unwrap();

    // Primary cursor unchanged.
    assert_eq!(
        h.cursor(&primary, TOPIC, 0).await,
        Some(4),
        "replay group must not perturb primary cursor"
    );
}
