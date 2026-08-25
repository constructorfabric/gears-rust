//! The reference implementation of the pricing `PRD.md` 17.2 formula matrix.
//!
//! This exists so "green" can be earned before the rating gear exists: it stands
//! in for Tariffs. When rating's evaluator lands it must reproduce the same
//! corpus; disagreement reddens the corpus rather than overriding either side.
//!
//! Kept deliberately small and readable - it is audited by reading, and it is
//! the only thing standing between the corpus and a shared misreading of 17.2.

use crate::traits::{CorpusEvaluator, EvalError, EvalInput, Evaluated};
use bss_fixtures::{
    AggregationFunction, Band, BandTop, Family, IncludedAllowance, ModelKind, ProrationBasis,
    ReservationFlavor, RolloverPolicy, Runtime, Snapshot,
};

pub struct ReferenceOracle;

impl CorpusEvaluator for ReferenceOracle {
    fn evaluate(&self, input: &EvalInput<'_>) -> Result<Evaluated, EvalError> {
        let snap = input.snapshot;

        // Gauge samples mean the case is asking for a granule fold.
        if !input.runtime.samples.is_empty() {
            // **A reserved level row asks two questions and this seam answers
            // one.** `fold` never reads `reservation_flavor`, so answering here
            // returns `Evaluated::Fold` and prices the reservation at nothing at
            // all. Refused rather than approximated, which is what this file does
            // everywhere it cannot honestly answer — `volume` refuses an uncovered
            // quantity, `package` a zero divisor, `integral` a non-integral fold.
            // The reference semantics for the combination are undecided (D-53 has
            // `consumption` on a non-`sum` row failing publish outright), and an
            // answer nobody can derive by hand is worse than no answer. **A case
            // authored for it goes red, not declined** — `Expect` has no decline
            // variant and `Outcome::passed` is false for every `Err`, so the
            // decline machinery is `PublishCase::declined_until`'s and does not
            // reach this suite. The corpus case is owed together with the
            // semantics, not ahead of them.
            if let Some(flavor) = snap.reservation_flavor {
                return Err(EvalError::UnrepresentableField {
                    field: "reservation_flavor",
                    value: format!("{flavor:?}, on a row folding gauge samples"),
                });
            }
            return fold(snap, input.runtime);
        }

        // A period in the given means the case is asking about apportionment,
        // not price. The two questions never mix in one assertion.
        if input.given.period_start.is_some() {
            let basis = snap
                .proration_basis
                .ok_or(EvalError::MissingField("proration_basis"))?;
            return prorate(basis, input.given);
        }

        // A reservation reshapes what the bands see, so it resolves before them.
        if let Some(flavor) = snap.reservation_flavor {
            // **The allowance would be dropped in silence.** On `consumption`,
            // `reservation` bands the remainder over `snap.bands` — the *authored*
            // ladder — so a row carrying both prices its remainder over a band set
            // the allowance was never compiled into, which is the reading
            // `compile_allowance`'s own doc says diverges from the design set's the
            // moment the ladder is not flat. On `capacity` the bands are never read
            // at all, so the allowance is dropped whole. Which of the two compiles
            // first is a semantics decision the design set has not taken, and both
            // flavors are refused rather than one guessed at.
            if let Some(allowance) = snap.included_allowance {
                return Err(EvalError::UnrepresentableField {
                    field: "included_allowance",
                    value: format!("{allowance:?}, on a row carrying a reservation flavor"),
                });
            }
            return reservation(snap, input.runtime, input.given.q, flavor);
        }

        let q = input.given.q;

        // An allowance is a *compile*, not a deduction, and it resolves before
        // the bands because it is the band set the bands are then walked over.
        // See `compile_allowance`.
        if let Some(allowance) = snap.included_allowance {
            let bands = compile_allowance(snap, allowance)?;
            return Ok(Evaluated::Charge(graduated(&bands, q)?));
        }

        let charge = match snap.model_kind {
            ModelKind::Graduated => graduated(&snap.bands, q)?,
            ModelKind::Volume => volume(&snap.bands, q)?,
            ModelKind::Package => {
                let size = snap
                    .package_size
                    .ok_or(EvalError::MissingField("package_size"))?;
                let price = snap
                    .package_price_minor
                    .ok_or(EvalError::MissingField("package_price_minor"))?;
                package(size, price, q)?
            }
            ModelKind::PerUnit => {
                let unit = snap
                    .amount_minor
                    .ok_or(EvalError::MissingField("amount_minor"))?;
                // Through `product`, not `*`: `quantity` refuses an over-range
                // `Q` and then this multiplied it by an authored rate unchecked,
                // which is the one operator `product`'s own doc says the refusal
                // was left short of. A rate inside `i64` times a quantity inside
                // `i64` is not.
                product(quantity(q)?, unit, "per-unit charge")?
            }
            // The quantity is deliberately unread: a flat row is owed in full
            // whatever was consumed. Multiplying here would silently turn it
            // into `per_unit`.
            ModelKind::Flat => snap
                .amount_minor
                .ok_or(EvalError::MissingField("amount_minor"))?,
        };

        Ok(Evaluated::Charge(charge))
    }

