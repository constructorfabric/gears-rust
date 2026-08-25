//! Tests for the model-kind rules.

use bss_fixtures::ModelKind;

use super::{ExplicitModelKind, KindChargeKindMatrix, KindForbiddenFields, KindRequiredFields};
use crate::domain::money::{MinorAmount, RateMinor};
use crate::domain::price_row::{
    BillingGranularity, PriceRow, QuantitySource, TierAggregationWindow, TierBand,
};
use crate::domain::rules::{
    AMOUNT_PLACEMENT_INVALID, EVAL_POLICY_MISPLACED, MODEL_KIND_CHARGEKIND_MISMATCH,
    MODEL_KIND_MISSING, PACKAGE_FIELDS_INVALID, QUANTITY_SOURCE_MISSING,
};
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

/// A publishable `flat` recurring row.
fn flat_recurring() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
    row.amount_minor = Some(minor(2_500));
    row
}

/// A publishable `per_unit` recurring (per-seat) row.
fn per_unit_recurring() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::PerUnit));
    row.unit_rate = Some(rate(900));
    row.quantity_source = Some(QuantitySource::SubscriptionSeatCount);
    row
}

/// A publishable `per_unit` usage row — the plain untiered metered rate.
fn per_unit_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
    row.unit_rate = Some(rate(3));
    row.meter = Some("api_calls".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row
}

/// A publishable `graduated` usage row.
fn graduated_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("api_calls".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);
    row.bands = vec![
        TierBand::closed(0, 1_000, rate(10)),
        TierBand::open(1_000, rate(6)),
    ];
    row
}

/// A publishable `volume` usage row.
///
/// `check_amount_placement` groups `Graduated | Volume | Package` in one
/// `(false, false)` arm, and no case ran `KindRequiredFields` over a `Volume` row
/// at all - so the arm was proved by two of its three members and a rewrite that
/// split `Volume` out of it would have reddened nothing.
fn volume_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Volume));
    row.meter = Some("storage_gb".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);
    row.bands = vec![
        TierBand::closed(0, 1_000, rate(10)),
        TierBand::open(1_000, rate(6)),
    ];
    row
}

/// A publishable `package` usage row.
fn package_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Package));
    row.meter = Some("sms".to_owned());
    row.billing_granularity = Some(BillingGranularity::WholeUnit);
    row.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    row.package_size = Some(100);
    row.package_price_minor = Some(minor(500));
    row
}

#[test]
fn a_row_without_a_model_kind_fails_publish() {
    let row = PriceRow::new(ChargeKind::Usage, None);

    assert_eq!(
        codes(&findings(&ExplicitModelKind, &row)),
        vec![MODEL_KIND_MISSING]
    );
}

#[test]
fn a_row_with_an_explicit_kind_passes() {
    assert!(
        findings(&ExplicitModelKind, &flat_recurring())
            .violations
            .is_empty()
    );
}

#[test]
fn the_field_rules_stay_silent_while_the_kind_is_missing() {
    // Otherwise one omission produces a report full of per-kind consequences of
    // a kind nobody authored, and the fault the author must fix first is buried.
    let row = PriceRow::new(ChargeKind::Usage, None);

    assert!(findings(&KindRequiredFields, &row).violations.is_empty());
    assert!(findings(&KindForbiddenFields, &row).violations.is_empty());
    assert!(findings(&KindChargeKindMatrix, &row).violations.is_empty());
}

