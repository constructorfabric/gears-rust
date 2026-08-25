//! The corpus case model.
//!
//! `Snapshot` carries only fields the pricing design set marks frozen in
//! `pricingSnapshotRef`; `Runtime` carries what the consumer supplies at
//! evaluation time. Both deny unknown fields, so the ownership boundary between
//! the gears is checked at load time rather than asserted in prose — a value the
//! documents place outside the snapshot (D-60's per-subscription trailing lock,
//! for one) simply fails to parse in `[snapshot]`.

pub use crate::kinds::ModelKind;
pub use crate::variant::Variant;
use serde::Deserialize;
use serde::de::{self, Deserializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    TierBoundary,
    Proration,
    Package,
    PerUnit,
    Flat,
    Reserved,
    SupersessionContinuity,
    LevelAggregation,
    TrailingTier,
}

impl Family {
    /// Every family the corpus knows. Used to report the ones an evaluator
    /// declines, so an unbuilt family can never read as green.
    pub const ALL: [Self; 9] = [
        Self::TierBoundary,
        Self::Proration,
        Self::Package,
        Self::PerUnit,
        Self::Flat,
        Self::Reserved,
        Self::SupersessionContinuity,
        Self::LevelAggregation,
        Self::TrailingTier,
    ];

    /// The registry [`Variant`] this family's fixtures are registered under.
    ///
    /// **The families are the variants** (§6). Four families are the four
    /// `modelKind` fixtures; three are the cross-cutting scenario fixtures the
    /// design set names as variants in their own right. `None` means the family
    /// gates no publish at all, and there are exactly two:
    ///
    /// - `proration` is AC #61, a field-consumption contract shared with
    ///   Subscriptions and Tariffs. It gates nothing **deliberately**, which is
    ///   what [`crate::corpus::GateRole::Conformance`] records.
    /// - `trailing-tier` is Slice 10's `inst-tt-fixture` (D-40). It is a
    ///   `FixtureGate` variant in the design set and is deliberately **not** one
    ///   here: the family carries no case and no `_family.toml`, so it could
    ///   register no fixture and would shut the gate permanently for every
    ///   `tierQualificationWindow = trailing_period` row. It stays declined,
    ///   never green and never absent, and the variant lands with the slice.
    #[must_use]
    pub const fn variant(self) -> Option<Variant> {
        match self {
            Self::TierBoundary | Self::Package | Self::PerUnit | Self::Flat => {
                Some(Variant::ModelKind)
            }
            Self::LevelAggregation => Some(Variant::LevelAggregation),
            Self::SupersessionContinuity => Some(Variant::SupersessionContinuity),
            Self::Reserved => Some(Variant::Reserved),
            Self::Proration | Self::TrailingTier => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// Prices something, or apportions a period.
    Evaluation,
    /// Asserts what publish does with an authored change.
    Publish,
}

/// The upper bound of a tier band. The top band is always open (D-17): "price
/// undefined above X" is never the commercial intent, so any quantity stays
/// rateable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandTop {
    Open,
    Closed(u64),
}

impl<'de> Deserialize<'de> for BandTop {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u64),
            Word(String),
        }

        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(BandTop::Closed(n)),
            Raw::Word(w) if w == "open" => Ok(BandTop::Open),
            Raw::Word(w) => Err(de::Error::custom(format!(
                "band top must be an integer or \"open\", got {w:?}"
            ))),
        }
    }
}

/// A half-open quantity band `[from_qty, to_qty)` with a unit price.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Band {
    pub from_qty: u64,
    pub to_qty: BandTop,
    pub unit_amount_minor: i64,
}

