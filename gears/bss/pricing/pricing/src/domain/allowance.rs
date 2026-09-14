//! The D-45 included-allowance compile
//! (`cpt-cf-bss-pricing-algo-allowance-compile`, `inst-ac-band`,
//! `inst-ac-marker`, `inst-ac-deterministic`).
//!
//! An author declares `includedAllowance {quantity N, rolloverPolicy}` on a
//! `usage` row and publish **compiles** it. This module is that compile, and it
//! is a **pure function of the authored row** — no tenant state, no store, no
//! clock — which is the whole of `inst-ac-deterministic`: re-publish,
//! supersession, repricing and clone all recompile identically because the input
//! is the row's own truth.
//!
//! # It is a projection, never a write-back (D-130)
//!
//! The row's truth stays exactly as authored: the declaration, the authored
//! `model_kind`, the authored `unit_rate`, and the authored
//! `pricing_price_tier_band` rows are untouched. What this produces —
//! [`CompiledAllowance`] — is materialized into the **read model** and the
//! `pricingSnapshotRef` at publish by [`crate::domain::projection`], and nothing
//! here ever reaches a `Set(...)`.
//!
//! That is what makes the compile re-entrant. An in-place rewrite destroys its own
//! input — the authored bounds are unrecoverable after the first publish — so a
//! supersession, repricing successor or clone built from the published row carries
//! already-offset bands *plus* the declaration, trips `ALLOWANCE_DOUBLE_FREE`, and
//! blocks the reprice of every allowance-carrying row (D-130).
//!
//! # The compile presupposes the gate
//!
//! [`compile`] answers `None` for every row
//! [`crate::domain::rules::allowance`] refuses. The two read the same six
//! conditions, and they are written once here
//! ([`Admissibility::of`]) so that a row cannot be refused by the gate and
//! compiled by the projector, or the reverse. A gate without its compile
//! manufactures the false assurance D-177 refused; a compile without its gate
//! manufactures artifacts for a row nobody judged.
//!
//! # `carry` compiles to a grant, and that grant has no store here
//!
//! `inst-ac-band` is the **`rolloverPolicy = none`** branch and it is what this
//! module builds. `inst-ac-carry` materializes a `promotional` D-43 grant into
//! `pricing_plan_grant` (D-52) — a table this gear does not have; `grep` the
//! migration set and the only grant column is Slice 6's
//! `pricing_plan.entitlement_grants`, which is a different object. So a `carry`
//! declaration is still refused on every authoring path and again at publish
//! under `PRIMITIVE_RULES_UNBUILT`
//! ([`crate::domain::publish::rules::unjudged_primitives`]), and [`compile`]
//! answers `None` for it rather than silently applying the band branch — which
//! would grant the free quantity twice over, once as a `$0` band and once as a
//! per-period grant, the day the grant lands.

use bss_fixtures::ModelKind;
use toolkit_macros::domain_model;

use crate::domain::money::RateMinor;
use crate::domain::price_row::{BandTop, PriceRow, RolloverPolicy, TierBand};

/// Where an allowance marker came from.
///
/// One variant, and that is the point rather than an unfinished enum: a marker
/// exists **only** where the compile made one. `inst-ac-marker` states the
/// observable difference D-45 exists for — *"a hand-authored `$0` band carries
/// no marker"* — so "authored" is not a source a marker can have, and the read
/// model's `source` member says which of the two shapes a consumer is looking at
/// without the consumer having to infer it from a `$0` band.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AllowanceSource {
    /// Produced by this compile from an authored `includedAllowance`.
    Compiled,
}

impl AllowanceSource {
    /// The wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compiled => "compiled",
        }
    }
}

/// The first-class allowance marker frozen beside the compiled band set
/// (`inst-ac-marker`).
///
/// The read model serves display (*"includes N units"*) and the included-vs-billed
/// reporting split from **this**, never by inferring an allowance from a `$0`
/// band — the inference is exactly what D-45 replaced, because it cannot tell a
/// deliberate free opening band from a granted allowance and cannot express
/// `rolloverPolicy` at all.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AllowanceMarker {
    /// The included quantity, in the row's billable units.
    pub quantity: u64,
    /// The authored rollover policy, carried verbatim.
    pub rollover_policy: RolloverPolicy,
    /// Always [`AllowanceSource::Compiled`] — see that type.
    pub source: AllowanceSource,
}