    fn supported_families(&self) -> Vec<Family> {
        vec![
            Family::TierBoundary,
            Family::Package,
            Family::PerUnit,
            Family::Flat,
            Family::Proration,
            Family::SupersessionContinuity,
            Family::LevelAggregation,
            Family::Reserved,
        ]
    }
}

/// Widens a quantity into the money domain, refusing rather than wrapping.
fn quantity(q: u64) -> Result<i64, EvalError> {
    i64::try_from(q).map_err(|_| EvalError::QuantityOutOfRange(q))
}

/// `a * b`, refusing rather than wrapping — [`quantity`]'s discipline carried
/// through to the operation it exists to feed.
///
/// [`quantity`] refused an over-range quantity and every caller then multiplied
/// it by a rate unchecked, which put the refusal one operator short of the thing
/// that could actually overflow: a quantity inside `i64` times a rate inside
/// `i64` is not.
fn product(a: i64, b: i64, what: &'static str) -> Result<i64, EvalError> {
    a.checked_mul(b)
        .ok_or(EvalError::ArithmeticOverflow { what })
}

/// `a + b`, refusing rather than wrapping. The band walk accumulates across an
/// unbounded band list, so each product fitting does not make the total fit.
fn sum(a: i64, b: i64, what: &'static str) -> Result<i64, EvalError> {
    a.checked_add(b)
        .ok_or(EvalError::ArithmeticOverflow { what })
}

