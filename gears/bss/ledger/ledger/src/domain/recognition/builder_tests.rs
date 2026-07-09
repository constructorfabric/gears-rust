//! Tests for the pure `ScheduleBuilder` derivation: straight-line segment
//! generation (count, sum, residual on last, consecutive periods), `POINT_IN_TIME`
//! ⇒ no deferral, the R4 immaterial-one-shot exemption boundary, the SSP-required
//! decision (multi-PO with/without ref), and the segment-count ceiling.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::config::RecognitionConfig;
use crate::domain::error::DomainError;
use crate::domain::recognition::input::{RecognitionInput, RecognitionTiming};
use crate::domain::recognition::ports::{
    DefaultDeferralPolicyResolver, DefaultSspResolver, DefaultVcResolver, RecognitionContext,
};

/// A straight-line spec over `periods`, first period defaulted from the invoice.
fn straight_line(periods: u32) -> RecognitionInput {
    RecognitionInput {
        policy_ref: "policy.sl.v1".to_owned(),
        timing: RecognitionTiming::StraightLine {
            periods,
            first_period_id: None,
        },
        po_allocation_group: Some("grp".to_owned()),
        multi_po: false,
        ssp_snapshot_ref: None,
        subscription_ref: Some("sub.1".to_owned()),
        vc_estimate_ref: None,
        vc_method_ref: None,
        immaterial_one_shot_sku: false,
    }
}

fn point_in_time() -> RecognitionInput {
    RecognitionInput {
        policy_ref: "policy.pit.v1".to_owned(),
        timing: RecognitionTiming::PointInTime,
        po_allocation_group: Some("default".to_owned()),
        multi_po: false,
        ssp_snapshot_ref: None,
        subscription_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
        immaterial_one_shot_sku: false,
    }
}

/// A context for `input`, with an explicit item amount + invoice total (for R4).
fn ctx_amt<'a>(
    input: &'a RecognitionInput,
    invoice_period: &'a str,
    amount: i64,
    invoice_total: i64,
) -> RecognitionContext<'a> {
    RecognitionContext {
        input,
        invoice_period_id: invoice_period,
        item_amount_minor_ex_tax: amount,
        invoice_total_minor: invoice_total,
        currency: "USD",
        revenue_stream: "recurring",
    }
}

/// Derive with the three v1 default resolvers + `config`.
fn derive(
    ctx: &RecognitionContext<'_>,
    config: &RecognitionConfig,
) -> Result<ScheduleOutcome, DomainError> {
    let policy = DefaultDeferralPolicyResolver;
    let ssp = DefaultSspResolver;
    let vc = DefaultVcResolver;
    ScheduleBuilder::new(&policy, &ssp, &vc, config).derive(ctx)
}

#[test]
fn straight_line_generates_n_segments_summing_to_deferred() {
    let cfg = RecognitionConfig::default();
    let input = straight_line(12);
    // 1000.00 over 12 → 11×83.33 + residual on last; Σ == 100_000.
    let outcome = derive(&ctx_amt(&input, "202606", 100_000, 100_000), &cfg).unwrap();
    let ScheduleOutcome::Schedule(s) = outcome else {
        panic!("expected a schedule");
    };
    assert_eq!(s.deferred_minor, 100_000);
    assert_eq!(s.segments.len(), 12);
    let sum: i64 = s.segments.iter().map(|seg| seg.amount_minor).sum();
    assert_eq!(sum, 100_000, "segments must sum to the deferred amount");
    // Residual cent lands on the LAST segment (allocate Residual::Last).
    let last = s.segments.last().unwrap().amount_minor;
    let first = s.segments.first().unwrap().amount_minor;
    assert!(last >= first, "residual is placed on the last segment");
    assert_eq!(first, 8_333);
    assert_eq!(last, 8_337); // 100_000 - 11*8_333
}