/// The canonical apportionment convention for a partial period (`PRD.md` 1.4).
///
/// Owned by the pricing gear and adopted **verbatim** by Tariffs and
/// Subscriptions, with the CI gate `pricing.contracts.enum_drift` blocking
/// drift. Pinned here in code so the corpus itself carries the enum: a value
/// added or renamed on one side fails to deserialise on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProrationBasis {
    /// Actual calendar days in the period.
    CalendarDaysActual,
    /// Fixed 30-day month, day count capped at 30.
    ///
    /// The wire name is pinned explicitly: serde's `snake_case` rule would emit
    /// `calendar_days30`, dropping the separator before the digit. The enum is
    /// adopted **verbatim** across three gears under the CI gate
    /// `pricing.contracts.enum_drift`, so the spelling is part of the contract.
    #[serde(rename = "calendar_days_30")]
    CalendarDays30,
    BySecond,
    /// No sub-period proration.
    WholeUnit,
    /// No proration at all: full-period charge, no partial credit.
    None,
}

/// When the tier counter `Q` resets (`inst-tb-window`; on a `package` row the
/// window `used` accumulates over before block round-up, `inst-pk-window` /
/// D-58).
///
/// Pinned here in code for the same reason as [`ProrationBasis`]: the corpus
/// itself carries the enum, so a window the design set does not define fails to
/// **load** instead of riding through an `Option<String>` into a case nobody can
/// evaluate. `billing_period` did exactly that — a plausible synonym of
/// `invoice_period` that four cases carried and no document defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TierAggregationWindow {
    CalendarMonth,
    /// The subscription's invoice period.
    InvoicePeriod,
    /// Never resets: the counter runs for the life of the subscription.
    SubscriptionLifetime,
    /// Resets every event, so `Q` is one event's quantity.
    PerEvent,
}

/// The billable unit a usage row's quantity is quantized into
/// (`inst-tb-units`), and therefore the unit its band bounds are counted in.
///
/// Pinned in code alongside [`TierAggregationWindow`], and for the same reason:
/// `per_unit` — the name of a `modelKind`, not of a granularity — sat in the
/// corpus as a `billingGranularity` for as long as the field was a string.
///
/// On a non-`sum` row the value must pair with `aggregationGranularity`
/// (`hour => per_hour`, `day => per_day`, D-77 / `inst-la-granularity`), which
/// is what keeps `inst-tb-units` and `inst-la-units` naming one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingGranularity {
    PerSecond,
    PerMinute,
    PerHour,
    PerDay,
    /// No sub-unit quantization: the quantity is counted in whole units.
    WholeUnit,
}

/// The `chargeKind` axis of the canonical scope key: which charge component of a
/// plan the row prices (`PRD.md` glossary, `design/01-foundation.md` 4.1).
///
/// Pinned here in code alongside [`ProrationBasis`], [`TierAggregationWindow`]
/// and [`BillingGranularity`], and for a sharper version of the same reason. It
/// is an **axis of the key** and it is frozen into `pricingSnapshotRef`, so a
/// subject that cannot read it has to invent it — and two subjects invent two
/// different things, which is the whole class of divergence this corpus exists
/// to prevent. The pricing gear invented "a row that names a `meter` is a usage
/// row"; nothing said so, nothing checked it, and no second gear was bound by it.
///
/// It also made one of the four model-kind rules unstateable. `inst-mk-chargekind`
/// is a `kind x chargeKind` matrix, and while the corpus carried no `chargeKind`
/// no case could describe a `flat` usage row or a `graduated` recurring row —
/// the two shapes the matrix exists to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeKind {
    /// A recurring subscription charge.
    Recurring,
    /// A metered usage charge — the only component with a metered `Q` for the
    /// tier, block and fold machinery to read.
    Usage,
    /// A one-off charge that is not a setup fee: a one-time plan's base row.
    OneTime,
    /// A setup charge on a recurring or hybrid plan. Distinct from
    /// [`ChargeKind::OneTime`] so a hybrid plan can carry both without them
    /// colliding on one scope key.
    OneTimeSetup,
}

