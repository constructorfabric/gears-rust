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
fn money(amount: &str) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::money::parse_decimal(amount).unwrap(),
        bss_ledger_sdk::money::CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

fn ctx_amt<'a>(
    input: &'a RecognitionInput,
    invoice_period: &'a str,
    amount: &str,
    invoice_total: &str,
) -> RecognitionContext<'a> {
    RecognitionContext {
        input,
        invoice_period_id: invoice_period,
        // Leaked so the borrowed context can outlive this helper (test-only).
        item_amount_ex_tax: Box::leak(Box::new(money(amount))),
        invoice_total: Box::leak(Box::new(money(invoice_total))),
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
    // 1000.00 over 12 → 11×83.33 + residual on last; Σ == 1000.00.
    let outcome = derive(&ctx_amt(&input, "202606", "1000", "1000"), &cfg).unwrap();
    let ScheduleOutcome::Schedule(s) = outcome else {
        panic!("expected a schedule");
    };
    assert_eq!(s.deferred, money("1000"));
    assert_eq!(s.segments.len(), 12);
    let sum: Decimal = s.segments.iter().map(|seg| seg.amount.amount()).sum();
    assert_eq!(
        sum,
        money("1000").amount(),
        "segments must sum to the deferred amount"
    );
    // Residual cent lands on the LAST segment (allocate Residual::Last).
    let last = s.segments.last().unwrap().amount.amount();
    let first = s.segments.first().unwrap().amount.amount();
    assert!(last >= first, "residual is placed on the last segment");
    assert_eq!(first, money("83.33").amount());
    assert_eq!(last, money("83.37").amount()); // 1000.00 - 11*83.33
}

#[test]
fn straight_line_lays_out_consecutive_periods_from_invoice_period() {
    let cfg = RecognitionConfig::default();
    let input = straight_line(3);
    let outcome = derive(&ctx_amt(&input, "202611", "3", "3"), &cfg).unwrap();
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
    assert_eq!(s.deferred.currency().code(), "USD");
}

#[test]
fn point_in_time_yields_no_deferral() {
    let cfg = RecognitionConfig::default();
    let input = point_in_time();
    let outcome = derive(&ctx_amt(&input, "202606", "50", "5000"), &cfg).unwrap();
    assert_eq!(outcome, ScheduleOutcome::NoDeferral);
    assert_eq!(outcome.deferred(), None);
}

#[test]
fn r4_exemption_just_under_threshold_recognizes_now() {
    // invoice_total = 5000 ⇒ 1% leg = 50 (< 100 absolute ceiling) ⇒ threshold
    // = 50. A SKU-flagged straight-line item of exactly 50 is immaterial
    // (<=), so it recognizes now (no schedule) despite the deferring timing.
    let cfg = RecognitionConfig::default();
    let input = RecognitionInput {
        immaterial_one_shot_sku: true,
        ..straight_line(12)
    };
    let outcome = derive(&ctx_amt(&input, "202606", "50", "5000"), &cfg).unwrap();
    assert_eq!(
        outcome,
        ScheduleOutcome::NoDeferral,
        "at/under the materiality threshold ⇒ exempt"
    );
}

#[test]
fn r4_exemption_just_over_threshold_defers() {
    // One posting increment over the 50 threshold ⇒ material ⇒ a real schedule.
    let cfg = RecognitionConfig::default();
    let input = RecognitionInput {
        immaterial_one_shot_sku: true,
        ..straight_line(12)
    };
    let outcome = derive(&ctx_amt(&input, "202606", "50.01", "5000"), &cfg).unwrap();
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
    let outcome = derive(&ctx_amt(&input, "202606", "10", "5000"), &cfg).unwrap();
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
    let err = derive(&ctx_amt(&input, "202606", "600", "600"), &cfg).unwrap_err();
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
    let outcome = derive(&ctx_amt(&input, "202606", "600", "600"), &cfg).unwrap();
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
    let err = derive(&ctx_amt(&input, "202606", "1210", "1210"), &cfg).unwrap_err();
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
    let outcome = derive(&ctx_amt(&input, "202606", "3", "3"), &cfg).unwrap();
    assert!(matches!(outcome, ScheduleOutcome::Schedule(_)));
    // One over the lowered ceiling blocks.
    let input4 = straight_line(4);
    let err = derive(&ctx_amt(&input4, "202606", "4", "4"), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::ScheduleTooLong(_)));
}

