//! The D-45 compile: what it produces, what it refuses to produce, and the two
//! properties the design set calls normative — **compile-equivalence** (AC #90a)
//! and **determinism / re-entry** (`inst-ac-deterministic`, D-130).

use super::{
    Admissibility, AllowanceSource, compile, is_presented_tiered, presented_bands,
    presented_model_kind,
};
use crate::domain::money::RateMinor;
use crate::domain::price_row::{
    AggregationFunction, AggregationGranularity, BandTop, BillingGranularity, IncludedAllowance,
    ModelKind, PriceRow, ReservationFlavor, RolloverPolicy, TierAggregationWindow, TierBand,
};
use crate::domain::scope_key::ChargeKind;

fn rate(minor_units: i64) -> RateMinor {
    RateMinor::from_minor_units(minor_units).expect("a non-negative rate")
}

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

/// The untiered shape. It carries a `tierAggregationWindow` although an untiered
/// row owes none: every case here hangs an allowance on it, and a compiled
/// allowance row is tiered for `inst-tb-window`'s purposes — the `[0, N) @ $0`
/// band is a counter and it has to reset. Without one this stood for a row that
/// could never publish.
fn per_unit_usage() -> PriceRow {
    let mut row = PriceRow::new(ChargeKind::Usage, Some(ModelKind::PerUnit));
    row.meter = Some("egress.gb".to_owned());
    row.billing_granularity = Some(BillingGranularity::PerHour);
    row.tier_aggregation_window = Some(TierAggregationWindow::InvoicePeriod);
    row.unit_rate = Some(rate(2));
    row
}

fn with(mut row: PriceRow, quantity: u64, policy: RolloverPolicy) -> PriceRow {
    row.included_allowance = Some(IncludedAllowance {
        quantity,
        rollover_policy: policy,
    });
    row
}

// ---------------------------------------------------------------------------
// `inst-ac-band` — the two compiled shapes.
// ---------------------------------------------------------------------------

#[test]
fn a_tiered_row_gets_the_free_band_prepended_and_every_authored_bound_offset() {
    let row = with(graduated_usage(), 100, RolloverPolicy::None);
    let compiled = compile(&row).expect("it compiles");

    assert_eq!(
        compiled.bands,
        vec![
            TierBand::closed(0, 100, RateMinor::ZERO),
            // the authored [0, 1000) becomes [100, 1100)
            TierBand::closed(100, 1_100, rate(5)),
            TierBand::open(1_100, rate(3)),
        ]
    );
    assert_eq!(compiled.presented_kind, ModelKind::Graduated);
    assert_eq!(compiled.authored_kind, ModelKind::Graduated);
}

#[test]
fn an_untiered_row_synthesizes_two_bands_and_is_presented_as_graduated() {
    let row = with(per_unit_usage(), 100, RolloverPolicy::None);
    let compiled = compile(&row).expect("it compiles");

    assert_eq!(
        compiled.bands,
        vec![
            TierBand::closed(0, 100, RateMinor::ZERO),
            TierBand::open(100, rate(2)),
        ],
        "the authored rate moves into the [N, null) band"
    );
    assert_eq!(compiled.presented_kind, ModelKind::Graduated);
    assert_eq!(
        compiled.authored_kind,
        ModelKind::PerUnit,
        "the authored kind is retained beside the marker (D-59), which is what tells a reader the \
         top band's rate is the folded unitRateNanoMinor"
    );
}

/// The design set says the compile folds the authored **`amount_minor`**; D-311
/// moved a `per_unit` row's money out of that column and into `unit_rate`, and
/// folding `amount_minor` would fold a NULL.
#[test]
fn the_untiered_fold_reads_the_rate_column_and_not_the_amount_column() {
    let mut row = with(per_unit_usage(), 10, RolloverPolicy::None);
    row.unit_rate = Some(rate(7));
    row.amount_minor = None;

    let compiled = compile(&row).expect("it compiles");
    assert_eq!(compiled.bands[1].unit_price_rate, rate(7));
}

// ---------------------------------------------------------------------------
// `inst-ac-marker`.
// ---------------------------------------------------------------------------

#[test]
fn the_marker_carries_the_declaration_and_says_it_was_compiled() {
    let row = with(graduated_usage(), 250, RolloverPolicy::None);
    let marker = compile(&row).expect("it compiles").marker;

    assert_eq!(marker.quantity, 250);
    assert_eq!(marker.rollover_policy, RolloverPolicy::None);
    assert_eq!(marker.source, AllowanceSource::Compiled);
    assert_eq!(marker.source.as_str(), "compiled");
}

