//! The gear's [`PublishValidator`]: what this catalog answers when the corpus
//! asks what publish does with an authored successor.
//!
//! Deliberately **not** part of the library. It compiles only into the
//! `regen_registry` example and the `corpus_publish` test, both of which build
//! with dev-dependencies, so `bss-fixtures-conformance` never becomes a
//! production edge of the gear — the corpus's stated invariant is that no
//! evaluator reaches a gear even transitively.
//!
//! ## Shape first, then the pair
//!
//! [`CatalogPublishValidator::validate`] runs the successor's **row-shape**
//! rules ([`price_row_rules`]) before the **supersession unit guard**
//! ([`supersession_rules`]), and reports the first violation of that order.
//!
//! The order is not a preference. A malformed row is malformed regardless of
//! what it supersedes: a `graduated` row with a closed top band prices a
//! quantity nobody can be billed for whether it lands on an occupied key or on
//! an empty one. The pair guard, by contrast, is a **comparison** — it only has
//! meaning once both sides are rows the catalog would have accepted on their
//! own. Asking "did this successor change the unit?" of a row that is not a
//! publishable row at all answers a question about a hypothetical.
//!
//! It is also what the gear does at publish time: the row-local pipeline is
//! registered once (D-21) and runs at save and again inside the publish commit;
//! the pair guard runs only where a predecessor exists.
//!
//! ## What "cannot be assessed" means
//!
//! [`EvalError`] is reserved for a snapshot the gear's row shape cannot hold —
//! a value outside a gear enum's vocabulary, or a field belonging to a slice
//! this gear has not built. A **rejection by the rules under test is a verdict**,
//! not an error: that is the case doing its job.
//!
//! `currency` is the one snapshot field read and then dropped without either.
//! It is a **scope-key axis**, not a row field ([`PriceRow`] carries no
//! currency, on purpose — pairing one into the amount would create a second
//! place to disagree about what a row is priced in), so no shape rule consumes
//! it and no rejection can turn on it.

use bss_fixtures::{
    AggregationFunction as CorpusAggregationFunction,
    AggregationGranularity as CorpusAggregationGranularity, Band, BandTop as CorpusBandTop,
    BillingGranularity as CorpusBillingGranularity, ChargeKind as CorpusChargeKind, Corpus,
    IncludedAllowance as CorpusIncludedAllowance, ModelKind as CorpusModelKind, ProrationBasis,
    PublishVerdict, ReservationFlavor, RolloverPolicy as CorpusRolloverPolicy, Snapshot,
    TierAggregationWindow as CorpusTierAggregationWindow,
};
use bss_fixtures_conformance::{EvalError, PublishReport, PublishValidator, run_publish_suite};

use bss_pricing::domain::money::{MinorAmount, RateMinor};
use bss_pricing::domain::price_row::{
    AggregationFunction, AggregationGranularity, BandTop, BillingGranularity, IncludedAllowance,
    PriceRow, QuantitySource, ReservationFlavor as GearReservationFlavor, RolloverPolicy,
    TierAggregationWindow, TierBand, TierQualificationWindow,
};
use bss_pricing::domain::rules::{SupersessionPair, price_row_rules, supersession_rules};
use bss_pricing::domain::scope_key::ChargeKind;

/// The pricing gear answering the corpus's publish cases.
pub struct CatalogPublishValidator;

impl PublishValidator for CatalogPublishValidator {
    /// # Errors
    ///
    /// [`EvalError::UnrepresentableField`] when either snapshot carries a field
    /// this gear's [`PriceRow`] cannot hold. A rejection by the rules is a
    /// [`PublishVerdict`], never an error.
    fn validate(
        &self,
        predecessor: &Snapshot,
        successor: &Snapshot,
    ) -> Result<PublishVerdict, EvalError> {
        let before = price_row(predecessor)?;
        let after = price_row(successor)?;

        let shape = price_row_rules().run(&after);
        if let Some(violation) = shape.violations.first() {
            return Ok(PublishVerdict::Rejected {
                error_code: violation.code.clone(),
            });
        }

        let pair = SupersessionPair::new(before, after);
        let guard = supersession_rules().run(&pair);
        if let Some(violation) = guard.violations.first() {
            return Ok(PublishVerdict::Rejected {
                error_code: violation.code.clone(),
            });
        }

        Ok(PublishVerdict::Accepted)
    }
}