/// Fields frozen in `pricingSnapshotRef`. Nothing else may appear here.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub model_kind: ModelKind,
    /// Scope-key axis 7, and **required** — the one snapshot field besides
    /// `model_kind` and `currency` that every case must state.
    ///
    /// Required rather than optional because there is no row without one: a
    /// published row sits on exactly one charge component of exactly one plan,
    /// and the axis is part of the key that makes it that row. A
    /// `#[serde(default)]` would put the deleted inference back one layer down —
    /// a case that forgot to say would silently read `recurring`, and the rules
    /// that turn on this axis (`inst-mk-chargekind`, `inst-mk-forbidden`,
    /// `inst-tb-window`, `inst-pk-window`, `inst-tb-supersession-units`) would
    /// judge a row nobody authored.
    pub charge_kind: ChargeKind,
    pub currency: String,
    #[serde(default)]
    pub bands: Vec<Band>,
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub package_size: Option<u64>,
    #[serde(default)]
    pub package_price_minor: Option<i64>,
    #[serde(default)]
    pub quantity_source: Option<String>,
    #[serde(default)]
    pub tier_aggregation_window: Option<TierAggregationWindow>,
    #[serde(default)]
    pub billing_granularity: Option<BillingGranularity>,
    /// Frozen in `pricingSnapshotRef`; drives all mid-period proration.
    #[serde(default)]
    pub proration_basis: Option<ProrationBasis>,

    // The unit/counter-determining fields. A successor landing on an occupied
    // published scope key must carry all of these unchanged, because the tier
    // counter `Q` continues across supersession and must keep its denomination
    // and its pricing math (`inst-tb-supersession-units`).
    #[serde(default)]
    pub meter: Option<String>,
    #[serde(default)]
    pub dimension_key: Option<String>,
    #[serde(default)]
    pub aggregation_function: Option<AggregationFunction>,
    #[serde(default)]
    pub aggregation_granularity: Option<AggregationGranularity>,
    /// `max_hold_granules` — an integer count of granules >= 1. No default: the
    /// sampling-gap bound is a commercial statement, authored explicitly.
    #[serde(default)]
    pub max_hold_granules: Option<u64>,
    #[serde(default)]
    pub tier_qualification_window: Option<String>,
    #[serde(default)]
    pub included_allowance: Option<IncludedAllowance>,
    /// Self-service reserved rate, sourced from the snapshot by Tariffs rather
    /// than from Contracts. Denominated in the row's billable unit, so on a
    /// level row it is money **per granule** (D-139).
    #[serde(default)]
    pub reserved_rate_minor: Option<i64>,
    #[serde(default)]
    pub reservation_flavor: Option<ReservationFlavor>,
}

/// `consumption` prices matched usage at the reserved rate and bands the
/// remainder; `capacity` bills the allocation whatever the usage.
///
/// On a non-`sum` row only `capacity` is authorable — `consumption` fails
/// publish with `LEVEL_RESERVATION_CONSUMPTION_FORBIDDEN` until per-granule
/// netting semantics are decided (D-53).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationFlavor {
    Consumption,
    Capacity,
}

/// How `Q` is derived (D-44 / rating T-D-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationFunction {
    /// The plain sum of normalised measures; not a fold.
    Sum,
    /// Max sample in the granule.
    Peak,
    /// Step-integral of the level over the granule.
    TimeWeighted,
}

/// The granule the window is cut into. D-77 pins the `billingGranularity`
/// pairing — `hour ⇒ per_hour`, `day ⇒ per_day` — to keep band edges aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationGranularity {
    Hour,
    Day,
}

impl AggregationGranularity {
    #[must_use]
    pub const fn seconds(self) -> u64 {
        match self {
            Self::Hour => 3_600,
            Self::Day => 86_400,
        }
    }
}

/// One gauge observation. The level holds from here until the next sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaugeSample {
    pub at: chrono::DateTime<chrono::Utc>,
    pub level: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloverPolicy {
    None,
    Carry,
}

