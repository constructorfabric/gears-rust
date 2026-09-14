//! `inst-ft-fallback` and `inst-ft-warn`, over a bare row.

use super::{
    FLOOR_FALLBACK_MISSING, FLOOR_FALLBACK_WITHOUT_FLOOR, FLOOR_INSIDE_PRICED_BAND,
    FloorFallbackDeclared, FloorOutsideBands,
};
use crate::domain::money::RateMinor;
use crate::domain::price_row::{
    BandTop, BillingGranularity, MinQtyUsageFallback, ModelKind, PriceRow, TierAggregationWindow,
    TierBand,
};
use crate::domain::scope_key::ChargeKind;
use crate::domain::validation::{ValidationPipeline, ValidationReport};

/// A band rate in whole minor units, scaled to the stored rate scale
/// (D-311) so these cases price what they always priced.
fn rate(minor_units: i64) -> RateMinor {
    RateMinor::from_minor_units(minor_units).expect("a non-negative rate")
}

fn run(row: &PriceRow) -> ValidationReport {
    ValidationPipeline::new()
        .with_rule(Box::new(FloorFallbackDeclared))
        .with_rule(Box::new(FloorOutsideBands))
        .run(row)
}

fn violations(report: &ValidationReport) -> Vec<String> {
    report.violations.iter().map(|v| v.code.clone()).collect()
}

fn warnings(report: &ValidationReport) -> Vec<String> {
    report.warnings.iter().map(|w| w.code.clone()).collect()
}

/// A tiered usage row banded `[0, 1000)` then `[1000, open)`.
fn banded_row() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("storage.gb".to_owned());
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    row.bands = vec![
        TierBand {
            from_qty: 0,
            to_qty: BandTop::Closed(1_000),
            unit_price_rate: rate(10),
        },
        TierBand {
            from_qty: 1_000,
            to_qty: BandTop::Open,
            unit_price_rate: rate(6),
        },
    ];
    row
}

/// The world in which every case below is observable.
#[test]
fn a_row_with_no_floor_at_all_is_clean() {
    let report = run(&banded_row());
    assert!(report.violations.is_empty(), "{:?}", violations(&report));
    assert!(report.warnings.is_empty(), "{:?}", warnings(&report));
}

/// `inst-ft-fallback`: the floor without the behaviour beneath it.
#[test]
fn a_usage_floor_without_a_fallback_is_refused() {
    let mut row = banded_row();
    row.min_qty_usage = Some(1_000);
    assert!(
        violations(&run(&row)).contains(&FLOOR_FALLBACK_MISSING.to_owned()),
        "{:?}",
        violations(&run(&row))
    );
}

/// And the complete pair is clean -- so the case above is evidence about the
/// missing fallback rather than about setting a usage floor at all.
#[test]
fn a_usage_floor_with_its_fallback_is_clean() {
    let mut row = banded_row();
    row.min_qty_usage = Some(1_000);
    row.min_qty_usage_fallback = Some(MinQtyUsageFallback::Exception);
    assert!(
        run(&row).violations.is_empty(),
        "{:?}",
        violations(&run(&row))
    );
}

/// A purchase floor needs no fallback: Subscriptions rejects the order outright,
/// so there is no below-floor line to decide the fate of.
#[test]
fn a_purchase_floor_needs_no_fallback() {
    let mut row = banded_row();
    row.min_qty_purchase = Some(1_000);
    assert!(
        run(&row).violations.is_empty(),
        "{:?}",
        violations(&run(&row))
    );
}

/// The inert half: a fallback with nothing to fall back from warns, never fails.
#[test]
fn a_fallback_without_a_floor_warns_and_does_not_refuse() {
    let mut row = banded_row();
    row.min_qty_usage_fallback = Some(MinQtyUsageFallback::Exception);
    let report = run(&row);
    assert!(report.violations.is_empty(), "{:?}", violations(&report));
    assert!(
        warnings(&report).contains(&FLOOR_FALLBACK_WITHOUT_FLOOR.to_owned()),
        "{:?}",
        warnings(&report)
    );
}

/// `inst-ft-warn`: a floor strictly inside a band hides quantity that band prices.
#[test]
fn a_floor_inside_a_band_warns_and_publishes() {
    let mut row = banded_row();
    row.min_qty_purchase = Some(500);
    let report = run(&row);
    assert!(
        report.violations.is_empty(),
        "the finding is advisory; the plan publishes: {:?}",
        violations(&report)
    );
    assert!(
        warnings(&report).contains(&FLOOR_INSIDE_PRICED_BAND.to_owned()),
        "{:?}",
        warnings(&report)
    );
}

