use crate::gts::StoredPosition;
use crate::sequence::Sequence;

use super::BatchHandlerOutcome;
use super::dispatcher::PartitionCursor;

fn position_at(offset: i64) -> StoredPosition {
    StoredPosition {
        partition: 0,
        sequence: Sequence::assigned(offset),
        sequence_time: "2026-01-02T03:04:05Z".parse().unwrap(),
    }
}

fn delivered_sparse_batch() -> Vec<StoredPosition> {
    [10, 17, 19].into_iter().map(position_at).collect()
}

#[test]
fn advance_through_counts_delivered_prefix_for_sparse_offsets() {
    let events = delivered_sparse_batch();

    let processed = (BatchHandlerOutcome::AdvanceThrough {
        offset: Sequence::assigned(17),
    })
    .processed_count(&events)
    .expect("handled offset is in delivered batch");

    assert_eq!(processed, Some(2));
}

#[test]
fn full_batch_success_counts_all_delivered_events() {
    let events = delivered_sparse_batch();

    let processed = BatchHandlerOutcome::Success
        .processed_count(&events)
        .expect("success always maps to full batch");

    assert_eq!(processed, Some(3));
}

#[test]
fn invalid_advance_through_offset_is_rejected_without_progress_count() {
    let events = delivered_sparse_batch();

    let err = (BatchHandlerOutcome::AdvanceThrough {
        offset: Sequence::assigned(11),
    })
    .processed_count(&events)
    .expect_err("offset not delivered in this batch must be rejected");

    assert!(err.to_string().contains("not present in delivered offsets"));
}

#[test]
fn retry_outcome_counts_no_delivered_progress() {
    let events = delivered_sparse_batch();

    let processed = (BatchHandlerOutcome::Retry {
        reason: "try later".to_owned(),
    })
    .processed_count(&events)
    .expect("retry is valid");

    assert_eq!(processed, None);
}

#[test]
fn cursor_advances_to_delivered_offset_not_first_offset_plus_count() {
    let events = delivered_sparse_batch();
    let mut cursor = PartitionCursor::default();

    let processed = (BatchHandlerOutcome::AdvanceThrough {
        offset: Sequence::assigned(17),
    })
    .processed_count(&events)
    .expect("handled offset is in delivered batch")
    .expect("partial outcome has progress");
    let frontier = cursor.advance_through_delivered_prefix(&events[..processed]);

    assert_eq!(frontier, Sequence::assigned(17));
    assert_eq!(cursor.latest_offset(), Sequence::assigned(17));
}

#[test]
fn tx_committed_offset_counts_delivered_prefix_for_sparse_offsets() {
    let events = delivered_sparse_batch();

    let processed = BatchHandlerOutcome::processed_count_through(Sequence::assigned(17), &events)
        .expect("tx committed offset is in delivered batch");

    assert_eq!(processed, 2);
}

#[test]
fn tx_committed_offset_rejects_offsets_outside_delivered_batch() {
    let events = delivered_sparse_batch();

    let err = BatchHandlerOutcome::processed_count_through(Sequence::assigned(20), &events)
        .expect_err("tx committed offset must be delivered in the current batch");

    assert!(err.to_string().contains("not present in delivered offsets"));
}