#[test]
fn a_field_the_charge_kind_alone_forbids_is_reported_while_the_kind_is_missing() {
    // The companion to the test above, and the one that makes it mean something.
    // "Silent while the kind is missing" is true of the *per-kind* consequences
    // only: `billingGranularity` on a recurring row is forbidden by the charge
    // kind by itself, no model kind participates in the fault, and a picker left
    // untouched must not hide it. `KindForbiddenFields` used to return early on a
    // missing kind and swallowed exactly this — the nine rows of the eleven D-312
    // counted against the stand, answered 201 by D-312's own write check and
    // reported by nothing at publish either.
    let mut row = PriceRow::new(ChargeKind::Recurring, None);
    row.billing_granularity = Some(BillingGranularity::WholeUnit);

    let report = findings(&KindForbiddenFields, &row);
    assert_eq!(codes(&report), vec![EVAL_POLICY_MISPLACED]);
    assert!(
        report
            .violations
            .iter()
            .all(|v| v.stage == crate::domain::validation::Stage::Write),
        "the fault's operands are the field and the frozen chargeKind, both present"
    );
}

#[test]
fn a_flat_row_without_an_amount_fails_publish() {
    let mut row = flat_recurring();
    row.amount_minor = None;

    assert_eq!(
        codes(&findings(&KindRequiredFields, &row)),
        vec![AMOUNT_PLACEMENT_INVALID]
    );
}

/// **A `per_unit` row prices from its own rate column, and only from it** (D-311).
///
/// Both directions, because each half failed on its own for the length of one
/// commit: the rule still demanded `amount_minor` on a `per_unit` row, so a row
/// authored the new way was refused for a missing amount while a row authored
/// the old way passed with its rate column empty — the new column unauthorable
/// and the superseded spelling still accepted, which is the worst of both.
#[test]
fn a_per_unit_row_keeps_its_unit_price_in_the_rate_column() {
    assert!(
        findings(&KindRequiredFields, &per_unit_recurring())
            .violations
            .is_empty(),
        "a rate is what a per_unit row is authored with"
    );

    let mut no_rate = per_unit_recurring();
    no_rate.unit_rate = None;
    assert_eq!(
        codes(&findings(&KindRequiredFields, &no_rate)),
        vec![AMOUNT_PLACEMENT_INVALID],
        "a per_unit row with no rate has no price"
    );

    let mut old_spelling = per_unit_recurring();
    old_spelling.unit_rate = None;
    old_spelling.amount_minor = Some(minor(900));
    assert_eq!(
        codes(&findings(&KindRequiredFields, &old_spelling)),
        vec![AMOUNT_PLACEMENT_INVALID, AMOUNT_PLACEMENT_INVALID],
        "the pre-D-311 spelling is not a second way to price a per_unit row, and it \
         is two faults rather than one: an amount where none belongs and no rate \
         where one must be"
    );

    let mut both = per_unit_recurring();
    both.amount_minor = Some(minor(900));
    assert_eq!(
        codes(&findings(&KindRequiredFields, &both)),
        vec![AMOUNT_PLACEMENT_INVALID],
        "two priced columns are two competing prices, and this is the pair that \
         used to be one column"
    );
}

/// **A rate is forbidden on every kind that does not multiply by one** — the
/// other half of the matrix, and the half a rule that only checked presence
/// would leave open.
#[test]
fn a_kind_that_charges_no_per_unit_multiple_refuses_a_rate() {
    for mut row in [
        flat_recurring(),
        graduated_usage(),
        volume_usage(),
        package_usage(),
    ] {
        let subject = row.subject();
        row.unit_rate = Some(rate(1));

        assert!(
            codes(&findings(&KindRequiredFields, &row)).contains(&AMOUNT_PLACEMENT_INVALID),
            "expected {subject} to refuse a unit rate"
        );
    }
}

#[test]
fn a_clean_volume_row_passes_the_kind_rule() {
    // The control every other kind in this file has. Both loops that consume
    // `volume_usage` assert a violation *after* poisoning the row, and a fixture
    // already carrying a placement fault would satisfy them.
    assert!(
        codes(&findings(&KindRequiredFields, &volume_usage())).is_empty(),
        "the volume fixture must be publishable before a case breaks it"
    );
}

