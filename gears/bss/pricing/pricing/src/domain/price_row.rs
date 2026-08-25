//! The authored **price row** — the Slice-3-owned shape
//! (`design/03-price-structure.md`).
//!
//! This is what an operator authors and what the Slice-3 rules judge: the
//! explicit `modelKind`, the per-kind money placement, the tier bands under the
//! `[fromQty, toQty)` convention, the package block fields, and the usage
//! evaluation policy (`billingGranularity`, `tierAggregationWindow`, the D-44
//! level-aggregation triple). The `chargeKind` rides along from the canonical
//! scope key because half the shape rules are a function of it.
//!
//! **Per-kind optionality is modelled honestly.** Nearly every field is an
//! `Option`, and none of them is defaulted here. A price row is *authored*
//! before it is *published*, so the type has to be able to hold a draft that is
//! not yet publishable — the rules in [`crate::domain::rules`] are what make a
//! field required for a kind, and a type that made `amount_minor` mandatory
//! would have made `AMOUNT_PLACEMENT_INVALID` unreachable for the one kind it
//! exists to catch.
//!
//! **This type computes no charge.** It carries structure; Tariffs evaluates it.
//!
//! ## Structural compatibility with the conformance corpus
//!
//! The corpus (`bss_fixtures::model::Snapshot`) is the shape the joint golden
//! fixtures freeze, and the publish validator compares against it. This type is
//! deliberately field-for-field compatible with it — same names, same meaning —
//! but it is a **separate type**: the corpus model sits behind the crate's
//! `corpus` feature, which a gear does not enable (the production surface is
//! `ModelKind` + `Registry`), and the corpus `Snapshot` additionally carries
//! `currency`, `proration_basis` and the Slice-10 reservation fields that are
//! not Slice-3-owned.
//!
//! [`ModelKind`] itself is **reused** from `bss_fixtures` rather than mirrored.
//! One vocabulary for the kind enum means the fixture gate's lookup is total by
//! construction, and a sixth kind cannot appear on one side only.

use std::fmt;

use toolkit_macros::domain_model;

pub use bss_fixtures::ModelKind;

use crate::domain::allowance::presented_model_kind;
use crate::domain::money::{MinorAmount, RateMinor};
use crate::domain::scope_key::ChargeKind;

/// How a non-usage `per_unit` row obtains its quantity.
///
/// Usage rows never carry this: their `Q` comes from the meter, and offering an
/// authored quantity beside a metered one would be two answers to "how much was
/// consumed" (`inst-mk-required`, review fix).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuantitySource {
    /// Subscriptions supplies the seat count at rating time.
    SubscriptionSeatCount,
    /// The author states a fixed quantity, carried in
    /// [`PriceRow::manual_quantity`].
    Manual,
}

impl QuantitySource {
    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubscriptionSeatCount => "subscription_seat_count",
            Self::Manual => "manual",
        }
    }
}

impl fmt::Display for QuantitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The quantization applied to a metered quantity before it is priced.
///
/// It is **not** a period bound: it says what one billable unit is, not over
/// what stretch the units accumulate. That is why a `package` row needs
/// [`TierAggregationWindow`] as well (`inst-pk-window`, D-58).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BillingGranularity {
    /// Seconds.
    PerSecond,
    /// Minutes.
    PerMinute,
    /// Hours. The counterpart of [`AggregationGranularity::Hour`] (D-77).
    PerHour,
    /// Days. The counterpart of [`AggregationGranularity::Day`] (D-77).
    PerDay,
    /// No quantization: the raw metered unit is the billable unit.
    WholeUnit,
}

impl BillingGranularity {
    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PerSecond => "per_second",
            Self::PerMinute => "per_minute",
            Self::PerHour => "per_hour",
            Self::PerDay => "per_day",
            Self::WholeUnit => "whole_unit",
        }
    }
}

