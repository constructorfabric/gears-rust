//! Retention by the backend's own workers (feature 0007 §2.6), as far as the
//! contract fixes it: an event within both bounds is kept, age goes by when
//! the backend stored the event - never by the producer's `occurred_at` - and
//! removal takes the oldest first. How soon past a bound an event goes, and in
//! what units, is each backend's own and tested with it.

use std::time::Duration;

use chrono::Utc;
use event_broker_sdk::backend::{Partition, Retention, TopicPartition};
use event_broker_sdk::{Position, Sequence};

use crate::fixture::{
    Writer, event, event_at, latest_past, provisioned, read_all, sequences, started, wait_for_count,
};
use crate::harness::Harness;

/// Events within both bounds stay stored for as long as retention takes to
/// act on anything.
pub async fn retention_keeps_events_within_bounds(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let roomy = Retention {
        max_age: Some(Duration::from_hours(1)),
        max_bytes: Some(1 << 30),
    };
    let spec = provisioned(backend.as_ref(), "within", 1, roomy).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..3).map(|n| event("k", n)).collect();
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &batch)
        .await;
    let stored = sequences(&wait_for_count(backend.as_ref(), &at, 3).await);
    assert_eq!(stored.len(), 3, "{stored:?}");

    tokio::time::sleep(harness.retention_deadline()).await;

    assert_eq!(sequences(&read_all(backend.as_ref(), &at).await), stored);
}

/// An event back-dated by years, stored a moment ago under a one-hour age,
/// is kept for as long as retention takes to act on anything.
pub async fn retention_ignores_occurred_at(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
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
    let stored = sequences(&wait_for_count(backend.as_ref(), &at, 1).await);
    assert_eq!(stored.len(), 1, "{stored:?}");

    tokio::time::sleep(harness.retention_deadline()).await;

    assert_eq!(sequences(&read_all(backend.as_ref(), &at).await), stored);
    assert!(Utc::now() > year_2000);
}

/// With every event past both bounds, whatever the backend removes is the
/// oldest: what stays is a suffix of what was stored, and `Latest` stays
/// where it was.
pub async fn retention_removes_the_oldest_first(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let tight = Retention {
        max_age: Some(Duration::from_secs(1)),
        max_bytes: Some(1),
    };
    let spec = provisioned(backend.as_ref(), "oldest_first", 1, tight).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let writer = Writer::new();
    // Each event's sequence from the head it raised: retention may remove an
    // event before a read could see it.
    let mut stored = Vec::new();
    let mut head = Sequence::NONE;
    for n in 0..5 {
        writer
            .append(backend.as_ref(), &spec.topic, &[event("k", n)])
            .await;
        head = latest_past(backend.as_ref(), &at, head).await;
        stored.push(head.as_i64());
    }

    tokio::time::sleep(harness.retention_deadline()).await;

    let left = sequences(&read_all(backend.as_ref(), &at).await);
    let removed = stored.len().saturating_sub(left.len());
    assert_eq!(left, stored[removed..], "what stays is a suffix");
    assert_eq!(
        backend
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        head
    );
}
