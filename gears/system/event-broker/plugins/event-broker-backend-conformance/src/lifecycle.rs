//! Starting and stopping a backend (feature 0007 §2.7).

use std::time::Duration;

use event_broker_sdk::Sequence;
use event_broker_sdk::backend::{BackendError, Partition, Retention, TopicPartition, TopicSpec};
use tokio_util::sync::CancellationToken;

use crate::fixture::{
    KEEP, Writer, event, provisioned, read_all, sequences, started, topic, wait_for_count,
};
use crate::harness::{Harness, Unreachable};

/// How long `start` and `stop` may take before a scenario calls them hung.
const PROMPT: Duration = Duration::from_secs(30);

/// A backend over a medium that is down starts, and answers each operation
/// `Unavailable` instead of failing the start.
pub async fn start_succeeds_with_the_medium_unreachable(harness: &dyn Harness) {
    let backend = match harness.build_unreachable().await {
        Unreachable::Backend(backend) => backend,
        Unreachable::NotApplicable { reason } => {
            tracing::info!(
                scenario = "start_succeeds_with_the_medium_unreachable",
                reason,
                "conformance scenario not applicable"
            );
            return;
        }
    };
    let start = tokio::time::timeout(PROMPT, backend.start())
        .await
        .expect("start returns without waiting on the medium");
    assert!(start.is_ok(), "start: {start:?}");
    let spec = TopicSpec {
        topic: topic("unreachable"),
        partitions: 1,
        retention: KEEP,
    };
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    let read = tokio::time::timeout(
        PROMPT,
        backend.read(&at, Sequence::NONE, 1, event_broker_sdk::backend::Wait::No),
    )
    .await
    .expect("an unreachable medium answers within 30s");

    assert!(
        matches!(read, Err(BackendError::Unavailable { .. })),
        "read: {read:?}"
    );
    backend.stop(CancellationToken::new()).await;
}

/// Once `stop` returns, the backend's own workers have ended: an event past a
/// one-second age is not removed. Observed on isolated storage, where no other
/// instance's workers run. A backend that answers `Unavailable` after its stop
/// leaves nothing to observe.
pub async fn stop_ends_the_workers(harness: &dyn Harness) {
    let backend = started(harness.build_isolated().await).await;
    let short = Retention {
        max_age: Some(Duration::from_secs(1)),
        max_bytes: None,
    };
    let spec = provisioned(backend.as_ref(), "stopped", 1, short).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &[event("k", 0)])
        .await;
    let stored = sequences(&wait_for_count(backend.as_ref(), &at, 1).await);
    assert_eq!(stored.len(), 1, "{stored:?}");

    tokio::time::timeout(PROMPT, backend.stop(CancellationToken::new()))
        .await
        .expect("stop returns once the workers have ended");
    tokio::time::sleep(harness.retention_deadline()).await;

    match backend
        .read(&at, Sequence::NONE, 10, event_broker_sdk::backend::Wait::No)
        .await
    {
        Err(BackendError::Unavailable { .. }) => {}
        _ => assert_eq!(sequences(&read_all(backend.as_ref(), &at).await), stored),
    }
}

/// `stop` before `start` returns at once, and so does a second `stop`.
pub async fn stop_is_harmless_unstarted_and_repeated(harness: &dyn Harness) {
    let backend = harness.build_isolated().await;
    tokio::time::timeout(PROMPT, backend.stop(CancellationToken::new()))
        .await
        .expect("stop before start returns");

    let backend = started(harness.build_isolated().await).await;
    tokio::time::timeout(PROMPT, backend.stop(CancellationToken::new()))
        .await
        .expect("stop returns");
    tokio::time::timeout(PROMPT, backend.stop(CancellationToken::new()))
        .await
        .expect("a second stop returns");
}