impl fmt::Display for BillingGranularity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// When the tier counter `Q` resets — and, on a `package` row, the window over
/// which `used` accumulates **before** block round-up (D-58).
///
/// The catalog persists the enum and nothing more: the reset semantics are
/// Tariffs-owned.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TierAggregationWindow {
    /// The calendar month.
    CalendarMonth,
    /// The subscription's invoice period.
    InvoicePeriod,
    /// Never resets.
    SubscriptionLifetime,
    /// Each event stands alone.
    PerEvent,
    /// Each clock hour stands alone: the counter resets hourly, so every hour's
    /// quantity picks its own band (D-313). Distinct from `billingGranularity`,
    /// which quantizes the billable unit and does not reset anything.
    PerHour,
}

impl TierAggregationWindow {
    /// Every window a row may carry, in the order a remediation offers them.
    pub const ALL: &'static [Self] = &[
        Self::CalendarMonth,
        Self::InvoicePeriod,
        Self::SubscriptionLifetime,
        Self::PerEvent,
        Self::PerHour,
    ];

    /// The legal set as a report sentence enumerates it.
    ///
    /// Rendered from [`Self::ALL`] rather than spelled out at each rule, and
    /// [`ALL`](Self::ALL) is the same list `price_repo::TIER_AGGREGATION_WINDOWS`
    /// admits off the wire — so the set a refusal *offers* and the set the gear
    /// *accepts* cannot be two lists. Two hand-written copies cost an author with
    /// an hourly fold a round trip changing a value the rule already accepts.
    #[must_use]
    pub fn offered() -> String {
        Self::ALL
            .iter()
            .map(|window| window.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CalendarMonth => "calendar_month",
            Self::InvoicePeriod => "invoice_period",
            Self::SubscriptionLifetime => "subscription_lifetime",
            Self::PerEvent => "per_event",
            Self::PerHour => "per_hour",
        }
    }
}

impl fmt::Display for TierAggregationWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The D-40 third window: which period **qualifies** the rate tier.
///
/// Orthogonal to both [`TierAggregationWindow`] (counter reset) and
/// [`BillingGranularity`] (billing cadence): `trailing_period` qualifies the tier
/// from the prior period's total and locks it for this one.
///
/// [`TierQualificationWindow::Current`] is the **default** the PRD states, so a
/// row that authors nothing qualifies from the current window.
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TierQualificationWindow {
    /// Qualify from the current window's own running total. The default.
    #[default]
    Current,
    /// Qualify from the prior period's total, then lock.
    TrailingPeriod,
}

impl TierQualificationWindow {
    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::TrailingPeriod => "trailing_period",
        }
    }
}

impl fmt::Display for TierQualificationWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a reservation on a usage row reserves (`inst-rv-attrs`, A1).
///
/// A reservation is an **attribute of the usage row it reserves**, never a
/// second row: A1 keeps one priced line per `(meter, dimensionKey)`, so the pair
/// `reserved_rate` + this flavor rides the on-demand row alongside its
/// price and tiers.
///
/// **There is no default, deliberately.** The two flavors bill differently
/// enough that an unauthored one cannot be guessed: [`Self::Consumption`]
/// excludes the matched reserved quantity from the on-demand tier counter `Q`
/// and lets the remainder restart at zero (`inst-rv-tier-q`), while
/// [`Self::Capacity`] never touches `Q` at all and accrues
/// `reservedRate x reservedQuantity x duration` per covered granule
/// (`inst-rv-level`, D-139). Absent means *the row reserves nothing*, which is
/// why the field is an `Option` on the row and why the pairing with
/// `reserved_rate` is a publish rule rather than a default.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReservationFlavor {
    /// Reserved **consumption**: the matched quantity is excluded from the
    /// on-demand tier counter and the remainder's `Q` starts at zero
    /// (`inst-rv-tier-q`).
    Consumption,
    /// Reserved **capacity**: the charge never enters `Q`, and accrues per
    /// covered granule (`inst-rv-level`, D-139). The only flavor authorable on
    /// a non-`sum` row at launch.
    Capacity,
}

impl ReservationFlavor {
    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Consumption => "consumption",
            Self::Capacity => "capacity",
        }
    }
}