#[test]
fn a_band_kind_carrying_an_amount_fails_publish() {
    // Two priced columns are two competing prices, and nothing downstream
    // adjudicates between them.
    for mut row in [graduated_usage(), volume_usage(), package_usage()] {
        row.amount_minor = Some(minor(1));

        assert!(
            codes(&findings(&KindRequiredFields, &row)).contains(&AMOUNT_PLACEMENT_INVALID),
            "expected {} to refuse an amount_minor",
            row.subject()
        );
    }
}

#[test]
fn a_per_unit_recurring_row_must_declare_a_quantity_source() {
    let mut row = per_unit_recurring();
    row.quantity_source = None;

    assert_eq!(
        codes(&findings(&KindRequiredFields, &row)),
        vec![QUANTITY_SOURCE_MISSING]
    );
}

#[test]
fn a_per_unit_usage_row_must_not_declare_a_quantity_source() {
    // The mirror image of the rule above, and the one that is easy to get
    // backwards: a usage row's quantity comes from the meter, so an authored
    // source would be a second answer to "how much was consumed".
    let mut row = per_unit_usage();
    row.quantity_source = Some(QuantitySource::SubscriptionSeatCount);

    assert!(findings(&KindRequiredFields, &row).violations.is_empty());
    assert_eq!(
        codes(&findings(&KindForbiddenFields, &row)),
        vec![EVAL_POLICY_MISPLACED]
    );
}

#[test]
fn a_manual_quantity_source_without_a_quantity_fails_publish() {
    let mut row = per_unit_recurring();
    row.quantity_source = Some(QuantitySource::Manual);

    assert_eq!(
        codes(&findings(&KindRequiredFields, &row)),
        vec![QUANTITY_SOURCE_MISSING]
    );

    row.manual_quantity = Some(12);
    assert!(findings(&KindRequiredFields, &row).violations.is_empty());
}

#[test]
fn a_manual_quantity_without_a_manual_source_fails_publish() {
    let mut row = per_unit_recurring();
    row.manual_quantity = Some(12);

    assert_eq!(
        codes(&findings(&KindForbiddenFields, &row)),
        vec![EVAL_POLICY_MISPLACED]
    );
}

#[test]
fn a_package_row_without_its_block_fields_fails_publish() {
    let mut row = package_usage();
    row.package_size = None;

    assert_eq!(
        codes(&findings(&KindRequiredFields, &row)),
        vec![PACKAGE_FIELDS_INVALID]
    );
}

#[test]
fn tier_bands_on_a_per_unit_row_fail_publish() {
    let mut row = per_unit_recurring();
    row.bands = vec![TierBand::open(0, rate(1))];

    assert_eq!(
        codes(&findings(&KindForbiddenFields, &row)),
        vec![EVAL_POLICY_MISPLACED]
    );
}

#[test]
fn evaluation_policy_on_a_non_usage_row_fails_publish() {
    let mut row = flat_recurring();
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.tier_aggregation_window = Some(TierAggregationWindow::CalendarMonth);

    let report = findings(&KindForbiddenFields, &row);

    assert_eq!(codes(&report), vec![EVAL_POLICY_MISPLACED]);
    let detail = &report.violations[0].detail;
    assert!(detail.contains("tierAggregationWindow"), "{detail}");
    assert!(detail.contains("billingGranularity"), "{detail}");
}

#[test]
fn a_per_unit_usage_row_carries_billing_granularity_like_every_usage_row() {
    // The 2026-07-28 correction: `billingGranularity` is usage-row-only, not
    // tiered-row-only, so the plain metered rate must not be refused for it.
    assert!(
        findings(&KindForbiddenFields, &per_unit_usage())
            .violations
            .is_empty()
    );
}

#[test]
fn flat_on_a_usage_row_fails_the_charge_kind_matrix() {
    let mut row = flat_recurring();
    row.charge_kind = ChargeKind::Usage;

    assert_eq!(
        codes(&findings(&KindChargeKindMatrix, &row)),
        vec![MODEL_KIND_CHARGEKIND_MISMATCH]
    );
}

