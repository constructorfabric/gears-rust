//! Tests for the tier-band geometry and the usage evaluation policy.

use bss_fixtures::ModelKind;

use super::{BandGeometry, BandOrigin, BandTopOpen, UsageEvaluationPolicy};
use crate::domain::money::{MinorAmount, RateMinor};
use crate::domain::price_row::{BillingGranularity, PriceRow, TierAggregationWindow, TierBand};
use crate::domain::rules::{
    EVAL_POLICY_MISSING, TIER_AGG_WINDOW_INCOMPATIBLE, TIER_BAND_EMPTY, TIER_BAND_PRICE_INCREASE,
    TIER_BANDS_GAP, TIER_BANDS_OVERLAP, TIER_TOP_CLOSED,
};
use crate::domain::scope_key::ChargeKind;
use crate::domain::validation::{Stage, ValidationReport, ValidationRule};

fn minor(units: i64) -> MinorAmount {
    MinorAmount::new(units).expect("test amount is non-negative")
}

/// A band rate, stated in whole minor units so these cases read as they
/// always did (D-311). The stored scale is 10^-9 of one.
fn rate(minor_units: i64) -> RateMinor {
    RateMinor::from_minor_units(minor_units).expect("test rate is non-negative")
}

fn findings(rule: &impl ValidationRule<PriceRow>, row: &PriceRow) -> ValidationReport {
    let mut report = ValidationReport::default();
    rule.evaluate(row, &mut report);
    report
}

fn codes(report: &ValidationReport) -> Vec<&str> {
    report
        .violations
        .iter()
        .map(|violation| violation.code.as_str())
        .collect()
}

/// A `graduated` usage row with a well-formed descending two-band ladder.
fn tiered(bands: Vec<TierBand>) -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("api_calls".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);
    row.bands = bands;
    row
}

fn descending_ladder() -> Vec<TierBand> {
    vec![
        TierBand::closed(0, 1_000, rate(10)),
        TierBand::closed(1_000, 10_000, rate(6)),
        TierBand::open(10_000, rate(3)),
    ]
}

#[test]
fn a_contiguous_ladder_passes() {
    let row = tiered(descending_ladder());

    let report = findings(&BandGeometry, &row);

    assert!(report.violations.is_empty());
    assert!(report.warnings.is_empty());
    assert!(findings(&BandOrigin, &row).violations.is_empty());
    assert!(findings(&BandTopOpen, &row).violations.is_empty());
}

#[test]
fn adjacent_bands_share_their_boundary_exactly_once() {
    // `[0, 100)` then `[100, open)`: quantity 100 belongs to the second band and
    // to no other. One unit either way is a gap or an overlap.
    let row = tiered(vec![
        TierBand::closed(0, 100, rate(10)),
        TierBand::open(100, rate(5)),
    ]);

    assert!(findings(&BandGeometry, &row).violations.is_empty());
}

#[test]
fn overlapping_bands_fail_publish() {
    let row = tiered(vec![
        TierBand::closed(0, 100, rate(10)),
        TierBand::open(50, rate(5)),
    ]);

    assert_eq!(
        codes(&findings(&BandGeometry, &row)),
        vec![TIER_BANDS_OVERLAP]
    );
}

#[test]
fn a_gap_between_bands_fails_publish() {
    let row = tiered(vec![
        TierBand::closed(0, 100, rate(10)),
        TierBand::open(150, rate(5)),
    ]);

    let report = findings(&BandGeometry, &row);

    assert_eq!(codes(&report), vec![TIER_BANDS_GAP]);
    assert!(
        report.violations[0].detail.contains("[100, 150)"),
        "the finding must name the unpriced stretch: {}",
        report.violations[0].detail
    );
}

/// A zero-width band is refused **at the write**, and the stage is the whole
/// point of the case.
///
/// This refusal sits at `violate_at_write` because
/// `chk_pricing_price_tier_band_width` would otherwise be the first thing to read
/// the pair, and the caller would get a 500 carrying a Postgres sentence.
/// Asserting the code
/// alone cannot see that: reverting `tier_bands.rs` to `violate` leaves the code
/// unchanged, this suite and every other one green, and the 500 restored --
/// `model_kind_tests` asserts the stage for its siblings for exactly this reason.
#[test]
fn a_zero_width_band_is_refused_at_the_write() {
    let row = tiered(vec![
        TierBand::closed(0, 100, rate(10)),
        TierBand::closed(100, 100, rate(8)),
        TierBand::open(100, rate(5)),
    ]);

    let report = findings(&BandGeometry, &row);
    let empty: Vec<Stage> = report
        .violations
        .iter()
        .filter(|violation| violation.code == TIER_BAND_EMPTY)
        .map(|violation| violation.stage)
        .collect();

    assert_eq!(empty, vec![Stage::Write], "{:?}", report.violations);
}

