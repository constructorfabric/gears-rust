//! Appending: heads, rising sequences, per-key order (AC-1), the append
//! limit, and concurrent appenders (AC-7).

use std::collections::{BTreeMap, BTreeSet};

use event_broker_sdk::backend::{
    AppendRequest, Appended, BackendError, Flush, Partition, TopicPartition,
};
use event_broker_sdk::{Position, Sequence};

use crate::fixture::{
    KEEP, Writer, event, heads_after, keys_and_ns, provisioned, read_all, rising_from_one,
    sequences, started, wait_for_count,
};
use crate::harness::Harness;

const KEYS: [&str; 6] = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];

/// `Heads` names every partition the call touched with the highest sequence
/// it assigned there, and only those; the stored events agree.
pub async fn append_reports_heads(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "heads", 4, KEEP).await;
    let batch: Vec<_> = (0..12)
        .map(|n| event(KEYS[n % KEYS.len()], n as u64))
        .collect();

    let appended = Writer::new()
        .append(backend.as_ref(), &spec.topic, &batch)
        .await;

    let mut stored: BTreeMap<Partition, i64> = BTreeMap::new();
    let mut count = 0;
    for partition in (0..spec.partitions).map(Partition::new) {
        let events = wait_for_count(
            backend.as_ref(),
            &TopicPartition::new(spec.topic.clone(), partition),
            0,
        )
        .await;
        let seqs = sequences(&events);
        assert!(rising_from_one(&seqs), "{partition}: {seqs:?}");
        count += events.len();
        if let Some(last) = seqs.last() {
            stored.insert(partition, *last);
        }
    }
    assert_eq!(count, 12, "every event stored once: {stored:?}");
    if let Appended::Heads(heads) = &appended {
        let reported: BTreeMap<Partition, i64> = heads
            .iter()
            .map(|h| (h.at.partition, h.sequence.as_i64()))
            .collect();
        assert_eq!(reported, stored);
        assert!(heads.iter().all(|h| h.at.topic == spec.topic));
    }
    let latest: Vec<i64> = heads_after(backend.as_ref(), &spec, &appended)
        .await
        .into_iter()
        .map(Sequence::as_i64)
        .collect();
    let expected: Vec<i64> = (0..spec.partitions)
        .map(Partition::new)
        .map(|p| stored.get(&p).copied().unwrap_or(0))
        .collect();
    assert_eq!(latest, expected);
}

/// A fresh partition numbers from 1 or above, rising across calls, and
/// `Latest` is the last event's sequence.
pub async fn append_assigns_rising_sequences(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "rising", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let writer = Writer::new();

    assert_eq!(
        backend
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        Sequence::NONE
    );
    writer
        .append(
            backend.as_ref(),
            &spec.topic,
            &[event("k", 0), event("k", 1)],
        )
        .await;
    writer
        .append(backend.as_ref(), &spec.topic, &[event("k", 2)])
        .await;

    let events = wait_for_count(backend.as_ref(), &at, 3).await;
    let seqs = sequences(&events);
    assert_eq!(seqs.len(), 3, "{seqs:?}");
    assert!(rising_from_one(&seqs), "{seqs:?}");
    assert_eq!(
        backend
            .resolve(&at, Position::Latest)
            .await
            .expect("resolve"),
        Sequence::assigned(seqs[2])
    );
    assert_eq!(
        keys_and_ns(&events),
        [
            ("k".to_owned(), 0),
            ("k".to_owned(), 1),
            ("k".to_owned(), 2)
        ]
    );
    assert!(events.iter().all(|e| e.partition == Some(0)));
}

/// Events of one key published in order are read back from one partition in
/// that order, and the mapping is the same for every instance.
pub async fn append_keeps_key_order(harness: &dyn Harness) {
    let first = started(harness.build().await).await;
    let second = started(harness.build().await).await;
    let spec = provisioned(first.as_ref(), "key_order", 3, KEEP).await;
    let writer = Writer::new();

    let mut n = 0_u64;
    for round in 0..4 {
        let batch: Vec<_> = KEYS
            .iter()
            .map(|key| {
                n += 1;
                event(key, n)
            })
            .collect();
        let backend = if round % 2 == 0 { &first } else { &second };
        writer.append(backend.as_ref(), &spec.topic, &batch).await;
    }

    let mut partition_of: BTreeMap<String, Partition> = BTreeMap::new();
    for partition in (0..spec.partitions).map(Partition::new) {
        let at = TopicPartition::new(spec.topic.clone(), partition);
        let events = wait_for_count(first.as_ref(), &at, 0).await;
        let mut per_key: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for (key, n) in keys_and_ns(&events) {
            let previous = partition_of.insert(key.clone(), partition);
            assert!(
                previous.is_none_or(|p| p == partition),
                "{key} split over partitions {previous:?} and {partition}"
            );
            per_key.entry(key).or_default().push(n);
        }
        for (key, ns) in per_key {
            let mut sorted = ns.clone();
            sorted.sort_unstable();
            assert_eq!(ns, sorted, "{key} out of publish order");
            assert_eq!(ns.len(), 4, "{key} lost or duplicated an event");
        }
        let seqs = sequences(&events);
        assert!(rising_from_one(&seqs), "{partition}: {seqs:?}");
    }
    assert_eq!(partition_of.len(), KEYS.len());
}

/// `max_events_per_append` events are stored; one more is refused and stores
/// nothing.
pub async fn append_respects_its_limit(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    let spec = provisioned(backend.as_ref(), "limit", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let limit = backend.max_events_per_append();
    let writer = Writer::new();
    let over: Vec<_> = (0..=limit as u64).map(|n| event("k", n)).collect();

    let refused = backend
        .append(&AppendRequest {
            fence: writer.next_fence(),
            topic: &spec.topic,
            events: &over,
            flush: Flush::Sync,
        })
        .await;
    writer
        .append(backend.as_ref(), &spec.topic, &over[..limit])
        .await;

    assert!(
        matches!(
            refused,
            Err(BackendError::AppendFailed { .. } | BackendError::InvalidConfig { .. })
        ),
        "an append over the limit must be refused: {refused:?}"
    );
    assert_eq!(
        wait_for_count(backend.as_ref(), &at, limit).await.len(),
        limit
    );
}

/// Two instances appending to one partition at once assign distinct, rising
/// sequences, and every event is stored once.
pub async fn concurrent_appenders_assign_distinct_sequences(harness: &dyn Harness) {
    let first = started(harness.build().await).await;
    let second = started(harness.build().await).await;
    let spec = provisioned(first.as_ref(), "concurrent", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let (one, two) = (Writer::new(), Writer::new());

    let run = |backend: std::sync::Arc<dyn event_broker_sdk::backend::Backend>,
               writer: Writer,
               base: u64| {
        let topic = spec.topic.clone();
        async move {
            for n in 0..25 {
                writer
                    .append(backend.as_ref(), &topic, &[event("k", base + n)])
                    .await;
            }
        }
    };
    tokio::join!(run(first.clone(), one, 0), run(second, two, 100));

    let events = wait_for_count(first.as_ref(), &at, 50).await;
    let seqs = sequences(&events);
    assert_eq!(seqs.len(), 50, "{seqs:?}");
    assert!(rising_from_one(&seqs), "{seqs:?}");
    let ns: BTreeSet<u64> = keys_and_ns(&events).into_iter().map(|(_, n)| n).collect();
    let all: BTreeSet<u64> = (0..25).chain(100..125).collect();
    assert_eq!(ns, all);
    assert_eq!(read_all(first.as_ref(), &at).await.len(), 50);
}
