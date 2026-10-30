//! Resolving positions to offsets (feature 0007 §2.4).

use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{Backend, BackendError, Partition, TopicPartition};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, OutOfRange, Position, Sequence};

use crate::fixture::{KEEP, Writer, event_at, provisioned, read_all, started, wait_for_count};
use crate::harness::Harness;

/// `Earliest`, `Latest`, `At(t)` and `Exact` on an empty and on a populated
/// partition. `At` goes by when the backend stored the event
/// (`sequence_time`), never by the producer's `occurred_at`. Sequences may
/// have gaps, so a position that delivery starts from names any offset
/// between the last event before it and the first event it delivers.
pub async fn resolve_positions(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "resolve", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let backend = backend.as_ref();

    assert_eq!(
        [
            resolved(backend, &at, Position::Earliest).await,
            resolved(backend, &at, Position::Latest).await,
        ],
        [Sequence::NONE, Sequence::NONE]
    );

    let (base, stored) = populate(backend, &spec.topic, &at).await;
    let [first, second, third] = stored.each_ref().map(|e| e.sequence.expect("sequence"));
    let third_stored_at = stored[2]
        .sequence_time
        .expect("a stored event carries its sequence_time");

    let earliest = resolved(backend, &at, Position::Earliest).await;
    let before_all = resolved(
        backend,
        &at,
        Position::At(base + chrono::TimeDelta::minutes(20)),
    )
    .await;
    let at_third = resolved(backend, &at, Position::At(third_stored_at)).await;
    let positions = [
        resolved(backend, &at, Position::Latest).await,
        resolved(
            backend,
            &at,
            Position::At(Utc::now() + chrono::TimeDelta::hours(1)),
        )
        .await,
        resolved(backend, &at, Position::Exact(first)).await,
    ];
    let above = backend
        .resolve(&at, Position::Exact(Sequence::assigned(third.as_i64() + 6)))
        .await;

    assert!(earliest < first, "Earliest {earliest:?} past {first:?}");
    assert!(
        before_all < first,
        "At before every event {before_all:?} past {first:?}"
    );
    assert!(
        (second..third).contains(&at_third),
        "At the third event {at_third:?} outside [{second:?}, {third:?})"
    );
    assert_eq!(positions, [third, third, first]);
    assert_eq!(
        out_of_range(above),
        (earliest, third, OutOfRange::AboveCeiling)
    );
}

/// `position` of `at` resolved; panics if the backend refuses.
async fn resolved(backend: &dyn Backend, at: &TopicPartition, position: Position) -> Sequence {
    backend.resolve(at, position).await.expect("resolve")
}

/// The floor, ceiling and breached bound of an `OffsetOutOfRange` refusal;
/// panics on any other answer.
fn out_of_range(answer: Result<Sequence, BackendError>) -> (Sequence, Sequence, OutOfRange) {
    match answer {
        Err(BackendError::OffsetOutOfRange {
            floor,
            ceiling,
            breached,
            ..
        }) => (floor, ceiling, breached),
        other => panic!("expected OffsetOutOfRange, got {other:?}"),
    }
}

/// Three events in two appends, apart in storage time; the producer's
/// `occurred_at`, an hour back and the last one back-dated by a day, runs
/// against it. Answers that base producer time and the three stored events.
async fn populate(
    backend: &dyn Backend,
    topic: &GtsInstanceId,
    at: &TopicPartition,
) -> (DateTime<Utc>, [Event; 3]) {
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
    let stored = read_all(backend, at)
        .await
        .try_into()
        .expect("three events stored");
    (base, stored)
}