/// Runs every publish case in `corpus` against this gear.
///
/// The single entry point the example and the test share, so the flag written
/// into `registry.toml` and the flag the test asserts come from one run.
#[must_use]
pub fn publish_report(corpus: &Corpus) -> PublishReport {
    run_publish_suite(&CatalogPublishValidator, corpus)
}

/// How a verdict reads in a report line.
#[must_use]
pub fn describe_verdict(verdict: &PublishVerdict) -> String {
    match verdict {
        PublishVerdict::Accepted => "accepted".to_owned(),
        PublishVerdict::Rejected { error_code } => format!("rejected({error_code})"),
    }
}

/// How an answered case reads in a report line, including a declined one.
#[must_use]
pub fn describe_answer(answer: &Result<PublishVerdict, EvalError>) -> String {
    match answer {
        Ok(verdict) => describe_verdict(verdict),
        Err(e) => format!("undecidable: {e}"),
    }
}

/// Projects a frozen corpus snapshot onto the gear's authored row shape, and
/// refuses the snapshot outright if it carries a field that shape cannot hold.
///
/// The two types are field-for-field compatible by design and are still separate
/// types: the corpus model sits behind the `corpus` feature the gear does not
/// enable, and the snapshot carries fields (`currency`, `proration_basis`, the
/// Slice-10 reservation pair) that are not Slice-3-owned.
fn price_row(snapshot: &Snapshot) -> Result<PriceRow, EvalError> {
    reject_unrepresentable(snapshot)?;
    slice3_row(snapshot)
}

/// The **Slice-3 part** of a snapshot, projected without the
/// unrepresentable-field gate.
///
/// Split out from [`price_row`] because the publish question and the row-shape
/// question are asked of different things. A publish verdict is a statement
/// about the whole authored row, so a snapshot carrying a field the gear cannot
/// represent has to be declined rather than judged. The row-shape rules, by
/// contrast, are Slice-3 rules over the Slice-3 fields, and they are perfectly
/// answerable about the Slice-3 part of a snapshot whose remaining fields belong
/// to a slice nobody has built — which is what
/// `tests/corpus_snapshot_shape.rs` asks of every snapshot in the corpus,
/// including the `proration` rows (`proration_basis`) and the `reserved` rows
/// (the Slice-10 reservation pair).
///
/// # Errors
///
/// [`EvalError::UnrepresentableField`] for a value outside a gear enum's
/// vocabulary, or a negative amount the money type refuses.
pub fn slice3_row(snapshot: &Snapshot) -> Result<PriceRow, EvalError> {
    Ok(PriceRow {
        charge_kind: charge_kind(snapshot.charge_kind),
        model_kind: Some(snapshot.model_kind),
        // **The corpus predates D-311 and states both prices in `amount_minor`**,
        // so this mapping is where a `per_unit` snapshot's whole-minor-unit price
        // becomes a rate. The corpus keeps its schema deliberately: what a fixture
        // asserts is charge arithmetic — `q x unit price` — which is unchanged by
        // where the catalog stores the multiplicand, and the file is shared with
        // Rating, which has no stake in this gear's column layout. Translating
        // here rather than editing 30-odd TOML files also keeps every fixture
        // meaning exactly what it meant.
        //
        // The conversion is exact and total: whole minor units are representable
        // in the rate scale by construction. It is the reverse direction — a
        // sub-minor rate the corpus cannot state — that has no counterpart, and
        // that is a gap in the corpus rather than in this mapping.
        amount_minor: match snapshot.model_kind {
            CorpusModelKind::PerUnit => None,
            _ => optional_amount(snapshot.amount_minor, "amount_minor")?,
        },
        unit_rate: match snapshot.model_kind {
            CorpusModelKind::PerUnit => Some(rate(snapshot.amount_minor, "amount_minor")?),
            _ => None,
        },
        bands: tier_bands(&snapshot.bands)?,
        package_size: snapshot.package_size,
        package_price_minor: optional_amount(snapshot.package_price_minor, "package_price_minor")?,
        quantity_source: quantity_source(snapshot.quantity_source.as_deref())?,
        // The corpus has no counterpart: `manual_quantity` is an authored
        // quantity, and the snapshot's `quantity_source` cases are all
        // externally supplied. A `manual` source would therefore be judged
        // without its quantity, which is a real rejection and not a mapping
        // fault.
        manual_quantity: None,
        meter: snapshot.meter.clone(),
        // `NOT NULL DEFAULT ''`: the empty string is the empty-tuple sentinel,
        // not an absent value, so an undimensioned row maps onto it rather than
        // onto a distinct `NULL`.
        dimension_key: snapshot.dimension_key.clone().unwrap_or_default(),
        billing_granularity: snapshot.billing_granularity.map(billing_granularity),
        tier_aggregation_window: snapshot
            .tier_aggregation_window
            .map(tier_aggregation_window),
        tier_qualification_window: tier_qualification_window(
            snapshot.tier_qualification_window.as_deref(),
        )?,
        aggregation_function: snapshot.aggregation_function.map(aggregation_function),
        aggregation_granularity: snapshot
            .aggregation_granularity
            .map(aggregation_granularity),
        max_hold_granules: snapshot.max_hold_granules,
        included_allowance: snapshot.included_allowance.map(included_allowance),
        // Slice 10's pair. The corpus and the gear spell the flavor with two
        // different enums for the reason the module doc gives -- the corpus model
        // sits behind a feature this gear does not enable -- so it is mapped
        // rather than passed through.
        // **The corpus still spells this in whole minor units and the gear no
        // longer does** (Z5-3, 2026-08-17). `reservedRate` is a rate — the oracle
        // multiplies it by a granule count — so the gear moved it to `RateMinor`
        // at the 10^-9 scale, and the corpus carries D-311's original defect one
        // field over: a corpus snapshot cannot express a reserved rate below one
        // minor unit at all. Converted here rather than silently truncated, and
        // the corpus-side widening is a joint fixtures/rating change because the
        // golden vectors are the contract between the two gears.
        reserved_rate: optional_rate(snapshot.reserved_rate_minor, "reserved_rate_minor")?,
        reservation_flavor: snapshot.reservation_flavor.map(reservation_flavor),
        // **The corpus has no counterpart for any of these four**, and that is a
        // statement about the corpus rather than a mapping shortcut. The typed
        // floors and the `discountRef` hook are enforced entirely downstream --
        // Subscriptions at order time, Tariffs/Rating at eligibility, Promotions
        // for the instrument -- so no joint fixture between this catalog and
        // Rating pins a number that depends on them, and `reject_unrepresentable`
        // has nothing to decline. If a case ever authors one, `Snapshot` gains
        // the field first and this mapping follows it.
        min_qty_purchase: None,
        min_qty_usage: None,
        min_qty_usage_fallback: None,
        discount_ref: None,
    })
}

