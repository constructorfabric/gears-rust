//! Retention by the backend's own workers (feature 0007 §2.6), ageing events
//! by when the backend stored them - never by the producer's `occurred_at`.

use std::time::Duration;

use chrono::Utc;
use event_broker_sdk::backend::{
    Backend, LogClaim, LogId, Partition, Replacing, Retention, TopicPartition, TopicSpec,
};
use event_broker_sdk::{Position, Sequence};
use tokio_util::sync::CancellationToken;

use crate::fixture::{
    KEEP, Writer, event, event_at, provisioned, read_all, sequences, settle, topic, wait_for_count,
};
use crate::harness::Harness;

const SHORT: Retention = Retention {
    max_age: Some(Duration::from_secs(1)),
    max_bytes: None,
};

/// Waits until `at` holds nothing, within the harness's retention deadline.
async fn until_empty(harness: &dyn Harness, backend: &dyn Backend, at: &TopicPartition) {
    let deadline = tokio::time::Instant::now() + harness.retention_deadline();
    loop {
        let left = read_all(backend, at).await;
        if left.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "events {:?} still stored after {:?}",
            sequences(&left),
            harness.retention_deadline()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// With nobody driving it, the backend removes events stored longer ago than
/// the age its topic was provisioned with; `Latest` stays where it was.
pub async fn retention_by_age_runs_on_its_own(harness: &dyn Harness) {
    let backend = harness.build(CancellationToken::new()).await;
    let spec = provisioned(backend.as_ref(), "by_age", 1, SHORT).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..3).map(|n| event("k", n)).collect();
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &batch)
        .await;

    until_empty(harness, backend.as_ref(), &at).await;

    assert_eq!(
        backend
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        Sequence::assigned(3)
    );
}

/// With a byte bound smaller than any event, the backend removes every event,
/// oldest first.
pub async fn retention_by_bytes_runs_on_its_own(harness: &dyn Harness) {
    let backend = harness.build(CancellationToken::new()).await;
    let tiny = Retention {
        max_age: None,
        max_bytes: Some(1),
    };
    let spec = provisioned(backend.as_ref(), "by_bytes", 1, tiny).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..3).map(|n| event("k", n)).collect();
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &batch)
        .await;

    until_empty(harness, backend.as_ref(), &at).await;

    assert_eq!(
        backend
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        Sequence::assigned(3)
    );
}

/// An event back-dated by years, stored a moment ago under a one-hour age,
/// is kept for as long as retention takes to act on anything.
pub async fn retention_ignores_occurred_at(harness: &dyn Harness) {
    let backend = harness.build(CancellationToken::new()).await;
    let hour = Retention {
        max_age: Some(Duration::from_hours(1)),
        max_bytes: None,
    };
    let spec = provisioned(backend.as_ref(), "back_dated", 1, hour).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let year_2000 = chrono::DateTime::from_timestamp(946_684_800, 0).expect("valid instant");
    Writer::new()
        .append(
            backend.as_ref(),
            &spec.topic,
            &[event_at("k", 0, year_2000)],
        )
        .await;
    wait_for_count(backend.as_ref(), &at, 1).await;

    tokio::time::sleep(harness.retention_deadline()).await;

    assert_eq!(sequences(&read_all(backend.as_ref(), &at).await), [1]);
    assert!(Utc::now() > year_2000);
}

/// Provisioning again under the same log id with a shorter age makes the
/// backend adopt it: events kept under the old bound are then removed.
pub async fn reprovisioning_adopts_new_bounds(harness: &dyn Harness) {
    let backend = harness.build(CancellationToken::new()).await;
    let log = LogId::new_v4();
    let mut spec = TopicSpec {
        topic: topic("rebound"),
        partitions: 1,
        retention: KEEP,
    };
    settle(
        backend.as_ref(),
        &spec,
        &LogClaim {
            replacing: Replacing::Nothing,
            new: log,
        },
    )
    .await
    .expect("provision");
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &[event("k", 0)])
        .await;
    wait_for_count(backend.as_ref(), &at, 1).await;

    spec.retention = SHORT;
    settle(
        backend.as_ref(),
        &spec,
        &LogClaim {
            replacing: Replacing::Log(log),
            new: log,
        },
    )
    .await
    .expect("provision again");

    until_empty(harness, backend.as_ref(), &at).await;
}
