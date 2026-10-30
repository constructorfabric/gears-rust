//! Watching heads (feature 0007 §2.5, AC-4): native and polled alike.

use std::time::Duration;

use event_broker_sdk::backend::{Partition, TopicPartition};
use futures_util::StreamExt;

use crate::fixture::{KEEP, Writer, event, head_stream, provisioned, started, wait_for_count};
use crate::harness::Harness;

/// A watch opened after an append first yields the head that append
/// assigned.
pub async fn watch_starts_with_the_current_head(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "current_head", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    Writer::new()
        .append(
            backend.as_ref(),
            &spec.topic,
            &[event("k", 0), event("k", 1)],
        )
        .await;
    let head = wait_for_count(backend.as_ref(), &at, 2)
        .await
        .last()
        .and_then(|e| e.sequence)
        .expect("two events stored");

    let first = tokio::time::timeout(
        Duration::from_secs(15),
        head_stream(&backend, std::slice::from_ref(&at)).next(),
    )
    .await
    .expect("a head within 15s")
    .expect("stream open")
    .expect("no error");

    assert_eq!((first.at, first.sequence), (at, head));
}

/// A watch over one partition yields that partition's heads, rising, and
/// nothing about the others.
pub async fn watch_yields_named_partitions_only(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "named", 2, KEEP).await;
    let watched = TopicPartition::new(spec.topic.clone(), Partition::new(1));
    let mut stream = head_stream(&backend, std::slice::from_ref(&watched));

    let batch: Vec<_> = (0..20).map(|n| event(&format!("key-{n}"), n)).collect();
    Writer::new()
        .append(backend.as_ref(), &spec.topic, &batch)
        .await;
    let head = wait_for_count(backend.as_ref(), &watched, 1)
        .await
        .last()
        .and_then(|e| e.sequence)
        .expect("twenty keys reach partition 1");

    let mut seen = Vec::new();
    while seen.last() != Some(&head) {
        let item = tokio::time::timeout(Duration::from_secs(15), stream.next())
            .await
            .expect("a head within 15s")
            .expect("stream open")
            .expect("no error");
        assert_eq!(item.at, watched, "a head of an unwatched partition");
        seen.push(item.sequence);
    }
    let mut rising = seen.clone();
    rising.sort_unstable();
    rising.dedup();
    assert_eq!(seen, rising, "heads must rise");
}
