//! `inst-ac-gate` and `inst-ac-band`'s compiled-set check, over a bare row.
//!
//! Row-local rules, so the subject is a row and nothing has to be assembled.
//!
//! Every refusal below is paired with a **positive control** — the row that is
//! accepted — because a refusal can be green for the wrong reason: a rule that
//! violated on every allowance-carrying row would satisfy all six negative cases
//! and none of the positives.

use super::{
    ALLOWANCE_DOUBLE_FREE, ALLOWANCE_KIND_UNSUPPORTED, ALLOWANCE_ON_NON_SUM,
    ALLOWANCE_ON_NON_USAGE, ALLOWANCE_QUANTITY_INVALID, ALLOWANCE_WITH_RESERVATION,
    AllowanceAuthorable, CompiledAllowanceWellFormed,
};
use crate::domain::money::{MinorAmount, RateMinor};
use crate::domain::price_row::{
    AggregationFunction, AggregationGranularity, BandTop, BillingGranularity, IncludedAllowance,
    ModelKind, PriceRow, ReservationFlavor, RolloverPolicy, TierAggregationWindow, TierBand,
};
use crate::domain::rules::tier_bands::BandGeometry;
use crate::domain::rules::{TIER_BAND_EMPTY, TIER_BANDS_GAP};
use crate::domain::scope_key::ChargeKind;
use crate::domain::validation::{Stage, ValidationPipeline, ValidationReport, ValidationRule};

fn rate(minor_units: i64) -> RateMinor {
    RateMinor::from_minor_units(minor_units).expect("a non-negative rate")
}

fn run(row: &PriceRow) -> ValidationReport {
    ValidationPipeline::new()
        .with_rule(Box::new(AllowanceAuthorable))
        .with_rule(Box::new(CompiledAllowanceWellFormed))
        .run(row)
}

fn codes(report: &ValidationReport) -> Vec<String> {
    report.violations.iter().map(|v| v.code.clone()).collect()
}

fn carries(row: &PriceRow, code: &str) -> bool {
    codes(&run(row)).iter().any(|found| found == code)
}

/// A `sum`, `graduated` usage row with a priced ladder: the shape an allowance
/// is authored on.
fn graduated_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::Graduated));
    row.meter = Some("egress.gb".to_owned());
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    row.bands = vec![
        TierBand::closed(0, 1_000, rate(5)),
        TierBand::open(1_000, rate(3)),
    ];
    row
}

/// The other authorable shape: an untiered metered rate.
///
/// It names a `tierAggregationWindow` even though an untiered row owes none.
/// Every case below hangs an allowance on it, and an allowance compiles it into
/// a band ladder whose first band is the free quantity — so the row this fixture
/// stands for is one `inst-tb-window` requires a reset window from. Authored
/// without one it was a row that could never publish, which is not the row any
/// case here means to be judging.
fn per_unit_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
    row.meter = Some("egress.gb".to_owned());
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    row.unit_rate = Some(rate(2));
    row
}

fn allowance(quantity: u64, policy: RolloverPolicy) -> IncludedAllowance {
    IncludedAllowance {
        quantity,
        rollover_policy: policy,
    }
}

// ---------------------------------------------------------------------------
// The world in which the six refusals are observable.
// ---------------------------------------------------------------------------

/// Without this every case below would pass identically against a rule that
/// refused every row it was handed.
#[test]
fn a_row_declaring_no_allowance_produces_nothing() {
    assert!(run(&graduated_usage()).violations.is_empty());
    assert!(run(&per_unit_usage()).violations.is_empty());
}

/// The positive control the whole gate exists to admit: `{N, none}` on each of
/// the two authorable shapes.
#[test]
fn the_two_authorable_shapes_carrying_an_allowance_are_accepted() {
    for (label, mut row) in [
        ("graduated", graduated_usage()),
        ("per_unit", per_unit_usage()),
    ] {
        row.included_allowance = Some(allowance(100, RolloverPolicy::None));
        assert!(
            run(&row).violations.is_empty(),
            "a {label} sum usage row with a positive quantity and no free opening band is the \
             authorable case: {:?}",
            codes(&run(&row))
        );
    }
}