/// What publish materializes for one allowance-carrying row.
///
/// Every member is **derived**; none of them is stored against the row.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledAllowance {
    /// The compiled band set: `[0, N) @ $0` followed by the authored bands
    /// offset by `+N`, or the two-band synthesis for an untiered row.
    pub bands: Vec<TierBand>,
    /// The kind the row is **presented** as. Always
    /// [`ModelKind::Graduated`] — the compiled artifact is a band ladder, and a
    /// band ladder is `graduated` whatever the row was authored as.
    pub presented_kind: ModelKind,
    /// The kind the row was **authored** as, retained beside the marker (D-59).
    /// `per_unit` here is what tells a reader the top band's rate is the folded
    /// `unitRateNanoMinor` rather than an authored band.
    pub authored_kind: ModelKind,
    /// The marker (`inst-ac-marker`).
    pub marker: AllowanceMarker,
}

/// Why a row does or does not compile.
///
/// Public because the gate rule reports one refusal per reason and this is the
/// single reading of the six conditions both surfaces share.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admissibility {
    /// The row carries no `includedAllowance` at all. Not a fault.
    NoDeclaration,
    /// `includedAllowance` on a row whose `chargeKind` is not `usage`.
    NonUsage,
    /// `quantity <= 0` — with a `u64` that is `0`, and it is still the design
    /// set's condition rather than a narrower one.
    QuantityInvalid,
    /// The row's kind has no band set to compile into (`package`), or is
    /// Tariffs Variant A where a `$0` first band is a cliff (`volume`), or
    /// prices no quantity at all (`flat`). `graduated` and untiered `per_unit`
    /// are the whole authorable set (D-59).
    KindUnsupported,
    /// A non-`sum` row — level-meter allowance is a named Future gate (D-44).
    NonSum,
    /// An authored `$0` first band beside the declaration: double free.
    DoubleFree,
    /// The row also carries reservation attributes.
    WithReservation,
    /// The kind is not authored yet. Not a fault — `inst-mk-explicit` owns that
    /// refusal, and re-deriving a per-kind consequence from a kind nobody
    /// authored fills the report with consequences of the one fault the author
    /// has to fix first.
    KindUnauthored,
    /// A `graduated` row with no band set, or a `per_unit` row with no
    /// `unit_rate`. Not a fault of this rule — the row has nothing to compile
    /// **into** or **from**, and `inst-tb-first` / `inst-mk-required` already
    /// refuse it — so no artifact is produced and no second refusal is added.
    NothingToCompileFrom,
    /// `rolloverPolicy = carry`, whose artifact is a `pricing_plan_grant` row
    /// this gear has no table for. Refused as `PRIMITIVE_RULES_UNBUILT`; see the
    /// module doc.
    CarryUnbuilt,
    /// The row compiles.
    Compiles,
}

impl Admissibility {
    /// Judge one row. The single reading both the gate and the compile use.
    ///
    /// The order is the order an author fixes the faults in and the order the
    /// gate reports them: whether the row may carry an allowance at all, whether
    /// the declaration is a number, whether the kind has a band set, whether the
    /// meter's shape admits one, and only then the two content collisions.
    ///
    /// It answers **one** reason where the gate reports several, because a
    /// compile is a yes/no and the artifact must not depend on which fault was
    /// read first. [`crate::domain::rules::allowance`] re-reads the individual
    /// conditions to enumerate them; this is what decides whether an artifact
    /// exists.
    #[must_use]
    pub fn of(row: &PriceRow) -> Self {
        let Some(allowance) = row.included_allowance else {
            return Self::NoDeclaration;
        };
        if !row.is_usage() {
            return Self::NonUsage;
        }
        if allowance.quantity == 0 {
            return Self::QuantityInvalid;
        }
        let Some(kind) = row.model_kind else {
            return Self::KindUnauthored;
        };
        if !matches!(kind, ModelKind::Graduated | ModelKind::PerUnit) {
            return Self::KindUnsupported;
        }
        if row.is_level() {
            return Self::NonSum;
        }
        if row.is_reserved() {
            return Self::WithReservation;
        }
        if authors_a_free_first_band(row) {
            return Self::DoubleFree;
        }
        if allowance.rollover_policy.is_carry() {
            return Self::CarryUnbuilt;
        }
        match kind {
            ModelKind::Graduated if row.bands.is_empty() => Self::NothingToCompileFrom,
            ModelKind::PerUnit if row.unit_rate.is_none() => Self::NothingToCompileFrom,
            _ => Self::Compiles,
        }
    }
}

