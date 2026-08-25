//! Tests for the supersession unit guard.

use bss_fixtures::ModelKind;

use super::{SupersessionPair, SupersessionUnitGuard};
use crate::domain::money::{MinorAmount, RateMinor};
use crate::domain::price_row::{
    AggregationFunction, AggregationGranularity, BillingGranularity, IncludedAllowance, PriceRow,
    ReservationFlavor, RolloverPolicy, TierAggregationWindow, TierBand, TierQualificationWindow,
    unit_determining_mismatch,
};
use crate::domain::rules::SUPERSESSION_UNIT_MISMATCH;
use crate::domain::scope_key::ChargeKind;
use crate::domain::validation::{ValidationReport, ValidationRule};

fn minor(units: i64) -> MinorAmount {
    MinorAmount::new(units).expect("test amount is non-negative")
}

/// A band rate, stated in whole minor units so these cases read as they
/// always did (D-311). The stored scale is 10^-9 of one.
fn rate(minor_units: i64) -> RateMinor {
    RateMinor::from_minor_units(minor_units).expect("test rate is non-negative")
}

fn judge(pair: &SupersessionPair) -> ValidationReport {
    let mut report = ValidationReport::default();
    SupersessionUnitGuard.evaluate(pair, &mut report);
    report
}

/// The published predecessor: a graduated usage row on a metered stream.
fn predecessor() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("egress_bytes".to_owned());
    row.dimension_key = String::new();
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);
    row.bands = vec![
        TierBand::closed(0, 1_000, rate(10)),
        TierBand::open(1_000, rate(6)),
    ];
    row
}

#[test]
fn an_identical_unit_successor_with_new_bands_publishes() {
    // The whole point of the rule: repricing is what supersession is *for*. The
    // continued counter is simply rated against the new ladder.
    let mut successor = predecessor();
    successor.bands = vec![
        TierBand::closed(0, 500, rate(9)),
        TierBand::closed(500, 5_000, rate(7)),
        TierBand::open(5_000, rate(4)),
    ];

    let pair = SupersessionPair::new(predecessor(), successor);

    assert!(pair.mismatched_unit_fields().is_empty());
    assert!(judge(&pair).is_publishable());
}

#[test]
fn a_billing_granularity_change_fails_and_names_the_field() {
    // `per_hour -> per_day` applies an hours-denominated continued `Q` to
    // day-denominated bands: the D-77 factor-of-24 band-edge class, back through
    // supersession.
    let mut successor = predecessor();
    successor.billing_granularity = Some(BillingGranularity::PerDay);

    let pair = SupersessionPair::new(predecessor(), successor);
    let report = judge(&pair);

    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].code, SUPERSESSION_UNIT_MISMATCH);
    assert!(
        report.violations[0].detail.contains("billingGranularity"),
        "the violation must name the offending field: {}",
        report.violations[0].detail
    );
}

#[test]
fn a_meter_change_fails_publish() {
    let mut successor = predecessor();
    successor.meter = Some("ingress_bytes".to_owned());

    assert_eq!(
        SupersessionPair::new(predecessor(), successor).mismatched_unit_fields(),
        vec!["meter"]
    );
}

#[test]
fn a_dimension_key_change_fails_publish() {
    let mut successor = predecessor();
    successor.dimension_key = "region".to_owned();

    assert_eq!(
        SupersessionPair::new(predecessor(), successor).mismatched_unit_fields(),
        vec!["dimensionKey"]
    );
}

/// **A hold bound is a unit change** (review M-3).
///
/// `LevelMaxHold` says what the field decides: `hold_last` carries the last
/// observed level across a sampling gap for at most this many granules, and past
/// the bound the level **reads 0**. So a successor that moves it inherits a
/// continued counter under a different rule for what a gap is worth — which is
/// what this guard refuses for every other unit-determining field.
///
/// It was in neither half of the guard: not in `unit_determining_mismatch`'s
/// shared seven and not in this function's own additions. A supersession moving
/// only this field produced an all-zero band delta, was classified
/// `AutoPublishable` and committed on one principal.
#[test]
fn a_hold_bound_change_fails_publish() {
    let mut successor = predecessor();
    successor.max_hold_granules = Some(8760);

    assert_eq!(
        SupersessionPair::new(predecessor(), successor).mismatched_unit_fields(),
        vec!["maxHoldGranules"]
    );
}

#[test]
fn a_kind_flip_fails_publish() {
    // D-98: `volume` applies the selected band's single rate to the whole window
    // `Q`, re-pricing units the predecessor already rated marginally.
    let mut successor = predecessor();
    successor.model_kind = Some(ModelKind::Volume);

    assert_eq!(
        SupersessionPair::new(predecessor(), successor).mismatched_unit_fields(),
        vec!["model_kind"]
    );
}