/// `includedAllowance {quantity, rolloverPolicy}` (D-45).
///
/// Protected by the succession unit guard only under `carry` (D-129): a carry
/// allowance compiles into a plan-scoped, revision-immutable grant row that a
/// supersession cannot rewrite, because a supersession opens no plan revision.
/// A `none`-policy allowance carries no plan-scoped artifact and stays a free
/// row-local lever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncludedAllowance {
    pub quantity: u64,
    pub rollover_policy: RolloverPolicy,
}

/// Per-file inputs the consumer supplies at evaluation time — constant across
/// the file's assertions. The varying quantity lives in [`Given`].
///
/// Empty for the Phase-1 families, which need no consumer-side context beyond
/// the quantity. It gains fields with `reserved` (allocated quantity),
/// `level-aggregation` (gauge samples) and `trailing-tier` (the prior-period
/// total and its pin).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    /// The rating window the fold runs over. Cut into granules of the row's
    /// `aggregation_granularity`.
    #[serde(default)]
    pub window_start: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub window_end: Option<chrono::DateTime<chrono::Utc>>,
    /// Gauge observations, consumer-supplied. Not snapshot-frozen: the catalog
    /// authors the aggregation policy and never sees a measurement.
    #[serde(default)]
    pub samples: Vec<GaugeSample>,
    /// The matched or allocated reserved quantity, supplied at runtime. The
    /// catalog never meters, allocates, or computes the charge.
    #[serde(default)]
    pub reserved_quantity: Option<u64>,
    /// The reservation's covered duration within the period, in the row's
    /// billable-unit granules (D-139 / rating T-D-25). Rating-computed coverage,
    /// never authored or frozen by the catalog.
    #[serde(default)]
    pub covered_granules: Option<u64>,
}

/// The per-assertion input.
///
/// `q` serves the charge families. The period trio serves proration, where the
/// question is what share of a period is chargeable rather than what a quantity
/// costs. All instants are UTC.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Given {
    #[serde(default)]
    pub q: u64,
    #[serde(default)]
    pub period_start: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub period_end: Option<chrono::DateTime<chrono::Utc>>,
    /// Start of the chargeable stretch inside the period.
    #[serde(default)]
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    /// End of the chargeable stretch; defaults to `period_end`.
    #[serde(default)]
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

/// What a case asserts.
///
/// Two shapes, because the two seams produce different things. The charge
/// families assert a final integer amount. Proration asserts a **unit count**,
/// not money: rating emits prorated components at full intermediate precision
/// and never rounds — Billing rounds — so a prorated minor amount does not
/// exist at the pricing↔rating seam and a fixture must not invent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Expect {
    Charge(ChargeExpect),
    Units(UnitsExpect),
    Fold(FoldExpect),
}

/// The output of a granule fold: `Q` in the billable unit, which is the level
/// unit times the granule duration (GB·h at `hour`, GB·day at `day`).
///
/// `Q` is asserted rather than a charge because `Q` is the new thing a level row
/// introduces — the band and package math over it must be **unchanged** from the
/// `sum` case, and the other families already pin that math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoldExpect {
    pub folded_q: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChargeExpect {
    pub charge_minor: i64,
}

/// The chargeable share of a period, as an exact integer ratio.
///
/// The unit depends on the basis: days for the calendar bases, seconds for
/// `by_second`, and the whole period for `whole_unit` / `none`. Integers
/// throughout, so the "slice fractions sum to exactly 1" rule (rating T-D-26)
/// becomes an exact equality rather than a float comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitsExpect {
    pub units_charged: u64,
    pub units_in_basis: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assertion {
    pub given: Given,
    pub expect: Expect,
    /// Why the expected number is what it is. A number without a reason cannot
    /// be reviewed.
    #[serde(default)]
    pub why: Option<String>,
}

