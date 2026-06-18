#[cfg(test)]
use super::helpers::*;

use super::helpers::{broker_with_topic, ctx, wire_event};
use crate::api::EventBroker;
use crate::producer::backend::IngestOutcome;

#[tokio::test]
async fn single_publish_accepted() {
    let (broker, _h) = broker_with_topic(TOPIC, 4).await;
    let c = ctx();
    let outcome = broker
        .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Accepted);
}

#[tokio::test]
async fn offsets_are_monotonic_per_partition() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    for _ in 0..5 {
        broker
            .publish(&c, &wire_event(TOPIC, EVT, c.subject_tenant_id()))
            .await
            .unwrap();
    }
    let stored = h.stored(TOPIC, 0).await;
    assert_eq!(stored.len(), 5);
    for (i, se) in stored.iter().enumerate() {
        assert_eq!(se.event.offset.unwrap(), i as i64);
    }
}

#[tokio::test]
async fn publish_unknown_topic_errors() {
    let broker = crate::mock::MockBroker::new();
    let c = ctx();
    let err = broker
        .publish(
            &c,
            &wire_event(
                "gts.cf.core.events.topic.v1~test.mock.broker.noexist.v1",
                EVT,
                c.subject_tenant_id(),
            ),
        )
        .await
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("gts.cf.core.events.topic.v1~test.mock.broker.noexist.v1")
            || msg.contains("TopicNotFound")
            || msg.contains("not found"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
async fn batch_homogeneous_accepted() {
    let (broker, h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let evs: Vec<_> = (0..3)
        .map(|_| wire_event(TOPIC, EVT, c.subject_tenant_id()))
        .collect();
    let outcomes = broker.publish_batch(&c, &evs).await.unwrap();
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(|o| *o == IngestOutcome::Accepted));
    assert_eq!(h.stored(TOPIC, 0).await.len(), 3);
}

#[tokio::test]
async fn batch_mixed_partitions_rejected() {
    use crate::internal_test_helpers::partition_for;
    let (broker, _h) = broker_with_topic(TOPIC, 2).await;
    let c = ctx();
    let p0 = partition_for(&c.subject_tenant_id().to_string(), 2);
    let other = if p0 == 0 { "other-one" } else { "other-zero" };
    if partition_for(other, 2) != p0 {
        let mut ev2 = wire_event(TOPIC, EVT, c.subject_tenant_id());
        ev2.partition_key = Some(other.to_owned());
        let err = broker
            .publish_batch(&c, &[wire_event(TOPIC, EVT, c.subject_tenant_id()), ev2])
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("mixed") || format!("{err:?}").contains("partition"));
    }
}

#[tokio::test]
async fn producer_cursors_empty_before_publish() {
    let (broker, _h) = broker_with_topic(TOPIC, 1).await;
    let c = ctx();
    let pid = broker
        .register_producer(&c, "stateless", "agent/1.0")
        .await
        .unwrap();
    assert!(broker.producer_cursors(&c, pid).await.unwrap().is_empty());
}