/// The corpus flavor as the gear's.
const fn reservation_flavor(flavor: ReservationFlavor) -> GearReservationFlavor {
    match flavor {
        ReservationFlavor::Consumption => GearReservationFlavor::Consumption,
        ReservationFlavor::Capacity => GearReservationFlavor::Capacity,
    }
}

/// The `chargeKind` axis, **read** from the snapshot.
///
/// It used to be inferred here — "a row that names a `meter` is a usage row,
/// and a row that does not is recurring" — because the corpus did not carry the
/// axis. The inference is deleted rather than kept as a fallback: an axis a
/// subject guesses is an axis two subjects can guess differently, which is the
/// class of divergence the corpus exists to prevent, and a surviving fallback is
/// a second reading waiting for the first case that does not fit it. It also
/// left `inst-mk-chargekind` — the `kind x chargeKind` matrix — unstateable: a
/// `flat` usage row and a `graduated` recurring row are the two shapes the
/// matrix refuses, and under the inference neither could be written down.
///
/// Total, like the other corpus-to-gear enum maps: a fifth `chargeKind` on
/// either side cannot appear without this match being extended.
const fn charge_kind(kind: CorpusChargeKind) -> ChargeKind {
    match kind {
        CorpusChargeKind::Recurring => ChargeKind::Recurring,
        CorpusChargeKind::Usage => ChargeKind::Usage,
        CorpusChargeKind::OneTime => ChargeKind::OneTime,
        CorpusChargeKind::OneTimeSetup => ChargeKind::OneTimeSetup,
    }
}