#[test]
fn straight_line_lays_out_consecutive_periods_from_invoice_period() {
    let cfg = RecognitionConfig::default();
    let input = straight_line(3);
    let outcome = derive(&ctx_amt(&input, "202611", 300, 300), &cfg).unwrap();
    let ScheduleOutcome::Schedule(s) = outcome else {
        panic!("expected a schedule");
    };
    let periods: Vec<&str> = s.segments.iter().map(|x| x.period_id.as_str()).collect();
    // From 2026-11, three consecutive months crossing the year boundary.
    assert_eq!(periods, vec!["202611", "202612", "202701"]);
    // segment_no is 1-based and 1:1 with period order.
    assert_eq!(
        s.segments.iter().map(|x| x.segment_no).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    // Stamped refs flow through from the input + context.
    assert_eq!(s.policy_ref, "policy.sl.v1");
    assert_eq!(s.po_allocation_group.as_deref(), Some("grp"));
    assert_eq!(s.subscription_ref.as_deref(), Some("sub.1"));
    assert_eq!(s.revenue_stream, "recurring");
    assert_eq!(s.currency, "USD");
}

#[test]
fn point_in_time_yields_no_deferral() {
    let cfg = RecognitionConfig::default();
    let input = point_in_time();
    let outcome = derive(&ctx_amt(&input, "202606", 5_000, 500_000), &cfg).unwrap();
    assert_eq!(outcome, ScheduleOutcome::NoDeferral);
    assert_eq!(outcome.deferred_minor(), 0);
}

#[test]
fn r4_exemption_just_under_threshold_recognizes_now() {
    // invoice_total = 500_000 ⇒ 1% leg = 5_000 (< 10_000 USD floor) ⇒ threshold
    // = 5_000. A SKU-flagged straight-line item of exactly 5_000 is immaterial
    // (<=), so it recognizes now (no schedule) despite the deferring timing.
    let cfg = RecognitionConfig::default();
    let input = RecognitionInput {
        immaterial_one_shot_sku: true,
        ..straight_line(12)
    };
    let outcome = derive(&ctx_amt(&input, "202606", 5_000, 500_000), &cfg).unwrap();
    assert_eq!(
        outcome,
        ScheduleOutcome::NoDeferral,
        "at/under the materiality threshold ⇒ exempt"
    );
}

#[test]
fn r4_exemption_just_over_threshold_defers() {
    // One minor unit over the 5_000 threshold ⇒ material ⇒ a real schedule.
    let cfg = RecognitionConfig::default();
    let input = RecognitionInput {
        immaterial_one_shot_sku: true,
        ..straight_line(12)
    };
    let outcome = derive(&ctx_amt(&input, "202606", 5_001, 500_000), &cfg).unwrap();
    assert!(
        matches!(outcome, ScheduleOutcome::Schedule(_)),
        "just over the threshold ⇒ not exempt, must defer"
    );
}

#[test]
fn r4_exemption_needs_the_sku_flag() {
    // Under the threshold but NOT SKU-flagged ⇒ no exemption, defers normally.
    let cfg = RecognitionConfig::default();
    let input = straight_line(12); // immaterial_one_shot_sku = false
    let outcome = derive(&ctx_amt(&input, "202606", 1_000, 500_000), &cfg).unwrap();
    assert!(
        matches!(outcome, ScheduleOutcome::Schedule(_)),
        "exemption requires the SKU flag, not just a small amount"
    );
}

#[test]
fn multi_po_without_ssp_ref_blocks() {
    let cfg = RecognitionConfig::default();
    let input = RecognitionInput {
        multi_po: true,
        ssp_snapshot_ref: None,
        ..straight_line(6)
    };
    let err = derive(&ctx_amt(&input, "202606", 60_000, 60_000), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::SspSnapshotRequired(_)));
}

#[test]
fn multi_po_with_ssp_ref_builds_and_stamps_it() {
    let cfg = RecognitionConfig::default();
    let input = RecognitionInput {
        multi_po: true,
        ssp_snapshot_ref: Some("ssp.pinned.v2".to_owned()),
        ..straight_line(6)
    };
    let outcome = derive(&ctx_amt(&input, "202606", 60_000, 60_000), &cfg).unwrap();
    let ScheduleOutcome::Schedule(s) = outcome else {
        panic!("expected a schedule");
    };
    assert_eq!(s.ssp_snapshot_ref.as_deref(), Some("ssp.pinned.v2"));
    assert_eq!(s.segments.len(), 6);
}

#[test]
fn segments_over_ceiling_block_with_schedule_too_long() {
    // Default ceiling is 120; 121 segments must block.
    let cfg = RecognitionConfig::default();
    let input = straight_line(121);
    let err = derive(&ctx_amt(&input, "202606", 121_000, 121_000), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::ScheduleTooLong(_)));
}

#[test]
fn segments_at_ceiling_are_allowed() {
    // Exactly at the ceiling is fine (the guard is strictly-greater-than).
    let cfg = RecognitionConfig {
        max_segments_per_schedule: 3,
        ..RecognitionConfig::default()
    };
    let input = straight_line(3);
    let outcome = derive(&ctx_amt(&input, "202606", 300, 300), &cfg).unwrap();
    assert!(matches!(outcome, ScheduleOutcome::Schedule(_)));
    // One over the lowered ceiling blocks.
    let input4 = straight_line(4);
    let err = derive(&ctx_amt(&input4, "202606", 400, 400), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::ScheduleTooLong(_)));
}

#[test]
fn zero_periods_is_rejected() {
    let cfg = RecognitionConfig::default();
    let input = straight_line(0);
    let err = derive(&ctx_amt(&input, "202606", 100, 100), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::AmountOutOfRange(_)));
}

#[test]
fn negative_amount_is_rejected() {
    let cfg = RecognitionConfig::default();
    let input = straight_line(3);
    let err = derive(&ctx_amt(&input, "202606", -1, 100), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::AmountOutOfRange(_)));
}

#[test]
fn built_schedule_exposes_the_fields_the_sidecar_needs() {
    // The Group C sidecar (infra) builds the repo rows from these public fields;
    // assert they are populated for a representative straight-line schedule.
    let cfg = RecognitionConfig::default();
    let input = straight_line(2);
    let outcome = derive(&ctx_amt(&input, "202606", 200, 200), &cfg).unwrap();
    let ScheduleOutcome::Schedule(s) = outcome else {
        panic!("expected a schedule");
    };
    assert_eq!(s.deferred_minor, 200);
    assert_eq!(s.policy_ref, "policy.sl.v1");
    assert_eq!(s.revenue_stream, "recurring");
    assert_eq!(s.currency, "USD");
    assert_eq!(s.subscription_ref.as_deref(), Some("sub.1"));
    assert_eq!(s.segments.len(), 2);
    assert_eq!(s.segments[0].segment_no, 1);
    assert_eq!(s.segments[0].period_id, "202606");
    assert_eq!(s.segments[1].period_id, "202607");
    assert_eq!(s.segments.iter().map(|x| x.amount_minor).sum::<i64>(), 200);
}
