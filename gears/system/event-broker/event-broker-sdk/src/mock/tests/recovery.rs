#[cfg(test)]
use super::helpers::*;

// 410/404 fault injection and stream termination.
use super::helpers::{broker_with_topic, ctx, join_group, make_group, wire_event};
use crate::api::EventBroker;
use crate::consumer::backend::WireFrame;
use futures_util::StreamExt;

#[tokio::test]
async fn inject_gone_terminates_stream() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let subscription = join_group(&c, &broker, &gid, TOPIC).await;
    h.inject_gone(subscription.subscription_id).await;

    let mut s = broker
        .stream(&c, subscription.subscription_id)
        .await
        .unwrap();
    let mut got_error = false;
    for _ in 0..10 {
        match s.next().await {
            Some(Err(_)) => {
                got_error = true;
                break;
            }
            Some(Ok(_)) => {}
            None => break,
        }
    }
    assert!(got_error, "inject_gone must cause stream to error");
}

#[tokio::test]
async fn inject_not_found_terminates_stream() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    let subscription = join_group(&c, &broker, &gid, TOPIC).await;
    h.inject_not_found(subscription.subscription_id).await;

    let mut s = broker
        .stream(&c, subscription.subscription_id)
        .await
        .unwrap();
    let mut got_error = false;
    for _ in 0..10 {
        match s.next().await {
            Some(Err(_)) => {
                got_error = true;
                break;
            }
            Some(Ok(_)) => {}
            None => break,
        }
    }
    assert!(got_error, "inject_not_found must cause stream to error");
}

/// Topology frame on rebalance is tested by verifying group state directly
/// (stream topology emission timing is non-deterministic in unit tests;
/// end-to-end tests cover the full stream topology flow).
#[tokio::test]
async fn rebalance_bumps_topology_version() {
    let (broker, h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let gid = make_group(&c, &broker).await;
    join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(h.topology_version(&gid).await, 1);
    join_group(&c, &broker, &gid, TOPIC).await;
    assert_eq!(h.topology_version(&gid).await, 2);
}
