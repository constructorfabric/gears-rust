//! Reading by offset: exclusive (AC-6), bounded, and the waiting read.

use std::time::Duration;

use event_broker_sdk::Sequence;
use event_broker_sdk::backend::{Partition, TopicPartition, Wait};
use tokio::time::Instant;

use crate::fixture::{
    KEEP, Writer, event, keys_and_ns, provisioned, rising_from_one, sequences, started,
    wait_for_count,
};
use crate::harness::Harness;

/// Never the event at `offset`; `Sequence::NONE` from the start;
/// `max_events` respected.
pub async fn read_is_exclusive_and_bounded(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "exclusive", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..5).map(|n| event("k", n)).collect();
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &batch)
        .await;
    let all = sequences(&wait_for_count(backend.as_ref(), &at, 5).await);
    assert_eq!(all.len(), 5, "{all:?}");
    assert!(rising_from_one(&all), "{all:?}");

    let read = |offset: i64, max: usize| {
        let backend = backend.clone();
        let at = at.clone();
        async move {
            sequences(
                &backend
                    .read(&at, Sequence::assigned(offset), max, Wait::No)
                    .await
                    .expect("read"),
            )
        }
    };

    assert_eq!(read(0, 100).await, all);
    assert_eq!(read(all[1], 100).await, all[2..]);
    assert_eq!(read(all[0], 2).await, all[1..3]);
    assert_eq!(read(all[4], 100).await, Vec::<i64>::new());
}

/// A read waiting at the head returns on a concurrent append, well before
/// its wait is over.
pub async fn read_wait_returns_on_append(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "wait_append", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let writer = Writer::new();
    writer
        .append(backend.as_ref(), &spec.topic, &[event("k", 0)])
        .await;
    let head = sequences(&wait_for_count(backend.as_ref(), &at, 1).await)[0];

    let started = Instant::now();
    let (read, ()) = tokio::join!(
        backend.read(
            &at,
            Sequence::assigned(head),
            100,
            Wait::For(Duration::from_secs(20))
        ),
        async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            writer
                .append(backend.as_ref(), &spec.topic, &[event("k", 1)])
                .await;
        }
    );
    let elapsed = started.elapsed();
    let mut read = read.expect("read");
    // A backend that cannot wait natively may return at once; the caller
    // retries (feature 0007 §2.3).
    let deadline = Instant::now() + Duration::from_secs(10);
    while read.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        read = backend
            .read(&at, Sequence::assigned(head), 100, Wait::No)
            .await
            .expect("read");
    }

    assert_eq!(keys_and_ns(&read), [("k".to_owned(), 1)]);
    assert!(sequences(&read)[0] > head, "{read:?}");
    assert!(elapsed < Duration::from_secs(10), "woke after {elapsed:?}");
}

/// A read waiting at the head with nothing appended returns empty.
pub async fn read_wait_times_out_empty(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "wait_empty", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    let started = Instant::now();
    let read = backend
        .read(
            &at,
            Sequence::NONE,
            100,
            Wait::For(Duration::from_millis(500)),
        )
        .await
        .expect("read");
    let elapsed = started.elapsed();

    assert_eq!(sequences(&read), Vec::<i64>::new());
    assert!(
        elapsed < Duration::from_secs(10),
        "returned after {elapsed:?}"
    );
}