/// And accepted by the **whole** row-local set, not only by the two rules this
/// file runs.
///
/// This is what reads the `tierAggregationWindow` the two fixtures above carry.
/// `inst-ac-gate` never looks at that field, so a fixture without one satisfied
/// every case in this file while standing for a row publish refuses — and on the
/// untiered shape the missing window is not a detail: the ladder the allowance
/// compiles has a `[0, N) @ $0` first band, and a counter with no reset grants
/// those `N` units once for the subscription's life instead of once per period.
#[test]
fn the_two_authorable_shapes_carrying_an_allowance_pass_the_whole_row_local_set() {
    for (label, mut row) in [
        ("graduated", graduated_usage()),
        ("per_unit", per_unit_usage()),
    ] {
        row.included_allowance = Some(allowance(100, RolloverPolicy::None));
        let report = crate::domain::rules::price_row_rules().run(&row);
        assert!(
            report.violations.is_empty(),
            "{label}: {:?}",
            report
                .violations
                .iter()
                .map(|violation| violation.code.as_str())
                .collect::<Vec<_>>()
        );

        // Drop the window and the untiered shape is refused for the counter it
        // would never reset; the tiered one was already refused without it.
        row.tier_aggregation_window = None;
        let without = crate::domain::rules::price_row_rules().run(&row);
        assert!(
            without
                .violations
                .iter()
                .any(|violation| violation.code == "EVAL_POLICY_MISSING"),
            "{label} without a reset window must be refused"
        );
    }
}

// ---------------------------------------------------------------------------
// The six.
// ---------------------------------------------------------------------------

#[test]
fn an_allowance_on_a_non_usage_row_is_refused_at_the_write() {
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
    row.amount_minor = Some(MinorAmount::new(999).expect("an amount"));
    row.included_allowance = Some(allowance(100, RolloverPolicy::None));

    let report = run(&row);
    assert!(
        codes(&report).contains(&ALLOWANCE_ON_NON_USAGE.to_owned()),
        "got {:?}",
        codes(&report)
    );
    // D-312: both operands are in the request and `chargeKind` is frozen by the
    // canonical key, so the authoring write can already judge it.
    let refusal = report
        .violations
        .iter()
        .find(|v| v.code == ALLOWANCE_ON_NON_USAGE)
        .expect("the refusal is present");
    assert_eq!(
        refusal.stage,
        Stage::Write,
        "no later call can move the charge kind, so the write refuses"
    );
}

#[test]
fn a_zero_quantity_is_refused_and_a_positive_one_is_not() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(0, RolloverPolicy::None));
    assert!(carries(&row, ALLOWANCE_QUANTITY_INVALID));

    row.included_allowance = Some(allowance(1, RolloverPolicy::None));
    assert!(
        !carries(&row, ALLOWANCE_QUANTITY_INVALID),
        "one unit is an allowance"
    );
}

#[test]
fn the_three_kinds_with_nothing_to_compile_into_are_refused_and_the_two_authorable_ones_are_not() {
    for kind in [ModelKind::Package, ModelKind::Volume, ModelKind::Flat] {
        let mut row = graduated_usage();
        row.model_kind = Some(kind);
        row.included_allowance = Some(allowance(100, RolloverPolicy::None));
        assert!(
            carries(&row, ALLOWANCE_KIND_UNSUPPORTED),
            "{kind:?} has no band set an allowance compiles into: {:?}",
            codes(&run(&row))
        );
    }
    for kind in [ModelKind::Graduated, ModelKind::PerUnit] {
        let mut row = graduated_usage();
        row.model_kind = Some(kind);
        row.unit_rate = Some(rate(2));
        row.bands = if kind == ModelKind::PerUnit {
            Vec::new()
        } else {
            graduated_usage().bands
        };
        row.included_allowance = Some(allowance(100, RolloverPolicy::None));
        assert!(
            !carries(&row, ALLOWANCE_KIND_UNSUPPORTED),
            "{kind:?} is authorable (D-59)"
        );
    }
}