/// The fields the gear's row shape has nowhere to put.
///
/// Not "fields the gear ignores": each of these carries commercial meaning, and
/// silently dropping one would let a case pass on a row that is not the row the
/// corpus described.
fn reject_unrepresentable(snapshot: &Snapshot) -> Result<(), EvalError> {
    // Slice 10's reservation pair **is** representable as of 2026-08-08: the two
    // fields are on `PriceRow` and `rules::reservation` judges them, so the
    // arms that declined every reserved snapshot here are gone and
    // `reserved/consumption-on-level-rejected` earns its evidence again.
    // The apportionment convention for a partial period. Frozen in the snapshot
    // and consumed by Rating; no Slice-3 row field holds it.
    if let Some(basis) = snapshot.proration_basis {
        return Err(EvalError::UnrepresentableField {
            field: "proration_basis",
            value: proration_basis_wire(basis).to_owned(),
        });
    }
    Ok(())
}

fn tier_bands(bands: &[Band]) -> Result<Vec<TierBand>, EvalError> {
    bands
        .iter()
        .map(|band| {
            Ok(TierBand {
                from_qty: band.from_qty,
                to_qty: match band.to_qty {
                    CorpusBandTop::Open => BandTop::Open,
                    CorpusBandTop::Closed(top) => BandTop::Closed(top),
                },
                // The corpus states band rates in whole minor units (D-311):
                // scaled here rather than re-authored, so the fixtures keep
                // meaning the prices they always meant.
                //
                // Through [`rate`], which is `RateMinor::from_minor_units` —
                // a `checked_mul` by `NANO_PER_MINOR_I64` — and not through a
                // literal `* 1_000_000_000`. That literal was an unchecked `i64`
                // multiply five lines from the checked helper the same file
                // already calls, so one file held two implementations of one
                // conversion and the corpus's band rates travelled the unchecked
                // one: a band at 2e10 minor units wrapped to a **positive**
                // 1.55e18 nano, past `from_nano_minor`'s negativity check, and
                // published a $15,532,559 rate for an authored $200,000,000 one
                // (Z5-7). It also wrote the scale out where `RATE_SUB_DECIMALS`
                // says the scale has exactly one place it can be changed (Z5-11).
                unit_price_rate: rate(
                    Some(amount(band.unit_amount_minor, "bands.unit_amount_minor")?.get()),
                    "bands.unit_amount_minor",
                )?,
            })
        })
        .collect()
}

/// A minor amount the gear's money type accepts.
///
/// The type refuses a negative, so a negative in the snapshot is a value the row
/// shape cannot hold rather than a rule the row breaks.
fn amount(units: i64, field: &'static str) -> Result<MinorAmount, EvalError> {
    MinorAmount::new(units).map_err(|_| EvalError::UnrepresentableField {
        field,
        value: units.to_string(),
    })
}

/// A **rate** from a snapshot column stated in whole minor units (D-311).
///
/// Absent is unrepresentable rather than merely missing: a `per_unit` fixture
/// with no price is a fixture that asserts a charge it never states, and the
/// publish rules would refuse the row for the same reason one step later.
fn rate(units: Option<i64>, field: &'static str) -> Result<RateMinor, EvalError> {
    let units = units.ok_or(EvalError::UnrepresentableField {
        field,
        value: "absent".to_owned(),
    })?;
    RateMinor::from_minor_units(units).map_err(|_| EvalError::UnrepresentableField {
        field,
        value: units.to_string(),
    })
}

/// A reserved rate from a snapshot column stated in whole minor units.
///
/// The optional twin of [`rate`], and the seam where the corpus and the gear now
/// disagree about scale: the gear moved `reservedRate` to `RateMinor` on
/// 2026-08-17 because it is a rate the oracle multiplies by a granule count,
/// while the corpus still spells it in whole minor units and therefore carries
/// D-311's original defect one field over — a snapshot cannot express a reserved
/// rate below one minor unit at all. Widening the corpus is a joint
/// fixtures/rating change, because the golden vectors are the contract between
/// the two gears, so the conversion lives here and the limitation is named
/// rather than hidden.
fn optional_rate(units: Option<i64>, field: &'static str) -> Result<Option<RateMinor>, EvalError> {
    units
        .map(|units| {
            RateMinor::from_minor_units(units).map_err(|_| EvalError::UnrepresentableField {
                field,
                value: units.to_string(),
            })
        })
        .transpose()
}