#[test]
fn the_tier_and_block_kinds_are_illegal_on_every_non_usage_row() {
    // The tier machinery presupposes a metered quantity stream, and no `Q`
    // semantics exist for a non-usage row.
    for charge_kind in [
        ChargeKind::Recurring,
        ChargeKind::OneTime,
        ChargeKind::OneTimeSetup,
    ] {
        for model_kind in [ModelKind::Graduated, ModelKind::Volume, ModelKind::Package] {
            let row = PriceRow::new(charge_kind, Some(model_kind));

            assert_eq!(
                codes(&findings(&KindChargeKindMatrix, &row)),
                vec![MODEL_KIND_CHARGEKIND_MISMATCH],
                "expected {} to be refused",
                row.subject()
            );
        }
    }
}

#[test]
fn each_kind_is_legal_on_the_rows_the_matrix_admits() {
    for model_kind in [ModelKind::Flat, ModelKind::PerUnit] {
        let row = PriceRow::new(ChargeKind::Recurring, Some(model_kind));
        assert!(
            findings(&KindChargeKindMatrix, &row).violations.is_empty(),
            "expected {} to be legal",
            row.subject()
        );
    }
    for model_kind in [
        ModelKind::PerUnit,
        ModelKind::Graduated,
        ModelKind::Volume,
        ModelKind::Package,
    ] {
        let row = PriceRow::new(ChargeKind::Usage, Some(model_kind));
        assert!(
            findings(&KindChargeKindMatrix, &row).violations.is_empty(),
            "expected {} to be legal",
            row.subject()
        );
    }
}

/// D-312 — which of these faults the **authoring write** can already judge.
///
/// The stage is a property of the violation, not of the rule: two of the rules
/// below emit both kinds, so a marker on the rule or a filter keyed on the code
/// would either miss half or refuse an author's legitimate intermediate state.
/// These cases pin the split at the granularity the fault actually has.
mod write_stage {
    use super::*;
    use crate::domain::validation::Stage;

    fn stages(report: &ValidationReport) -> Vec<(&str, Stage)> {
        report
            .violations
            .iter()
            .map(|v| (v.code.as_str(), v.stage))
            .collect()
    }