/// The observable difference D-45 exists for: a hand-authored `$0` band is not
/// an allowance and carries no marker, so display and the included-vs-billed
/// split can tell the two apart.
#[test]
fn a_hand_authored_free_band_carries_no_marker() {
    let mut row = graduated_usage();
    row.bands = vec![
        TierBand::closed(0, 100, RateMinor::ZERO),
        TierBand::open(100, rate(5)),
    ];
    assert!(compile(&row).is_none());
}

// ---------------------------------------------------------------------------
// AC #90a — compile-equivalence.
// ---------------------------------------------------------------------------

/// The projected output is the hand-authored equivalent, band for band. If the
/// two ever diverge, rating bills an allowance row differently from the row an
/// operator would have written by hand to mean the same thing.
#[test]
fn the_compiled_ladder_equals_the_hand_authored_equivalent() {
    let compiled = compile(&with(graduated_usage(), 100, RolloverPolicy::None))
        .expect("it compiles")
        .bands;

    let mut by_hand = graduated_usage();
    by_hand.bands = vec![
        TierBand::closed(0, 100, RateMinor::ZERO),
        TierBand::closed(100, 1_100, rate(5)),
        TierBand::open(1_100, rate(3)),
    ];

    assert_eq!(compiled, by_hand.bands);
}

// ---------------------------------------------------------------------------
// `inst-ac-deterministic` / D-130 — the input survives its own compile.
// ---------------------------------------------------------------------------

/// The compile is a projection: the row it reads is byte-identical afterwards,
/// which is what makes the second run possible at all.
#[test]
fn the_compile_never_touches_its_input() {
    let row = with(graduated_usage(), 100, RolloverPolicy::None);
    let before = row.clone();
    drop(compile(&row));
    assert_eq!(row, before);
}

/// The second run of the mechanism. Under the pre-D-130 in-place rewrite the
/// first publish destroyed the authored bounds, so a supersession, repricing
/// successor or clone recompiled from already-offset bands — and tripped
/// `ALLOWANCE_DOUBLE_FREE` on its own output.
#[test]
fn a_successor_authored_from_the_stored_row_recompiles_identically() {
    let published = with(graduated_usage(), 100, RolloverPolicy::None);
    let first = compile(&published).expect("it compiles");

    // What a supersession, a repricing run or a clone hands the compiler: the
    // stored row, unchanged, because nothing wrote the compiled artifacts back.
    let successor = published;
    let second = compile(&successor).expect("it recompiles");

    assert_eq!(first, second);
    assert!(
        !crate::domain::allowance::authors_a_free_first_band(&successor),
        "and the stored row still authors no free opening band, so the double-free refusal does \
         not fire on the compiler's own output"
    );
}

// ---------------------------------------------------------------------------
// `Admissibility` — the single reading the gate and the compile share.
// ---------------------------------------------------------------------------

