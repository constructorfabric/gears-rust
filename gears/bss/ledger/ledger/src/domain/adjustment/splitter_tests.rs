//! Tests for the pure `RecognizedDeferredSplitter`: fully-recognized / fully-deferred
//! / mixed splits, the deferred-over-releasable block, correct multi-stream
//! (drain-all) splitting, the indeterminable per-stream block (no pro-rata), the
//! duplicate-stream / no-schedule blocks, and the deterministic `split_basis_ref`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::domain::error::DomainError;
use crate::domain::status::SCHEDULE_STATUS_COMPLETED;

/// An ACTIVE schedule-stream state with the given deferred/recognized totals.
fn active(
    stream: &str,
    schedule_id: &str,
    total_deferred: i64,
    recognized: i64,
) -> ScheduleStreamState {
    ScheduleStreamState {
        revenue_stream: stream.to_owned(),
        schedule_id: schedule_id.to_owned(),
        total_deferred_minor: total_deferred,
        recognized_minor: recognized,
        status: SCHEDULE_STATUS_ACTIVE.to_owned(),
        version: 1,
    }
}

/// A non-ACTIVE (here COMPLETED) schedule-stream state — no releasable remainder.
fn completed(stream: &str, schedule_id: &str, total_deferred: i64) -> ScheduleStreamState {
    ScheduleStreamState {
        revenue_stream: stream.to_owned(),
        schedule_id: schedule_id.to_owned(),
        total_deferred_minor: total_deferred,
        recognized_minor: total_deferred,
        status: SCHEDULE_STATUS_COMPLETED.to_owned(),
        version: 5,
    }
}

/// A split input over `streams` for one item, ex-tax `amount` with
/// `requested_deferred` targeting the unreleased deferred balance.
fn input(streams: &[ScheduleStreamState], amount: i64, requested_deferred: i64) -> SplitInput<'_> {
    SplitInput {
        source_invoice_item_ref: "inv-1:item-1",
        po_allocation_group: Some("po-grp-1"),
        streams,
        amount_minor_ex_tax: amount,
        requested_deferred_minor: requested_deferred,
    }
}

fn split(input: &SplitInput<'_>) -> Result<SplitResult, DomainError> {
    RecognizedDeferredSplitter::split(input)
}

#[test]
fn fully_recognized_split_has_zero_deferred() {
    // One stream, 600 deferred / 600 recognized (drained) ⇒ no releasable; a
    // wholly-recognized note (requested_deferred = 0).
    let streams = [active("recurring", "sch-1", 600, 600)];
    let result = split(&input(&streams, 10_000, 0)).unwrap();
    assert_eq!(result.recognized_part_minor, 10_000);
    assert_eq!(result.deferred_part_minor, 0);
    assert_eq!(result.per_stream.len(), 1);
    assert_eq!(result.per_stream[0].recognized_part_minor, 10_000);
    assert_eq!(result.per_stream[0].deferred_part_minor, 0);
    assert_eq!(result.per_stream[0].revenue_stream, "recurring");
    assert_eq!(result.per_stream[0].schedule_id, "sch-1");
}

#[test]
fn fully_deferred_split_has_zero_recognized() {
    // One ACTIVE stream with 10_000 releasable; the whole note is deferred.
    let streams = [active("recurring", "sch-1", 12_000, 2_000)]; // releasable 10_000
    let result = split(&input(&streams, 10_000, 10_000)).unwrap();
    assert_eq!(result.recognized_part_minor, 0);
    assert_eq!(result.deferred_part_minor, 10_000);
    assert_eq!(result.per_stream[0].deferred_part_minor, 10_000);
    assert_eq!(result.per_stream[0].recognized_part_minor, 0);
}

#[test]
fn mixed_split_places_recognized_and_deferred_on_the_single_stream() {
    // 10_000 note; 3_000 targets the deferred remainder (releasable 4_000), 7_000
    // reduces recognized revenue.
    let streams = [active("recurring", "sch-1", 9_000, 5_000)]; // releasable 4_000
    let result = split(&input(&streams, 10_000, 3_000)).unwrap();
    assert_eq!(result.recognized_part_minor, 7_000);
    assert_eq!(result.deferred_part_minor, 3_000);
    assert_eq!(result.per_stream.len(), 1);
    assert_eq!(result.per_stream[0].recognized_part_minor, 7_000);
    assert_eq!(result.per_stream[0].deferred_part_minor, 3_000);
    // recognized + deferred == the note amount.
    let s = &result.per_stream[0];
    assert_eq!(s.recognized_part_minor + s.deferred_part_minor, 10_000);
}