/// The advisory is read by a human, so the number in it has to be in the unit it
/// says it is.
#[test]
fn the_advisory_names_the_unit_the_number_is_actually_in() {
    // `RateMinor::nano_minor()` is a count of **nano**-minor units (D-311's stored
    // scale), and the sentence labelled it `minor`: a band at $0.023/GB was
    // rendered "priced at 23000000 minor", which reads as $230,000.00 per unit -
    // out by a factor of a billion, in the one place an operator is being asked to
    // decide whether the authoring is a mistake.
    //
    // The type deliberately cannot render a decimal - that needs the currency,
    // which `RateMinor` does not carry - so the fix is to name the unit, not to
    // convert the number.
    let mut row = banded_row();
    row.min_qty_purchase = Some(500);
    let report = run(&row);

    let advisory = report
        .warnings
        .iter()
        .find(|w| w.code == FLOOR_INSIDE_PRICED_BAND)
        .expect("the advisory fires");

    assert!(
        advisory.detail.contains("nano-minor"),
        "the number is a nano-minor count and the sentence must say so: {}",
        advisory.detail
    );
}

/// The boundary the rule is deliberately strict about: a floor **at** a band's
/// lower bound hides nothing, and is the authoring an operator who noticed the
/// overlap would produce. Warning there would train operators to ignore it.
#[test]
fn a_floor_exactly_at_a_band_boundary_does_not_warn() {
    for at in [0_u64, 1_000] {
        let mut row = banded_row();
        row.min_qty_purchase = Some(at);
        assert!(
            !warnings(&run(&row)).contains(&FLOOR_INSIDE_PRICED_BAND.to_owned()),
            "a floor at {at} sits on a band bound and hides nothing"
        );
    }
}

/// A floor above every band's start, inside the open top band, still warns --
/// the open top has no ceiling to be outside of.
#[test]
fn a_floor_inside_the_open_top_band_warns() {
    let mut row = banded_row();
    row.min_qty_usage = Some(5_000);
    row.min_qty_usage_fallback = Some(MinQtyUsageFallback::Exception);
    assert!(
        warnings(&run(&row)).contains(&FLOOR_INSIDE_PRICED_BAND.to_owned()),
        "{:?}",
        warnings(&run(&row))
    );
}

// ---------------------------------------------------------------------------
// `inst-ft-warn`'s allowance half (D-45) — the compiled `[0, N)` band.
// ---------------------------------------------------------------------------

/// A usage floor inside the **compiled** allowance band warns, "where the floor
/// silently voids part of the granted allowance".
///
/// This is the half `FLOOR_INSIDE_PRICED_BAND`'s doc recorded as unbuilt: the
/// `[0, N)` band exists only in the projection, so a rule reading
/// `subject.bands` could not see it — and on an untiered row there are no
/// authored bands at all.
#[test]
fn a_usage_floor_inside_the_compiled_allowance_band_warns() {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
    row.meter = Some("storage.gb".to_owned());
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.unit_rate = Some(rate(6));
    row.min_qty_usage = Some(40);
    row.min_qty_usage_fallback = Some(MinQtyUsageFallback::Exception);

    // Without a declaration the row has no bands at all, so nothing to warn on.
    assert!(
        warnings(&run(&row)).is_empty(),
        "the untiered row carries no band the floor could sit inside"
    );

    // With one, the compiled `[0, 100)` band is what the floor of 40 hides half
    // of.
    row.included_allowance = Some(crate::domain::price_row::IncludedAllowance {
        quantity: 100,
        rollover_policy: crate::domain::price_row::RolloverPolicy::None,
    });
    assert_eq!(
        warnings(&run(&row)),
        vec![FLOOR_INSIDE_PRICED_BAND.to_owned()],
        "the floor voids 40 of the 100 granted units, and the operator hears about it before the \
         publish freezes it"
    );
    assert!(violations(&run(&row)).is_empty(), "it warns, never blocks");
}

/// And the offset ladder is what a floor on a tiered allowance row is judged
/// against, not the authored one — a floor of `1_050` sits inside the *compiled*
/// `[100, 1_100)` band and outside the authored `[0, 1_000)`.
#[test]
fn a_floor_on_a_tiered_allowance_row_is_judged_against_the_offset_ladder() {
    let mut row = banded_row();
    row.min_qty_purchase = Some(1_050);
    // Against the authored ladder 1_050 falls in `[1_000, open)`, which warns.
    assert_eq!(
        warnings(&run(&row)),
        vec![FLOOR_INSIDE_PRICED_BAND.to_owned()]
    );

    row.included_allowance = Some(crate::domain::price_row::IncludedAllowance {
        quantity: 100,
        rollover_policy: crate::domain::price_row::RolloverPolicy::None,
    });
    // Against the compiled ladder it falls in `[100, 1_100)` — still a warning,
    // and about a different band. The assertion that matters is that the *band
    // named* is the compiled one, because that is the quantity the floor really
    // hides.
    let detail = run(&row).warnings[0].detail.clone();
    assert!(
        detail.contains("starting at 100"),
        "the compiled band is the one the floor hides quantity in: {detail}"
    );
}