/// `includedAllowance` compiled into a band ladder — `inst-ac-band`, D-43/D-45.
///
/// **The allowance is not a subtraction from `Q`.** It compiles, and the two
/// readings diverge the moment the ladder is not flat: subtracting `N` from `Q`
/// and walking the *authored* ladder would price unit `N+1` at the first band's
/// rate **and** count it against the first band's width, so a `[0, 1000) @ 5`
/// row with `N = 100` would charge `1000 × 5` for `Q = 1100` where the compile
/// charges `1000 × 5 + 100 × <second rate>`. The design set's rule is explicit
/// about which one it is, and this is the artifact where a shared misreading
/// between this gear and Rating would otherwise ship silently:
///
/// * **tiered (`graduated`)** — prepend `[0, N) @ $0` and offset every authored
///   bound by `+N`, so an authored `[0, X)` becomes `[N, N+X)`. The ladder keeps
///   its shape and slides up by the free quantity.
/// * **untiered (`per_unit`)** — synthesize `[0, N) @ $0` and `[N, open) @ rate`.
///
/// Every other kind, and `rolloverPolicy = carry`, are refused rather than
/// approximated, matching the gear's publish gate exactly: `volume` under
/// Variant A would express a **cliff** rather than an allowance (D-59), a
/// `package` row has no band set to compile into, a `flat` row has no `Q`, and
/// the carry compile is a plan-scoped grant row this evaluator has no notion of.
/// A refusal here is `UnrepresentableField`, which reddens the case rather than
/// answering it: the evaluation runner has no decline: `Outcome::passed` is false
/// for every `Err` and `Report::failures` filters on nothing else. Declining is
/// `PublishCase::declined_until`'s, one suite over.
///
/// # Errors
///
/// [`EvalError::MissingField`] for a `per_unit` row with no `amount_minor`;
/// [`EvalError::UnrepresentableField`] for a kind or policy the compile does not
/// cover; [`EvalError::ArithmeticOverflow`] if an offset bound leaves `u64`.
fn compile_allowance(
    snap: &Snapshot,
    allowance: IncludedAllowance,
) -> Result<Vec<Band>, EvalError> {
    if allowance.rollover_policy != RolloverPolicy::None {
        return Err(EvalError::UnrepresentableField {
            field: "included_allowance.rollover_policy",
            value: format!("{:?}", allowance.rollover_policy),
        });
    }
    let n = allowance.quantity;
    if n == 0 {
        return Err(EvalError::UnrepresentableField {
            field: "included_allowance.quantity",
            value: "0".to_owned(),
        });
    }

    let offset = |bound: u64| -> Result<u64, EvalError> {
        bound.checked_add(n).ok_or(EvalError::ArithmeticOverflow {
            what: "allowance band offset",
        })
    };

    let free = Band {
        from_qty: 0,
        to_qty: BandTop::Closed(n),
        unit_amount_minor: 0,
    };

    match snap.model_kind {
        ModelKind::Graduated => {
            let mut out = vec![free];
            for b in &snap.bands {
                out.push(Band {
                    from_qty: offset(b.from_qty)?,
                    to_qty: match b.to_qty {
                        BandTop::Open => BandTop::Open,
                        BandTop::Closed(t) => BandTop::Closed(offset(t)?),
                    },
                    unit_amount_minor: b.unit_amount_minor,
                });
            }
            Ok(out)
        }
        ModelKind::PerUnit => {
            let rate = snap
                .amount_minor
                .ok_or(EvalError::MissingField("amount_minor"))?;
            Ok(vec![
                free,
                Band {
                    from_qty: n,
                    to_qty: BandTop::Open,
                    unit_amount_minor: rate,
                },
            ])
        }
        other => Err(EvalError::UnrepresentableField {
            field: "included_allowance",
            value: format!("{other:?}"),
        }),
    }
}

/// Marginal: each band prices only the units falling inside it.
///
/// # Errors
///
/// Returns [`EvalError::QuantityOutOfRange`] if a band's unit count exceeds the
/// money domain, or [`EvalError::ArithmeticOverflow`] if a band's charge or the
/// running total does not.
pub(crate) fn graduated(bands: &[Band], q: u64) -> Result<i64, EvalError> {
    let mut total: i64 = 0;
    for b in bands {
        let top = match b.to_qty {
            BandTop::Open => q,
            BandTop::Closed(t) => t.min(q),
        };
        if top <= b.from_qty {
            continue;
        }
        let band_charge = product(
            quantity(top - b.from_qty)?,
            b.unit_amount_minor,
            "graduated band charge",
        )?;
        total = sum(total, band_charge, "graduated band charge")?;
    }
    Ok(total)
}

/// Variant A: the band holding `q` sets one rate for the whole quantity.
/// Variant B (a per-tier flat fee) is not authorable in the catalog.
///
/// # Errors
///
/// Returns [`EvalError::NoBandCoversQuantity`] if no band covers `q`, rather
/// than inventing a rate; [`EvalError::ArithmeticOverflow`] if the charge does
/// not fit.
pub(crate) fn volume(bands: &[Band], q: u64) -> Result<i64, EvalError> {
    let band = bands
        .iter()
        .find(|b| {
            q >= b.from_qty
                && match b.to_qty {
                    BandTop::Open => true,
                    BandTop::Closed(t) => q < t,
                }
        })
        .ok_or(EvalError::NoBandCoversQuantity(q))?;

    product(quantity(q)?, band.unit_amount_minor, "volume charge")
}

