//! The artifact pricing's `FixtureGate` reads at publish time.
//!
//! Generated from the corpus by pricing's `regen_registry` example, committed,
//! and checked for freshness by that gear's `corpus_publish` test. The gate runs
//! inside a publish transaction and cannot run an oracle there, so it needs a
//! static answer.
//!
//! A row is keyed `(kind, variant)` — §6's `model_kind` / `variant` pair — so a
//! kind has as many rows as it has fixtures. This file is **not** the gear-side
//! table `pricing_conformance_fixture_registry`; see
//! `bss_pricing::infra::fixture_gate` for what the difference is and who owns
//! which.
//!
//! The generator is an example target of the **gear** rather than a binary of
//! either fixture crate because the two flags below are earned by two parties
//! that cannot see each other: the reference oracle lives in
//! `bss-fixtures-conformance`, pricing's validator lives in the gear, and the
//! harness is a dev-dependency of the gear so that no evaluator reaches it even
//! transitively. An example compiles with dev-dependencies, so it is the one
//! build in which both halves are visible.

use crate::kinds::ModelKind;
use crate::variant::Variant;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One row of the registry: what is known about **one fixture of one kind**.
///
/// Keyed by `(kind, variant)`, which is §6's `model_kind` / `variant` pair. Keyed
/// by kind alone the three cross-cutting variants gate nothing: a `peak` row
/// would publish on a `sum` row's fixture, because the only question asked would
/// be "is `graduated` green".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the three flags are the design: each is earned by a different party (oracle, pricing, rating) and collapsing them would hide which half is missing"
)]
pub struct VariantStatus {
    pub kind: ModelKind,
    /// Which of this kind's fixtures the row records. See [`Variant`].
    pub variant: Variant,
    /// The reference oracle reproduces every evaluation case for this kind.
    pub oracle: bool,
    /// Pricing's `PublishValidator` reproduces every publish case for this kind.
    /// A kind the corpus carries no publish case for earns nothing — absent
    /// coverage must never read as success.
    pub publish: bool,
    /// Rating's evaluator reproduces the same cases. False until rating exists;
    /// when it lands it must **agree** with the oracle, and disagreement reddens
    /// the corpus rather than overriding either side.
    pub rating: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default)]
    pub variants: Vec<VariantStatus>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

impl Registry {
    /// Reads the generated registry from disk.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if the file cannot be read or does not parse.
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let text = std::fs::read_to_string(path).map_err(|source| RegistryError::Io {
            path: path.display().to_string(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| RegistryError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// What `FixtureGate` asks, **once per variant a row requires**.
    ///
    /// Deliberately `oracle && publish`, not all three. The oracle is the
    /// executable §17.2 and stands in for Tariffs until Tariffs exists; making
    /// the gate wait for `rating` would block every publish at launch, since
    /// the rating gear has no code.
    ///
    /// An unregistered `(kind, variant)` pair is never open, and that is now the
    /// load-bearing half: `(volume, level_aggregation)` is absent because no
    /// case folds a level on a `volume` row, so a non-`sum` `volume` row is
    /// refused rather than admitted on `(volume, model_kind)`.
    ///
    /// Which variants a given row requires is the **gear's** question, not the
    /// registry's — see `bss_pricing::infra::fixture_gate::required_variants`.
    /// **Every row for the key must be green, not one of them.** Nothing forbids
    /// two rows under one `(kind, variant)` — `registry_gen::build` writes one row
    /// per `(family, gated kind)`, and four families map to `Variant::ModelKind`,
    /// so two of them gating one kind emit two rows for the same key. Under `any` a
    /// green sibling overrides a family that legitimately went red, and the refusal
    /// an operator greps this file for is simply absent.
    /// Emptiness still answers `false`, which is the unregistered-pair half above.
    #[must_use]
    pub fn gate_open_for(&self, kind: ModelKind, variant: Variant) -> bool {
        let mut rows = self
            .variants
            .iter()
            .filter(|v| v.kind == kind && v.variant == variant)
            .peekable();
        rows.peek().is_some() && rows.all(|v| v.oracle && v.publish)
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
