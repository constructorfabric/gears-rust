//! Building and stopping a backend (feature 0007 §2.7).

use std::time::Duration;

use event_broker_sdk::Sequence;
use event_broker_sdk::backend::{BackendError, Partition, Retention, TopicPartition, TopicSpec};
use tokio_util::sync::CancellationToken;

use crate::fixture::{
    KEEP, Writer, event, provisioned, read_all, sequences, topic, wait_for_count,
};
use crate::harness::{Harness, Unreachable};

/// A backend over a medium that is down builds, and answers each operation
/// `Unavailable` instead of failing the start.
pub async fn build_succeeds_with_the_medium_unreachable(harness: &dyn Harness) {
    let backend = match harness.build_unreachable(CancellationToken::new()).await {
        Unreachable::Backend(backend) => backend,
        Unreachable::NotApplicable { reason } => {
            println!(
                "conformance build_succeeds_with_the_medium_unreachable: not applicable - {reason}"
            );
            return;
        }
    };
    let spec = TopicSpec {
        topic: topic("unreachable"),
        partitions: 1,
        retention: KEEP,
    };
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    let read = tokio::time::timeout(
        Duration::from_secs(30),
        backend.read(&at, Sequence::NONE, 1, event_broker_sdk::backend::Wait::No),
    )
    .await
    .expect("an unreachable medium answers within 30s");

    assert!(
        matches!(read, Err(BackendError::Unavailable { .. })),
        "read: {read:?}"
    );
}

/// After its stop is cancelled, a backend's own workers stop: an event past a
/// one-second age stays stored. Observed on isolated storage, where no other
/// instance's workers run.
pub async fn stop_ends_the_workers(harness: &dyn Harness) {
    let stop = CancellationToken::new();
    let backend = harness.build_isolated(stop.clone()).await;
    let short = Retention {
        max_age: Some(Duration::from_secs(1)),
        max_bytes: None,
    };
    let spec = provisioned(backend.as_ref(), "stopped", 1, short).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    stop.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &[event("k", 0)])
        .await;
    wait_for_count(backend.as_ref(), &at, 1).await;

    tokio::time::sleep(harness.retention_deadline()).await;

    assert_eq!(sequences(&read_all(backend.as_ref(), &at).await), [1]);
}
