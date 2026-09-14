use super::*;
use crate::traits::EvalError;
use crate::traits::EvalInput;
use crate::traits::Evaluated;
use bss_fixtures::{Band, BandTop, Corpus, Expect, Family};

/// Every assertion in a family must be reproduced by the oracle. This is what
/// "green" means for the `oracle` flag.
fn assert_family_reproduced(family: Family) {
    let corpus = Corpus::load(&Corpus::corpus_root()).expect("corpus loads");
    let oracle = ReferenceOracle;

    let mut checked = 0;
    for case in corpus.cases_for(family) {
        // Publish cases are answered by a `PublishValidator`, not the oracle.
        let bss_fixtures::Case::Evaluation(case) = case else {
            continue;
        };
        for (i, a) in case.assert.iter().enumerate() {
            let input = EvalInput {
                snapshot: &case.snapshot,
                runtime: &case.runtime,
                given: &a.given,
            };
            let got = oracle
                .evaluate(&input)
                .unwrap_or_else(|e| panic!("{}[{i}]: oracle errored: {e}", case.id));
            let want = match a.expect {
                Expect::Charge(c) => Evaluated::Charge(c.charge_minor),
                Expect::Units(u) => Evaluated::Units {
                    charged: u.units_charged,
                    in_basis: u.units_in_basis,
                },
                Expect::Fold(f) => Evaluated::Fold { q: f.folded_q },
            };
            assert_eq!(
                got, want,
                "{}[{i}]: oracle says {got:?}, corpus says {want:?}",
                case.id
            );
            checked += 1;
        }
    }

    assert!(
        checked > 0,
        "{family:?} has no assertions -- an empty family is not green"
    );
}

fn two_bands() -> Vec<Band> {
    vec![
        Band {
            from_qty: 0,
            to_qty: BandTop::Closed(1000),
            unit_amount_minor: 5,
        },
        Band {
            from_qty: 1000,
            to_qty: BandTop::Open,
            unit_amount_minor: 3,
        },
    ]
}

#[test]
fn oracle_reproduces_tier_boundary() {
    assert_family_reproduced(Family::TierBoundary);
}

#[test]
fn graduated_is_marginal_at_the_band_edge() {
    // Guards the single most expensive misreading: treating q == to_qty as
    // already inside the next band for graduated.
    let bands = two_bands();
    assert_eq!(graduated(&bands, 1000).unwrap(), 5000);
    assert_eq!(graduated(&bands, 1001).unwrap(), 5003);
}

#[test]
fn volume_selects_one_band_for_the_whole_quantity() {
    let bands = two_bands();
    assert_eq!(volume(&bands, 999).unwrap(), 4995);
    assert_eq!(volume(&bands, 1000).unwrap(), 3000);
}

#[test]
fn oracle_reproduces_package() {
    assert_family_reproduced(Family::Package);
}

#[test]
fn package_rounds_blocks_up_never_down() {
    assert_eq!(package(1000, 2500, 0).unwrap(), 0);
    assert_eq!(package(1000, 2500, 1).unwrap(), 2500);
    assert_eq!(package(1000, 2500, 1000).unwrap(), 2500);
    assert_eq!(package(1000, 2500, 1001).unwrap(), 5000);
}

#[test]
fn package_rejects_a_zero_size() {
    // `div_ceil` would panic; fail closed instead.
    assert_eq!(package(0, 2500, 10), Err(EvalError::ZeroPackageSize));
}

#[test]
fn oracle_reproduces_per_unit() {
    assert_family_reproduced(Family::PerUnit);
}

#[test]
fn oracle_reproduces_flat() {
    assert_family_reproduced(Family::Flat);
}