/// The kind gate guards only the fault that reads a kind — D-312's line, which
/// `inst-mk-forbidden`'s first implementation got wrong in exactly this place.
#[test]
fn a_row_with_no_kind_yet_still_takes_the_five_faults_that_read_no_kind() {
    let mut row = graduated_usage();
    row.model_kind = None;
    row.included_allowance = Some(allowance(0, RolloverPolicy::None));

    assert!(
        carries(&row, ALLOWANCE_QUANTITY_INVALID),
        "a zero quantity is a zero quantity whether or not a kind has been picked"
    );
    assert!(
        !carries(&row, ALLOWANCE_KIND_UNSUPPORTED),
        "and the kind fault waits for a kind, rather than reporting the consequence of \
         MODEL_KIND_MISSING"
    );
}

#[test]
fn an_allowance_on_a_level_row_is_refused_and_a_sum_row_is_not() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(100, RolloverPolicy::None));
    assert!(!carries(&row, ALLOWANCE_ON_NON_SUM));

    row.aggregation_function = Some(AggregationFunction::Peak);
    row.aggregation_granularity = Some(AggregationGranularity::Hour);
    row.max_hold_granules = Some(1);
    assert!(carries(&row, ALLOWANCE_ON_NON_SUM));

    // An authored `sum` and an unauthored function are one row here, as
    // everywhere else in the set.
    row.aggregation_function = Some(AggregationFunction::Sum);
    row.aggregation_granularity = None;
    row.max_hold_granules = None;
    assert!(!carries(&row, ALLOWANCE_ON_NON_SUM));
}

#[test]
fn an_allowance_beside_a_reservation_is_refused_from_either_half() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(100, RolloverPolicy::None));
    assert!(!carries(&row, ALLOWANCE_WITH_RESERVATION));

    // `is_reserved` is a disjunction, so a half-authored reservation is still a
    // row somebody meant to reserve — the allowance must not slip past on the
    // strength of an incomplete pair.
    row.reserved_rate = Some(RateMinor::from_minor_units(4).expect("a non-negative rate"));
    assert!(carries(&row, ALLOWANCE_WITH_RESERVATION));

    row.reserved_rate = None;
    row.reservation_flavor = Some(ReservationFlavor::Capacity);
    assert!(carries(&row, ALLOWANCE_WITH_RESERVATION));
}

#[test]
fn an_authored_free_opening_band_beside_a_declaration_is_double_free() {
    let mut row = graduated_usage();
    row.bands = vec![
        TierBand::closed(0, 100, RateMinor::ZERO),
        TierBand::open(100, rate(5)),
    ];
    row.included_allowance = Some(allowance(100, RolloverPolicy::None));
    assert!(carries(&row, ALLOWANCE_DOUBLE_FREE));

    // A free band that is not at the origin is not a double free: it prices a
    // middle stretch at nothing, which is odd and is not this fault.
    row.bands = vec![
        TierBand::closed(0, 100, rate(5)),
        TierBand::closed(100, 200, RateMinor::ZERO),
        TierBand::open(200, rate(3)),
    ];
    assert!(!carries(&row, ALLOWANCE_DOUBLE_FREE));
}

/// Authoring order does not survive the store, so the rule reads the set sorted
/// — a `$0` band written last is still the first band.
#[test]
fn the_double_free_check_reads_the_sorted_band_set() {
    let mut row = graduated_usage();
    row.bands = vec![
        TierBand::open(100, rate(5)),
        TierBand::closed(0, 100, RateMinor::ZERO),
    ];
    row.included_allowance = Some(allowance(50, RolloverPolicy::None));
    assert!(carries(&row, ALLOWANCE_DOUBLE_FREE));
}