/// A case that prices something, or apportions a period.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCase {
    pub family: Family,
    pub id: String,
    pub kind: CaseKind,
    /// The normative clauses this case encodes. Mandatory.
    pub provenance: Vec<String>,
    pub snapshot: Snapshot,
    #[serde(default)]
    pub runtime: Runtime,
    pub assert: Vec<Assertion>,
}

/// What publish must do with an authored change.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "publish", rename_all = "snake_case")]
pub enum PublishVerdict {
    Accepted,
    /// Rejected, naming the error code the design set specifies. The code is
    /// mandatory: "publish fails" without saying how is not reviewable, and the
    /// codes are themselves part of the contract.
    Rejected {
        error_code: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishAssertion {
    pub expect: PublishVerdict,
    #[serde(default)]
    pub why: Option<String>,
}

/// A case that asserts a publish outcome rather than a number.
///
/// The successor lands on the predecessor's canonical scope key — that is what
/// makes it a supersession rather than a new row — so the pair is the unit of
/// assertion.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishCase {
    pub family: Family,
    pub id: String,
    pub kind: CaseKind,
    pub provenance: Vec<String>,
    pub predecessor: Snapshot,
    pub successor: Snapshot,
    pub assert: Vec<PublishAssertion>,
    /// The slice this case is authored against, when nothing has built it yet.
    ///
    /// The `trailing-tier` precedent, one axis over. `trailing-tier` is named in
    /// [`Family::ALL`], carries no case, and is therefore reported **declined** —
    /// never green, never absent. A case cannot be declined that way: omitting
    /// it would delete an authored rule of a slice the design set already
    /// states. So the corpus says the same thing in the case file, and the
    /// runner treats a subject's decline as the **anticipated** answer: recorded,
    /// earning nothing, and never a pass.
    ///
    /// It is not an escape hatch. The verdict stays authored and stays checked:
    /// a subject that *answers* the case is judged against it exactly as before,
    /// so a wrong verdict is still red. And a subject that answers it **right**
    /// retires this field — the declaration is then stale, which the runner
    /// reports, because "this cannot be answered yet" must stop being true the
    /// moment it stops being true.
    #[serde(default)]
    pub declined_until: Option<String>,
}

/// A corpus case.
///
/// Deliberately a plain Rust enum rather than a serde-tagged one: an internally
/// tagged enum cannot carry `deny_unknown_fields`, and rejecting stray keys is
/// the property that keeps the snapshot/runtime ownership boundary honest. The
/// loader reads `kind` first and then parses the whole file into the matching
/// type.
#[derive(Debug, Clone)]
pub enum Case {
    Evaluation(Box<EvaluationCase>),
    Publish(Box<PublishCase>),
}

/// Just enough of a case file to choose its type.
#[derive(Debug, Clone, Deserialize)]
pub struct CaseHeader {
    pub kind: CaseKind,
}

impl Case {
    #[must_use]
    pub fn family(&self) -> Family {
        match self {
            Self::Evaluation(c) => c.family,
            Self::Publish(c) => c.family,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Evaluation(c) => &c.id,
            Self::Publish(c) => &c.id,
        }
    }

    #[must_use]
    pub fn provenance(&self) -> &[String] {
        match self {
            Self::Evaluation(c) => &c.provenance,
            Self::Publish(c) => &c.provenance,
        }
    }

    /// How many assertions the case carries; zero means it proves nothing.
    #[must_use]
    pub fn assertion_count(&self) -> usize {
        match self {
            Self::Evaluation(c) => c.assert.len(),
            Self::Publish(c) => c.assert.len(),
        }
    }

    /// The `modelKind` this case exercises. For a publish case that is the
    /// successor's, since the successor is the row under test.
    #[must_use]
    pub fn model_kind(&self) -> ModelKind {
        match self {
            Self::Evaluation(c) => c.snapshot.model_kind,
            Self::Publish(c) => c.successor.model_kind,
        }
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;