#[test]
fn flat_ignores_the_quantity_where_per_unit_scales_by_it() {
    // The pair that gets confused: PRD 6.2 bans `flat` on a usage row precisely
    // because the plain untiered metered rate is `per_unit`. An evaluator that
    // multiplies a flat row has silently swapped one for the other.
    let flat = bss_fixtures::Snapshot {
        model_kind: bss_fixtures::ModelKind::Flat,
        // `flat` is legal on non-usage rows only, and this pair is the whole
        // point of `inst-mk-chargekind`: the two kinds it separates are these
        // two, on the same charge component.
        charge_kind: bss_fixtures::ChargeKind::Recurring,
        currency: "USD".into(),
        bands: Vec::new(),
        amount_minor: Some(9900),
        package_size: None,
        package_price_minor: None,
        quantity_source: None,
        tier_aggregation_window: None,
        billing_granularity: None,
        proration_basis: None,
        meter: None,
        dimension_key: None,
        aggregation_function: None,
        aggregation_granularity: None,
        max_hold_granules: None,
        tier_qualification_window: None,
        included_allowance: None,
        reserved_rate_minor: None,
        reservation_flavor: None,
    };
    let per_unit = bss_fixtures::Snapshot {
        model_kind: bss_fixtures::ModelKind::PerUnit,
        ..flat.clone()
    };
    let rt = bss_fixtures::Runtime::default();
    let oracle = ReferenceOracle;

    for q in [0, 1, 1000] {
        let flat_charge = oracle
            .evaluate(&EvalInput {
                snapshot: &flat,
                runtime: &rt,
                given: &bss_fixtures::Given {
                    q,
                    ..bss_fixtures::Given::default()
                },
            })
            .expect("flat prices");
        assert_eq!(
            flat_charge,
            Evaluated::Charge(9900),
            "flat must not move with q={q}"
        );
    }

    let per_unit_charge = oracle
        .evaluate(&EvalInput {
            snapshot: &per_unit,
            runtime: &rt,
            given: &bss_fixtures::Given {
                q: 3,
                ..bss_fixtures::Given::default()
            },
        })
        .expect("per_unit prices");
    assert_eq!(
        per_unit_charge,
        Evaluated::Charge(29700),
        "per_unit must scale by q"
    );
}

#[test]
fn oracle_reproduces_proration() {
    assert_family_reproduced(Family::Proration);
}

#[test]
fn a_split_period_sums_to_the_basis_exactly() {
    // Rating T-D-26: the slice fractions of one period must sum to exactly 1.
    // In integer units that is an exact equality, not a float comparison — which
    // is the whole reason this family reports a ratio instead of money.
    let corpus = Corpus::load(&Corpus::corpus_root()).expect("corpus loads");
    let case = corpus
        .cases_for(Family::Proration)
        .find_map(|c| match c {
            bss_fixtures::Case::Evaluation(e) if e.id == "pricewindow-split-sums-to-one" => Some(e),
            _ => None,
        })
        .expect("the split case must exist");
    let oracle = ReferenceOracle;

    let mut slices = Vec::new();
    let mut basis = None;
    for a in &case.assert {
        // The uncut whole-period assertion is not a slice; skip it.
        if a.given.from.is_none() && a.given.to.is_none() {
            continue;
        }
        let got = oracle
            .evaluate(&EvalInput {
                snapshot: &case.snapshot,
                runtime: &case.runtime,
                given: &a.given,
            })
            .expect("slice prorates");
        if let Evaluated::Units { charged, in_basis } = got {
            slices.push(charged);
            basis = Some(in_basis);
        }
    }

    assert_eq!(slices.len(), 2, "the case must carry two slices");
    assert_eq!(
        slices.iter().sum::<u64>(),
        basis.expect("a basis"),
        "no unit may be billed twice or lost at the cut"
    );
}

#[test]
fn oracle_reproduces_supersession_continuity() {
    assert_family_reproduced(Family::SupersessionContinuity);
}

#[test]
fn a_reset_counter_would_price_differently() {
    // What `inst-tb-window-continuity` is worth in money. The successor's bands
    // applied to the continued window total (1500) against the same bands
    // applied to post-supersession usage alone (900): a 2700 difference on one
    // row. The corpus asserts only the correct number; this pins the size of
    // the mistake it prevents.
    let bands = vec![
        Band {
            from_qty: 0,
            to_qty: BandTop::Closed(1000),
            unit_amount_minor: 7,
        },
        Band {
            from_qty: 1000,
            to_qty: BandTop::Open,
            unit_amount_minor: 4,
        },
    ];

    let continued = graduated(&bands, 1500).expect("prices");
    let had_it_reset = graduated(&bands, 900).expect("prices");

    assert_eq!(continued, 9000);
    assert_eq!(had_it_reset, 6300);
    assert_eq!(continued - had_it_reset, 2700);
}