/// One authored fact, read six ways — the author sees every way it is wrong in
/// one report rather than one per re-publish.
#[test]
fn a_row_wrong_in_several_ways_reports_each_of_them() {
    let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Package));
    row.package_size = Some(10);
    row.aggregation_function = Some(AggregationFunction::Peak);
    row.reserved_rate = Some(RateMinor::from_minor_units(4).expect("a non-negative rate"));
    row.included_allowance = Some(allowance(0, RolloverPolicy::None));

    let found = codes(&run(&row));
    for expected in [
        ALLOWANCE_ON_NON_USAGE,
        ALLOWANCE_QUANTITY_INVALID,
        ALLOWANCE_KIND_UNSUPPORTED,
        ALLOWANCE_ON_NON_SUM,
        ALLOWANCE_WITH_RESERVATION,
    ] {
        assert!(
            found.contains(&expected.to_owned()),
            "{expected} missing from {found:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// `inst-ac-band` — the compiled set is judged.
// ---------------------------------------------------------------------------

/// The fault the compile can introduce and the authored set does not have.
#[test]
fn a_saturating_offset_produces_a_malformed_compiled_set_and_is_reported() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(u64::MAX, RolloverPolicy::None));

    let found = codes(&run(&row));
    assert!(
        found.contains(&TIER_BAND_EMPTY.to_owned()),
        "offsetting [0, 1000) and [1000, ..) by u64::MAX saturates both bounds onto each other, \
         so the compiled ladder has a band covering nothing: {found:?}"
    );
}

/// **A saturated bound does not always produce a malformed set.** The case above
/// is the easy one — both bounds land on `u64::MAX` and the band between them
/// covers nothing. Five units lower and the compiled ladder is a perfectly good
/// geometry: contiguous, anchored at the origin, open at the top. Every one of
/// the three Slice-3 band rules is silent, and the authored 1000-unit band is
/// materialized **five** units wide.
///
/// So the saturation has to be detected as itself. It is the declared quantity
/// that is at fault — the authored ladder is well-formed and stays so at any
/// smaller `N` — and `ALLOWANCE_QUANTITY_INVALID` is the code that names it.
#[test]
fn a_saturating_offset_that_still_looks_like_a_geometry_is_refused() {
    let quantity = u64::MAX - 5;
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(quantity, RolloverPolicy::None));

    // The harm, stated: 1000 units authored, 5 units materialized.
    let compiled = crate::domain::allowance::compile(&row).expect("it compiles");
    assert_eq!(
        compiled.bands,
        vec![
            TierBand::closed(0, quantity, RateMinor::ZERO),
            TierBand::closed(quantity, u64::MAX, rate(5)),
            TierBand::open(u64::MAX, rate(3)),
        ]
    );

    let found = codes(&run(&row));
    // ... and it is invisible to the geometry: nothing overlaps, nothing is
    // empty, nothing is missing. This is what makes the check below the only
    // thing standing between the fault and a published price.
    assert!(
        !found.contains(&TIER_BAND_EMPTY.to_owned()) && !found.contains(&TIER_BANDS_GAP.to_owned()),
        "the compiled set is a well-formed geometry; that is the point: {found:?}"
    );
    assert!(
        found.contains(&ALLOWANCE_QUANTITY_INVALID.to_owned()),
        "an allowance whose offset overflows the authored ladder must be refused: {found:?}"
    );
}

/// The positive control, one unit inside the boundary: the largest quantity that
/// offsets this ladder exactly — `u64::MAX - 1000` puts the authored top on
/// `u64::MAX` with nothing lost. It publishes.
#[test]
fn the_largest_quantity_that_offsets_the_ladder_exactly_is_accepted() {
    let quantity = u64::MAX - 1_000;
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(quantity, RolloverPolicy::None));

    let compiled = crate::domain::allowance::compile(&row).expect("it compiles");
    assert_eq!(
        compiled.bands[1],
        TierBand::closed(quantity, u64::MAX, rate(5)),
        "1000 units authored, 1000 units materialized"
    );
    assert!(
        run(&row).violations.is_empty(),
        "nothing overflowed, so nothing is refused: {:?}",
        codes(&run(&row))
    );
}