#[test]
fn a_band_below_an_open_top_authored_last_is_not_an_overlap() {
    // The set below is a contiguous ladder written bottom-last. The persisted
    // band table is keyed (price_id, from_qty) with no ordinal, so authoring
    // order does not survive the store, and a verdict that depended on it would
    // differ between the save-time pre-check and the identical re-run inside
    // the publish commit — a verdict that depends on how the author happened to
    // type it.
    //
    // This is the whole of the out-of-order geometry claim; the negative
    // control that keeps sorting from laundering a broken set is
    // `an_out_of_order_set_with_a_real_gap_still_fails`, and the positive
    // overlap is `a_band_below_an_open_top_reads_as_an_overlap` one case down.
    let row = tiered(vec![
        TierBand::open(1_000, rate(5)),
        TierBand::closed(0, 1_000, rate(10)),
    ]);

    assert!(findings(&BandGeometry, &row).is_publishable());
}

#[test]
fn a_band_below_an_open_top_reads_as_an_overlap() {
    // An open top covers every quantity above its floor, so anything after it in
    // the set is inside it by construction.
    let row = tiered(vec![
        TierBand::open(0, rate(10)),
        TierBand::open(1_000, rate(5)),
    ]);

    assert!(codes(&findings(&BandGeometry, &row)).contains(&TIER_BANDS_OVERLAP));
}

#[test]
fn a_rising_ladder_warns_without_blocking() {
    // The non-volume-discount pattern is unusual, not illegal: congestion and
    // penalty tiers are real. A warning that could block would be a fail-closed
    // rule hiding behind a soft word.
    let row = tiered(vec![
        TierBand::closed(0, 1_000, rate(3)),
        TierBand::open(1_000, rate(9)),
    ]);

    let report = findings(&BandGeometry, &row);

    assert!(report.is_publishable());
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].code, TIER_BAND_PRICE_INCREASE);
}

#[test]
fn a_flat_step_in_the_ladder_is_not_a_rise() {
    let row = tiered(vec![
        TierBand::closed(0, 1_000, rate(5)),
        TierBand::open(1_000, rate(5)),
    ]);

    assert!(findings(&BandGeometry, &row).warnings.is_empty());
}

#[test]
fn a_closed_top_band_fails_publish() {
    // D-17: "price undefined above X" is never the commercial intent. Capping is
    // an entitlement quota, so any quantity stays rateable.
    let row = tiered(vec![
        TierBand::closed(0, 1_000, rate(10)),
        TierBand::closed(1_000, 10_000, rate(5)),
    ]);

    assert_eq!(codes(&findings(&BandTopOpen, &row)), vec![TIER_TOP_CLOSED]);
}

#[test]
fn an_open_top_band_passes() {
    let row = tiered(descending_ladder());

    assert!(findings(&BandTopOpen, &row).violations.is_empty());
}

#[test]
fn a_tiered_row_whose_first_band_misses_the_origin_fails_publish() {
    let row = tiered(vec![
        TierBand::closed(10, 1_000, rate(10)),
        TierBand::open(1_000, rate(5)),
    ]);

    assert_eq!(codes(&findings(&BandOrigin, &row)), vec![TIER_BANDS_GAP]);
}

#[test]
fn a_zero_priced_first_band_is_valid() {
    // The exception that matters: a free opening band is how "N included" is
    // authored by hand, and it is the shape the D-45 allowance compile projects.
    let row = tiered(vec![
        TierBand::closed(0, 100, rate(0)),
        TierBand::open(100, rate(5)),
    ]);

    assert!(findings(&BandOrigin, &row).violations.is_empty());
    assert!(findings(&BandGeometry, &row).is_publishable());
}

#[test]
fn a_tiered_row_with_no_bands_at_all_fails_publish() {
    let row = tiered(Vec::new());

    assert_eq!(codes(&findings(&BandOrigin, &row)), vec![TIER_BANDS_GAP]);
}