impl fmt::Display for ReservationFlavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a `usage` floor does with below-floor usage (`inst-ft-fallback`).
///
/// **One variant at launch, and that is the decision rather than a placeholder.**
/// The design set could have left the behaviour implicit and had every row mean
/// `exception`; `inst-ft-fallback` says instead that "the fallback is authored,
/// not implied", so an author declares it and it freezes in the snapshot. The
/// value of a one-variant enum is what happens when the second lands: every
/// already-published row already says which behaviour it chose, and none of them
/// silently inherits a new default.
///
/// Richer fallbacks -- an alternative row, for instance -- are a named Future.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MinQtyUsageFallback {
    /// Fail closed into the rating exception path: visible and resolvable, never
    /// silently zero-rated and never silently charged.
    Exception,
}

impl MinQtyUsageFallback {
    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exception => "exception",
        }
    }
}

impl fmt::Display for MinQtyUsageFallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How `Q` is derived from the measures in the window (D-44).
///
/// [`AggregationFunction::Sum`] is the **default**: a row that authors nothing
/// is a `sum` row. The two non-`sum` functions are granule *folds* and are what
/// make a row "level-shaped"; the fold itself is Rating's (T-D-17).
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AggregationFunction {
    /// The plain window sum of normalised measures. The default; not a fold.
    #[default]
    Sum,
    /// The maximum sample in each granule.
    Peak,
    /// The step-integral of the level over each granule.
    TimeWeighted,
}

impl AggregationFunction {
    /// Is this the default window sum — i.e. does the row have no granule fold?
    #[must_use]
    pub const fn is_sum(self) -> bool {
        matches!(self, Self::Sum)
    }

    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Peak => "peak",
            Self::TimeWeighted => "time_weighted",
        }
    }
}

impl fmt::Display for AggregationFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The granule a non-`sum` window is cut into (D-44).
///
/// [`AggregationGranularity::Hour`] is the default. Each value has exactly one
/// legal [`BillingGranularity`] counterpart (D-77); see
/// [`AggregationGranularity::billing_counterpart`].
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AggregationGranularity {
    /// One-hour granules. The default.
    #[default]
    Hour,
    /// One-day granules.
    Day,
}

impl AggregationGranularity {
    /// The one [`BillingGranularity`] this granule may pair with (D-77).
    ///
    /// The pairing is what stops `inst-tb-units` and `inst-la-units` naming
    /// different units for the same band — a factor-of-24 error at the band
    /// edge that every other stated check passes.
    #[must_use]
    pub const fn billing_counterpart(self) -> BillingGranularity {
        match self {
            Self::Hour => BillingGranularity::PerHour,
            Self::Day => BillingGranularity::PerDay,
        }
    }

    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
        }
    }
}

impl fmt::Display for AggregationGranularity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happens to an unused included allowance at the end of a period (D-45).
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RolloverPolicy {
    /// Unused allowance expires. A purely row-local lever.
    None,
    /// Unused allowance carries forward. Compiles into a **plan-scoped**,
    /// revision-immutable grant row, which is why a supersession may not change
    /// it (D-129).
    Carry,
}

impl RolloverPolicy {
    /// Does this policy compile into a plan-scoped grant?
    #[must_use]
    pub const fn is_carry(self) -> bool {
        matches!(self, Self::Carry)
    }

    /// The persisted / wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Carry => "carry",
        }
    }
}

impl fmt::Display for RolloverPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `includedAllowance {quantity, rolloverPolicy}` (D-45).
///
/// The **authored declaration**, and it stays the row's truth: publish *compiles*
/// it ([`crate::domain::allowance`]) into a `$0` first band, an offset ladder and
/// a display marker, and materializes those into the read model without writing
/// any of them back (D-130). That is what makes the compile re-entrant — a
/// supersession, repricing successor or clone recompiles from this field and the
/// authored bands, both of which are still here.
///
/// `rolloverPolicy = carry` is a different artifact — a per-period promotional
/// grant in `pricing_plan_grant` (`inst-ac-carry`, D-52) — and it is still refused
/// on every authoring path, because that table does not exist in this gear.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IncludedAllowance {
    /// The included quantity, in the row's billable unit.
    pub quantity: u64,
    /// What happens to the unused remainder.
    pub rollover_policy: RolloverPolicy,
}

/// The upper bound of a tier band.
///
/// An enum rather than an `Option<u64>` so it reads the way the corpus and the
/// column do — `NULL` / `"open"` is a *state* of the band, not an absent value —
/// and so the D-17 top-band rule has something to match on.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BandTop {
    /// No upper bound. The top band is always this (D-17).
    Open,
    /// The exclusive upper bound.
    Closed(u64),
}