/// An untiered row has no authored bound to offset — the two synthesized bands
/// are `[0, N)` and `[N, null)`, neither of which adds anything to `N` — so the
/// overflow check must not fire on it at any quantity.
#[test]
fn an_untiered_row_cannot_overflow_its_offset() {
    let mut row = per_unit_usage();
    row.included_allowance = Some(allowance(u64::MAX, RolloverPolicy::None));

    assert!(run(&row).violations.is_empty(), "{:?}", codes(&run(&row)));
}

/// And it stays silent while the authored ladder is the thing at fault, so one
/// edit is reported once.
#[test]
fn a_malformed_authored_ladder_is_not_reported_twice() {
    let mut row = graduated_usage();
    row.bands = vec![TierBand::open(10, rate(5))];
    row.included_allowance = Some(allowance(100, RolloverPolicy::None));

    let found = codes(&run(&row));
    assert_eq!(
        found.iter().filter(|code| *code == TIER_BANDS_GAP).count(),
        0,
        "the authored gap is `BandOrigin`'s to report against the authored row; this rule adds \
         nothing on top of it: {found:?}"
    );
}

/// A well-formed ladder compiles to a well-formed ladder — the property that
/// makes the rule above quiet on every real row.
#[test]
fn a_clean_ladder_compiles_to_a_clean_one() {
    for quantity in [1_u64, 100, 999_999] {
        let mut row = graduated_usage();
        row.included_allowance = Some(allowance(quantity, RolloverPolicy::None));
        assert!(
            run(&row).violations.is_empty(),
            "quantity {quantity}: {:?}",
            codes(&run(&row))
        );
    }
}

/// A `carry` row produces no band artifact, so the compiled-set rule has nothing
/// to say about it — the refusal is `PRIMITIVE_RULES_UNBUILT`'s and stays there.
#[test]
fn a_carry_row_is_not_band_compiled_here() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(100, RolloverPolicy::Carry));
    assert!(
        run(&row).violations.is_empty(),
        "the gate admits the shape; what refuses a carry row is the unbuilt-grant rule: {:?}",
        codes(&run(&row))
    );
    assert!(
        crate::domain::allowance::compile(&row).is_none(),
        "and no $0 band is compiled for it, which would double the benefit the day the grant \
         table lands"
    );
}

/// The band advisory does not fire on the compiled ladder, whose first band is
/// free by construction — a warning on every allowance row would teach authors
/// that the warnings channel is noise.
#[test]
fn the_compiled_ladder_raises_no_rising_price_advisory() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(100, RolloverPolicy::None));
    assert!(run(&row).warnings.is_empty(), "{:?}", run(&row).warnings);
}

/// `BandTop` is destructured exhaustively by the compile, so an open top stays
/// open — the one bound `+N` must not move.
#[test]
fn the_open_top_survives_the_offset() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(100, RolloverPolicy::None));
    let compiled = crate::domain::allowance::compile(&row).expect("it compiles");
    assert_eq!(
        compiled.bands.last().expect("a top band").to_qty,
        BandTop::Open
    );
}

// ---------------------------------------------------------------------------
// D-312 -- which of `inst-ac-band`'s two findings the authoring write may
// refuse, and which one it must not (review M26).
// ---------------------------------------------------------------------------

/// **The saturating quantity is the operand the write door is allowed to see**,
/// because it is the only one the author can move.
///
/// The rule produces at most two kinds of finding and they have opposite stages.
/// `ALLOWANCE_QUANTITY_INVALID` is about the number in the request: both operands
/// are present, the authored ladder is well formed and stays so at every smaller
/// `N`, and no later call can make a saturating offset fit. That is D-312's
/// criterion, so it is stamped by `violate_at_write` and the door that keeps
/// `write_stage_only()` sees it.
///
/// Until 2026-08-20 it was stamped `Publish` and therefore filtered out at that
/// door -- the one remediable operand was the one the author never saw.
///
/// The `u64::MAX - 5` quantity is chosen so the *compiled* ladder is a clean
/// geometry (the case above proves it is), which leaves this finding alone in
/// the report and makes the equality below an equality rather than a `contains`.
#[test]
fn a_saturating_quantity_is_what_the_write_door_is_handed() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(u64::MAX - 5, RolloverPolicy::None));

    let report = run(&row);
    let at_the_write = report
        .write_stage_only()
        .expect("a saturating quantity is judgeable at the authoring write");
    assert_eq!(
        codes(&at_the_write),
        vec![ALLOWANCE_QUANTITY_INVALID.to_owned()],
        "the declared quantity is in the request and nothing a later call adds can make the \
         offset fit, so the write refuses it rather than the publish: {:?}",
        codes(&report)
    );

    // The control, without which a rule that write-stamped every finding it made
    // would satisfy the assertion above: a quantity the ladder's top bound can be
    // raised by leaves the write door nothing at all.
    let mut fits = graduated_usage();
    fits.included_allowance = Some(allowance(100, RolloverPolicy::None));
    assert!(
        run(&fits).write_stage_only().is_none(),
        "an ordinary allowance is not a write-stage refusal: {:?}",
        codes(&run(&fits))
    );
}

