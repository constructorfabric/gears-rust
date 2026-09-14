//! The catalog kind enum — the whole of what a gear needs at publish time.
//!
//! Kept in its own module so the production surface stays minimal: pricing's
//! `FixtureGate` needs to ask "is this kind green", which takes this enum and
//! [`crate::registry`] and nothing else. Everything that reads the corpus from
//! disk sits behind the `corpus` feature.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Flat,
    PerUnit,
    Graduated,
    Volume,
    Package,
}

impl ModelKind {
    /// The catalog's complete kind enum (`PRD.md` 6.2). Every one of these must
    /// be gated by some family, or `inst-fx-gate` blocks a kind nothing covers
    /// — the hole `flat` fell into.
    pub const ALL: [Self; 5] = [
        Self::Flat,
        Self::PerUnit,
        Self::Graduated,
        Self::Volume,
        Self::Package,
    ];
}