impl BandTop {
    /// Is the band unbounded above?
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }

    /// The exclusive upper bound, when there is one.
    #[must_use]
    pub const fn closed_at(self) -> Option<u64> {
        match self {
            Self::Open => None,
            Self::Closed(top) => Some(top),
        }
    }
}

impl fmt::Display for BandTop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => f.write_str("open"),
            Self::Closed(top) => write!(f, "{top}"),
        }
    }
}

/// One tier band: the half-open quantity range `[from_qty, to_qty)` and the unit
/// price that applies inside it.
///
/// **Band quantities are billable units — the units that exist *after*
/// [`BillingGranularity`] quantization.** A `per_hour` row's bands count hours,
/// never raw seconds; a `per_day` row's bands count days. This is normative
/// (`inst-tb-units`) and it is the sentence that stops the catalog and Tariffs
/// diverging about what `from_qty = 100` means. On a non-`sum` (level) row the
/// granule fold *is* the quantization, so the billable unit is
/// `level unit x granule` (GB-hours at `hour`, GB-days at `day`) and the rule
/// holds unchanged — which is exactly why D-77 pins the granularity pairing.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TierBand {
    /// Inclusive lower bound, in billable units.
    pub from_qty: u64,
    /// Exclusive upper bound, in billable units; [`BandTop::Open`] on the top
    /// band.
    pub to_qty: BandTop,
    /// The band's rate — a **multiplier, not an amount** (D-311). `0` is valid:
    /// a free first band is a normal way to author "N included".
    ///
    /// Not a [`MinorAmount`]: a band routinely prices a unit below one minor unit,
    /// and truncating there collapses a `0.0150 / 0.0110` ladder and a
    /// `0.0230 / 0.0120` ladder both to `0.01` at every band — two tariffs become
    /// one, on rows that look well-formed.
    pub unit_price_rate: RateMinor,
}

impl TierBand {
    /// A closed band.
    #[must_use]
    pub const fn closed(from_qty: u64, to_qty: u64, unit_price_rate: RateMinor) -> Self {
        Self {
            from_qty,
            to_qty: BandTop::Closed(to_qty),
            unit_price_rate,
        }
    }

    /// An open-topped band.
    #[must_use]
    pub const fn open(from_qty: u64, unit_price_rate: RateMinor) -> Self {
        Self {
            from_qty,
            to_qty: BandTop::Open,
            unit_price_rate,
        }
    }

    /// Does this band cover no quantity at all (`to_qty <= from_qty`)?
    ///
    /// A band that covers nothing is not harmlessly ignorable: it makes the band
    /// set's contiguity ambiguous and it is always an authoring mistake.
    #[must_use]
    pub const fn is_zero_width(&self) -> bool {
        match self.to_qty {
            BandTop::Open => false,
            BandTop::Closed(top) => top <= self.from_qty,
        }
    }
}

impl fmt::Display for TierBand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}, {})", self.from_qty, self.to_qty)
    }
}