/// **The derived band complaint is re-stamped `Publish`, whatever the band rule
/// stamped it.**
///
/// `BandGeometry` judges `TIER_BAND_EMPTY` at the *write* since 2026-08-20, and
/// it is right to: a zero-width band is read from the band alone. But the ladder
/// this rule hands it is not one anybody authored -- it is the compile's, and the
/// band it names here is `[18446744073709551615, 18446744073709551615)`, produced
/// by offsetting `[0, 1000)` and `[1000, ..)` by a quantity that saturates both
/// bounds onto each other. Carrying that stamp through made the authoring write
/// refuse a row naming a band the request never contained, and the author had no
/// edit that would answer it: the authored ladder was checked clean three lines
/// earlier.
///
/// So the report is asserted twice over -- the finding's own stage, and what the
/// door actually receives -- and then against the same code raised on a band the
/// author *did* send, which is what keeps this about the derived subject rather
/// than about `TIER_BAND_EMPTY`.
#[test]
fn the_compiled_ladders_complaint_never_reaches_the_write_door() {
    let mut row = graduated_usage();
    row.included_allowance = Some(allowance(u64::MAX, RolloverPolicy::None));

    let report = run(&row);
    let derived = report
        .violations
        .iter()
        .find(|violation| violation.code == TIER_BAND_EMPTY)
        .unwrap_or_else(|| panic!("the compiled ladder collapses: {:?}", codes(&report)));
    assert!(
        derived.detail.contains(&u64::MAX.to_string()),
        "the finding names a bound the request never carried -- the authored ladder is [0, 1000) \
         and [1000, open): {}",
        derived.detail
    );
    assert_eq!(
        derived.stage,
        Stage::Publish,
        "a derived subject must not produce a write-stage refusal, whatever the band rule \
         stamped on its own subject"
    );

    // And the door is what makes that observable: the one remediable operand is
    // all the author is asked to move.
    let at_the_write = report
        .write_stage_only()
        .expect("the saturating quantity is still write-judgeable");
    assert_eq!(
        codes(&at_the_write),
        vec![ALLOWANCE_QUANTITY_INVALID.to_owned()],
        "the write door refuses the quantity and says nothing about the ladder the compile \
         built out of it"
    );

    // The control. The identical code, on a zero-width band the author sent
    // themselves, is stamped `Write` by the rule that raises it -- so the
    // assertion above is about who the subject belongs to and not about the code
    // having been demoted everywhere.
    let mut authored = graduated_usage();
    authored.bands = vec![
        TierBand::closed(0, 1_000, rate(5)),
        TierBand::closed(1_000, 1_000, rate(4)),
        TierBand::open(1_000, rate(3)),
    ];
    let mut raised = ValidationReport::default();
    BandGeometry.evaluate(&authored, &mut raised);
    let sent = raised
        .violations
        .iter()
        .find(|violation| violation.code == TIER_BAND_EMPTY)
        .expect("the authored band covers no quantity");
    assert_eq!(
        sent.stage,
        Stage::Write,
        "a band in the request is judged at the write, which is the stamp this rule overrides \
         only because its own subject is derived"
    );
}