#[test]
fn every_shape_the_gate_refuses_compiles_to_nothing() {
    let cases: Vec<(Admissibility, PriceRow)> = vec![
        (Admissibility::NoDeclaration, graduated_usage()),
        (Admissibility::NonUsage, {
            let mut row = PriceRow::new(ChargeKind::Recurring, Some(ModelKind::Flat));
            row.included_allowance = Some(IncludedAllowance {
                quantity: 10,
                rollover_policy: RolloverPolicy::None,
            });
            row
        }),
        (
            Admissibility::QuantityInvalid,
            with(graduated_usage(), 0, RolloverPolicy::None),
        ),
        (Admissibility::KindUnsupported, {
            let mut row = with(graduated_usage(), 10, RolloverPolicy::None);
            row.model_kind = Some(ModelKind::Volume);
            row
        }),
        (Admissibility::KindUnauthored, {
            let mut row = with(graduated_usage(), 10, RolloverPolicy::None);
            row.model_kind = None;
            row
        }),
        (Admissibility::NonSum, {
            let mut row = with(graduated_usage(), 10, RolloverPolicy::None);
            row.aggregation_function = Some(AggregationFunction::Peak);
            row.aggregation_granularity = Some(AggregationGranularity::Hour);
            row.max_hold_granules = Some(1);
            row
        }),
        (Admissibility::WithReservation, {
            let mut row = with(graduated_usage(), 10, RolloverPolicy::None);
            row.reservation_flavor = Some(ReservationFlavor::Capacity);
            row.reserved_rate = Some(RateMinor::from_minor_units(4).expect("a non-negative rate"));
            row
        }),
        (Admissibility::DoubleFree, {
            let mut row = with(graduated_usage(), 10, RolloverPolicy::None);
            row.bands = vec![
                TierBand::closed(0, 10, RateMinor::ZERO),
                TierBand::open(10, rate(5)),
            ];
            row
        }),
        (
            Admissibility::CarryUnbuilt,
            with(graduated_usage(), 10, RolloverPolicy::Carry),
        ),
        (Admissibility::NothingToCompileFrom, {
            let mut row = with(graduated_usage(), 10, RolloverPolicy::None);
            row.bands = Vec::new();
            row
        }),
        (Admissibility::NothingToCompileFrom, {
            let mut row = with(per_unit_usage(), 10, RolloverPolicy::None);
            row.unit_rate = None;
            row
        }),
    ];

    for (expected, row) in cases {
        assert_eq!(Admissibility::of(&row), expected, "{row:?}");
        assert!(
            compile(&row).is_none(),
            "{expected:?} must produce no artifact at all"
        );
    }

    // The positive control: without it every line above would pass against a
    // function that answered `None` for every row ever handed to it.
    let admitted = with(graduated_usage(), 10, RolloverPolicy::None);
    assert_eq!(Admissibility::of(&admitted), Admissibility::Compiles);
    assert!(compile(&admitted).is_some());
}

// ---------------------------------------------------------------------------
// What publishes: the presented kind and the presented bands.
// ---------------------------------------------------------------------------

/// `inst-ac-band` fixture-gates the row on the **compiled** kind, so an untiered
/// row carrying an allowance must not be gated on `per_unit`'s fixture.
#[test]
fn an_allowance_row_presents_the_compiled_kind_and_a_plain_row_presents_its_own() {
    assert_eq!(
        presented_model_kind(&with(per_unit_usage(), 10, RolloverPolicy::None)),
        Some(ModelKind::Graduated)
    );
    assert!(is_presented_tiered(&with(
        per_unit_usage(),
        10,
        RolloverPolicy::None
    )));

    assert_eq!(
        presented_model_kind(&per_unit_usage()),
        Some(ModelKind::PerUnit)
    );
    assert!(!is_presented_tiered(&per_unit_usage()));
    assert_eq!(
        presented_model_kind(&PriceRow::new(ChargeKind::Usage, None)),
        None
    );
}

/// And the presented kind is what the evaluation-policy rule reads, which is
/// what the fixture's `tierAggregationWindow` is there for: the same row, the
/// same predicate, judged by `inst-tb-window`.
#[test]
fn the_shape_that_presents_as_tiered_is_the_shape_that_owes_a_reset_window() {
    use crate::domain::rules::tier_bands::UsageEvaluationPolicy;
    use crate::domain::validation::{ValidationReport, ValidationRule};

    let window_findings = |row: &PriceRow| {
        let mut report = ValidationReport::default();
        UsageEvaluationPolicy.evaluate(row, &mut report);
        report.violations.len()
    };

    let carrying = with(per_unit_usage(), 10, RolloverPolicy::None);
    assert!(is_presented_tiered(&carrying));
    assert_eq!(
        window_findings(&carrying),
        0,
        "the fixture names its window, so the compiled counter resets"
    );

    let mut unwindowed = carrying;
    unwindowed.tier_aggregation_window = None;
    assert_eq!(
        window_findings(&unwindowed),
        1,
        "and without one the allowance would be granted once for the subscription's life"
    );
}

#[test]
fn presented_bands_are_the_compiled_set_where_one_exists_and_the_authored_set_otherwise() {
    let plain = graduated_usage();
    assert_eq!(presented_bands(&plain), plain.bands);

    let carrying = with(graduated_usage(), 100, RolloverPolicy::None);
    assert_eq!(
        presented_bands(&carrying).first().expect("a first band"),
        &TierBand::closed(0, 100, RateMinor::ZERO)
    );
    assert_eq!(
        presented_bands(&carrying)
            .last()
            .expect("a top band")
            .to_qty,
        BandTop::Open
    );
}