#[test]
fn a_reset_window_change_fails_publish() {
    let mut successor = predecessor();
    successor.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);

    assert_eq!(
        SupersessionPair::new(predecessor(), successor).mismatched_unit_fields(),
        vec!["tierAggregationWindow"]
    );
}

#[test]
fn a_qualification_window_change_fails_publish() {
    let mut successor = predecessor();
    successor.tier_qualification_window = Some(TierQualificationWindow::TrailingPeriod);

    assert_eq!(
        SupersessionPair::new(predecessor(), successor).mismatched_unit_fields(),
        vec!["tierQualificationWindow"]
    );
}

#[test]
fn a_derivation_change_fails_publish() {
    let mut before = predecessor();
    before.aggregation_function = Some(AggregationFunction::Peak);
    before.aggregation_granularity = Some(AggregationGranularity::Hour);
    before.max_hold_granules = Some(2);
    let mut successor = before.clone();
    successor.aggregation_function = Some(AggregationFunction::TimeWeighted);

    assert_eq!(
        SupersessionPair::new(before, successor).mismatched_unit_fields(),
        vec!["aggregationFunction"]
    );
}

#[test]
fn spelling_out_the_sum_default_is_not_a_change() {
    // A successor that authors `sum` where the predecessor authored nothing is
    // byte-different and semantically identical; failing it would refuse a
    // supersession that changed nothing at all.
    let mut successor = predecessor();
    successor.aggregation_function = Some(AggregationFunction::Sum);

    assert!(
        SupersessionPair::new(predecessor(), successor)
            .mismatched_unit_fields()
            .is_empty()
    );
}

#[test]
fn a_package_size_change_fails_while_a_package_price_change_publishes() {
    // D-122: rating counts blocks by a cumulative ceil-diff that presupposes one
    // block size per window, so a mid-window resize re-buckets the already
    // accumulated `used`. The block *price* is the legitimate lever.
    let mut before = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Package));
    before.meter = Some("sms".to_owned());
    before.billing_granularity = Some(BillingGranularity::WholeUnit);
    before.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    before.package_size = Some(100);
    before.package_price_minor = Some(minor(500));

    let mut resized = before.clone();
    resized.package_size = Some(250);
    assert_eq!(
        SupersessionPair::new(before.clone(), resized).mismatched_unit_fields(),
        vec!["package_size"]
    );

    let mut repriced = before.clone();
    repriced.package_price_minor = Some(minor(900));
    assert!(
        SupersessionPair::new(before, repriced)
            .mismatched_unit_fields()
            .is_empty()
    );
}

#[test]
fn a_carry_allowance_change_fails_publish() {
    // D-129: a `carry` declaration compiles into a plan-scoped,
    // revision-immutable grant row, and a supersession opens no plan revision.
    let mut before = predecessor();
    before.included_allowance = Some(IncludedAllowance {
        quantity: 100,
        rollover_policy: RolloverPolicy::Carry,
    });
    let mut successor = before.clone();
    successor.included_allowance = Some(IncludedAllowance {
        quantity: 250,
        rollover_policy: RolloverPolicy::Carry,
    });

    assert_eq!(
        SupersessionPair::new(before, successor).mismatched_unit_fields(),
        vec!["included_allowance"]
    );
}

#[test]
fn a_none_policy_allowance_change_publishes() {
    // The exception: a `none` allowance carries no plan-scoped artifact, so it
    // stays a free row-local lever.
    let mut before = predecessor();
    before.included_allowance = Some(IncludedAllowance {
        quantity: 100,
        rollover_policy: RolloverPolicy::None,
    });
    let mut successor = before.clone();
    successor.included_allowance = Some(IncludedAllowance {
        quantity: 250,
        rollover_policy: RolloverPolicy::None,
    });

    assert!(
        SupersessionPair::new(before, successor)
            .mismatched_unit_fields()
            .is_empty()
    );
}

/// A `none` allowance is a price lever **while the presented kind stays put**.
/// On an untiered row it does not: `{N, none}` compiles `per_unit` into the
/// ladder `[0, N) @ $0, [N, null) @ rate` (`inst-ac-band`), and the successor's
/// bands are applied to a counter the predecessor accumulated with no bands and
/// no allowance at all. The authored `model_kind` never moves, so the guard's
/// `model_kind` clause cannot see it.
#[test]
fn introducing_a_none_allowance_on_an_untiered_successor_fails_publish() {
    let mut before = predecessor();
    before.model_kind = Some(ModelKind::PerUnit);
    before.bands = Vec::new();
    before.unit_rate = Some(rate(6));

    let mut successor = before.clone();
    successor.included_allowance = Some(IncludedAllowance {
        quantity: 100,
        rollover_policy: RolloverPolicy::None,
    });

    let pair = SupersessionPair::new(before.clone(), successor.clone());
    assert_eq!(
        pair.mismatched_unit_fields(),
        vec!["includedAllowance (presented modelKind)"]
    );
    assert!(!judge(&pair).is_publishable());

    // Dropping it is the same move in the other direction.
    let dropping = SupersessionPair::new(successor, before);
    assert_eq!(
        dropping.mismatched_unit_fields(),
        vec!["includedAllowance (presented modelKind)"]
    );
}