fn optional_amount(
    units: Option<i64>,
    field: &'static str,
) -> Result<Option<MinorAmount>, EvalError> {
    units.map(|units| amount(units, field)).transpose()
}

/// Both of these are **total**, and that is the whole of what changed when the
/// corpus started carrying the two enums instead of two strings.
///
/// They used to be fallible string lookups whose `other` arm reported an
/// `UnrepresentableField`. That arm was never reachable from a well-authored
/// corpus and it was reachable from a badly-authored one — which is how
/// `billing_period` and a `billingGranularity` of `per_unit` lived in five case
/// files: the gear declined the case, the decline was one line in a report, and
/// nothing in the corpus said the values did not exist. Now they fail to
/// **load**, so the two vocabularies cannot drift without the corpus itself
/// refusing to parse.
const fn billing_granularity(raw: CorpusBillingGranularity) -> BillingGranularity {
    match raw {
        CorpusBillingGranularity::PerSecond => BillingGranularity::PerSecond,
        CorpusBillingGranularity::PerMinute => BillingGranularity::PerMinute,
        CorpusBillingGranularity::PerHour => BillingGranularity::PerHour,
        CorpusBillingGranularity::PerDay => BillingGranularity::PerDay,
        CorpusBillingGranularity::WholeUnit => BillingGranularity::WholeUnit,
    }
}

const fn tier_aggregation_window(raw: CorpusTierAggregationWindow) -> TierAggregationWindow {
    match raw {
        CorpusTierAggregationWindow::CalendarMonth => TierAggregationWindow::CalendarMonth,
        CorpusTierAggregationWindow::InvoicePeriod => TierAggregationWindow::InvoicePeriod,
        CorpusTierAggregationWindow::SubscriptionLifetime => {
            TierAggregationWindow::SubscriptionLifetime
        }
        CorpusTierAggregationWindow::PerEvent => TierAggregationWindow::PerEvent,
    }
}

fn tier_qualification_window(
    raw: Option<&str>,
) -> Result<Option<TierQualificationWindow>, EvalError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mapped = match raw {
        "current" => TierQualificationWindow::Current,
        "trailing_period" => TierQualificationWindow::TrailingPeriod,
        other => {
            return Err(EvalError::UnrepresentableField {
                field: "tier_qualification_window",
                value: other.to_owned(),
            });
        }
    };
    Ok(Some(mapped))
}

fn quantity_source(raw: Option<&str>) -> Result<Option<QuantitySource>, EvalError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mapped = match raw {
        "subscription_seat_count" => QuantitySource::SubscriptionSeatCount,
        "manual" => QuantitySource::Manual,
        other => {
            return Err(EvalError::UnrepresentableField {
                field: "quantity_source",
                value: other.to_owned(),
            });
        }
    };
    Ok(Some(mapped))
}

const fn aggregation_function(function: CorpusAggregationFunction) -> AggregationFunction {
    match function {
        CorpusAggregationFunction::Sum => AggregationFunction::Sum,
        CorpusAggregationFunction::Peak => AggregationFunction::Peak,
        CorpusAggregationFunction::TimeWeighted => AggregationFunction::TimeWeighted,
    }
}

const fn aggregation_granularity(
    granularity: CorpusAggregationGranularity,
) -> AggregationGranularity {
    match granularity {
        CorpusAggregationGranularity::Hour => AggregationGranularity::Hour,
        CorpusAggregationGranularity::Day => AggregationGranularity::Day,
    }
}

const fn included_allowance(allowance: CorpusIncludedAllowance) -> IncludedAllowance {
    IncludedAllowance {
        quantity: allowance.quantity,
        rollover_policy: match allowance.rollover_policy {
            CorpusRolloverPolicy::None => RolloverPolicy::None,
            CorpusRolloverPolicy::Carry => RolloverPolicy::Carry,
        },
    }
}

const fn proration_basis_wire(basis: ProrationBasis) -> &'static str {
    match basis {
        ProrationBasis::CalendarDaysActual => "calendar_days_actual",
        ProrationBasis::CalendarDays30 => "calendar_days_30",
        ProrationBasis::BySecond => "by_second",
        ProrationBasis::WholeUnit => "whole_unit",
        ProrationBasis::None => "none",
    }
}
