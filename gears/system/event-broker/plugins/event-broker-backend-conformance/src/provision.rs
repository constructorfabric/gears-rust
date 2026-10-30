//! Provisioning (feature 0006 §2.3, feature 0007 §2.6) and topics the backend
//! does not hold.

use event_broker_sdk::backend::{
    AppendRequest, BackendError, Flush, LogClaim, LogId, Partition, Replacing, TopicPartition,
    TopicSpec, Wait,
};
use event_broker_sdk::{Position, Sequence};
use futures_util::StreamExt;

use crate::fixture::{KEEP, Writer, event, head_stream, settle, started, topic};
use crate::harness::Harness;

/// Accepted over nothing, over itself, and over the attempt it names;
/// refused as a log conflict over anything else.
pub async fn provision_compare_and_set(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = TopicSpec {
        topic: topic("provision"),
        partitions: 2,
        retention: KEEP,
    };
    let (first, second, stranger) = (LogId::new_v4(), LogId::new_v4(), LogId::new_v4());

    let mut outcomes = Vec::new();
    for claim in [
        LogClaim {
            replacing: Replacing::Nothing,
            new: first,
        }, // over nothing
        LogClaim {
            replacing: Replacing::Nothing,
            new: first,
        }, // the same attempt, continued
        LogClaim {
            replacing: Replacing::Nothing,
            new: second,
        }, // an attempt unaware of `first`
        LogClaim {
            replacing: Replacing::Log(stranger),
            new: second,
        }, // replacing what is not held
        LogClaim {
            replacing: Replacing::Log(first),
            new: second,
        }, // replacing `first`
        LogClaim {
            replacing: Replacing::Nothing,
            new: first,
        }, // the replaced attempt, late
    ] {
        outcomes.push(match settle(backend.as_ref(), &spec, &claim).await {
            Ok(partitions) => format!("ready {partitions}"),
            Err(BackendError::LogConflict { .. }) => "conflict".to_owned(),
            Err(other) => panic!("provision failed: {other}"),
        });
    }
    assert_eq!(
        outcomes,
        [
            "ready 2", "ready 2", "conflict", "conflict", "ready 2", "conflict"
        ]
    );
}

/// `Ready` reports the partition count the backend holds: a topic provisioned
/// with 4 and asked again for 8 under the same log id still holds 4.
pub async fn provision_reports_the_held_partition_count(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let log = LogId::new_v4();
    let mut spec = TopicSpec {
        topic: topic("held_count"),
        partitions: 4,
        retention: KEEP,
    };
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: log,
    };
    let first = settle(backend.as_ref(), &spec, &claim)
        .await
        .expect("provision");
    spec.partitions = 8;
    let again = settle(
        backend.as_ref(),
        &spec,
        &LogClaim {
            replacing: Replacing::Log(log),
            new: log,
        },
    )
    .await
    .expect("provision again");

    assert_eq!((first, again), (4, 4));
}

/// `append`, `read`, `resolve` and a watch item on a topic this backend never
/// provisioned are `NotProvisioned`, and the append stores nothing.
pub async fn not_provisioned_everywhere(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let topic = topic("unprovisioned");
    let at = TopicPartition::new(topic.clone(), Partition::new(0));
    let writer = Writer::new();

    let append = backend
        .append(&AppendRequest {
            fence: writer.next_fence(),
            topic: &topic,
            events: &[event("a", 0)],
            flush: Flush::Sync,
        })
        .await;
    let read = backend.read(&at, Sequence::NONE, 10, Wait::No).await;
    let resolve = backend.resolve(&at, Position::Latest).await;
    let watched = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        head_stream(&backend, std::slice::from_ref(&at)).next(),
    )
    .await
    .expect("a watch item within 15s");

    assert!(
        matches!(append, Err(BackendError::NotProvisioned { .. })),
        "append: {append:?}"
    );
    assert!(
        matches!(read, Err(BackendError::NotProvisioned { .. })),
        "read: {read:?}"
    );
    assert!(
        matches!(resolve, Err(BackendError::NotProvisioned { .. })),
        "resolve: {resolve:?}"
    );
    assert!(
        matches!(watched, Some(Err(BackendError::NotProvisioned { .. }))),
        "watch: {watched:?}"
    );
}