/// The positive control for the case above: on an untiered row that already
/// publishes a ladder, moving `N` moves only the price of it.
#[test]
fn a_none_allowance_quantity_change_on_an_untiered_row_publishes() {
    let mut before = predecessor();
    before.model_kind = Some(ModelKind::PerUnit);
    before.bands = Vec::new();
    before.unit_rate = Some(rate(6));
    before.included_allowance = Some(IncludedAllowance {
        quantity: 100,
        rollover_policy: RolloverPolicy::None,
    });
    let mut successor = before.clone();
    successor.included_allowance = Some(IncludedAllowance {
        quantity: 250,
        rollover_policy: RolloverPolicy::None,
    });

    let pair = SupersessionPair::new(before, successor);
    assert!(pair.mismatched_unit_fields().is_empty());
    assert!(judge(&pair).is_publishable());
}

#[test]
fn introducing_or_dropping_a_carry_allowance_fails_publish() {
    // Either direction mints or orphans a plan-scoped grant row, which is
    // structural whichever way it moves.
    let mut carrying = predecessor();
    carrying.included_allowance = Some(IncludedAllowance {
        quantity: 100,
        rollover_policy: RolloverPolicy::Carry,
    });

    assert_eq!(
        SupersessionPair::new(predecessor(), carrying.clone()).mismatched_unit_fields(),
        vec!["included_allowance"]
    );
    assert_eq!(
        SupersessionPair::new(carrying, predecessor()).mismatched_unit_fields(),
        vec!["included_allowance"]
    );
}

#[test]
fn the_violation_names_every_offending_field() {
    // A publish that failed without saying which field moved is not remediable,
    // and a report that named only the first would take N round trips.
    let mut successor = predecessor();
    successor.meter = Some("ingress_bytes".to_owned());
    successor.billing_granularity = Some(BillingGranularity::PerDay);
    successor.model_kind = Some(ModelKind::Volume);

    let pair = SupersessionPair::new(predecessor(), successor);
    let report = judge(&pair);

    assert_eq!(report.violations.len(), 1);
    let detail = &report.violations[0].detail;
    for field in ["meter", "model_kind", "billingGranularity"] {
        assert!(detail.contains(field), "{field} missing from: {detail}");
    }
}

#[test]
fn a_non_usage_successor_is_not_guarded() {
    // There is no continued counter on a non-usage key, so the row-local rules
    // are the whole of what constrains it.
    let mut before = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
    before.amount_minor = Some(minor(2_500));
    let mut successor = before.clone();
    successor.model_kind = Some(ModelKind::PerUnit);

    assert!(judge(&SupersessionPair::new(before, successor)).is_publishable());
}

#[test]
fn authoring_the_default_qualification_window_is_not_a_unit_change() {
    // The PRD states `current` as this window's default, so a predecessor that
    // authored nothing and a successor that spells `current` out are one row.
    // Comparing the raw Options would reject a supersession that changed only
    // the amounts — the same false rejection the aggregation fields avoid.
    let mut before = predecessor();
    before.tier_qualification_window = None;
    let mut after = predecessor();
    after.tier_qualification_window = Some(TierQualificationWindow::Current);
    after.bands = vec![
        TierBand::closed(0, 1_000, rate(9)),
        TierBand::open(1_000, rate(5)),
    ];

    assert!(judge(&SupersessionPair::new(before, after)).is_publishable());
}

