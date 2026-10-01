//! Resolving positions to offsets (feature 0007 §2.4).

use std::time::Duration;

use chrono::Utc;
use event_broker_sdk::backend::{BackendError, Partition, TopicPartition};
use event_broker_sdk::{OutOfRange, Position, Sequence};
use tokio_util::sync::CancellationToken;

use crate::fixture::{KEEP, Writer, event_at, provisioned, wait_for_count};
use crate::harness::Harness;

/// `Earliest`, `Latest`, `At(t)` and `Exact` on an empty and on a populated
/// partition. `At` goes by the event's `occurred_at`.
pub async fn resolve_positions(harness: &dyn Harness) {
    let backend = harness.build(CancellationToken::new()).await;
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

    let base = Utc::now() - chrono::TimeDelta::hours(1);
    let writer = Writer::new();
    writer
        .append(
            backend.as_ref(),
            &spec.topic,
            &[
                event_at("k", 0, base),
                event_at("k", 1, base + chrono::TimeDelta::minutes(10)),
            ],
        )
        .await;
    writer
        .append(
            backend.as_ref(),
            &spec.topic,
            &[event_at("k", 2, base + chrono::TimeDelta::minutes(30))],
        )
        .await;
    wait_for_count(backend.as_ref(), &at, 3).await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let positions = [
        resolve(Position::Earliest).await.expect("resolve"),
        resolve(Position::Latest).await.expect("resolve"),
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