/// Does the row author a `$0` band at the origin (`ALLOWANCE_DOUBLE_FREE`)?
///
/// Judged over the band set **sorted by `from_qty`**, for `BandGeometry`'s
/// reason: the persisted band table is keyed `(price_id, from_qty)` and carries
/// no ordinal, so authoring order does not survive a round trip and a rule that
/// read it would answer differently at save and at publish.
///
/// Since D-130 this reads what it says again: the compiler no longer writes its
/// own `[0, N) @ $0` band back onto the row, so the only way a row reaches here
/// with a free opening band is that somebody authored one.
#[must_use]
pub fn authors_a_free_first_band(row: &PriceRow) -> bool {
    row.bands
        .iter()
        .min_by_key(|band| band.from_qty)
        .is_some_and(|first| first.from_qty == 0 && first.unit_price_rate == RateMinor::ZERO)
}

/// Compile a row's allowance, or answer `None` when it has none or carries one
/// the gate refuses.
///
/// # `none` — the band compile (`inst-ac-band`)
///
/// - **tiered (`graduated`)**: prepend `[0, N) @ $0` and offset every authored
///   bound by `+N`, so the authored `[0, X)` becomes `[N, N+X)`;
/// - **untiered (`per_unit`)**: synthesize `[0, N) @ $0` and `[N, null) @ rate`,
///   folding the authored `unit_rate` into the top band and presenting the kind
///   as `graduated`.
///
/// The second bullet is where the design set has gone stale and it is corrected
/// rather than followed: `inst-ac-band` says the compile moves *"the authored
/// `amount_minor`"* into the top band, which was true until **D-311**
/// split the `per_unit` rate out of `amount_minor` into `unit_rate` — a rate is
/// not an amount, and a `per_unit` row has carried `amount_minor = NULL` ever
/// since. Folding `amount_minor` would fold a `NULL`.
///
/// # Overflow saturates, and the saturation is judged as itself
///
/// `from_qty + N` is a `saturating_add`, because panicking or silently wrapping
/// would both turn an authoring error into something other than a validation
/// report.
///
/// **A saturated bound is not reliably visible in the compiled geometry.**
/// With the authored ladder
/// `[0, 1000)`, `[1000, null)` and `N = u64::MAX - 5`, the compiled set is
/// `[0, N) @ $0`, `[N, u64::MAX)`, `[u64::MAX, null)` — ascending,
/// non-overlapping, contiguous, no zero-width band, open at the top. Every one
/// of the three Slice-3 band rules passes it while the authored 1000-unit band
/// is materialized **five** units wide. Only where both bounds saturate onto
/// each other (`N = u64::MAX` on that ladder) does the geometry itself fail, so
/// *"publish still validates the compiled set"* catches the loud half and misses
/// the quiet one.
///
/// So the saturation is detected as itself, by [`offsets_saturate`], and
/// `inst-ac-band`'s rule refuses on it. The compile still produces the saturated
/// set rather than answering `None`: what it produces is never stored (D-130),
/// the geometry rules keep reporting the collapsed shapes they can see, and the
/// row does not publish either way.
#[must_use]
pub fn compile(row: &PriceRow) -> Option<CompiledAllowance> {
    if Admissibility::of(row) != Admissibility::Compiles {
        return None;
    }
    let allowance = row.included_allowance?;
    let quantity = allowance.quantity;
    let authored_kind = row.model_kind?;

    let free = TierBand::closed(0, quantity, RateMinor::ZERO);
    let bands = if authored_kind == ModelKind::PerUnit {
        vec![free, TierBand::open(quantity, row.unit_rate?)]
    } else {
        let mut authored = row.bands.clone();
        authored.sort_by_key(|band| band.from_qty);
        let mut bands = vec![free];
        bands.extend(authored.into_iter().map(|band| TierBand {
            from_qty: band.from_qty.saturating_add(quantity),
            to_qty: match band.to_qty {
                BandTop::Open => BandTop::Open,
                BandTop::Closed(top) => BandTop::Closed(top.saturating_add(quantity)),
            },
            unit_price_rate: band.unit_price_rate,
        }));
        bands
    };

    Some(CompiledAllowance {
        bands,
        presented_kind: ModelKind::Graduated,
        authored_kind,
        marker: AllowanceMarker {
            quantity,
            rollover_policy: allowance.rollover_policy,
            source: AllowanceSource::Compiled,
        },
    })
}