    #[test]
    fn a_kind_illegal_on_the_charge_kind_is_judgeable_at_the_write() {
        // `flat` on `usage`: both operands present, and `chargeKind` is frozen by
        // the scope key — the row is knowably unpublishable when it arrives.
        let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(250));
        let report = findings(&KindChargeKindMatrix, &row);
        assert_eq!(
            stages(&report),
            vec![(MODEL_KIND_CHARGEKIND_MISMATCH, Stage::Write)]
        );
    }

    #[test]
    fn an_eval_policy_field_on_a_non_usage_row_is_judgeable_at_the_write() {
        let mut row = flat_recurring();
        row.billing_granularity = Some(BillingGranularity::WholeUnit);
        let report = findings(&KindForbiddenFields, &row);
        assert_eq!(stages(&report), vec![(EVAL_POLICY_MISPLACED, Stage::Write)]);
    }

    #[test]
    fn tier_bands_on_a_flat_row_are_judgeable_at_the_write() {
        // Publish-staged until D-312's 2026-08-20 amendment, on the argument
        // that both operands are content and a later `model_kind: graduated`
        // would resolve it by adding intent rather than by retracting what was
        // just sent -- so refusing at the write would break multi-call
        // authoring.
        //
        // That state cannot be reached. A probe against the deployed platform
        // `PATCH`ed a band set onto a stored `flat` row and got **500**, the
        // same answer as creating the pair in one call, because
        // `trg_pricing_price_tier_band_kind` refuses the band insert by either
        // route. There is no half-authored band ladder to protect, and all the
        // deferral bought was a Postgres sentence under a 500 where a named 400
        // belonged.
        let mut row = flat_recurring();
        row.bands = vec![TierBand::open(0, rate(1))];
        let report = findings(&KindForbiddenFields, &row);
        assert_eq!(stages(&report), vec![(EVAL_POLICY_MISPLACED, Stage::Write)]);
    }

    #[test]
    fn a_quantity_source_on_a_usage_key_is_judgeable_at_the_write() {
        // A metered row takes its quantity from the meter under *every* model
        // kind, so no later call can legalise this and `chargeKind` cannot move.
        let mut row = per_unit_usage();
        row.quantity_source = Some(QuantitySource::SubscriptionSeatCount);
        let report = findings(&KindForbiddenFields, &row);
        assert_eq!(stages(&report), vec![(EVAL_POLICY_MISPLACED, Stage::Write)]);
    }

    #[test]
    fn a_quantity_source_on_a_non_usage_row_of_the_wrong_kind_is_judgeable_at_the_write() {
        // The mirror image of the band arm, and it moved with it under D-312's
        // 2026-08-20 amendment. Both operands are content here too, and the
        // amendment's line is that a row whose operands are *present and
        // contradictory* is a mistake rather than a stage of authoring.
        //
        // Nothing in the schema refuses this pair, so unlike the band ladder it
        // was stored happily and read back as though it were authorable. That
        // asymmetry is the argument: the store catches some invalid combinations
        // and not others, so the rule has to be what decides.
        let mut row = flat_recurring();
        row.quantity_source = Some(QuantitySource::SubscriptionSeatCount);
        let report = findings(&KindForbiddenFields, &row);
        assert_eq!(stages(&report), vec![(EVAL_POLICY_MISPLACED, Stage::Write)]);
    }

    #[test]
    fn a_quantity_source_on_a_usage_key_is_judged_before_a_kind_is_picked() {
        // The usage-key half reads no kind, so the kind gate must not cover it.
        let mut row = PriceRow::new(ChargeKind::Usage, None);
        row.meter = Some("addon_dr".to_owned());
        row.quantity_source = Some(QuantitySource::SubscriptionSeatCount);
        let report = findings(&KindForbiddenFields, &row);
        assert_eq!(stages(&report), vec![(EVAL_POLICY_MISPLACED, Stage::Write)]);
    }

    #[test]
    fn an_absent_operand_is_never_judgeable_at_the_write() {
        // The family the design protects: the kind has not arrived yet, and a
        // later call adds it.
        let row = PriceRow::new(ChargeKind::Usage, None);
        let report = findings(&ExplicitModelKind, &row);
        assert_eq!(stages(&report), vec![(MODEL_KIND_MISSING, Stage::Publish)]);
    }

    #[test]
    fn a_missing_priced_column_is_never_judgeable_at_the_write() {
        // Also absence, and the one most likely to be swept in by mistake: the
        // row's kind and charge kind agree, only the money has not been typed.
        let row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        let report = findings(&KindRequiredFields, &row);
        assert!(
            report.violations.iter().all(|v| v.stage == Stage::Publish),
            "an absent amount is completed by a later call: {:?}",
            stages(&report)
        );
    }

    #[test]
    fn the_write_subset_is_exactly_the_write_stage_violations() {
        let mut report = ValidationReport::default();
        report.violate("PUBLISH_ONLY", "s", "d");
        report.violate_at_write("WRITE_JUDGEABLE", "s", "d");
        report.warn("ADVISORY", "s", "d");

        let subset = report
            .write_stage_only()
            .expect("one write-stage violation");
        assert_eq!(codes(&subset), vec!["WRITE_JUDGEABLE"]);
        // Advisories are dropped: one riding along with a refusal would suggest
        // it had blocked something.
        assert!(subset.warnings.is_empty());
        // And the full report is untouched — publish still sees everything.
        assert_eq!(report.violations.len(), 2);
    }

    #[test]
    fn a_report_with_no_write_stage_violation_refuses_nothing() {
        let mut report = ValidationReport::default();
        report.violate("PUBLISH_ONLY", "s", "d");
        assert!(report.write_stage_only().is_none());
    }
}