/// The authored price row: the Slice-3 shape, plus the `chargeKind` axis it is
/// judged against.
///
/// Fields are public and unvalidated on purpose. The row is the *subject* the
/// [`crate::domain::validation::ValidationPipeline`] runs over, and a
/// constructor that refused an unpublishable combination would move the rules
/// into the type — where they could not be enumerated into one aggregate report,
/// which is the whole point of the fail-closed pipeline.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceRow {
    /// Scope-key axis 7. Not authored on the row itself — it comes from the key
    /// — but it is carried here because the kind matrix, the evaluation-policy
    /// placement rules and the supersession guard are all functions of it.
    pub charge_kind: ChargeKind,
    /// The explicit model kind. `None` is the authoring state
    /// `MODEL_KIND_MISSING` rejects; there is no implicit default at rating time.
    pub model_kind: Option<ModelKind>,
    /// The single amount on `flat`, and **NULL** on every other kind — the
    /// `per_unit` rate lives in [`Self::unit_rate`], and `graduated` / `volume` /
    /// `package` carry theirs in the band or package column, so no row ever
    /// carries two competing prices.
    ///
    /// Not "the unit price on `per_unit`" (D-311): one column standing for an
    /// invoice sum **and** for a multiplier is the conflation the rate type exists
    /// to end.
    pub amount_minor: Option<MinorAmount>,
    /// The `per_unit` **rate** — what one unit costs, as a multiplier (D-311).
    ///
    /// NULL on every other kind. An invoice carries `amount × quantity` rounded
    /// once; this is the multiplier, so it is not bounded by the currency's
    /// ISO-4217 scale the way an amount is.
    pub unit_rate: Option<RateMinor>,
    /// The authored tier bands, in authored order. Empty on a non-tiered row.
    ///
    /// Authored bands only: the D-45 allowance compile is a projection and never
    /// rewrites this set (D-130).
    pub bands: Vec<TierBand>,
    /// Units per block; `package` only, `> 0`.
    pub package_size: Option<u64>,
    /// Price per block; `package` only.
    pub package_price_minor: Option<MinorAmount>,
    /// Where a **non-usage** `per_unit` row's quantity comes from.
    pub quantity_source: Option<QuantitySource>,
    /// The fixed quantity, required when
    /// [`QuantitySource::Manual`] is authored.
    pub manual_quantity: Option<u64>,
    /// The published metering unit a usage row prices.
    pub meter: Option<String>,
    /// The dimension discriminator on the `(meter, dimensionKey)` line.
    ///
    /// Not an `Option`: the column is `NOT NULL DEFAULT ''` and the empty string
    /// is the **empty-tuple sentinel**, so the Slice-2 injectivity index collides
    /// undimensioned rows instead of treating them as distinct `NULL`s.
    pub dimension_key: String,
    /// The billable-unit quantization; required on every usage row.
    pub billing_granularity: Option<BillingGranularity>,
    /// The counter-reset window; required on tiered and `package` usage rows.
    pub tier_aggregation_window: Option<TierAggregationWindow>,
    /// The D-40 tier-qualification window; optional on a tiered row.
    pub tier_qualification_window: Option<TierQualificationWindow>,
    /// How `Q` is derived. `None` means [`AggregationFunction::Sum`].
    pub aggregation_function: Option<AggregationFunction>,
    /// The fold granule. `None` means [`AggregationGranularity::Hour`] on a
    /// non-`sum` row, and is the only legal value on a `sum` row.
    pub aggregation_granularity: Option<AggregationGranularity>,
    /// The `hold_last` bound, in granules; required on non-`sum` rows, forbidden
    /// otherwise. No default — the sampling-gap bound is a commercial statement.
    pub max_hold_granules: Option<u64>,
    /// The D-45 included allowance.
    pub included_allowance: Option<IncludedAllowance>,
    /// The reserved rate, in the row's currency (`inst-rv-attrs`, A1).
    ///
    /// **A rate, at the stored 10^-9 scale — not an amount.** Outside the
    /// evaluation-policy roster all the same: what keeps it out is that it is
    /// money rather than a quantity-derivation input, and that is unchanged.
    /// Under D-139 it is denominated in the row's billable unit — level unit x
    /// granule duration on a `capacity` reservation — so it is money **per
    /// granule** rather than a period charge, which is D-311's own definition of
    /// a rate.
    ///
    /// Not [`MinorAmount`], for the reason D-311 gives one column over: *"a metered
    /// rate routinely prices below the currency's minor unit"*, and truncation
    /// *"collapsed a `0.0150 / 0.0110` ladder and a `0.0230 / 0.0120` ladder both to
    /// `0.01` at every band, so two different tariffs became the same tariff on rows
    /// that looked well-formed."* D-311 moved [`TierBand::unit_price_rate`] and
    /// [`Self::unit_rate`] and missed this one, because its census enumerated
    /// references to `unit_price_minor` and this field is not one of them — the
    /// wrong-operand
    /// census D-323 already records as having cost one site.
    ///
    /// In whole minor units the smallest expressible non-zero value was one cent,
    /// so a per-second reserved capacity — `max_hold_granules` names `per_second`
    /// as the granularity that motivated widening that column — could not be
    /// authored at all: `$0.0000166667` per GB-second is `0.00166667` minor units,
    /// and the author had to submit `0` or `1`, the latter 600x the intent. Not
    /// truncated; unrepresentable.
    pub reserved_rate: Option<RateMinor>,
    /// What the reservation reserves; present iff [`Self::reserved_rate`]
    /// is (`inst-rv-attrs`).
    ///
    /// **In** the evaluation-policy roster: it decides whether the reserved
    /// quantity leaves the on-demand tier counter (`inst-rv-tier-q`) or never
    /// enters it (`inst-rv-level`), which is quantity derivation.
    pub reservation_flavor: Option<ReservationFlavor>,
    /// The **purchase** floor: Subscriptions rejects an order below it rather
    /// than silently zeroing (`inst-ft-typed`).
    ///
    /// Outside the evaluation-policy roster: it is a permission -- whether the
    /// order may be placed at all -- and decides nothing about how a rated line
    /// is derived.
    pub min_qty_purchase: Option<u64>,
    /// The **usage** floor: below-floor usage is ineligible and fails closed
    /// (`inst-ft-typed`).
    ///
    /// **In** the roster. It decides what quantity is billable, which is
    /// quantity derivation.
    pub min_qty_usage: Option<u64>,
    /// What happens beneath [`Self::min_qty_usage`]; REQUIRED when it is set
    /// (`inst-ft-fallback`).
    ///
    /// **In** the roster, with its floor: the two are one evaluation rule read
    /// in two fields.
    pub min_qty_usage_fallback: Option<MinQtyUsageFallback>,
    /// The optional reference to an external discount instrument
    /// (`inst-dr-referential`).
    ///
    /// Outside the roster. Promotions owns the instrument and Promotions/Tariffs
    /// evaluate it; this gear only **persists** it (`inst-dr-boundary`), so it
    /// changes nothing about how *this* row derives a quantity or selects a rate.
    ///
    /// **It is not validated, and this doc claimed it was for one wave** (D-255).
    /// `inst-dr-referential` asks for referential integrity against a registered
    /// instrument, and there is no Promotions or Tariffs registry client, port or
    /// contract anywhere in this gear — so `DISCOUNT_REF_UNRESOLVED` is declared
    /// and unraisable, and an unresolvable ref publishes and freezes into the
    /// snapshot unchecked. S10 §5 and `pricing_plan_addon_rule` both record that; this
    /// field's own doc said the opposite, which is the reading a caller is most
    /// likely to trust.
    pub discount_ref: Option<String>,
}