/// Does offsetting this row's authored ladder by its allowance overflow `u64`?
///
/// The one fault [`compile`] can introduce that the authored set does not
/// already have, and one the compiled geometry does **not** reliably show:
/// see [`compile`]'s own doc
/// for the `u64::MAX - 5` ladder that saturates into a perfectly well-formed
/// set five units narrower than it was authored.
///
/// Read against the **authored** bounds, which is where the addition happens.
/// The untiered branch adds nothing to anything — `[0, N)` and `[N, null)` are
/// synthesized, not offset — so it cannot overflow and this answers `false` for
/// it at every `N`.
///
/// Answers `false` for a row with no compilable allowance: there is no offset to
/// overflow, and a row the gate already refuses must not collect a second
/// refusal derived from an artifact it will never have.
#[must_use]
pub fn offsets_saturate(row: &PriceRow) -> bool {
    if Admissibility::of(row) != Admissibility::Compiles {
        return false;
    }
    let Some(allowance) = row.included_allowance else {
        return false;
    };
    if row.model_kind == Some(ModelKind::PerUnit) {
        return false;
    }
    row.bands.iter().any(|band| {
        band.from_qty.checked_add(allowance.quantity).is_none()
            || band
                .to_qty
                .closed_at()
                .is_some_and(|top| top.checked_add(allowance.quantity).is_none())
    })
}

/// The kind the row publishes as: the compiled kind where one exists, else the
/// authored one.
///
/// `inst-ac-band` fixture-gates an allowance row **on the compiled kind**, and
/// this is the read that makes that true: an untiered `per_unit` row carrying an
/// allowance is evaluated as a band ladder, so gating it on `per_unit`'s fixture
/// would admit a shape no joint fixture covers.
#[must_use]
pub fn presented_model_kind(row: &PriceRow) -> Option<ModelKind> {
    compile(row).map_or(row.model_kind, |compiled| Some(compiled.presented_kind))
}

/// The band set the row publishes with: the compiled set where one exists, else
/// the authored one.
///
/// What a consumer rates from, and therefore what a rule about *billed* quantity
/// reads — `inst-ft-warn`'s allowance half is the one that needed it.
#[must_use]
pub fn presented_bands(row: &PriceRow) -> Vec<TierBand> {
    compile(row).map_or_else(|| row.bands.clone(), |compiled| compiled.bands)
}

/// Is the row's **presented** shape a tiered usage row?
///
/// [`PriceRow::is_tiered`] answers about the authored kind, which is what every
/// Slice-3 shape rule must read. The fixture gate asks the other question: a
/// compiled `per_unit` row needs the tiered-usage variants because a band ladder
/// is what publishes.
#[must_use]
pub fn is_presented_tiered(row: &PriceRow) -> bool {
    matches!(
        presented_model_kind(row),
        Some(ModelKind::Graduated | ModelKind::Volume)
    )
}

#[cfg(test)]
#[path = "allowance_tests.rs"]
mod allowance_tests;