#[test]
fn deferred_request_over_releasable_remainder_blocks() {
    // releasable is 4_000; a 5_000 deferred request over-reduces the in-flight
    // schedule ⇒ block-on-ambiguous (NOT a silent clamp / pro-rata).
    let streams = [active("recurring", "sch-1", 9_000, 5_000)]; // releasable 4_000
    let err = split(&input(&streams, 10_000, 5_000)).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditNoteSplitAmbiguous(_)),
        "deferred over releasable must block, got {err:?}"
    );
}

#[test]
fn deferred_request_with_no_schedule_state_blocks() {
    // A note requesting a deferred portion but the line has NO schedule to reduce
    // ⇒ ambiguous (no item→schedule mapping).
    let streams: [ScheduleStreamState; 0] = [];
    let err = split(&input(&streams, 10_000, 1)).unwrap_err();
    assert!(matches!(err, DomainError::CreditNoteSplitAmbiguous(_)));
}

#[test]
fn wholly_recognized_note_with_no_schedule_state_is_ok() {
    // No schedule + zero deferred request ⇒ a fully point-in-time line; the whole
    // amount is recognized, no per-stream reduction.
    let streams: [ScheduleStreamState; 0] = [];
    let result = split(&input(&streams, 10_000, 0)).unwrap();
    assert_eq!(result.recognized_part_minor, 10_000);
    assert_eq!(result.deferred_part_minor, 0);
    assert!(result.per_stream.is_empty());
}

#[test]
fn multi_stream_full_drain_splits_per_stream() {
    // Two ACTIVE streams, releasable 4_000 + 6_000 = 10_000; a deferred request of
    // exactly 10_000 drains BOTH (unambiguous), each reduction kept on its stream.
    let streams = [
        active("recurring", "sch-A", 4_000, 0),  // releasable 4_000
        active("usage", "sch-B", 10_000, 4_000), // releasable 6_000
    ];
    let result = split(&input(&streams, 10_000, 10_000)).unwrap();
    assert_eq!(result.deferred_part_minor, 10_000);
    assert_eq!(result.recognized_part_minor, 0);
    assert_eq!(result.per_stream.len(), 2);
    // Each stream is drained to its own releasable remainder, same stream/schedule.
    assert_eq!(result.per_stream[0].revenue_stream, "recurring");
    assert_eq!(result.per_stream[0].schedule_id, "sch-A");
    assert_eq!(result.per_stream[0].deferred_part_minor, 4_000);
    assert_eq!(result.per_stream[1].revenue_stream, "usage");
    assert_eq!(result.per_stream[1].schedule_id, "sch-B");
    assert_eq!(result.per_stream[1].deferred_part_minor, 6_000);
}

#[test]
fn multi_stream_single_releasable_target_places_on_that_stream() {
    // Two streams but only ONE is ACTIVE-with-remainder (the other is COMPLETED ⇒
    // releasable 0); the deferred part lands unambiguously on the live one.
    let streams = [
        completed("recurring", "sch-A", 5_000), // releasable 0
        active("usage", "sch-B", 8_000, 3_000), // releasable 5_000
    ];
    let result = split(&input(&streams, 7_000, 5_000)).unwrap();
    assert_eq!(result.deferred_part_minor, 5_000);
    assert_eq!(result.recognized_part_minor, 2_000);
    // Deferred + recognized remainder both land on the single live stream (sch-B).
    assert_eq!(result.per_stream[0].deferred_part_minor, 0);
    assert_eq!(result.per_stream[0].recognized_part_minor, 0);
    assert_eq!(result.per_stream[1].deferred_part_minor, 5_000);
    assert_eq!(result.per_stream[1].recognized_part_minor, 2_000);
}

#[test]
fn multi_stream_partial_deferred_split_is_indeterminable_and_blocks() {
    // Two releasable streams (4_000 + 6_000 = 10_000) but a PARTIAL deferred request
    // of 3_000 (< total releasable) ⇒ no unambiguous per-stream placement; spreading
    // it would be pro-rata ⇒ block.
    let streams = [
        active("recurring", "sch-A", 4_000, 0), // releasable 4_000
        active("usage", "sch-B", 6_000, 0),     // releasable 6_000
    ];
    let err = split(&input(&streams, 10_000, 3_000)).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditNoteSplitAmbiguous(_)),
        "partial multi-stream deferred split must block, got {err:?}"
    );
}