/// Repeating block: `blocks = ceil(used / packageSize)`, charge = blocks x price.
/// Distinct from Volume Variant B (a per-tier flat fee), which is not authorable.
///
/// # Errors
///
/// Returns [`EvalError::ZeroPackageSize`] for a zero size, which would otherwise
/// panic in `div_ceil`; [`EvalError::ArithmeticOverflow`] if the charge does not
/// fit.
pub(crate) fn package(size: u64, price_minor: i64, q: u64) -> Result<i64, EvalError> {
    if size == 0 {
        return Err(EvalError::ZeroPackageSize);
    }
    product(quantity(q.div_ceil(size))?, price_minor, "package charge")
}

/// `Q` = Σ granule folds (D-44 / rating T-D-17).
///
/// The window is cut into granules of the row's `aggregation_granularity`.
/// `peak` folds each granule to its maximum sample; `time_weighted` folds it to
/// the step-integral of the level, with the last level held across granule
/// boundaries for up to `max_hold_granules`. Beyond the hold the level reads 0 —
/// deliberately the customer-favourable floor, and provisional, because a
/// backfilled sample re-folds the affected granules.
///
/// The integral accumulates in level-seconds and converts to the billable unit
/// only when the division is exact. Rounding is Billing's, not this seam's, so a
/// fold that does not land on a whole unit is refused rather than rounded.
///
/// # Errors
///
/// Returns [`EvalError`] if the window or the level triple is absent, or if a
/// fold does not divide exactly into the billable unit.
fn fold(snap: &Snapshot, rt: &Runtime) -> Result<Evaluated, EvalError> {
    let granularity = snap
        .aggregation_granularity
        .ok_or(EvalError::MissingField("aggregation_granularity"))?;
    let max_hold = snap
        .max_hold_granules
        .ok_or(EvalError::MissingField("max_hold_granules"))?;
    let function = snap
        .aggregation_function
        .ok_or(EvalError::MissingField("aggregation_function"))?;
    let window_start = rt
        .window_start
        .ok_or(EvalError::MissingGiven("window_start"))?;
    let window_end = rt.window_end.ok_or(EvalError::MissingGiven("window_end"))?;

    if window_end <= window_start {
        return Err(EvalError::DegeneratePeriod);
    }

    let step = granularity.seconds();
    let span = seconds(window_start, window_end);
    // **A window that is not a whole number of granules is refused, not floored.**
    // `div_euclid` dropped the trailing partial granule, so a 90-minute window on
    // an hourly granularity folded 60 minutes and under-reported `Q` — silently,
    // and this is the reference implementation rating must reproduce. It is the
    // same inexactness `integral` refuses one screen down with `NonIntegralFold`,
    // for the same stated reason: rounding is Billing's, not this seam's.
    if !span.is_multiple_of(step) {
        return Err(EvalError::PartialGranuleWindow { span, step });
    }
    let granule_count = span.div_euclid(step);

    let mut samples = rt.samples.clone();
    samples.sort_by_key(|s| s.at);

    let mut total: u64 = 0;
    for g in 0..granule_count {
        // `unwrap_or(0)` here turned an out-of-range granule offset into **zero**
        // — every granule then starting at `window_start`, the fold re-reading the
        // same samples for each of them, and a plausible-looking number coming out
        // instead of an error. Neither conversion can fail today, and the reason
        // is worth stating rather than relying on: `g < granule_count = span /
        // step`, so `g * step < span`, and `span` came from `num_seconds()` and is
        // therefore already inside `i64`. The point is that when the premise
        // changes the answer is a refusal and not a wrong fold.
        let g_offset = granule_offset(g, step)?;
        let g_start = window_start + chrono::Duration::seconds(g_offset);
        let g_end = g_start
            + chrono::Duration::seconds(i64::try_from(step).map_err(|_| {
                EvalError::ArithmeticOverflow {
                    what: "granule length",
                }
            })?);

        let in_granule: Vec<_> = samples
            .iter()
            .filter(|s| s.at >= g_start && s.at < g_end)
            .collect();

        // The level carried in from earlier, if the hold still reaches here.
        let carried = held_level(&samples, window_start, g, step, max_hold);

        let folded = match function {
            // The peak of the *level* over the granule, not of the samples in
            // it. The level carried in from an earlier sample was really the
            // level until the first in-granule observation, so it competes.
            //
            // The doc sentence "peak = max sample in the granule" is shorthand.
            // Read literally it is gameable: a level held at 100 all hour folds
            // to 100 while unsampled, but to 1 the moment a single sample of 1
            // arrives - adding an observation would cut the bill a hundredfold.
            // It would also split `peak` from `time_weighted`, which integrates
            // the same level and does honour the hold.
            AggregationFunction::Peak => in_granule
                .iter()
                .map(|s| s.level)
                .chain(carried)
                .max()
                .unwrap_or(0),
            AggregationFunction::TimeWeighted => {
                integral(&in_granule, carried, g_start, g_end, step)?
            }
            // `sum` is not a fold; a sum row never reaches here.
            AggregationFunction::Sum => {
                return Err(EvalError::SumIsNotAFold);
            }
        };
        // Checked, for `sum`'s reason one module section up: the walk accumulates
        // across an unbounded granule count, so each granule's fold fitting does
        // not make the total fit. `integral` below already refuses rather than
        // wraps on the same operands, and this was the one accumulation in the
        // module that did not.
        total = total
            .checked_add(folded)
            .ok_or(EvalError::ArithmeticOverflow {
                what: "granule fold total",
            })?;
    }

    Ok(Evaluated::Fold { q: total })
}