#[test]
fn the_origin_rule_judges_only_the_kinds_whose_money_is_in_bands() {
    // A `package` or `per_unit` row owes no origin band; carrying bands at all
    // is a placement fault the kind rules own.
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Package));
    row.package_size = Some(100);

    assert!(findings(&BandOrigin, &row).violations.is_empty());
}

#[test]
fn a_usage_row_without_billing_granularity_fails_publish() {
    let mut row = tiered(descending_ladder());
    row.billing_granularity = None;

    assert_eq!(
        codes(&findings(&UsageEvaluationPolicy, &row)),
        vec![EVAL_POLICY_MISSING]
    );
}

#[test]
fn a_tiered_usage_row_without_a_reset_window_fails_publish() {
    let mut row = tiered(descending_ladder());
    row.tier_aggregation_window = None;

    assert_eq!(
        codes(&findings(&UsageEvaluationPolicy, &row)),
        vec![EVAL_POLICY_MISSING]
    );
}

#[test]
fn a_non_usage_row_owes_no_evaluation_policy() {
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
    row.amount_minor = Some(minor(100));

    assert!(findings(&UsageEvaluationPolicy, &row).violations.is_empty());
}

#[test]
fn a_package_rows_missing_window_is_reported_once_by_the_package_rule() {
    // `inst-pk-window` owns the `package` case, so this rule must stay quiet
    // about it: one missing field has to read as one fault.
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Package));
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row.package_size = Some(100);
    row.package_price_minor = Some(minor(500));

    assert!(findings(&UsageEvaluationPolicy, &row).violations.is_empty());
}

#[test]
fn an_out_of_order_set_with_a_real_gap_still_fails() {
    // Sorting must not launder a broken set into a whole one.
    let row = tiered(vec![
        TierBand::open(1_500, rate(6)),
        TierBand::closed(0, 1_000, rate(10)),
    ]);

    let report = findings(&BandGeometry, &row);

    assert!(!report.is_publishable());
    assert!(codes(&report).contains(&TIER_BANDS_GAP));
}

#[test]
fn a_rise_out_of_a_free_opening_band_does_not_warn() {
    // "N included, then priced" is the canonical allowance shape and the one the
    // D-45 compile projects. Warning on it would fire on nearly every allowance
    // row, and a channel that is noisy by default is a channel authors stop
    // reading.
    let row = tiered(vec![
        TierBand::closed(0, 100, rate(0)),
        TierBand::open(100, rate(5)),
    ]);

    assert!(findings(&BandGeometry, &row).warnings.is_empty());
}

#[test]
fn a_rise_out_of_a_priced_band_still_warns() {
    // The exemption is narrow on purpose: it is about a free opening band, not
    // about every ladder whose first step is cheap.
    let row = tiered(vec![
        TierBand::closed(0, 100, rate(1)),
        TierBand::open(100, rate(5)),
    ]);

    let report = findings(&BandGeometry, &row);

    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].code, TIER_BAND_PRICE_INCREASE);
}

/// D-313's hourly window, and the one pair of legal values that cannot meet.
mod per_hour_window {
    use super::*;

    /// An hourly counter is a legal answer to `inst-tb-window`'s requirement, so
    /// the row that carries it owes nothing further. Asserted because the value
    /// was added to the enum after the rule was written, and a rule that had
    /// enumerated its own accepted set instead of reading the type would have gone
    /// on demanding a window that was already there.
    #[test]
    fn an_hourly_window_satisfies_the_requirement_it_is_a_member_of() {
        let mut row = tiered(descending_ladder());
        row.tier_aggregation_window = Some(TierAggregationWindow::PerHour);
        row.billing_granularity = Some(BillingGranularity::PerHour);

        assert!(
            findings(&UsageEvaluationPolicy, &row).violations.is_empty(),
            "per_hour is a window, not the absence of one"
        );
    }

    /// The refusal: a day-long billable unit cannot be banded by the hour.
    #[test]
    fn an_hourly_counter_under_a_daily_billable_unit_is_refused() {
        let mut row = tiered(descending_ladder());
        row.tier_aggregation_window = Some(TierAggregationWindow::PerHour);
        row.billing_granularity = Some(BillingGranularity::PerDay);

        assert_eq!(
            codes(&findings(&UsageEvaluationPolicy, &row)),
            vec![TIER_AGG_WINDOW_INCOMPATIBLE]
        );
    }