#[test]
fn zero_periods_is_rejected() {
    let cfg = RecognitionConfig::default();
    let input = straight_line(0);
    let err = derive(&ctx_amt(&input, "202606", "1", "1"), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::AmountOutOfRange(_)));
}

#[test]
fn negative_amount_is_rejected() {
    let cfg = RecognitionConfig::default();
    let input = straight_line(3);
    let err = derive(&ctx_amt(&input, "202606", "-0.01", "1"), &cfg).unwrap_err();
    assert!(matches!(err, DomainError::AmountOutOfRange(_)));
}

#[test]
fn built_schedule_exposes_the_fields_the_sidecar_needs() {
    // The Group C sidecar (infra) builds the repo rows from these public fields;
    // assert they are populated for a representative straight-line schedule.
    let cfg = RecognitionConfig::default();
    let input = straight_line(2);
    let outcome = derive(&ctx_amt(&input, "202606", "2", "2"), &cfg).unwrap();
    let ScheduleOutcome::Schedule(s) = outcome else {
        panic!("expected a schedule");
    };
    assert_eq!(s.deferred, money("2"));
    assert_eq!(s.policy_ref, "policy.sl.v1");
    assert_eq!(s.revenue_stream, "recurring");
    assert_eq!(s.deferred.currency().code(), "USD");
    assert_eq!(s.subscription_ref.as_deref(), Some("sub.1"));
    assert_eq!(s.segments.len(), 2);
    assert_eq!(s.segments[0].segment_no, 1);
    assert_eq!(s.segments[0].period_id, "202606");
    assert_eq!(s.segments[1].period_id, "202607");
    assert_eq!(
        s.segments
            .iter()
            .map(|x| x.amount.amount())
            .sum::<Decimal>(),
        money("2").amount()
    );
}