impl PriceRow {
    /// An otherwise-empty row on `charge_kind` carrying `model_kind`.
    ///
    /// Every optional field starts absent and `dimension_key` starts at the
    /// empty-tuple sentinel, which is the authored state of a row nobody has
    /// filled in yet — not a publishable one.
    #[must_use]
    pub fn new(charge_kind: ChargeKind, model_kind: Option<ModelKind>) -> Self {
        Self {
            charge_kind,
            model_kind,
            amount_minor: None,
            unit_rate: None,
            bands: Vec::new(),
            package_size: None,
            package_price_minor: None,
            quantity_source: None,
            manual_quantity: None,
            meter: None,
            dimension_key: String::new(),
            billing_granularity: None,
            tier_aggregation_window: None,
            tier_qualification_window: None,
            aggregation_function: None,
            aggregation_granularity: None,
            max_hold_granules: None,
            included_allowance: None,
            reserved_rate: None,
            reservation_flavor: None,
            min_qty_purchase: None,
            min_qty_usage: None,
            min_qty_usage_fallback: None,
            discount_ref: None,
        }
    }

    /// Does this row carry a reservation (`inst-rv-attrs`)?
    ///
    /// **Either half is enough**, and that is the point rather than an
    /// oversight: the pairing rule
    /// ([`RESERVATION_PAIR_INCOMPLETE`](crate::domain::publish::rules::RESERVATION_PAIR_INCOMPLETE))
    /// refuses a half-authored reservation at publish, and until it has run, a
    /// row carrying one half is a row somebody meant to reserve. Reading this as
    /// a conjunction would let a flavor-without-rate row slip past the
    /// `FixtureGate` on its way to that refusal, which is the wrong order to
    /// discover the two problems in.
    #[must_use]
    pub const fn is_reserved(&self) -> bool {
        self.reserved_rate.is_some() || self.reservation_flavor.is_some()
    }

