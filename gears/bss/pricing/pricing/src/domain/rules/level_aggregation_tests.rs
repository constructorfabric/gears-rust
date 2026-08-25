//! Tests for the D-44 level-aggregation authoring rules.

use bss_fixtures::ModelKind;

use super::{LevelFields, LevelGranularityPairing, LevelMaxHold};
use crate::domain::money::{MinorAmount, RateMinor};
use crate::domain::price_row::{
    AggregationFunction, AggregationGranularity, BillingGranularity, PriceRow,
    TierAggregationWindow,
};
use crate::domain::rules::model_kind::KindRequiredFields;
use crate::domain::rules::{
    AMOUNT_PLACEMENT_INVALID, LEVEL_FIELDS_INVALID, LEVEL_GRANULARITY_MISMATCH,
};
use crate::domain::scope_key::ChargeKind;
use crate::domain::validation::{ValidationReport, ValidationRule};

fn minor(units: i64) -> MinorAmount {
    MinorAmount::new(units).expect("test amount is non-negative")
}

/// A per-unit rate, stated in whole minor units. The stored scale is 10^-9 of one.
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

/// A plain `sum` usage row — the launch default shape.
///
/// **The money is in `unit_rate` and that is not incidental.** `per_unit` prices
/// from the rate column (D-311), so a fixture carrying `amount_minor` instead is
/// refused twice by `inst-mk-required` — money in a forbidden column and the
/// required column absent — and every conclusion drawn from it would be a
/// conclusion about a row no publish can carry.
/// [`the_level_fixtures_are_rows_a_publish_could_carry`] holds that.
fn sum_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
    row.unit_rate = Some(rate(3));
    row.meter = Some("api_calls".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row
}

/// A publishable level row: cloudlet peak per hour.
fn peak_hourly() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("cloudlet".to_owned());
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);
    row.aggregation_function = Some(AggregationFunction::Peak);
    row.aggregation_granularity = Some(AggregationGranularity::Hour);
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.max_hold_granules = Some(2);
    row
}

#[test]
fn a_sum_row_owes_no_level_fields() {
    let row = sum_usage();

    assert!(findings(&LevelFields, &row).violations.is_empty());
    assert!(
        findings(&LevelGranularityPairing, &row)
            .violations
            .is_empty()
    );
    assert!(findings(&LevelMaxHold, &row).violations.is_empty());
}

#[test]
fn a_well_formed_level_row_passes_every_level_rule() {
    let row = peak_hourly();

    assert!(findings(&LevelFields, &row).violations.is_empty());
    assert!(
        findings(&LevelGranularityPairing, &row)
            .violations
            .is_empty()
    );
    assert!(findings(&LevelMaxHold, &row).violations.is_empty());
}

#[test]
fn level_fields_on_a_non_usage_row_fail_publish() {
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
    row.amount_minor = Some(minor(100));
    row.aggregation_function = Some(AggregationFunction::TimeWeighted);

    assert_eq!(
        codes(&findings(&LevelFields, &row)),
        vec![LEVEL_FIELDS_INVALID]
    );
}

/// Every fixture this file draws conclusions from is a row a publish could
/// actually carry.
///
/// The level rules read `aggregationFunction`, `aggregationGranularity` and the
/// charge kind and never touch a money column, so a fixture whose money sits in
/// the wrong one passes every case here while `inst-mk-required` — a rule this
/// file never runs — refuses it. Every case drawing on the fixtures would then be
/// a conclusion about a row no author can publish, and nothing in the suite would
/// say so.
///
/// The **negative control is the mis-placed shape**: money in `amount_minor` on a
/// `per_unit` row, which draws both arms of the placement matrix at once. Without it
/// a rule that had stopped judging anything would satisfy the claim above.
#[test]
fn the_level_fixtures_are_rows_a_publish_could_carry() {
    assert_eq!(
        codes(&findings(&KindRequiredFields, &sum_usage())),
        Vec::<&str>::new(),
        "the sum fixture prices from the column its kind prices from"
    );
    assert_eq!(
        codes(&findings(&KindRequiredFields, &peak_hourly())),
        Vec::<&str>::new(),
        "the level fixture is `graduated`, whose money is in its bands, so neither \
         single-valued money column may carry any"
    );

    let mut misplaced = sum_usage();
    misplaced.unit_rate = None;
    misplaced.amount_minor = Some(minor(3));
    assert_eq!(
        codes(&findings(&KindRequiredFields, &misplaced)),
        vec![AMOUNT_PLACEMENT_INVALID, AMOUNT_PLACEMENT_INVALID],
        "control: money in a forbidden column, and the column the kind prices from absent"
    );
}