/// The level held into a granule from the most recent earlier sample, if that
/// sample is within `max_hold` granules. Beyond the hold the level reads 0.
///
/// # A sample taken before the window sits in granule 0, however far before
///
/// [`seconds`] floors at zero, so `seconds(window_start, last.at)` is `0` for any
/// `last.at < window_start` and `last_index` is therefore `0` — a level observed
/// one second before the window and one observed a year before it are the same
/// distance from every granule, and both are held wherever `g_index <= max_hold`.
///
/// **This is a floor, not a measurement, and it is stated rather than left inside
/// the helper** (Z5-15). It is defensible for a step function — the level really
/// did persist, and a gauge sample is an assertion about every instant after it
/// until the next one — and it does mean `max_hold_granules` does not bound
/// staleness on the one side where staleness is unbounded: an opening level can be
/// arbitrarily old and is still carried into the window's first granules.
///
/// Whether a pre-window sample should instead be aged by its true distance is a
/// question about the **reference semantics** that rating must reproduce, so it
/// belongs to the corpus and not to this function — and since 2026-08-18 it is
/// there: `level-aggregation/pre-window-sample-floor` carries a sample a **year**
/// before the window and pins `Q = 110` against the 10 an aged reading would
/// give. A year rather than a minute on purpose: a minute early is
/// indistinguishable from a rounding question. `oracle_tests::
/// a_sample_before_the_window_opens_the_level_and_is_never_aged_out` still pins
/// the behaviour here, but it is one implementation talking to itself; the corpus
/// case is the half rating has to agree with.
fn held_level(
    samples: &[bss_fixtures::GaugeSample],
    window_start: chrono::DateTime<chrono::Utc>,
    g_index: u64,
    step: u64,
    max_hold: u64,
) -> Option<u64> {
    let g_start = window_start + chrono::Duration::seconds(granule_offset(g_index, step).ok()?);
    let last = samples.iter().rfind(|s| s.at < g_start)?;
    // Distance in *granules*, not seconds: a sample late in the previous granule
    // is one granule away, however few seconds separate it from the boundary.
    let last_index = seconds(window_start, last.at).div_euclid(step);
    if g_index.saturating_sub(last_index) <= max_hold {
        Some(last.level)
    } else {
        None
    }
}