fn specified(amount: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::money::parse_decimal(amount).unwrap(),
        bss_ledger_sdk::money::CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

#[test]
fn recognition_thirds_preserve_stored_specs_and_exact_total_at_all_scales() {
    for (scale, first, last) in [
        (0, "0", "1"),
        (2, "0.33", "0.34"),
        (3, "0.333", "0.334"),
        (8, "0.33333333", "0.33333334"),
        (
            28,
            "0.3333333333333333333333333333",
            "0.3333333333333333333333333334",
        ),
    ] {
        let input = straight_line(3);
        let amount = specified("1", "EUR", scale);
        let mut ctx = ctx_amt(&input, "202611", "1", "1");
        ctx.item_amount_ex_tax = &amount;
        ctx.invoice_total = &amount;
        let ScheduleOutcome::Schedule(schedule) =
            derive(&ctx, &RecognitionConfig::default()).unwrap()
        else {
            panic!("schedule");
        };
        assert_eq!(&schedule.deferred, ctx.item_amount_ex_tax);
        assert_eq!(schedule.segments[0].amount, specified(first, "EUR", scale));
        assert_eq!(schedule.segments[2].amount, specified(last, "EUR", scale));
        let amounts: Vec<_> = schedule
            .segments
            .iter()
            .map(|segment| segment.amount.clone())
            .collect();
        assert_eq!(
            crate::domain::exact_money::sum_posted(&amounts, schedule.deferred.currency().clone())
                .unwrap(),
            schedule.deferred
        );
        assert_eq!(
            schedule
                .segments
                .iter()
                .map(|s| s.period_id.as_str())
                .collect::<Vec<_>>(),
            ["202611", "202612", "202701"]
        );
    }
}

#[test]
fn materiality_absolute_and_relative_boundaries_are_inclusive_at_all_scales() {
    for (scale, ceiling, above, invoice, below_invoice) in [
        (0, "10000", "10001", "1000000", "999999"),
        (2, "100", "100.01", "10000", "9999.99"),
        (3, "10", "10.001", "1000", "999.999"),
        (8, "0.0001", "0.00010001", "0.01", "0.00999999"),
        (
            28,
            "0.000000000000000000000001",
            "0.0000000000000000000000010001",
            "0.0000000000000000000001",
            "0.0000000000000000000000999999",
        ),
    ] {
        let item = specified(ceiling, "EUR", scale);
        let total = specified(invoice, "EUR", scale);
        assert!(is_immaterial(&item, &total).unwrap());
        assert!(
            !is_immaterial(
                &specified(above, "EUR", scale),
                &specified("1000000", "EUR", scale)
            )
            .unwrap()
        );
        assert!(!is_immaterial(&item, &specified(below_invoice, "EUR", scale)).unwrap());
        assert!(
            is_immaterial(&specified("0", "EUR", scale), &specified("0", "EUR", scale)).unwrap()
        );
    }
}

#[test]
fn materiality_uses_exact_products_beyond_posted_bounds() {
    let huge = specified("9999999999999999999999999999", "EUR", 0);
    assert!(!is_immaterial(&huge, &huge).unwrap());
    let input = RecognitionInput {
        immaterial_one_shot_sku: true,
        ..straight_line(1)
    };
    let mut ctx = ctx_amt(&input, "202606", "1", "1");
    ctx.item_amount_ex_tax = &huge;
    ctx.invoice_total = &huge;
    let ScheduleOutcome::Schedule(schedule) = derive(&ctx, &RecognitionConfig::default()).unwrap()
    else {
        panic!("schedule");
    };
    assert_eq!(schedule.segments[0].amount, huge);
}

#[test]
fn zero_metadata_mismatch_blocks_before_ssp_policy_or_point_in_time() {
    for timing in [
        RecognitionTiming::PointInTime,
        RecognitionTiming::StraightLine {
            periods: 3,
            first_period_id: None,
        },
    ] {
        let input = RecognitionInput {
            timing,
            multi_po: true,
            immaterial_one_shot_sku: true,
            ..point_in_time()
        };
        let eur_zero = specified("0", "EUR", 2);
        let usd_zero_at_3 = specified("0", "USD", 3);
        let mut ctx = ctx_amt(&input, "bad period", "0", "0");
        ctx.invoice_total = &eur_zero;
        assert!(matches!(
            derive(&ctx, &RecognitionConfig::default()),
            Err(DomainError::CurrencyMismatch(_))
        ));
        assert!(matches!(
            is_immaterial(ctx.item_amount_ex_tax, ctx.invoice_total),
            Err(DomainError::CurrencyMismatch(_))
        ));
        ctx.invoice_total = &usd_zero_at_3;
        assert!(matches!(
            derive(&ctx, &RecognitionConfig::default()),
            Err(DomainError::InconsistentScale(_))
        ));
        assert!(matches!(
            is_immaterial(ctx.item_amount_ex_tax, ctx.invoice_total),
            Err(DomainError::InconsistentScale(_))
        ));
    }
}

#[test]
fn segment_count_guards_run_before_weights_allocation() {
    assert_eq!(segment_count(120, 120).unwrap(), 120);
    assert_eq!(
        segment_count(i32::MAX as u32, usize::MAX).unwrap(),
        i32::MAX as usize
    );
    for periods in [i32::MAX as u32 + 1, u32::MAX] {
        assert!(matches!(
            segment_count(periods, usize::MAX),
            Err(DomainError::ScheduleTooLong(_))
        ));
        let input = straight_line(periods);
        let config = RecognitionConfig {
            max_segments_per_schedule: usize::MAX,
            ..RecognitionConfig::default()
        };
        assert!(matches!(
            derive(&ctx_amt(&input, "202606", "1", "1"), &config),
            Err(DomainError::ScheduleTooLong(_))
        ));
    }
    assert!(matches!(
        segment_count(121, 120),
        Err(DomainError::ScheduleTooLong(_))
    ));
    assert!(matches!(
        segment_count(0, 120),
        Err(DomainError::AmountOutOfRange(_))
    ));
    let input = straight_line(120);
    let ScheduleOutcome::Schedule(schedule) = derive(
        &ctx_amt(&input, "202606", "120", "120"),
        &RecognitionConfig::default(),
    )
    .unwrap() else {
        panic!("schedule");
    };
    assert_eq!(schedule.segments.len(), 120);
    assert_eq!(schedule.segments[119].segment_no, 120);
    assert!(schedule.segments.iter().all(|s| s.amount == money("1")));
}

#[test]
fn negative_last_residual_is_named_failure_and_zero_segments_remain_allowed() {
    let input = straight_line(6);
    assert!(
        matches!(derive(&ctx_amt(&input, "202606", "0.04", "0.04"), &RecognitionConfig::default()), Err(DomainError::AmountOutOfRange(detail)) if detail.contains("negative"))
    );
    for amount in ["0", "0.01"] {
        let ScheduleOutcome::Schedule(schedule) = derive(
            &ctx_amt(&input, "202606", amount, amount),
            &RecognitionConfig::default(),
        )
        .unwrap() else {
            panic!("schedule");
        };
        assert!(
            schedule.segments[..5]
                .iter()
                .all(|s| s.amount == money("0"))
        );
        assert_eq!(schedule.segments[5].amount, money(amount));
    }
}

#[test]
fn builder_stamps_vc_only_after_numeric_ssp_policy_and_layout_gates() {
    struct RejectVc;
    impl VcResolver for RejectVc {
        fn resolve(
            &self,
            _: &RecognitionContext<'_>,
        ) -> Result<(Option<String>, Option<String>), DomainError> {
            Err(DomainError::InvalidRequest("VC gate reached".to_owned()))
        }
    }
    let cfg = RecognitionConfig::default();
    let policy = DefaultDeferralPolicyResolver;
    let ssp = DefaultSspResolver;
    let builder = ScheduleBuilder::new(&policy, &ssp, &RejectVc, &cfg);
    let input = point_in_time();
    assert_eq!(
        builder
            .derive(&ctx_amt(&input, "202606", "1", "1"))
            .unwrap(),
        ScheduleOutcome::NoDeferral
    );
    let input = RecognitionInput {
        immaterial_one_shot_sku: true,
        ..straight_line(3)
    };
    assert_eq!(
        builder
            .derive(&ctx_amt(&input, "202606", "1", "100"))
            .unwrap(),
        ScheduleOutcome::NoDeferral
    );
    let input = RecognitionInput {
        multi_po: true,
        ..straight_line(0)
    };
    assert!(matches!(
        builder.derive(&ctx_amt(&input, "202606", "1", "1")),
        Err(DomainError::SspSnapshotRequired(_))
    ));
    let input = straight_line(0);
    assert!(matches!(
        builder.derive(&ctx_amt(&input, "202606", "1", "1")),
        Err(DomainError::AmountOutOfRange(_))
    ));
    let input = straight_line(1);
    assert!(
        matches!(builder.derive(&ctx_amt(&input, "202606", "1", "1")), Err(DomainError::InvalidRequest(detail)) if detail == "VC gate reached")
    );
    let input = RecognitionInput {
        vc_estimate_ref: Some("vc.est.1".to_owned()),
        vc_method_ref: Some("vc.method.1".to_owned()),
        ..straight_line(1)
    };
    let ScheduleOutcome::Schedule(schedule) =
        derive(&ctx_amt(&input, "202606", "1", "1"), &cfg).unwrap()
    else {
        panic!("schedule");
    };
    assert_eq!(schedule.vc_estimate_ref.as_deref(), Some("vc.est.1"));
    assert_eq!(schedule.vc_method_ref.as_deref(), Some("vc.method.1"));
}

#[test]
fn numeric_error_categories_remain_specific_and_structural_defects_internal() {
    assert!(matches!(
        map_money_error(MoneyError::InvalidDecimal),
        DomainError::InvalidRequest(_)
    ));
    assert!(matches!(
        map_money_error(MoneyError::InvalidCurrency),
        DomainError::InvalidRequest(_)
    ));
    assert!(matches!(
        map_money_error(MoneyError::ScaleOutOfRange),
        DomainError::ScaleOutOfRange(_)
    ));
    assert!(matches!(
        map_money_error(MoneyError::AmountOutOfRange),
        DomainError::AmountOutOfRange(_)
    ));
    assert!(matches!(
        map_money_error(MoneyError::InvalidPostingIncrement),
        DomainError::InvalidPostingIncrement(_)
    ));
    assert!(matches!(
        map_money_error(MoneyError::CurrencyMismatch),
        DomainError::CurrencyMismatch(_)
    ));
    assert!(matches!(
        map_money_error(MoneyError::ScaleMismatch),
        DomainError::InconsistentScale(_)
    ));
    assert!(matches!(
        map_exact_error(ExactError::Money(MoneyError::ScaleMismatch)),
        DomainError::InconsistentScale(_)
    ));
    assert!(matches!(
        map_exact_error(ExactError::ArithmeticLimit),
        DomainError::AmountOutOfRange(_)
    ));
    assert!(matches!(
        map_exact_error(ExactError::TooManyTerms),
        DomainError::ScheduleTooLong(_)
    ));
    for error in [
        ExactError::DivisionByZero,
        ExactError::EmptyWeights,
        ExactError::NegativeWeight,
    ] {
        assert!(matches!(map_exact_error(error), DomainError::Internal(_)));
    }
}
