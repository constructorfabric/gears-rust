//! The Slice-3 **price-row shape rules** (`design/03-price-structure.md`).
//!
//! Each instruction of the slice becomes one
//! [`ValidationRule`](crate::domain::validation::ValidationRule) over a
//! [`PriceRow`], registered into the Foundation's pipeline. The rules append and
//! never short-circuit: a row with a missing kind *and* a closed top band
//! reports both, because the author remediates a plan in one pass.
//!
//! The `const` codes below are the contract. They are the machine-readable
//! discriminators the design set names verbatim (§5), they are what the
//! conformance corpus's publish cases assert, and they are what an RFC 9457
//! response carries — so they are written once, here, and referenced everywhere
//! rather than spelled at each `report.violate` call site.
//!
//! ## What is deliberately absent
//!
//! `inst-la-units` (the meter must be gauge-kind and the SKU-declared billable
//! unit must equal `level unit x granule`) and `inst-la-composite` (a non-`sum`
//! function on a derived meter) are **not implemented here**. Both are
//! cross-entity checks against the product registry, and this gear has no
//! registry client yet. They are left out rather than stubbed: a rule that
//! always passes is indistinguishable from a rule that holds, and it would make
//! `LEVEL_UNIT_MISMATCH` and `LEVEL_COMPOSITE_FORBIDDEN` read as enforced when
//! nothing enforces them.

pub mod allowance;
pub mod floor_typing;
pub mod level_aggregation;
pub mod model_kind;
pub mod package;
pub mod reservation;
pub mod supersession;
pub mod tier_bands;

use crate::domain::price_row::PriceRow;
use crate::domain::validation::ValidationPipeline;

pub use supersession::SupersessionPair;

// ---------------------------------------------------------------------------
// Blocking codes (design/03-price-structure.md 5)
// ---------------------------------------------------------------------------

/// No explicit `modelKind`. "Tiered (unspecified)" is not publishable.
pub const MODEL_KIND_MISSING: &str = "MODEL_KIND_MISSING";

/// The kind is not legal on this `chargeKind` (D-18).
pub const MODEL_KIND_CHARGEKIND_MISMATCH: &str = "MODEL_KIND_CHARGEKIND_MISMATCH";

/// A non-usage `per_unit` row has no `quantitySource`, or a `manual` source has
/// no quantity.
pub const QUANTITY_SOURCE_MISSING: &str = "QUANTITY_SOURCE_MISSING";

/// `amount_minor` is absent where the kind carries its money, or present where
/// the money lives in the band / package column.
pub const AMOUNT_PLACEMENT_INVALID: &str = "AMOUNT_PLACEMENT_INVALID";

/// The package block fields are missing, out of range, or on a row whose kind
/// does not price in blocks — including a `package` row that also carries bands.
pub const PACKAGE_FIELDS_INVALID: &str = "PACKAGE_FIELDS_INVALID";

/// An evaluation-policy or quantity field on a row whose kind and charge
/// component do not admit it.
pub const EVAL_POLICY_MISPLACED: &str = "EVAL_POLICY_MISPLACED";

/// A usage row without `billingGranularity`, or a tiered / `package` usage row
/// without `tierAggregationWindow`.
pub const EVAL_POLICY_MISSING: &str = "EVAL_POLICY_MISSING";

/// The two windows are each legal and cannot be combined: an hourly tier counter
/// under a billable unit that spans longer than an hour (D-313).
///
/// A separate code from [`EVAL_POLICY_MISPLACED`] because the fault is neither a
/// missing field nor a field on the wrong kind — both values are authorable and
/// both are correct alone. Named after the reason, as this gear's codes are, and
/// deliberately the same shape as `TIER_QUAL_WINDOW_INCOMPATIBLE`, which D-60
/// minted for the other window pair that cannot meet.
pub const TIER_AGG_WINDOW_INCOMPATIBLE: &str = "TIER_AGG_WINDOW_INCOMPATIBLE";

/// Two bands cover the same quantity — including a band set that is not
/// ascending.
pub const TIER_BANDS_OVERLAP: &str = "TIER_BANDS_OVERLAP";

/// The band set leaves a quantity unpriced: a hole between two bands, or a
/// first band that does not start at the origin.
pub const TIER_BANDS_GAP: &str = "TIER_BANDS_GAP";

/// A band covers no quantity (`toQty <= fromQty`).
pub const TIER_BAND_EMPTY: &str = "TIER_BAND_EMPTY";

/// The top band is closed (D-17). Any quantity must stay rateable.
pub const TIER_TOP_CLOSED: &str = "TIER_TOP_CLOSED";

/// A level-aggregation field is on a row that admits none, is missing where one
/// is required, or is out of range (D-44).
pub const LEVEL_FIELDS_INVALID: &str = "LEVEL_FIELDS_INVALID";