/// Step-integral of the level over one granule, in the billable unit.
fn integral(
    in_granule: &[&bss_fixtures::GaugeSample],
    carried: Option<u64>,
    g_start: chrono::DateTime<chrono::Utc>,
    g_end: chrono::DateTime<chrono::Utc>,
    step: u64,
) -> Result<u64, EvalError> {
    let mut level_seconds: u64 = 0;
    let mut cursor = g_start;
    let mut level = carried.unwrap_or(0);

    for s in in_granule {
        level_seconds = level_seconds
            .checked_add(level.checked_mul(seconds(cursor, s.at)).ok_or(
                EvalError::ArithmeticOverflow {
                    what: "level-seconds",
                },
            )?)
            .ok_or(EvalError::ArithmeticOverflow {
                what: "level-seconds",
            })?;
        cursor = s.at;
        level = s.level;
    }
    level_seconds = level_seconds
        .checked_add(level.checked_mul(seconds(cursor, g_end)).ok_or(
            EvalError::ArithmeticOverflow {
                what: "level-seconds",
            },
        )?)
        .ok_or(EvalError::ArithmeticOverflow {
            what: "level-seconds",
        })?;

    if level_seconds.is_multiple_of(step) {
        Ok(level_seconds.div_euclid(step))
    } else {
        Err(EvalError::NonIntegralFold {
            level_seconds,
            step,
        })
    }
}

/// Reservation pricing (`inst-rv-tier-q`, `inst-rv-level`, D-53, D-139).
///
/// `capacity` bills the allocation whatever the usage and never touches `Q`;
/// under D-139 the charge accrues per covered granule, because `reservedRate`
/// is denominated in the row's billable unit and is therefore money per
/// granule rather than a period charge.
///
/// `consumption` prices the matched quantity at the reserved rate and bands the
/// **remainder from zero** — the matched quantity leaves the tier counter
/// entirely rather than offsetting it.
///
/// # Errors
///
/// Returns [`EvalError`] when the rate, the allocation or the coverage is
/// absent.
fn reservation(
    snap: &Snapshot,
    rt: &Runtime,
    q: u64,
    flavor: ReservationFlavor,
) -> Result<Evaluated, EvalError> {
    let rate = snap
        .reserved_rate_minor
        .ok_or(EvalError::MissingField("reserved_rate_minor"))?;
    let reserved = rt
        .reserved_quantity
        .ok_or(EvalError::MissingGiven("reserved_quantity"))?;

    let charge = match flavor {
        ReservationFlavor::Capacity => {
            let granules = rt
                .covered_granules
                .ok_or(EvalError::MissingGiven("covered_granules"))?;
            // The quantity is deliberately unread: capacity is never reduced by
            // absent usage.
            let per_granule = product(quantity(reserved)?, rate, "reserved capacity charge")?;
            product(per_granule, quantity(granules)?, "reserved capacity charge")?
        }
        ReservationFlavor::Consumption => {
            let matched = q.min(reserved);
            let remainder = q - matched;
            let matched_charge = product(quantity(matched)?, rate, "reserved consumption charge")?;
            sum(
                matched_charge,
                graduated(&snap.bands, remainder)?,
                "reserved consumption charge",
            )?
        }
    };

    Ok(Evaluated::Charge(charge))
}

/// The offset of granule `g` from the window start, in seconds.
///
/// One spelling for the two sites that need it, because they had drifted: the
/// fold loop wrote `unwrap_or(0)` and `held_level`, five lines below, wrote
/// `.ok()?`. The first is what makes an out-of-range offset produce a wrong
/// answer instead of no answer.
fn granule_offset(g: u64, step: u64) -> Result<i64, EvalError> {
    g.checked_mul(step)
        .and_then(|offset| i64::try_from(offset).ok())
        .ok_or(EvalError::ArithmeticOverflow {
            what: "granule offset",
        })
}

const SECONDS_PER_DAY: u64 = 86_400;

/// Whole seconds between two instants; never negative.
fn seconds(a: chrono::DateTime<chrono::Utc>, b: chrono::DateTime<chrono::Utc>) -> u64 {
    u64::try_from((b - a).num_seconds().max(0)).unwrap_or(0)
}

