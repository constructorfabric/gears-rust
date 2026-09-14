//! `inst-rv-attrs` / `inst-rv-usage` / `inst-rv-level`, over a bare row.
//!
//! Row-local rules, so the subject is a row and nothing has to be assembled --
//! which is the same reason `corpus/reserved/*` can reach them.

use super::{
    LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN, RESERVATION_ON_NON_USAGE, RESERVATION_PAIR_INCOMPLETE,
    ReservationWellFormed,
};
use crate::domain::money::{MinorAmount, RateMinor};
use crate::domain::price_row::{
    AggregationFunction, AggregationGranularity, BandTop, BillingGranularity, ModelKind, PriceRow,
    ReservationFlavor, TierAggregationWindow, TierBand,
};
use crate::domain::scope_key::ChargeKind;
use crate::domain::validation::{ValidationPipeline, ValidationReport};

/// A band rate in whole minor units, scaled to the stored rate scale
/// (D-311) so these cases price what they always priced.
fn rate(minor_units: i64) -> RateMinor {
    RateMinor::from_minor_units(minor_units).expect("a non-negative rate")
}

fn minor(units: i64) -> MinorAmount {
    MinorAmount::new(units).expect("a non-negative amount")
}

fn run(row: &PriceRow) -> ValidationReport {
    ValidationPipeline::new()
        .with_rule(Box::new(ReservationWellFormed))
        .run(row)
}

fn codes(report: &ValidationReport) -> Vec<String> {
    report.violations.iter().map(|v| v.code.clone()).collect()
}

/// A `sum` usage row: the shape a reservation is authored on.
fn usage_row() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("storage.gb".to_owned());
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    row.bands = vec![TierBand {
        from_qty: 0,
        to_qty: BandTop::Open,
        unit_price_rate: rate(5),
    }];
    row
}

/// The same row on a level meter (`peak`), which is D-53's subject.
fn level_row() -> PriceRow {
    let mut row = usage_row();
    row.aggregation_function = Some(AggregationFunction::Peak);
    row.aggregation_granularity = Some(AggregationGranularity::Hour);
    row.max_hold_granules = Some(1);
    row
}

/// The world in which every case below is observable: without it they would all
/// pass identically against a rule that violated on every row.
#[test]
fn a_row_that_reserves_nothing_produces_no_violation() {
    assert!(run(&usage_row()).violations.is_empty());
    assert!(run(&level_row()).violations.is_empty());
}

/// And the complete, legal reservation -- the launch product itself.
#[test]
fn a_complete_capacity_reservation_on_a_level_usage_row_is_clean() {
    let mut row = level_row();
    row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
    row.reservation_flavor = Some(ReservationFlavor::Capacity);
    assert!(
        run(&row).violations.is_empty(),
        "reserved cloudlets with peak metering is exactly what D-53 keeps authorable: {:?}",
        codes(&run(&row))
    );
}

/// `inst-rv-usage` / A1.
#[test]
fn a_reservation_on_a_non_usage_row_is_refused() {
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
    row.amount_minor = Some(minor(9900));
    row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
    row.reservation_flavor = Some(ReservationFlavor::Capacity);

    let found = codes(&run(&row));
    assert!(
        found.contains(&RESERVATION_ON_NON_USAGE.to_owned()),
        "{found:?}"
    );
    assert!(
        !found.contains(&RESERVATION_PAIR_INCOMPLETE.to_owned()),
        "the pair is complete; reporting it as incomplete would tell the author to fix \
         something they did right: {found:?}"
    );
}

/// `inst-rv-attrs`: a rate that does not determine a charge.
#[test]
fn a_rate_without_a_flavor_is_refused() {
    let mut row = usage_row();
    row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
    assert!(
        codes(&run(&row)).contains(&RESERVATION_PAIR_INCOMPLETE.to_owned()),
        "{:?}",
        codes(&run(&row))
    );
}

/// `inst-rv-attrs`, the other half: capacity reserved at an unstated price.
#[test]
fn a_flavor_without_a_rate_is_refused() {
    let mut row = usage_row();
    row.reservation_flavor = Some(ReservationFlavor::Capacity);
    assert!(
        codes(&run(&row)).contains(&RESERVATION_PAIR_INCOMPLETE.to_owned()),
        "{:?}",
        codes(&run(&row))
    );
}

/// D-53 / `inst-rv-level`, and the paired negative that makes it evidence about
/// the **flavor** rather than about reserving on a level meter at all.
#[test]
fn consumption_on_a_level_row_is_refused_and_capacity_on_the_same_row_is_not() {
    let reserved = |flavor| {
        let mut row = level_row();
        row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
        row.reservation_flavor = Some(flavor);
        codes(&run(&row))
    };

    let consumption = reserved(ReservationFlavor::Consumption);
    assert!(
        consumption.contains(&LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN.to_owned()),
        "{consumption:?}"
    );
    assert!(
        !reserved(ReservationFlavor::Capacity)
            .contains(&LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN.to_owned()),
        "capacity is the flavor D-53 keeps authorable on a level row"
    );
}

/// The same flavor on a `sum` row is fine -- D-53 is about the level fold, not
/// about `consumption`.
#[test]
fn consumption_on_a_sum_row_is_permitted() {
    let mut row = usage_row();
    row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
    row.reservation_flavor = Some(ReservationFlavor::Consumption);
    assert!(run(&row).violations.is_empty(), "{:?}", codes(&run(&row)));
}

/// D-312 — the non-usage half of this rule is judgeable at the authoring write.
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
    fn a_reservation_on_a_non_usage_row_is_judgeable_at_the_write() {
        // There is no meter, no quantity and no counter for the reserved rate to
        // apply to, and `chargeKind` is frozen — so no later call helps.
        let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
        row.amount_minor = Some(minor(2_500));
        row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
        row.reservation_flavor = Some(ReservationFlavor::Capacity);
        let report = run(&row);
        assert_eq!(stage_of(&report, RESERVATION_ON_NON_USAGE), Stage::Write);
    }

    #[test]
    fn the_incomplete_pair_is_not_judgeable_at_the_write() {
        // Both operands are content: the missing half is exactly what a later
        // call supplies, which is the authoring the design protects.
        let mut row = usage_row();
        row.reserved_rate = Some(RateMinor::from_minor_units(3).expect("a non-negative rate"));
        let report = run(&row);
        assert_eq!(
            stage_of(&report, RESERVATION_PAIR_INCOMPLETE),
            Stage::Publish
        );
    }
}