#[test]
fn aggregation_granularity_on_a_sum_row_fails_publish() {
    // Forbidden, not ignored: there is no granule fold to parameterize, so a
    // frozen granularity would state a policy nothing applies and an author
    // would reasonably read it as having taken effect.
    let mut row = sum_usage();
    row.aggregation_granularity = Some(AggregationGranularity::Day);

    assert_eq!(
        codes(&findings(&LevelFields, &row)),
        vec![LEVEL_FIELDS_INVALID]
    );
}

#[test]
fn a_time_weighted_hourly_row_billed_per_day_fails_publish() {
    // The D-77 factor-of-24 band-edge case: bands would be GB-hours under
    // `inst-tb-units` and GB-days under `inst-la-units`.
    let mut row = peak_hourly();
    row.aggregation_function = Some(AggregationFunction::TimeWeighted);
    row.billing_granularity = Some(BillingGranularity::PerDay);

    assert_eq!(
        codes(&findings(&LevelGranularityPairing, &row)),
        vec![LEVEL_GRANULARITY_MISMATCH]
    );
}

#[test]
fn a_day_granule_row_must_be_billed_per_day() {
    let mut row = peak_hourly();
    row.aggregation_granularity = Some(AggregationGranularity::Day);

    assert_eq!(
        codes(&findings(&LevelGranularityPairing, &row)),
        vec![LEVEL_GRANULARITY_MISMATCH]
    );

    row.billing_granularity = Some(BillingGranularity::PerDay);
    assert!(
        findings(&LevelGranularityPairing, &row)
            .violations
            .is_empty()
    );
}

#[test]
fn the_sub_hour_and_unquantized_granularities_are_illegal_on_a_level_row() {
    for granularity in [
        BillingGranularity::PerSecond,
        BillingGranularity::PerMinute,
        BillingGranularity::WholeUnit,
    ] {
        let mut row = peak_hourly();
        row.billing_granularity = Some(granularity);

        assert_eq!(
            codes(&findings(&LevelGranularityPairing, &row)),
            vec![LEVEL_GRANULARITY_MISMATCH],
            "expected {granularity} to be refused on an hourly fold"
        );
    }
}

#[test]
fn an_unauthored_granularity_on_a_level_row_pairs_with_per_hour() {
    // `hour` is the default, so a row that authors only the function is still
    // judged against `per_hour` — otherwise the pairing could be evaded by
    // omission.
    let mut row = peak_hourly();
    row.aggregation_granularity = None;

    assert!(
        findings(&LevelGranularityPairing, &row)
            .violations
            .is_empty()
    );

    row.billing_granularity = Some(BillingGranularity::PerDay);
    assert_eq!(
        codes(&findings(&LevelGranularityPairing, &row)),
        vec![LEVEL_GRANULARITY_MISMATCH]
    );
}

#[test]
fn a_missing_billing_granularity_is_not_reported_as_a_pairing_failure() {
    // It is a missing field, reported once by `inst-tb-window`. Two codes for
    // one omission would make one fault look like two.
    let mut row = peak_hourly();
    row.billing_granularity = None;

    assert!(
        findings(&LevelGranularityPairing, &row)
            .violations
            .is_empty()
    );
}

#[test]
fn a_level_row_without_a_max_hold_fails_publish() {
    let mut row = peak_hourly();
    row.max_hold_granules = None;

    assert_eq!(
        codes(&findings(&LevelMaxHold, &row)),
        vec![LEVEL_FIELDS_INVALID]
    );
}

#[test]
fn a_zero_max_hold_fails_publish() {
    // Zero granules is not the way to say "never hold": it is a bound that holds
    // nothing, and the sampling-gap policy has to be a positive statement.
    let mut row = peak_hourly();
    row.max_hold_granules = Some(0);

    assert_eq!(
        codes(&findings(&LevelMaxHold, &row)),
        vec![LEVEL_FIELDS_INVALID]
    );
}

