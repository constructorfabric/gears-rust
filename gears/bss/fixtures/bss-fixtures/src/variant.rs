//! The **variant** axis of the conformance registry.
//!
//! `design/03-price-structure.md` §6 keys the registry by `model_kind` /
//! `variant`, and the gate rules are written in variants, not in kinds:
//!
//! - `inst-la-fixture` — the `level-aggregation` variant is a registered
//!   `FixtureGate` variant, and publish of any non-`sum` row without its green
//!   joint fixture is blocked "exactly like a `modelKind` variant";
//! - §6 / D-22 — `variant = supersession_continuity` is registered on the
//!   **tiered** kinds, and the continuity fixture gates the first publish of any
//!   tiered usage kind, *alongside* that kind's own fixture;
//! - `inst-fx-kinds` — the reservation variant of a usage row requires its own
//!   fixture (Slice 10 registers it into this gate).
//!
//! Keyed by kind alone, three of those say nothing: a `peak` row publishes on
//! the strength of a `sum` row's fixture, and the level-aggregation,
//! supersession-continuity and reservation variants gate nothing at all.
//!
//! ## The families are the variants
//!
//! This is not a second vocabulary invented beside [`crate::corpus::FamilyMeta`].
//! A variant is what a family **is**, and it is read off the family itself
//! ([`crate::model::Family::variant`]) rather than authored again in
//! `_family.toml` — so a family and its variant cannot drift, and adding a
//! family without deciding what it gates is a compile error rather than a quiet
//! omission.
//!
//! Kept out of the `corpus` feature because [`crate::registry`] is keyed by it,
//! and a gear compiles the registry alone.

use serde::{Deserialize, Serialize};

/// Which fixture of a `modelKind` a registry row records.
///
/// A row is `(kind, variant)`. One kind therefore has several rows, and they are
/// earned independently: `graduated` may have a green `model_kind` fixture and
/// no `level_aggregation` one, which is exactly the state in which a `peak`
/// `graduated` row must not publish and a `sum` one may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    /// The kind's own fixture (`inst-fx-gate`, `inst-fx-kinds`). Every row
    /// needs it; `package` (repeating-block) and `per_unit` (external-quantity)
    /// are the two the design set names explicitly.
    ModelKind,
    /// The granule fold, the late-sample re-fold and the `maxHold` gap (D-44,
    /// PRD §13/§17.2) — required by any non-`sum` row (`inst-la-fixture`).
    LevelAggregation,
    /// The mid-window supersession and phase-conversion continuity scenario
    /// (`inst-tb-window-continuity`), required by any **tiered usage** kind
    /// before its first publish (D-22).
    SupersessionContinuity,
    /// The reservation scenario (`inst-rv-tier-q`, `inst-rv-level`, D-53,
    /// D-139), required by a reserved usage row. Slice 10 owns the field that
    /// makes a row reserved, so the pricing gear cannot yet *see* one — see
    /// `bss_pricing::infra::fixture_gate::Reservation`, where that hole is named
    /// and tested rather than left silent.
    Reserved,
}

impl Variant {
    /// Every variant the registry knows.
    pub const ALL: [Self; 4] = [
        Self::ModelKind,
        Self::LevelAggregation,
        Self::SupersessionContinuity,
        Self::Reserved,
    ];

    /// The registry / wire spelling.
    ///
    /// Exhaustive on purpose, and not `Debug`: it is the token a refusal renders
    /// and the token an operator greps `registry.toml` for, so it has to match
    /// the file byte for byte.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::ModelKind => "model_kind",
            Self::LevelAggregation => "level_aggregation",
            Self::SupersessionContinuity => "supersession_continuity",
            Self::Reserved => "reserved",
        }
    }
}