#[test]
fn the_twelve_field_list_is_the_shared_seven_axes_between_this_guards_own_five() {
    // The regression the factoring had to not cause. `mismatched_unit_fields`
    // is `unit_determining_mismatch` with `meter` and `dimensionKey` in front
    // and `reservationFlavor` plus the carry-conditioned allowance behind, in
    // that order - a refactor that reordered the report, dropped a field, or
    // quietly grew a second copy of the seven would still compile.
    //
    // `reservation_flavor` was NOT set here until 2026-08-18, so this test named
    // ten labels and could not have caught its removal - the one field D-254
    // added is the one field the guard against dropping a field did not cover.
    // Same edit, same cause as the stale census on `mismatched_unit_fields`:
    // D-254 widened the list and left both readers behind.
    //
    // **And it happened again the same week.** `max_hold_granules` (review M-3,
    // 2026-08-19) was added to `mismatched_unit_fields` by a wave that had this
    // very paragraph in front of it, and was not set here either - so reverting
    // that `changed.push` left this census green and only a one-element assertion
    // elsewhere red, which cannot see ordering, duplication, or a second copy of
    // the shared seven. Set every field this guard names, or the guard against
    // dropping a field has a blind one to hide in (review F5).
    let mut successor = predecessor();
    successor.meter = Some("ingress_bytes".to_owned());
    successor.dimension_key = "region".to_owned();
    successor.model_kind = Some(ModelKind::Package);
    successor.bands = Vec::new();
    successor.package_size = Some(100);
    successor.package_price_minor = Some(minor(500));
    successor.billing_granularity = Some(BillingGranularity::PerDay);
    successor.aggregation_function = Some(AggregationFunction::Peak);
    successor.aggregation_granularity = Some(AggregationGranularity::Day);
    successor.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    successor.tier_qualification_window = Some(TierQualificationWindow::TrailingPeriod);
    successor.reservation_flavor = Some(ReservationFlavor::Consumption);
    successor.max_hold_granules = Some(8760);
    successor.included_allowance = Some(IncludedAllowance {
        quantity: 100,
        rollover_policy: RolloverPolicy::Carry,
    });

    let pair = SupersessionPair::new(predecessor(), successor.clone());
    let changed = pair.mismatched_unit_fields();

    assert_eq!(
        changed,
        vec![
            "meter",
            "dimensionKey",
            "model_kind",
            "billingGranularity",
            "aggregationFunction",
            "aggregationGranularity",
            "tierAggregationWindow",
            "tierQualificationWindow",
            "package_size",
            "reservationFlavor",
            "maxHoldGranules",
            "included_allowance",
        ]
    );
    // And the middle seven are that function's answer, not a copy of it.
    assert_eq!(
        changed[2..9],
        unit_determining_mismatch(&predecessor(), &successor)
    );
    // The count is asserted against the two halves rather than against a literal,
    // so an axis added to either side has to be added here too: 2 + 7 + 3 = 12.
    //
    // Seven and not eight although the shared list holds eight **labels**: its
    // first axis reports as `model_kind` or as
    // `includedAllowance (presented modelKind)` and never as both (D-317, review
    // Z23-1). This fixture moves the authored kind, so it takes the first arm; the
    // second is pinned by
    // `introducing_a_none_allowance_on_an_untiered_successor_fails_publish` above,
    // at this same door.
    assert_eq!(
        changed.len(),
        2 + unit_determining_mismatch(&predecessor(), &successor).len() + 3,
        "every label must come from one of the three sources this test names"
    );
}

#[test]
fn moving_to_the_trailing_window_is_a_unit_change() {
    // The other direction still binds: trailing_period re-qualifies the rate
    // from the prior period's total and locks it, which is not a price move.
    let before = predecessor();
    let mut after = predecessor();
    after.tier_qualification_window = Some(TierQualificationWindow::TrailingPeriod);

    let report = judge(&SupersessionPair::new(before, after));

    assert!(!report.is_publishable());
    assert_eq!(report.violations[0].code, SUPERSESSION_UNIT_MISMATCH);
    assert!(
        report.violations[0]
            .detail
            .contains("tierQualificationWindow")
    );
}

/// **A flavor flip mid-window is a unit change** (D-254), and it was not one until
/// the review that found it.
///
/// `capacity -> consumption` changes whether the reserved quantity ever enters the
/// on-demand counter: under `capacity` the charge never touches `Q`
/// (`inst-rv-level`, D-139), under `consumption` the matched quantity is excluded
/// from it and the remainder's `Q` restarts at zero (`inst-rv-tier-q`). A successor
/// that flips it inherits a **continued** counter accumulated under the other
/// reading, which is precisely the hazard this rule refuses for the seven fields
/// it already listed — and the same series files the flavor inside the
/// evaluation-policy roster for the same reason.
#[test]
fn a_reservation_flavor_flip_is_a_unit_change() {
    let mut before = predecessor();
    before.reserved_rate = Some(RateMinor::from_minor_units(1000).expect("a non-negative rate"));
    before.reservation_flavor = Some(ReservationFlavor::Capacity);
    let mut successor = before.clone();
    successor.reservation_flavor = Some(ReservationFlavor::Consumption);

    let pair = SupersessionPair::new(before, successor);
    let report = judge(&pair);

    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].code, SUPERSESSION_UNIT_MISMATCH);
    assert!(
        report.violations[0].detail.contains("reservationFlavor"),
        "the violation must name the offending field: {}",
        report.violations[0].detail
    );
}