#[test]
fn a_max_hold_on_a_sum_row_fails_publish() {
    // The other half of the rule, and the easy one to forget: a `sum` row has no
    // gap fold to bound.
    let mut row = sum_usage();
    row.max_hold_granules = Some(3);

    assert_eq!(
        codes(&findings(&LevelMaxHold, &row)),
        vec![LEVEL_FIELDS_INVALID]
    );
}

#[test]
fn a_max_hold_on_a_recurring_row_fails_publish() {
    // A non-usage row derives nothing, so it is a `sum` row by default and the
    // same half of the rule catches it.
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
    row.amount_minor = Some(minor(100));
    row.max_hold_granules = Some(3);

    assert_eq!(
        codes(&findings(&LevelMaxHold, &row)),
        vec![LEVEL_FIELDS_INVALID]
    );
}

/// D-312 — which half of this rule the authoring write can already judge.
mod write_stage {
    use super::*;
    use crate::domain::validation::Stage;

    fn stage_of(report: &ValidationReport, code: &str) -> Stage {
        report
            .violations
            .iter()
            .find(|v| v.code == code)
            .unwrap_or_else(|| panic!("expected a {code} violation: {:?}", codes(report)))
            .stage
    }

    #[test]
    fn a_level_field_on_a_non_usage_row_is_judgeable_at_the_write() {
        // `chargeKind` is frozen by the scope key and the misplaced field is in
        // the request, so nothing a later call adds can make this row publish.
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.aggregation_function = Some(AggregationFunction::Peak);
        let report = findings(&LevelFields, &row);
        assert_eq!(stage_of(&report, LEVEL_FIELDS_INVALID), Stage::Write);
    }

    #[test]
    fn a_granularity_on_a_sum_row_is_not_judgeable_at_the_write() {
        // The sibling fault, same code, same rule — and both its operands are
        // content: `aggregationFunction: peak` resolves it by adding intent.
        let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
        row.unit_rate = Some(rate(1));
        row.billing_granularity = Some(BillingGranularity::WholeUnit);
        row.aggregation_granularity = Some(AggregationGranularity::Hour);
        let report = findings(&LevelFields, &row);
        assert_eq!(stage_of(&report, LEVEL_FIELDS_INVALID), Stage::Publish);
    }

    /// **`LevelMaxHold` splits the same way, and its own arms are the split**
    /// (review M27).
    ///
    /// The two cases above are `LevelFields`; this rule was left whole until
    /// 2026-08-20 and answered `Stage::Publish` for both of its worlds. On a
    /// **non-usage** row that was wrong in the way that costs an author a
    /// round trip: `is_level()` can never become true there, because
    /// `LevelFields` refuses `aggregationFunction` at the write on such a key,
    /// so nothing a later call adds can legalise `maxHoldGranules`. The row was
    /// answered 201, read back as authorable, and died at publish -- with no DB
    /// constraint tying the column to the charge kind
    /// (`chk_pricing_price_max_hold_granules` bounds it at `>= 1` and nothing
    /// more).
    ///
    /// The detail is asserted beside the stage because the two arms say
    /// different sentences: a stage moved without the sentence moving with it
    /// would leave the author reading "forbidden on a sum row" about a
    /// `recurring` row that has no aggregation function at all.
    #[test]
    fn a_max_hold_on_a_non_usage_row_is_judgeable_at_the_write() {
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.max_hold_granules = Some(3);

        let report = findings(&LevelMaxHold, &row);
        assert_eq!(
            stage_of(&report, LEVEL_FIELDS_INVALID),
            Stage::Write,
            "the second operand is the frozen chargeKind, which is D-312's criterion"
        );
        let detail = report
            .violations
            .iter()
            .find(|violation| violation.code == LEVEL_FIELDS_INVALID)
            .map(|violation| violation.detail.clone())
            .expect("the refusal is present");
        assert!(
            detail.contains("recurring"),
            "the non-usage arm names the key component the author cannot move: {detail}"
        );
    }