/// The chargeable share of a period, as an exact integer ratio.
///
/// Value glosses are the pricing glossary's, verbatim: `calendar_days_30` is a
/// fixed 30-day month with the day count capped at 30; `whole_unit` performs no
/// sub-period proration; `none` means no proration at all — full-period charge.
///
/// Deliberately returns no money. Rating emits prorated components at full
/// intermediate precision and never rounds; Billing rounds. A prorated minor
/// amount does not exist at this seam.
///
/// # Errors
///
/// Returns [`EvalError::MissingGiven`] if the period is not fully specified, or
/// [`EvalError::DegeneratePeriod`] if it is empty or inverted.
fn prorate(basis: ProrationBasis, given: &bss_fixtures::Given) -> Result<Evaluated, EvalError> {
    let period_start = given
        .period_start
        .ok_or(EvalError::MissingGiven("period_start"))?;
    let period_end = given
        .period_end
        .ok_or(EvalError::MissingGiven("period_end"))?;
    // The chargeable stretch defaults to the whole period.
    let from = given.from.unwrap_or(period_start);
    let to = given.to.unwrap_or(period_end);

    if period_end <= period_start || to < from {
        return Err(EvalError::DegeneratePeriod);
    }
    // **The stretch must be inside the period it is apportioned over** (Z5-8).
    // Without this, `calendar_days_actual` and `by_second` both return a factor
    // above 1 — a 59-day stretch over a 31-day period is 1.90, i.e. 190% of a full
    // period — while `calendar_days_30` alone defended itself, by clamping, for
    // the stated reason in its own arm below. One of three arms guarding against a
    // ratio above 1 is the sibling-outlier shape, not a design.
    //
    // Refused rather than clamped, following what this file does everywhere else
    // it is handed an input it cannot honestly answer: `volume` refuses rather
    // than inventing a rate, `package` refuses a zero divisor, `integral` refuses
    // a fold that does not divide. A clamp would answer 1.00 for a `Given` that is
    // a data error, and this is the reference a second implementation is measured
    // against.
    //
    // **The corpus cannot pin this refusal today, and the reason is the model's
    // shape rather than an omission** (2026-08-18, asked for and declined).
    // `Expect` is `Charge | Units | Fold` — three *values*. An evaluation case can
    // assert what something costs; it has no way to assert that a subject refuses,
    // which is the publish half's `expect = { publish = "rejected", error_code =
    // ... }` and has no evaluation twin. So a case for a stretch outside its
    // period would need a fourth `Expect` variant, a runner arm that compares an
    // `EvalError` rather than an `Evaluated`, and a decision about which errors
    // are part of the joint contract and which are one implementation's internals
    // — all of it a change to the artifact both gears deserialize, not a new case
    // in it. That is a design question for the corpus owner, and rating's
    // evaluator arriving is when it has to be answered. Until then the behaviour
    // is pinned in `oracle_tests`, which is one implementation talking to itself,
    // and this comment is the record of what that does not cover.
    if from < period_start || to > period_end {
        return Err(EvalError::StretchOutsidePeriod);
    }

    let days = |a: chrono::DateTime<chrono::Utc>, b: chrono::DateTime<chrono::Utc>| -> u64 {
        seconds(a, b).div_euclid(SECONDS_PER_DAY)
    };

    Ok(match basis {
        ProrationBasis::CalendarDaysActual => Evaluated::Units {
            charged: days(from, to),
            in_basis: days(period_start, period_end),
        },
        // Fixed 30-day month; the day count is capped at 30 so a 31-day month
        // never bills 31/30 of itself.
        ProrationBasis::CalendarDays30 => Evaluated::Units {
            charged: days(from, to).min(30),
            in_basis: 30,
        },
        ProrationBasis::BySecond => Evaluated::Units {
            charged: seconds(from, to),
            in_basis: seconds(period_start, period_end),
        },
        // Neither apportions: the whole period is charged whatever the stretch.
        ProrationBasis::WholeUnit | ProrationBasis::None => Evaluated::Units {
            charged: 1,
            in_basis: 1,
        },
    })
}

#[cfg(test)]
#[path = "oracle_tests.rs"]
mod oracle_tests;