#[test]
fn multi_stream_full_drain_with_recognized_remainder_blocks() {
    // Drain both streams (deferred = total releasable 10_000) BUT the note also has
    // a recognized remainder (amount 12_000 > 10_000) — attributing the 2_000
    // recognized part across two drained streams would be pro-rata ⇒ block.
    let streams = [
        active("recurring", "sch-A", 4_000, 0), // releasable 4_000
        active("usage", "sch-B", 6_000, 0),     // releasable 6_000
    ];
    let err = split(&input(&streams, 12_000, 10_000)).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditNoteSplitAmbiguous(_)),
        "multi-stream drain with a recognized remainder must block, got {err:?}"
    );
}

#[test]
fn wholly_recognized_note_against_multi_stream_obligation_blocks() {
    // A note with NO deferred part (requested_deferred = 0) but a non-zero recognized
    // remainder against a multi-stream obligation has no unambiguous stream to
    // attribute the recognized reduction to (deferred placement is empty) ⇒ block
    // (attributing across streams would be pro-rata). The handler must target a
    // single stream (or supply single-stream state) for a recognized-only credit.
    let streams = [
        active("recurring", "sch-A", 4_000, 0),
        active("usage", "sch-B", 6_000, 0),
    ];
    let err = split(&input(&streams, 5_000, 0)).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditNoteSplitAmbiguous(_)),
        "recognized-only multi-stream note must block (no pro-rata), got {err:?}"
    );
}

#[test]
fn duplicate_stream_state_blocks() {
    // The same revenue_stream supplied twice ⇒ ambiguous item→schedule mapping.
    let streams = [
        active("recurring", "sch-A", 4_000, 0),
        active("recurring", "sch-B", 6_000, 0),
    ];
    let err = split(&input(&streams, 5_000, 0)).unwrap_err();
    assert!(matches!(err, DomainError::CreditNoteSplitAmbiguous(_)));
}

#[test]
fn negative_amount_is_rejected() {
    let streams = [active("recurring", "sch-1", 9_000, 5_000)];
    let err = split(&input(&streams, -1, 0)).unwrap_err();
    assert!(matches!(err, DomainError::AmountOutOfRange(_)));
}

#[test]
fn requested_deferred_over_amount_is_rejected() {
    let streams = [active("recurring", "sch-1", 9_000, 0)];
    let err = split(&input(&streams, 1_000, 2_000)).unwrap_err();
    assert!(matches!(err, DomainError::AmountOutOfRange(_)));
}

#[test]
fn non_active_schedule_has_no_releasable_remainder() {
    // A COMPLETED schedule yields 0 releasable even though total_deferred > 0, so a
    // deferred request against it blocks (no live balance to reduce).
    let streams = [completed("recurring", "sch-1", 8_000)];
    assert_eq!(streams[0].releasable_remaining_minor(), 0);
    let err = split(&input(&streams, 5_000, 1_000)).unwrap_err();
    assert!(matches!(err, DomainError::CreditNoteSplitAmbiguous(_)));
}

#[test]
fn split_basis_ref_is_deterministic_and_carries_the_basis() {
    // Same inputs ⇒ identical basis ref; it names the item, PO group, and each
    // stream's schedule@version + releasable state.
    let streams = [active("recurring", "sch-1", 9_000, 5_000)];
    let a = split(&input(&streams, 10_000, 3_000)).unwrap();
    let b = split(&input(&streams, 10_000, 3_000)).unwrap();
    assert_eq!(
        a.split_basis_ref, b.split_basis_ref,
        "basis must be reproducible"
    );
    assert!(a.split_basis_ref.contains("inv-1:item-1"));
    assert!(a.split_basis_ref.contains("po-grp-1"));
    assert!(a.split_basis_ref.contains("recurring"));
    assert!(a.split_basis_ref.contains("sch-1"));
    // releasable remainder (4_000) is recorded for audit/replay.
    assert!(a.split_basis_ref.contains("rel=4000"));
}

#[test]
fn split_basis_ref_handles_no_po_group_and_no_streams() {
    let streams: [ScheduleStreamState; 0] = [];
    let inp = SplitInput {
        source_invoice_item_ref: "inv-9:item-2",
        po_allocation_group: None,
        streams: &streams,
        amount_minor_ex_tax: 500,
        requested_deferred_minor: 0,
    };
    let result = split(&inp).unwrap();
    assert!(result.split_basis_ref.contains("inv-9:item-2"));
    assert!(result.split_basis_ref.contains("po=-"));
    assert!(result.split_basis_ref.contains("streams=none"));
}