#[test]
fn the_supersession_guard_pins_both_verdicts() {
    // A guard that only ever rejected would make supersession unusable, so the
    // family has to carry the accepted case too: supersession *is* a price
    // change on one key.
    let corpus = Corpus::load(&Corpus::corpus_root()).expect("corpus loads");

    let mut rejections: Vec<&str> = Vec::new();
    let mut acceptances: Vec<&str> = Vec::new();
    for case in corpus.cases_for(Family::SupersessionContinuity) {
        let bss_fixtures::Case::Publish(p) = case else {
            continue;
        };
        for a in &p.assert {
            match &a.expect {
                bss_fixtures::PublishVerdict::Rejected { error_code } => {
                    assert_eq!(
                        error_code, "SUPERSESSION_UNIT_MISMATCH",
                        "{}: the guard has one code",
                        p.id
                    );
                    rejections.push(&p.id);
                }
                bss_fixtures::PublishVerdict::Accepted => acceptances.push(&p.id),
            }
        }
    }
    rejections.sort_unstable();
    acceptances.sort_unstable();

    // The rosters rather than their sizes, and rather than a floor: a lower bound
    // cannot see the family shrinking towards the refusal-only state
    // `IntegrityViolation::ModelKindWithoutAcceptedPublishCase` forbids, and it
    // cannot say *which* case left. A case added here reddens this and is meant to:
    // the near and the far side of the guard are both authored, not accumulated.
    assert_eq!(
        rejections,
        [
            "carry-allowance-change-rejected",
            "dimension-key-change-rejected",
            "kind-flip-rejected",
            "meter-change-rejected",
            "package-size-change-rejected",
            "per-unit-usage-granularity-change-rejected",
            "unit-change-rejected",
        ]
    );
    assert_eq!(
        acceptances,
        [
            "flat-price-change-accepted",
            "package-price-change-accepted",
            "per-unit-non-usage-price-change-accepted",
            "per-unit-usage-price-change-accepted",
            "price-change-accepted",
            "volume-band-change-accepted",
        ]
    );
}

#[test]
fn oracle_reproduces_level_aggregation() {
    assert_family_reproduced(Family::LevelAggregation);
}