    /// Is this a metered usage row?
    #[must_use]
    pub const fn is_usage(&self) -> bool {
        matches!(self.charge_kind, ChargeKind::Usage)
    }

    /// Is the kind one whose money lives in tier bands?
    #[must_use]
    pub const fn is_tiered(&self) -> bool {
        matches!(
            self.model_kind,
            Some(ModelKind::Graduated | ModelKind::Volume)
        )
    }

    /// The row's effective derivation function: what it authored, else the
    /// [`AggregationFunction::Sum`] default.
    ///
    /// Every level rule reads the row through here rather than through the raw
    /// `Option`, because "authored nothing" and "authored `sum`" are the same
    /// row and must not validate differently.
    #[must_use]
    pub fn effective_aggregation_function(&self) -> AggregationFunction {
        self.aggregation_function.unwrap_or_default()
    }

    /// The row's effective fold granule: what it authored, else the
    /// [`AggregationGranularity::Hour`] default.
    #[must_use]
    pub fn effective_aggregation_granularity(&self) -> AggregationGranularity {
        self.aggregation_granularity.unwrap_or_default()
    }

    /// The row's effective tier-qualification window: what it authored, else
    /// the [`TierQualificationWindow::Current`] default the PRD states
    /// ("`current` (default) | `trailing_period`").
    ///
    /// Read through here for the same reason as the two above: an unauthored
    /// window and an authored `current` are the same row, and comparing the raw
    /// `Option`s across a supersession would reject a change that changed
    /// nothing.
    #[must_use]
    pub fn effective_tier_qualification_window(&self) -> TierQualificationWindow {
        self.tier_qualification_window.unwrap_or_default()
    }

    /// Does this row carry a granule fold (a non-`sum` derivation)?
    #[must_use]
    pub fn is_level(&self) -> bool {
        !self.effective_aggregation_function().is_sum()
    }

    /// How a finding locates this row for the author.
    ///
    /// The row has no identity of its own at this layer — a price id is assigned
    /// by the persistence path, and the S3 shape is exactly the authored fields —
    /// so the subject is the coordinates that make the finding readable: the
    /// charge component and the kind under judgement.
    #[must_use]
    pub fn subject(&self) -> String {
        let kind = self.model_kind.map_or("(no model kind)", model_kind_wire);
        format!("{}/{kind}", self.charge_kind)
    }
}