    /// And the requirement arm does not fire on a key that cannot carry one.
    ///
    /// `is_level()` reads the aggregation function and no charge kind, so a
    /// `recurring` row authoring `aggregationFunction: peak` takes the arm that
    /// demands `maxHoldGranules`. `LevelFields` has already refused that function
    /// on this key at the write, so the second line is a consequence of the one
    /// fault the author must fix -- the report shape `model_kind`'s own doc
    /// refuses, and the bound would be owed on a row that has no gap fold in the
    /// first place.
    #[test]
    fn a_level_function_on_a_non_usage_row_does_not_also_demand_a_max_hold() {
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.aggregation_function = Some(AggregationFunction::Peak);

        assert!(
            findings(&LevelMaxHold, &row).violations.is_empty(),
            "the misplaced function is `LevelFields`' to report: {:?}",
            codes(&findings(&LevelMaxHold, &row))
        );
        // The control: a usage row with the same aggregation function still owes
        // the bound, so the gate has not switched the rule off.
        let mut usage = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
        usage.meter = Some("cloudlet".to_owned());
        usage.aggregation_function = Some(AggregationFunction::Peak);
        usage.aggregation_granularity = Some(AggregationGranularity::Hour);
        usage.billing_granularity = Some(BillingGranularity::PerHour);
        assert_eq!(
            codes(&findings(&LevelMaxHold, &usage)),
            vec![LEVEL_FIELDS_INVALID]
        );
    }

    /// And a non-usage row that authors **both** is answered at the write.
    ///
    /// `aggregationFunction: peak` and a `maxHoldGranules` on a `recurring` key
    /// reach the misplacement arm together. The stage is the write one for the
    /// same reason the case above it is: the second operand is the frozen
    /// `chargeKind`, and the sentence names it rather than talking about a sum
    /// row the key has no aggregation function to be.
    #[test]
    fn a_level_function_and_a_max_hold_on_a_non_usage_row_are_judged_at_the_write() {
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.aggregation_function = Some(AggregationFunction::Peak);
        row.max_hold_granules = Some(3);

        let report = findings(&LevelMaxHold, &row);
        assert_eq!(stage_of(&report, LEVEL_FIELDS_INVALID), Stage::Write);
        let detail = report
            .violations
            .iter()
            .find(|violation| violation.code == LEVEL_FIELDS_INVALID)
            .map(|violation| violation.detail.clone())
            .expect("the refusal is present");
        assert!(
            detail.contains("recurring"),
            "the non-usage arm names the key component the author cannot move: {detail}"
        );
    }

    /// And a zero bound on such a row is answered as the placement fault it is.
    ///
    /// The cell where the guard swaps one refusal for another rather than adding
    /// or removing one: a `recurring` row carrying `peak` and
    /// `maxHoldGranules: 0` used to be told the bound must be at least 1, at the
    /// publish stage. It is told instead that the field is usage-row only, at the
    /// write - which is the fault the author can act on, since raising the bound
    /// to 1 on a key that carries no gap fold fixes nothing.
    #[test]
    fn a_zero_max_hold_on_a_non_usage_row_is_the_placement_fault_not_the_bound_one() {
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.aggregation_function = Some(AggregationFunction::Peak);
        row.max_hold_granules = Some(0);

        let report = findings(&LevelMaxHold, &row);
        assert_eq!(stage_of(&report, LEVEL_FIELDS_INVALID), Stage::Write);
        let detail = report
            .violations
            .iter()
            .find(|violation| violation.code == LEVEL_FIELDS_INVALID)
            .map(|violation| violation.detail.clone())
            .expect("the refusal is present");
        assert!(
            detail.contains("usage-row only"),
            "the placement fault, not the bound: {detail}"
        );
        assert!(
            !detail.contains("at least 1"),
            "raising the bound fixes nothing on this key: {detail}"
        );
    }

    /// The sibling world, and the control that makes the case above a split
    /// rather than a move: on a **usage** `sum` row both operands are content and
    /// a later `aggregationFunction: peak` resolves the fault by adding intent,
    /// so the publish stage stays right.
    #[test]
    fn a_max_hold_on_a_usage_sum_row_is_not_judgeable_at_the_write() {
        let mut row = sum_usage();
        row.max_hold_granules = Some(3);

        let report = findings(&LevelMaxHold, &row);
        assert_eq!(
            stage_of(&report, LEVEL_FIELDS_INVALID),
            Stage::Publish,
            "a level row is one `aggregationFunction` away, and that call is legitimate"
        );
    }
}