/// On a non-`sum` row `billingGranularity` is not the counterpart of
/// `aggregationGranularity` (D-77).
pub const LEVEL_GRANULARITY_MISMATCH: &str = "LEVEL_GRANULARITY_MISMATCH";

/// A successor on an occupied published scope key changes a field the continued
/// `Q` is denominated in, derived from, or priced by (D-82 / D-98 / D-122 /
/// D-127 / D-129).
pub const SUPERSESSION_UNIT_MISMATCH: &str = "SUPERSESSION_UNIT_MISMATCH";

/// A composite meter naming fewer than two constituents (S10 §5,
/// `inst-cm-constituents`). Its sibling `COMPOSITE_CONSTITUENT_UNPUBLISHED` is
/// declared in the design set and **raised nowhere**, because this gear has no
/// registry to ask whether a unit is published — see `plan_rules::composite`.
pub const COMPOSITE_TOO_FEW_CONSTITUENTS: &str = "COMPOSITE_TOO_FEW_CONSTITUENTS";

/// A composite meter that resolves to itself through its constituents, directly
/// or transitively (S10 §5, `inst-cm-formula`).
pub const COMPOSITE_SELF_REFERENCE: &str = "COMPOSITE_SELF_REFERENCE";

/// A band prices a unit **above** the band below it — the non-volume-discount
/// pattern.
///
/// Advisory, never blocking: an increasing ladder is unusual but legitimate
/// (congestion pricing, penalty tiers), so the author is told and the publish
/// proceeds. The design set states the finding (`inst-tb-order`) without naming
/// a code; this is the discriminator the gear reports it under, in the shape the
/// rest of the code set uses.
pub const TIER_BAND_PRICE_INCREASE: &str = "TIER_BAND_PRICE_INCREASE";

/// Every Slice-3 row-local rule, in report order.
///
/// Ordered by theme — kind, then bands, then package, then level aggregation —
/// so a report reads top-down the way the row is authored. All of these are
/// **row-local** (D-21), which is why they are one pipeline: they run at save
/// *and* again inside the publish commit, and neither run may depend on the
/// other.
///
/// The supersession guard is **not** here. It judges a pair, not a row, so it
/// has its own subject type and its own pipeline
/// ([`supersession_rules`]).
#[must_use]
pub fn price_row_rules() -> ValidationPipeline<PriceRow> {
    ValidationPipeline::new()
        .with_rule(Box::new(model_kind::ExplicitModelKind))
        .with_rule(Box::new(model_kind::KindRequiredFields))
        .with_rule(Box::new(model_kind::KindForbiddenFields))
        .with_rule(Box::new(model_kind::KindChargeKindMatrix))
        .with_rule(Box::new(tier_bands::BandGeometry))
        .with_rule(Box::new(tier_bands::BandOrigin))
        .with_rule(Box::new(tier_bands::BandTopOpen))
        .with_rule(Box::new(tier_bands::UsageEvaluationPolicy))
        .with_rule(Box::new(package::PackageFields))
        .with_rule(Box::new(package::PackageWindow))
        .with_rule(Box::new(level_aggregation::LevelFields))
        .with_rule(Box::new(level_aggregation::LevelGranularityPairing))
        .with_rule(Box::new(level_aggregation::LevelMaxHold))
        // Slice 10's reservation set (`inst-rv-attrs` / `inst-rv-usage` /
        // `inst-rv-level`). Row-local by construction -- see that module's doc
        // for why they are here rather than in the Foundation plan set, and for
        // what the corpus could not reach while they were not.
        .with_rule(Box::new(reservation::ReservationWellFormed))
        // Slice 10's allowance gate and the compiled set it admits
        // (`inst-ac-gate` / `inst-ac-band`, D-45). Row-local for the reservation
        // set's reason, and registered **after** it so that
        // `ALLOWANCE_WITH_RESERVATION` reads back below the reservation's own
        // findings -- an author who authored both a broken reservation and an
        // allowance beside it fixes the reservation first.
        .with_rule(Box::new(allowance::AllowanceAuthorable))
        .with_rule(Box::new(allowance::CompiledAllowanceWellFormed))
        // Slice 10's floor typing (`inst-ft-fallback` / `inst-ft-warn`).
        // Row-local for the reservation set's reason; see that module for why
        // `FLOOR_TYPE_MISSING` has no rule and where it is owed.
        .with_rule(Box::new(floor_typing::FloorFallbackDeclared))
        .with_rule(Box::new(floor_typing::FloorOutsideBands))
}

/// The supersession unit guard, as a pipeline over a predecessor/successor pair.
///
/// One rule, deliberately: the guard binds the **key**, not the mechanism, so
/// there is exactly one of it and both sanctioned producers of
/// `published -> superseded` run the same pipeline over the same subject
/// (D-127).
#[must_use]
pub fn supersession_rules() -> ValidationPipeline<SupersessionPair> {
    ValidationPipeline::new().with_rule(Box::new(supersession::SupersessionUnitGuard))
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod rules_tests;