/// The **unit- and counter-determining fields** two rows of one metered line
/// disagree on, in the spelling the design set names them.
///
/// Seven fields — `model_kind`, `billingGranularity`, `aggregationFunction`,
/// `aggregationGranularity`, `tierAggregationWindow`, `tierQualificationWindow`
/// and `package_size` (the D-82 / D-98 list, extended by D-122). Empty means the
/// two rows meter, derive and price the same way, and whatever else differs
/// between them is a **price** lever.
///
/// ## One list, because two guards ask the same question
///
/// The tier counter `Q` is keyed `(subscription, meter, dimensionKey, window)`
/// and belongs to the subscription's usage history rather than to any row. Two
/// mechanisms hand a subscriber from one row to another **without** resetting
/// it, and each has its own rule saying the denomination must not move across
/// the handover:
///
/// - **supersession** ([`SupersessionPair::mismatched_unit_fields`](crate::domain::rules::supersession::SupersessionPair::mismatched_unit_fields),
///   `inst-tb-supersession-units`), which adds `meter`, `dimensionKey` and the
///   `carry`-conditioned `included_allowance` to this list;
/// - **phase conversion**
///   ([`PhaseOverrideUnits`](crate::domain::plan_rules::phase_graph::PhaseOverrideUnits),
///   `inst-ph-override-units`, D-89), where a `per_hour` trial row converting
///   into a `per_day` evergreen row applies an hours-denominated `Q` to
///   day-denominated bands.
///
/// Both are the D-77 factor-of-24 class arriving through a different door, so
/// the list is written **once**. Two hand-maintained copies is exactly how that
/// class re-enters: the D-127 lesson is that a guard must not be able to differ
/// by which mechanism asked, and a list that can drift is a guard that differs.
///
/// The comparison is **symmetric** — it answers *which fields moved*, not which
/// way — so a caller passes its two rows in whatever order reads chronologically
/// at the call site.
///
/// `aggregationFunction`, `aggregationGranularity` and `tierQualificationWindow`
/// are read through the `effective_*` accessors and never through the raw
/// `Option`s: "authored nothing" and "authored the default" are the same row,
/// and comparing the `Option`s would report a change on a pair that changed
/// nothing at all.
///
/// ## The kind is compared twice, because a row has two of them (D-45)
///
/// `model_kind` is on this list because the **formula** that prices the
/// continued counter must not move (D-98). Since the included-allowance compile
/// the formula is the **presented** kind, and the two part company on exactly one
/// shape: an untiered `per_unit` row carrying `includedAllowance {N, none}`
/// publishes as the ladder `[0, N) @ $0`, `[N, null) @ rate` (`inst-ac-band`)
/// with its authored `model_kind` untouched (D-130 — the compile is a
/// projection, so there is nothing on the row for an authored comparison to
/// see).
///
/// Introducing or dropping that declaration mid-window is therefore the D-82
/// class through a field nobody had listed: the predecessor accumulated a
/// counter under no bands at all, and the successor applies a band set to it
/// whose first band is the allowance. Neither answer is defined — start `Q` at
/// zero and the subscriber is granted `N` free units mid-period on top of what
/// they already bought; continue it and the allowance is silently consumed by
/// quantity that was billed at full rate before it existed. The two differ by
/// `N × rate` on every subscriber on the key.
///
/// So the presented kind is compared as well, and reported under its own name
/// because the remediable operand is the **declaration**: an author diffing two
/// rows that both read `model_kind = per_unit` cannot act on a finding that
/// names `model_kind`.
///
/// A **quantity** change on a row that presents a ladder on both sides moves
/// nothing here and stays what the note below calls it — a price lever. That is
/// what the presented comparison buys over binding `included_allowance`
/// outright, and it is why the D-129 `carry` clause on
/// [`SupersessionPair::mismatched_unit_fields`](crate::domain::rules::supersession::SupersessionPair::mismatched_unit_fields)
/// stays where it is: that one binds a plan-scoped grant row and is a different
/// argument on the same field.
#[must_use]
pub fn unit_determining_mismatch(before: &PriceRow, after: &PriceRow) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if before.model_kind != after.model_kind {
        changed.push("model_kind");
    } else if presented_model_kind(before) != presented_model_kind(after) {
        changed.push("includedAllowance (presented modelKind)");
    }
    if before.billing_granularity != after.billing_granularity {
        changed.push("billingGranularity");
    }
    if before.effective_aggregation_function() != after.effective_aggregation_function() {
        changed.push("aggregationFunction");
    }
    if before.effective_aggregation_granularity() != after.effective_aggregation_granularity() {
        changed.push("aggregationGranularity");
    }
    if before.tier_aggregation_window != after.tier_aggregation_window {
        changed.push("tierAggregationWindow");
    }
    if before.effective_tier_qualification_window() != after.effective_tier_qualification_window() {
        changed.push("tierQualificationWindow");
    }
    if before.package_size != after.package_size {
        changed.push("package_size");
    }
    changed
}

/// The corpus / wire spelling of a model kind.
///
/// A free function rather than an inherent method because [`ModelKind`] belongs
/// to `bss_fixtures`. Exhaustive on purpose: a sixth kind cannot enter the enum
/// without this match being extended, and the spelling has to match the fixture
/// registry byte for byte — `PerUnit` is not a string that appears in
/// `registry.toml`, and it is not a token any rejection may render.
#[must_use]
pub const fn model_kind_wire(kind: ModelKind) -> &'static str {
    match kind {
        ModelKind::Flat => "flat",
        ModelKind::PerUnit => "per_unit",
        ModelKind::Graduated => "graduated",
        ModelKind::Volume => "volume",
        ModelKind::Package => "package",
    }
}

#[cfg(test)]
#[path = "price_row_tests.rs"]
mod price_row_tests;
