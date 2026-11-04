//! What one instance stores, another instance - or the same after a restart
//! - sees (AC-3).

use std::time::Duration;

use event_broker_sdk::backend::{Partition, TopicPartition, Wait};
use event_broker_sdk::{Position, Sequence};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::fixture::{
    KEEP, Writer, event, keys_and_ns, provisioned, read_all, rising_from_one, sequences, started,
};
use crate::harness::Harness;

/// Events an append acknowledged are read back by a backend built afresh over
/// the same storage, and numbering continues above where it left off.
pub async fn appended_events_survive_a_rebuild(harness: &dyn Harness) {
    let first = started(harness.build().await).await;
    let spec = provisioned(first.as_ref(), "rebuild", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let writer = Writer::new();
    writer
        .append(first.as_ref(), &spec.topic, &[event("k", 0), event("k", 1)])
        .await;
    writer
        .append(first.as_ref(), &spec.topic, &[event("k", 2)])
        .await;
    let before = sequences(&read_all(first.as_ref(), &at).await);
    assert_eq!(before.len(), 3, "{before:?}");
    first.stop(CancellationToken::new()).await;
    drop(first);

    let second = started(harness.build().await).await;
    assert_eq!(
        second
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        Sequence::assigned(before[2])
    );
    writer
        .append(second.as_ref(), &spec.topic, &[event("k", 3)])
        .await;
    let events = read_all(second.as_ref(), &at).await;

    let after = sequences(&events);
    assert_eq!(after.len(), 4, "{after:?}");
    assert_eq!(after[..3], before[..]);
    assert!(rising_from_one(&after), "{after:?}");
    assert_eq!(
        keys_and_ns(&events),
        [
            ("k".to_owned(), 0),
            ("k".to_owned(), 1),
            ("k".to_owned(), 2),
            ("k".to_owned(), 3)
        ]
    );
}

/// A waiting read on one instance returns an append made through another.
pub async fn tail_read_sees_another_instance(harness: &dyn Harness) {
    let writer_backend = started(harness.build().await).await;
    let reader = started(harness.build().await).await;
    let spec = provisioned(writer_backend.as_ref(), "cross_instance", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let writer = Writer::new();

    let started = Instant::now();
    let (read, ()) = tokio::join!(
        reader.read(&at, Sequence::NONE, 100, Wait::For(Duration::from_secs(20))),
        async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            writer
                .append(writer_backend.as_ref(), &spec.topic, &[event("k", 0)])
                .await;
        }
    );
    let elapsed = started.elapsed();
    let mut read = read.expect("read");
    let deadline = Instant::now() + Duration::from_secs(10);
    while read.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        read = reader
            .read(&at, Sequence::NONE, 100, Wait::No)
            .await
            .expect("read");
    }

    assert_eq!(keys_and_ns(&read), [("k".to_owned(), 0)]);
    assert!(rising_from_one(&sequences(&read)), "{read:?}");
    assert!(elapsed < Duration::from_secs(15), "woke after {elapsed:?}");
}
