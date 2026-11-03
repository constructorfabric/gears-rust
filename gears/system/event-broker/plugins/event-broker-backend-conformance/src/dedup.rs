//! Exactly once per fence (feature 0007 §2.2, AC-9, AC-10). A backend that
//! declares `AtLeastOnce` promises none of this; for it these scenarios only
//! check that the declaration is what it is.

use event_broker_sdk::backend::{
    Appended, Backend, Dedup, Fence, FenceKey, FenceSeq, Partition, TopicPartition,
};
use uuid::Uuid;

use crate::fixture::{
    KEEP, append_fenced, event, keys_and_ns, provisioned, started, wait_for_count,
};
use crate::harness::Harness;

fn fence(key: FenceKey, seq: i64) -> Fence {
    Fence {
        key,
        seq: FenceSeq::new(seq),
    }
}

fn fresh_key() -> FenceKey {
    FenceKey::new(Uuid::new_v4().as_u128())
}

fn deduplicates(backend: &dyn Backend) -> bool {
    backend.dedup() != Dedup::AtLeastOnce
}

/// A retry of an applied fence answers `Duplicate` and stores nothing.
pub async fn dedup_turns_a_retry_into_a_duplicate(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    if !deduplicates(backend.as_ref()) {
        return;
    }
    let spec = provisioned(backend.as_ref(), "retry", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let key = fresh_key();

    let first = append_fenced(
        backend.as_ref(),
        fence(key, 1),
        &spec.topic,
        &[event("k", 0)],
    )
    .await;
    let retry = append_fenced(
        backend.as_ref(),
        fence(key, 1),
        &spec.topic,
        &[event("k", 0)],
    )
    .await;

    assert!(
        !matches!(first, Appended::Duplicate),
        "the first append is no duplicate"
    );
    assert_eq!(retry, Appended::Duplicate);
    assert_eq!(
        keys_and_ns(&wait_for_count(backend.as_ref(), &at, 1).await),
        [("k".to_owned(), 0)]
    );
}

/// A copy of fence 5 arriving after fence 6 was applied stores nothing.
pub async fn dedup_refuses_a_late_copy(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    if !deduplicates(backend.as_ref()) {
        return;
    }
    let spec = provisioned(backend.as_ref(), "late_copy", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let key = fresh_key();

    append_fenced(
        backend.as_ref(),
        fence(key, 5),
        &spec.topic,
        &[event("k", 5)],
    )
    .await;
    append_fenced(
        backend.as_ref(),
        fence(key, 6),
        &spec.topic,
        &[event("k", 6)],
    )
    .await;
    let late = append_fenced(
        backend.as_ref(),
        fence(key, 5),
        &spec.topic,
        &[event("k", 5)],
    )
    .await;

    assert_eq!(late, Appended::Duplicate);
    assert_eq!(
        keys_and_ns(&wait_for_count(backend.as_ref(), &at, 2).await),
        [("k".to_owned(), 5), ("k".to_owned(), 6)]
    );
}

/// Gaps in the fence sequence are accepted.
pub async fn dedup_accepts_gaps(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    if !deduplicates(backend.as_ref()) {
        return;
    }
    let spec = provisioned(backend.as_ref(), "gaps", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let key = fresh_key();

    append_fenced(
        backend.as_ref(),
        fence(key, 3),
        &spec.topic,
        &[event("k", 3)],
    )
    .await;
    let after_gap = append_fenced(
        backend.as_ref(),
        fence(key, 7),
        &spec.topic,
        &[event("k", 7)],
    )
    .await;

    assert!(!matches!(after_gap, Appended::Duplicate));
    assert_eq!(
        keys_and_ns(&wait_for_count(backend.as_ref(), &at, 2).await),
        [("k".to_owned(), 3), ("k".to_owned(), 7)]
    );
}

/// Distinct keys keep separate fences, however few bits they differ in: the
/// same seq under each is stored.
pub async fn dedup_keeps_keys_apart(harness: &dyn Harness) {
    let backend = started(harness.build().await).await;
    if !deduplicates(backend.as_ref()) {
        return;
    }
    let spec = provisioned(backend.as_ref(), "keys", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let one = fresh_key();
    let low_bit = FenceKey::new(one.value() ^ 1);
    let high_bit = FenceKey::new(one.value() ^ (1 << 127));

    let answers = [
        append_fenced(
            backend.as_ref(),
            fence(one, 1),
            &spec.topic,
            &[event("k", 1)],
        )
        .await,
        append_fenced(
            backend.as_ref(),
            fence(low_bit, 1),
            &spec.topic,
            &[event("k", 2)],
        )
        .await,
        append_fenced(
            backend.as_ref(),
            fence(high_bit, 1),
            &spec.topic,
            &[event("k", 3)],
        )
        .await,
    ];

    assert!(
        answers.iter().all(|a| !matches!(a, Appended::Duplicate)),
        "{answers:?}"
    );
    assert_eq!(
        keys_and_ns(&wait_for_count(backend.as_ref(), &at, 3).await),
        [
            ("k".to_owned(), 1),
            ("k".to_owned(), 2),
            ("k".to_owned(), 3)
        ]
    );
}

/// Fence state survives a backend built afresh over the same storage.
pub async fn dedup_survives_a_rebuild(harness: &dyn Harness) {
    let first = started(harness.build().await).await;
    if !deduplicates(first.as_ref()) {
        return;
    }
    let spec = provisioned(first.as_ref(), "dedup_rebuild", 1, KEEP).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let key = fresh_key();
    append_fenced(first.as_ref(), fence(key, 1), &spec.topic, &[event("k", 0)]).await;
    drop(first);

    let second = started(harness.build().await).await;
    let retry = append_fenced(
        second.as_ref(),
        fence(key, 1),
        &spec.topic,
        &[event("k", 0)],
    )
    .await;

    assert_eq!(retry, Appended::Duplicate);
    assert_eq!(
        keys_and_ns(&wait_for_count(second.as_ref(), &at, 1).await),
        [("k".to_owned(), 0)]
    );
}