    /// **The deliberate non-refusal**, and the half a narrower check would have
    /// got wrong. `whole_unit` quantizes to a unit of the meter and names no
    /// period, so an hourly counter over whole units is coherent; a rule that
    /// refused "everything not finer than an hour" would refuse it and no test
    /// would say why that is wrong.
    #[test]
    fn an_hourly_counter_over_whole_units_is_left_alone() {
        let mut row = tiered(descending_ladder());
        row.tier_aggregation_window = Some(TierAggregationWindow::PerHour);
        row.billing_granularity = Some(BillingGranularity::WholeUnit);

        assert!(
            findings(&UsageEvaluationPolicy, &row).violations.is_empty(),
            "whole_unit is not a long period, it is no period at all"
        );
    }

    /// Every granularity that fits inside an hour, so the refusal is pinned to the
    /// one value it is about rather than to "not `per_hour`".
    #[test]
    fn every_sub_hourly_granularity_pairs_with_the_hourly_counter() {
        for granularity in [
            BillingGranularity::PerSecond,
            BillingGranularity::PerMinute,
            BillingGranularity::PerHour,
        ] {
            let mut row = tiered(descending_ladder());
            row.tier_aggregation_window = Some(TierAggregationWindow::PerHour);
            row.billing_granularity = Some(granularity);

            assert!(
                findings(&UsageEvaluationPolicy, &row).violations.is_empty(),
                "{granularity} fits inside the hour it is counted in"
            );
        }
    }

    /// The window is only refused beside the granularity it contradicts: the same
    /// `per_day` unit under a monthly counter is the ordinary case.
    #[test]
    fn a_daily_billable_unit_is_fine_under_a_window_that_contains_it() {
        let mut row = tiered(descending_ladder());
        row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);
        row.billing_granularity = Some(BillingGranularity::PerDay);

        assert!(findings(&UsageEvaluationPolicy, &row).violations.is_empty());
    }
}
// ---------------------------------------------------------------------------
// The window follows the **presented** shape (D-45).
// ---------------------------------------------------------------------------

/// An untiered `per_unit` usage row with no allowance: it prices every unit at
/// one rate, no counter accumulates, and nothing resets. The positive control
/// that keeps the case below from being green because the rule started asking
/// every `per_unit` row for a window.
fn per_unit_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
    row.meter = Some("api_calls".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row.unit_rate = Some(rate(10));
    row
}

fn allowance(quantity: u64) -> crate::domain::price_row::IncludedAllowance {
    crate::domain::price_row::IncludedAllowance {
        quantity,
        rollover_policy: crate::domain::price_row::RolloverPolicy::None,
    }
}

#[test]
fn an_untiered_per_unit_row_owes_no_reset_window() {
    let row = per_unit_usage();

    assert!(
        findings(&UsageEvaluationPolicy, &row).violations.is_empty(),
        "a per_unit row with no allowance accumulates no counter"
    );
}

/// The money case. A `per_unit` row carrying `includedAllowance {N, none}`
/// **publishes as a band ladder** (`inst-ac-band`): `[0, N) @ $0`, then the
/// folded rate. That ladder has a tier counter, and with no
/// `tierAggregationWindow` the counter never resets — so the N free units are
/// granted once for the subscription's whole life instead of once per period.
#[test]
fn a_per_unit_row_compiled_into_a_ladder_by_an_allowance_needs_a_reset_window() {
    let mut row = per_unit_usage();
    row.included_allowance = Some(allowance(100));

    assert_eq!(
        codes(&findings(&UsageEvaluationPolicy, &row)),
        vec![EVAL_POLICY_MISSING],
        "the compiled [0, 100) $0 band is a tier counter and it must reset"
    );
}

/// The positive control for the case above: the same row, window authored.
#[test]
fn a_compiled_allowance_row_that_names_its_window_passes() {
    let mut row = per_unit_usage();
    row.included_allowance = Some(allowance(100));
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);

    assert!(findings(&UsageEvaluationPolicy, &row).violations.is_empty());
}

/// The narrowness control. A `carry` allowance does not compile (its grant has
/// no store), so the row still presents `per_unit` and still owes no window —
/// the rule must read what publishes, not the mere presence of the field.
#[test]
fn an_uncompiled_allowance_leaves_the_row_untiered() {
    let mut row = per_unit_usage();
    row.included_allowance = Some(crate::domain::price_row::IncludedAllowance {
        quantity: 100,
        rollover_policy: crate::domain::price_row::RolloverPolicy::Carry,
    });

    assert!(findings(&UsageEvaluationPolicy, &row).violations.is_empty());
}