#[test]
fn a_late_sample_moves_only_its_own_granule() {
    // The base fold and the backfilled one differ by exactly the lift of hour 1's
    // peak: the level carried in from 00:40 stands at 20 until the backfilled
    // sample raises it to 25. Hours 0 and 2 are untouched, which is what makes the
    // correction a standard delta rather than a window-wide recompute. 15 is one of
    // hour 1's samples and never its fold — the reading `peak-granule-fold` exists
    // to refute.
    let corpus = Corpus::load(&Corpus::corpus_root()).expect("corpus loads");
    let q_of = |id: &str| {
        let case = corpus
            .cases_for(Family::LevelAggregation)
            .find_map(|c| match c {
                bss_fixtures::Case::Evaluation(e) if e.id == id => Some(e),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} must exist"));
        match ReferenceOracle
            .evaluate(&EvalInput {
                snapshot: &case.snapshot,
                runtime: &case.runtime,
                given: &case.assert[0].given,
            })
            .expect("folds")
        {
            Evaluated::Fold { q } => q,
            other => panic!("{id} folded to {other:?}"),
        }
    };

    assert_eq!(q_of("late-sample-refold") - q_of("peak-granule-fold"), 5);
}

#[test]
fn oracle_reproduces_reserved() {
    assert_family_reproduced(Family::Reserved);
}

#[test]
fn volume_without_a_covering_band_fails_rather_than_guessing() {
    // A closed top band is publish-blocked (D-17), but the evaluator must not
    // invent a rate if one ever reaches it.
    let closed_top = vec![Band {
        from_qty: 0,
        to_qty: BandTop::Closed(10),
        unit_amount_minor: 5,
    }];

    assert_eq!(
        volume(&closed_top, 10),
        Err(EvalError::NoBandCoversQuantity(10))
    );
}

// ---------------------------------------------------------------------------
// Over-range and out-of-period inputs. This file's own doc says the oracle is
// "audited by reading", which raises rather than lowers the bar on its
// arithmetic being total.
// ---------------------------------------------------------------------------

fn instant(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(y, m, d, 0, 0, 0)
        .single()
        .expect("a real instant")
}

/// A chargeable stretch that leaves the period is refused, on **every** basis.
///
/// `prorate` refuses a zero-length period and an inverted stretch and never
/// asked for containment, so `calendar_days_actual` and `by_second` could both
/// return a ratio above 1 — a stretch of 59 days apportioned over a 31-day period
/// is a factor of 1.90, i.e. 190% of a full period. `calendar_days_30` clamps and
/// says why it clamps ("so a 31-day month never bills 31/30 of itself"), which is
/// what makes this a sibling outlier rather than a uniform gap: one of three arms
/// defended against a factor above 1, for a stated reason, and its two siblings
/// had the same exposure and no defence.
///
/// Refused rather than clamped, because that is what this file does everywhere
/// else it is handed an input it cannot honestly answer — `volume` refuses rather
/// than inventing a rate, `package` refuses a zero divisor, `integral` refuses a
/// fold that does not divide. A clamp would silently answer 1.00 for a Given that
/// is a data error, and the corpus is the contract a second implementation must
/// reproduce.
#[test]
fn a_chargeable_stretch_outside_the_period_is_refused_on_every_basis() {
    let period_start = instant(2026, 1, 1);
    let period_end = instant(2026, 2, 1);

    let past_the_end = bss_fixtures::Given {
        q: 0,
        period_start: Some(period_start),
        period_end: Some(period_end),
        from: Some(period_start),
        to: Some(instant(2026, 3, 1)),
    };
    let before_the_start = bss_fixtures::Given {
        q: 0,
        period_start: Some(period_start),
        period_end: Some(period_end),
        from: Some(instant(2025, 12, 1)),
        to: Some(period_end),
    };

    for basis in [
        bss_fixtures::ProrationBasis::CalendarDaysActual,
        bss_fixtures::ProrationBasis::CalendarDays30,
        bss_fixtures::ProrationBasis::BySecond,
    ] {
        assert_eq!(
            prorate(basis, &past_the_end),
            Err(EvalError::StretchOutsidePeriod),
            "{basis:?} must refuse a stretch that ends after the period"
        );
        assert_eq!(
            prorate(basis, &before_the_start),
            Err(EvalError::StretchOutsidePeriod),
            "{basis:?} must refuse a stretch that starts before the period"
        );
    }

    // And the whole period is still the default and still charges in full, so the
    // containment check has not made the ordinary case degenerate.
    let whole = bss_fixtures::Given {
        q: 0,
        period_start: Some(period_start),
        period_end: Some(period_end),
        from: None,
        to: None,
    };
    assert_eq!(
        prorate(bss_fixtures::ProrationBasis::CalendarDaysActual, &whole),
        Ok(Evaluated::Units {
            charged: 31,
            in_basis: 31
        })
    );
    // `calendar_days_30`'s own clamp keeps its job: a contained 31-day stretch is
    // still capped at 30, which is the case its comment is about and is a
    // different case from the one above.
    assert_eq!(
        prorate(bss_fixtures::ProrationBasis::CalendarDays30, &whole),
        Ok(Evaluated::Units {
            charged: 30,
            in_basis: 30
        })
    );
}

/// The band walk refuses a product it cannot hold, rather than wrapping into a
/// plausible number.
///
/// `quantity` exists to "widen a quantity into the money domain, refusing rather
/// than wrapping", and every caller then multiplied it by a rate unchecked. A
/// release build wraps and answers; a debug build panics. Both are worse than an
/// error from a reference implementation whose corpus is the contract another
/// gear must reproduce.
#[test]
fn a_charge_that_does_not_fit_the_money_domain_is_refused_not_wrapped() {
    let huge = vec![Band {
        from_qty: 0,
        to_qty: BandTop::Open,
        unit_amount_minor: 1_000_000_000,
    }];
    assert_eq!(
        graduated(&huge, 1_000_000_000_000),
        Err(EvalError::ArithmeticOverflow {
            what: "graduated band charge"
        })
    );
    assert_eq!(
        volume(&huge, 1_000_000_000_000),
        Err(EvalError::ArithmeticOverflow {
            what: "volume charge"
        })
    );
    assert_eq!(
        package(1, 1_000_000_000, 1_000_000_000_000),
        Err(EvalError::ArithmeticOverflow {
            what: "package charge"
        })
    );

    // The accumulator across bands is the other half: each band's own product
    // fits and the running total does not.
    let two_halves = vec![
        Band {
            from_qty: 0,
            to_qty: BandTop::Closed(5_000_000_000),
            unit_amount_minor: 1_000_000_000,
        },
        Band {
            from_qty: 5_000_000_000,
            to_qty: BandTop::Open,
            unit_amount_minor: 1_000_000_000,
        },
    ];
    assert_eq!(
        graduated(&two_halves, 10_000_000_000),
        Err(EvalError::ArithmeticOverflow {
            what: "graduated band charge"
        })
    );

    // And the ordinary ladder is untouched.
    assert_eq!(graduated(&two_bands(), 1500), Ok(6500));
}

/// The reserved-capacity charge is a **triple** product and the most exposed
/// expression in the file: a `per_second` row over a month carries roughly 2.6e6
/// covered granules, so `reserved x rate` has about 3.5e12 of headroom left.
#[test]
fn a_reserved_capacity_charge_that_overflows_is_refused() {
    let snap = bss_fixtures::Snapshot {
        model_kind: bss_fixtures::ModelKind::PerUnit,
        charge_kind: bss_fixtures::ChargeKind::Usage,
        currency: "USD".to_owned(),
        bands: Vec::new(),
        amount_minor: None,
        package_size: None,
        package_price_minor: None,
        quantity_source: None,
        tier_aggregation_window: None,
        billing_granularity: None,
        proration_basis: None,
        meter: None,
        dimension_key: None,
        aggregation_function: None,
        aggregation_granularity: None,
        max_hold_granules: None,
        tier_qualification_window: None,
        included_allowance: None,
        reserved_rate_minor: Some(4_000_000),
        reservation_flavor: Some(bss_fixtures::ReservationFlavor::Capacity),
    };
    // `reserved x rate` is 4e12 and fits; `x granules` takes it to 1.04e19,
    // past `i64::MAX` at 9.22e18. So it is the **outer** product that refuses.
    let rt = bss_fixtures::Runtime {
        reserved_quantity: Some(1_000_000),
        covered_granules: Some(2_600_000),
        ..bss_fixtures::Runtime::default()
    };
    assert_eq!(
        reservation(&snap, &rt, 0, bss_fixtures::ReservationFlavor::Capacity),
        Err(EvalError::ArithmeticOverflow {
            what: "reserved capacity charge"
        })
    );

    // And the inner one, so both halves of the triple are covered: 4e9 x 4e6 is
    // 1.6e16 — fits — while 4e9 x 4e9 does not, and the granule count never
    // enters it.
    let inner = bss_fixtures::Runtime {
        reserved_quantity: Some(4_000_000_000),
        covered_granules: Some(1),
        ..bss_fixtures::Runtime::default()
    };
    let mut big_rate = snap.clone();
    big_rate.reserved_rate_minor = Some(4_000_000_000);
    assert_eq!(
        reservation(
            &big_rate,
            &inner,
            0,
            bss_fixtures::ReservationFlavor::Capacity
        ),
        Err(EvalError::ArithmeticOverflow {
            what: "reserved capacity charge"
        })
    );

    // The ordinary shape still answers: 1000 units at 3 minor for 24 granules.
    let ordinary = bss_fixtures::Runtime {
        reserved_quantity: Some(1_000),
        covered_granules: Some(24),
        ..bss_fixtures::Runtime::default()
    };
    // `snap` is not read again, so this moves rather than clones —
    // `clippy::redundant_clone` is in `clippy::perf`, which `CLIPPY_FLAGS` denies.
    let mut small_rate = snap;
    small_rate.reserved_rate_minor = Some(3);
    assert_eq!(
        reservation(
            &small_rate,
            &ordinary,
            0,
            bss_fixtures::ReservationFlavor::Capacity
        ),
        Ok(Evaluated::Charge(72_000))
    );
}

/// A sample taken before the window opens the level, and no distance ages it out.
///
/// `seconds` floors at zero, so every pre-window sample reads as granule 0 and is
/// held into granule `g` for as long as `g <= max_hold` — a level observed a year
/// before the window is carried exactly as far as one observed a second before it.
/// Pinned rather than changed: what a pre-window sample means is a fact about the
/// reference semantics rating has to reproduce, so it belongs in the corpus, and
/// no committed case carries one. This test is what makes a change to it visible.
#[test]
fn a_sample_before_the_window_opens_the_level_and_is_never_aged_out() {
    let window_start = instant(2026, 1, 8);
    let step: u64 = 3_600;

    let a_second_before = vec![bss_fixtures::GaugeSample {
        at: window_start - chrono::Duration::seconds(1),
        level: 40,
    }];
    let a_year_before = vec![bss_fixtures::GaugeSample {
        at: window_start - chrono::Duration::days(365),
        level: 40,
    }];

    for (label, samples) in [
        ("one second", &a_second_before),
        ("one year", &a_year_before),
    ] {
        assert_eq!(
            held_level(samples, window_start, 0, step, 2),
            Some(40),
            "{label} before the window opens the level in granule 0"
        );
        assert_eq!(
            held_level(samples, window_start, 2, step, 2),
            Some(40),
            "{label} before the window is still held at the edge of max_hold"
        );
        assert_eq!(
            held_level(samples, window_start, 3, step, 2),
            None,
            "{label} before the window falls out one granule past max_hold — the \
             hold is measured from granule 0 in both cases, which is the floor"
        );
    }

    // The contrast that makes the floor visible: an *in-window* sample is aged
    // from the granule it actually fell in.
    let inside = vec![bss_fixtures::GaugeSample {
        at: window_start + chrono::Duration::seconds(i64::try_from(step * 3 + 1).expect("fits")),
        level: 40,
    }];
    assert_eq!(held_level(&inside, window_start, 5, step, 2), Some(40));
    assert_eq!(
        held_level(&inside, window_start, 6, step, 2),
        None,
        "three granules past the sample's own granule is out of a two-granule hold"
    );
}

/// The **granule accumulator** refuses a total it cannot hold, rather than
/// wrapping — `sum`'s discipline applied to the fold walk.
///
/// `integral` refuses on the same operands four `checked_*` sites over, and
/// `sum`'s own doc names precisely this hazard ("the band walk accumulates across
/// an unbounded band list, so each product fitting does not make the total fit").
/// The granule walk is the same shape over a granule count the window length
/// alone decides, and it was the one accumulation in the module still spelled
/// `+=`.
///
/// Two granules, each folding to `2^63`: every per-granule fold fits
/// and their sum is exactly `u64::MAX + 1`. Before the fix this panicked in a
/// debug build and wrapped to `0` in a release one — the reference implementation
/// answering a plausible `Q` no rating engine could reproduce.
#[test]
fn a_granule_fold_total_that_does_not_fit_is_refused_not_wrapped() {
    let window_start = instant(2026, 1, 8);
    // `2^63`: two of these sum to `u64::MAX + 1`, the smallest total that does
    // not fit. A shift rather than `u64::MAX / 2 + 1` because the lint set denies
    // integer division.
    let half = 1_u64 << 63;

    let snap = bss_fixtures::Snapshot {
        model_kind: bss_fixtures::ModelKind::PerUnit,
        charge_kind: bss_fixtures::ChargeKind::Usage,
        currency: "USD".to_owned(),
        bands: Vec::new(),
        amount_minor: None,
        package_size: None,
        package_price_minor: None,
        quantity_source: None,
        tier_aggregation_window: None,
        billing_granularity: None,
        proration_basis: None,
        meter: None,
        dimension_key: None,
        aggregation_function: Some(bss_fixtures::AggregationFunction::Peak),
        aggregation_granularity: Some(bss_fixtures::AggregationGranularity::Hour),
        max_hold_granules: Some(0),
        tier_qualification_window: None,
        included_allowance: None,
        reserved_rate_minor: None,
        reservation_flavor: None,
    };
    let rt = bss_fixtures::Runtime {
        window_start: Some(window_start),
        window_end: Some(window_start + chrono::Duration::seconds(7_200)),
        samples: vec![
            bss_fixtures::GaugeSample {
                at: window_start,
                level: half,
            },
            bss_fixtures::GaugeSample {
                at: window_start + chrono::Duration::seconds(3_600),
                level: half,
            },
        ],
        ..bss_fixtures::Runtime::default()
    };
    assert_eq!(
        fold(&snap, &rt),
        Err(EvalError::ArithmeticOverflow {
            what: "granule fold total"
        })
    );

    // The positive control, so the case cannot pass by refusing every fold: the
    // same two granules at an ordinary level fold to their sum.
    let ordinary = bss_fixtures::Runtime {
        samples: vec![
            bss_fixtures::GaugeSample {
                at: window_start,
                level: 40,
            },
            bss_fixtures::GaugeSample {
                at: window_start + chrono::Duration::seconds(3_600),
                level: 2,
            },
        ],
        ..rt
    };
    assert_eq!(fold(&snap, &ordinary), Ok(Evaluated::Fold { q: 42 }));
}

/// A window that is not a whole number of granules is refused, not floored.
///
/// `div_euclid` dropped the trailing partial granule, so a 90-minute window on an
/// hourly granularity folded its first hour and answered as if the remaining
/// thirty minutes had not happened. `Q` came back **under**-reported with no
/// error, and this is the reference implementation rating must reproduce.
///
/// No corpus case exercises the shape — every committed window is an exact
/// multiple — so the class had neither a case nor a comment stating the floor.
#[test]
fn a_window_that_is_not_whole_granules_is_refused_rather_than_floored() {
    let window_start = instant(2026, 1, 8);
    let snap = bss_fixtures::Snapshot {
        model_kind: bss_fixtures::ModelKind::PerUnit,
        charge_kind: bss_fixtures::ChargeKind::Usage,
        currency: "USD".to_owned(),
        bands: Vec::new(),
        amount_minor: None,
        package_size: None,
        package_price_minor: None,
        quantity_source: None,
        tier_aggregation_window: None,
        billing_granularity: None,
        proration_basis: None,
        meter: None,
        dimension_key: None,
        aggregation_function: Some(bss_fixtures::AggregationFunction::Peak),
        aggregation_granularity: Some(bss_fixtures::AggregationGranularity::Hour),
        max_hold_granules: Some(0),
        tier_qualification_window: None,
        included_allowance: None,
        reserved_rate_minor: None,
        reservation_flavor: None,
    };
    let samples: Vec<bss_fixtures::GaugeSample> = vec![
        bss_fixtures::GaugeSample {
            at: window_start,
            level: 40,
        },
        bss_fixtures::GaugeSample {
            at: window_start + chrono::Duration::seconds(3_600),
            level: 2,
        },
    ];
    // 90 minutes on an hourly granule: one whole granule and half of a second.
    let partial = bss_fixtures::Runtime {
        window_start: Some(window_start),
        window_end: Some(window_start + chrono::Duration::seconds(5_400)),
        samples,
        ..bss_fixtures::Runtime::default()
    };
    assert_eq!(
        fold(&snap, &partial),
        Err(EvalError::PartialGranuleWindow {
            span: 5_400,
            step: 3_600
        })
    );

    // The positive control, so the case cannot pass by refusing every fold: the
    // same samples over two whole granules fold to their sum.
    let whole = bss_fixtures::Runtime {
        window_end: Some(window_start + chrono::Duration::seconds(7_200)),
        ..partial
    };
    assert_eq!(fold(&snap, &whole), Ok(Evaluated::Fold { q: 42 }));
}

/// The `per_unit` arm's product is refused too, not only the ladder's.
///
/// `quantity` widens `Q` into the money domain "refusing rather than wrapping"
/// and `product` exists because every caller then multiplied it by a rate
/// unchecked. Three of the four call sites went through `product`; this arm was
/// still a bare `*`, so the discipline had two gaps rather than the one
/// `a_charge_that_does_not_fit_the_money_domain_is_refused_not_wrapped` closed.
#[test]
fn a_per_unit_charge_that_does_not_fit_the_money_domain_is_refused_not_wrapped() {
    let snap = bss_fixtures::Snapshot {
        model_kind: bss_fixtures::ModelKind::PerUnit,
        charge_kind: bss_fixtures::ChargeKind::Usage,
        currency: "USD".to_owned(),
        bands: Vec::new(),
        amount_minor: Some(1_000_000_000),
        package_size: None,
        package_price_minor: None,
        quantity_source: None,
        tier_aggregation_window: None,
        billing_granularity: None,
        proration_basis: None,
        meter: None,
        dimension_key: None,
        aggregation_function: None,
        aggregation_granularity: None,
        max_hold_granules: None,
        tier_qualification_window: None,
        included_allowance: None,
        reserved_rate_minor: None,
        reservation_flavor: None,
    };
    let rt = bss_fixtures::Runtime::default();
    let evaluate = |q: u64| {
        ReferenceOracle.evaluate(&EvalInput {
            snapshot: &snap,
            runtime: &rt,
            given: &bss_fixtures::Given {
                q,
                ..bss_fixtures::Given::default()
            },
        })
    };
    assert_eq!(
        evaluate(1_000_000_000_000),
        Err(EvalError::ArithmeticOverflow {
            what: "per-unit charge"
        })
    );
    // The positive control.
    assert_eq!(evaluate(3), Ok(Evaluated::Charge(3_000_000_000)));
}

/// The two combinations `evaluate` refuses rather than answers.
///
/// Its arms resolve `runtime.samples`, `reservation_flavor` and
/// `included_allowance`, and each one returns. An arm that answered without
/// looking at the fields the others own would drop them: samples beside a flavor
/// would fold and never price the reservation, and a flavor beside an allowance
/// would band its remainder over the authored ladder rather than the compiled
/// one. Both are plausible-looking numbers, which is the shape a reference
/// implementation must never produce — the corpus is the contract a second
/// evaluator has to reproduce. So each arm checks for the fields it does not own
/// and answers `UnrepresentableField`.
///
/// No corpus case can express either — `reserved/capacity-on-level.toml`, the case
/// named for the first combination, carries the level fields but no
/// `[runtime].samples` — so the refusal is pinned here until the semantics are
/// decided and a case can be authored.
#[test]
fn a_row_whose_fields_ask_two_questions_is_declined_rather_than_half_answered() {
    let base = bss_fixtures::Snapshot {
        model_kind: bss_fixtures::ModelKind::Graduated,
        charge_kind: bss_fixtures::ChargeKind::Usage,
        currency: "USD".into(),
        bands: two_bands(),
        amount_minor: None,
        package_size: None,
        package_price_minor: None,
        quantity_source: None,
        tier_aggregation_window: None,
        billing_granularity: None,
        proration_basis: None,
        meter: Some("storage.gb".into()),
        dimension_key: None,
        aggregation_function: Some(bss_fixtures::AggregationFunction::Peak),
        aggregation_granularity: Some(bss_fixtures::AggregationGranularity::Hour),
        max_hold_granules: Some(1),
        tier_qualification_window: None,
        included_allowance: None,
        reserved_rate_minor: Some(3),
        reservation_flavor: Some(bss_fixtures::ReservationFlavor::Capacity),
    };
    let oracle = ReferenceOracle;
    let given = bss_fixtures::Given::default();

    // Samples plus a flavor: `fold` would answer and never read the flavor.
    // A fixed hour, not a wall clock: this crate builds `chrono` without `clock`,
    // and a fold's answer must not depend on when the suite runs.
    let window_start =
        chrono::DateTime::from_timestamp(0, 0).expect("the epoch is a valid instant");
    let folding = bss_fixtures::Runtime {
        window_start: Some(window_start),
        window_end: Some(window_start + chrono::Duration::hours(1)),
        samples: vec![bss_fixtures::GaugeSample {
            at: window_start,
            level: 10,
        }],
        reserved_quantity: Some(500),
        covered_granules: Some(1),
    };
    let err = oracle
        .evaluate(&EvalInput {
            snapshot: &base,
            runtime: &folding,
            given: &given,
        })
        .expect_err("a reserved level row must decline rather than fold");
    assert!(
        matches!(
            err,
            EvalError::UnrepresentableField {
                field: "reservation_flavor",
                ..
            }
        ),
        "and it must name the field the fold would have dropped: {err}"
    );

    // A flavor plus an allowance: `reservation` would band the remainder over the
    // authored ladder and never compile the allowance in.
    let with_allowance = bss_fixtures::Snapshot {
        included_allowance: Some(bss_fixtures::IncludedAllowance {
            quantity: 100,
            rollover_policy: bss_fixtures::RolloverPolicy::None,
        }),
        reservation_flavor: Some(bss_fixtures::ReservationFlavor::Consumption),
        ..base
    };
    let reserving = bss_fixtures::Runtime {
        reserved_quantity: Some(500),
        covered_granules: Some(1),
        ..bss_fixtures::Runtime::default()
    };
    let err = oracle
        .evaluate(&EvalInput {
            snapshot: &with_allowance,
            runtime: &reserving,
            given: &bss_fixtures::Given {
                q: 900,
                ..bss_fixtures::Given::default()
            },
        })
        .expect_err("a reserved row carrying an allowance must decline rather than band");
    assert!(
        matches!(
            err,
            EvalError::UnrepresentableField {
                field: "included_allowance",
                ..
            }
        ),
        "and it must name the field the reservation would have dropped: {err}"
    );

    // Both flavors, because the guard refuses both and they drop the allowance by
    // different routes: `consumption` bands the remainder over the authored ladder,
    // `capacity` never reads the bands at all.
    let capacity_with_allowance = bss_fixtures::Snapshot {
        reservation_flavor: Some(bss_fixtures::ReservationFlavor::Capacity),
        ..with_allowance
    };
    let err = oracle
        .evaluate(&EvalInput {
            snapshot: &capacity_with_allowance,
            runtime: &reserving,
            given: &given,
        })
        .expect_err("a capacity reservation carrying an allowance must decline too");
    assert!(
        matches!(
            err,
            EvalError::UnrepresentableField {
                field: "included_allowance",
                ..
            }
        ),
        "and it must name the same field on the flavor that never reads the bands: {err}"
    );
}
