//! Resolving positions to offsets (feature 0007 §2.4).

use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{Backend, BackendError, Partition, TopicPartition};
use event_broker_sdk::{GtsInstanceId, OutOfRange, Position, Sequence};

use crate::fixture::{KEEP, Writer, event_at, provisioned, read_all, started, wait_for_count};
use crate::harness::Harness;

/// `Earliest`, `Latest`, `At(t)` and `Exact` on an empty and on a populated
/// partition. `At` goes by when the backend stored the event
/// (`sequence_time`), never by the producer's `occurred_at`.
pub async fn resolve_positions(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "resolve", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let resolve = |position| {
        let backend = backend.clone();
        let at = at.clone();
        async move { backend.resolve(&at, position).await }
    };

    assert_eq!(
        resolve(Position::Earliest).await.expect("resolve"),
        Sequence::NONE
    );
    assert_eq!(
        resolve(Position::Latest).await.expect("resolve"),
        Sequence::NONE
    );

    let (base, third_stored_at) = populate(backend.as_ref(), &spec.topic, &at).await;

    let positions = [
        resolve(Position::Earliest).await.expect("resolve"),
        resolve(Position::Latest).await.expect("resolve"),
        resolve(Position::At(third_stored_at))
            .await
            .expect("resolve"),
        resolve(Position::At(base + chrono::TimeDelta::minutes(20)))
            .await
            .expect("resolve"),
        resolve(Position::At(Utc::now() + chrono::TimeDelta::hours(1)))
            .await
            .expect("resolve"),
        resolve(Position::Exact(Sequence::assigned(1)))
            .await
            .expect("resolve"),
    ];
    let above = resolve(Position::Exact(Sequence::assigned(9))).await;

    assert_eq!(
        positions,
        [
            Sequence::NONE,
            Sequence::assigned(3),
            Sequence::assigned(2),
            Sequence::NONE,
            Sequence::assigned(3),
            Sequence::assigned(1),
        ]
    );
    match above {
        Err(BackendError::OffsetOutOfRange {
            floor,
            ceiling,
            breached,
            ..
        }) => assert_eq!(
            (floor, ceiling, breached),
            (
                Sequence::NONE,
                Sequence::assigned(3),
                OutOfRange::AboveCeiling
            )
        ),
        other => panic!("expected OffsetOutOfRange, got {other:?}"),
    }
}

/// Three events in two appends, apart in storage time; the producer's
/// `occurred_at`, an hour back and the last one back-dated by a day, runs
/// against it. Answers that base producer time and the third event's
/// `sequence_time`.
async fn populate(
    backend: &dyn Backend,
    topic: &GtsInstanceId,
    at: &TopicPartition,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let base = Utc::now() - chrono::TimeDelta::hours(1);
    let writer = Writer::new();
    writer
        .append(
            backend,
            topic,
            &[
                event_at("k", 0, base),
                event_at("k", 1, base + chrono::TimeDelta::minutes(10)),
            ],
        )
        .await;
    // Apart from the first batch in storage time, so the third event's
    // `sequence_time` is strictly later than the second's.
    tokio::time::sleep(Duration::from_millis(10)).await;
    writer
        .append(
            backend,
            topic,
            &[event_at("k", 2, base - chrono::TimeDelta::days(1))],
        )
        .await;
    wait_for_count(backend, at, 3).await;
    let third_stored_at = read_all(backend, at).await[2]
        .sequence_time
        .expect("a stored event carries its sequence_time");
    (base, third_stored_at)
}
