//! Backward compatibility against one baseline (ADR-0003, SPEC §8.1 step 3).
//!
//! ```text
//! not a Type Schema       -> no comparison; validate the Instance instead
//! major 0                 -> no comparison (ADR-0015)
//! vM~, creation           -> no comparison
//! vM~, revision           -> current revision; never waivable
//! vM.0~                   -> no comparison; opens the major
//! vM.n~, n > 0, creation  -> current definition of vM.(n-1)~; waivable
//! ```
//!
//! Both edges require `Valid(baseline) ⊆ Valid(candidate)`. Floating `$ref`s
//! follow intra-entity revisions automatically, so only cross-minor checks
//! may be waived (ADR-0004). Transitivity avoids rechecking history.
//!
//! Contiguity fixes the predecessor identifier; searching for the highest
//! admitted minor would make selection race-dependent. Deleted predecessors
//! remain baselines.

use gts::{CompatibilityVerdict, GtsId, GtsIdSegment, GtsStore, SchemaComparison, StoreError};
use toolkit_macros::domain_model;

use super::sides::{BaselineDoc, CandidateDoc};
use crate::domain::admission::{AdmissionFailureReason, Precondition};
use crate::domain::family::{VersionProbe, version_probe};

/// Cases with no comparison owed, distinct from a refused or undecidable comparison.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exemption {
    /// An Instance is validated against a Type Schema revision, never compared
    /// against a previous value.
    NotATypeSchema,
    /// ADR-0015: major 0 is an unstable profile that enforces no mode, so it
    /// carries no whole-history guarantee to protect.
    MajorZero,
    /// A first admission of a major-only identifier: nothing precedes it.
    FirstAdmission,
    /// `vM.0~` opens its major, so it has no preceding minor (ADR-0004).
    FirstMinor,
}

impl Exemption {
    /// A stable token naming why no comparison was owed.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotATypeSchema => "exempt_not_a_type_schema",
            Self::MajorZero => "exempt_major_zero",
            Self::FirstAdmission => "exempt_first_admission",
            Self::FirstMinor => "exempt_first_minor",
        }
    }
}

/// The definition one candidate is checked against.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// No comparison is owed. Carries which case it was, so a span can record it.
    Exempt(Exemption),
    /// The candidate's own current revision — intra-entity, never waivable.
    CurrentRevision,
    /// The current definition of the preceding minor — cross-minor, waivable by
    /// `force` where the deployment permits it.
    PrecedingMinor { gts_id: String },
}

/// The last segment has no readable major. Return an error so this cannot be
/// mistaken for an exemption or a non-waivable baseline.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "'{gts_id}' names no readable major in its last segment, so no compatibility baseline can be selected"
)]
pub struct UnreadableVersion {
    pub gts_id: String,
}

impl UnreadableVersion {
    /// The vocabulary code a refusal carries. An associated constant rather than a
    /// method: the code does not depend on which identifier was unreadable.
    pub const REASON: AdmissionFailureReason = AdmissionFailureReason::UnreadableVersion;
}

impl Baseline {
    /// Whether `force` has this check to waive: the cross-minor edge, and only it.
    #[must_use]
    pub const fn waivable(&self) -> bool {
        matches!(self, Self::PrecedingMinor { .. })
    }

    /// Stable span token for the selection or exemption; identifiers are separate fields.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Exempt(exemption) => exemption.label(),
            Self::CurrentRevision => "current_revision",
            Self::PrecedingMinor { .. } => "preceding_minor",
        }
    }
}

/// Select the baseline from the identifier and accepted precondition, without I/O.
/// Revisions use the non-waivable current definition. This also protects the
/// minor-bearing revision arm, although acceptance rejects it (ADR-0004).
///
/// # Errors
/// [`UnreadableVersion`] if the last segment has no readable major.
pub fn select_baseline(
    id: &GtsId,
    precondition: Precondition,
) -> Result<Baseline, UnreadableVersion> {
    let unreadable = || UnreadableVersion {
        gts_id: id.id().to_owned(),
    };
    if !id.is_type() {
        return Ok(Baseline::Exempt(Exemption::NotATypeSchema));
    }
    // Asked before the minor arithmetic: quarantine is a property of the major, so
    // `v0.1~` is exempt rather than compared against `v0.0~`.
    match id.segments().last().and_then(GtsIdSegment::ver_major_opt) {
        None => return Err(unreadable()),
        Some(0) => return Ok(Baseline::Exempt(Exemption::MajorZero)),
        Some(_) => {}
    }
    // Reuse the contiguity probe so both rules name the same predecessor.
    let Some(probe) = version_probe(id) else {
        return Err(unreadable());
    };
    Ok(match (probe, precondition) {
        // Nothing precedes a first admission, and `vM.0~` opens its major.
        (VersionProbe::MajorOnly { .. }, Precondition::MustNotExist) => {
            Baseline::Exempt(Exemption::FirstAdmission)
        }
        (VersionProbe::FirstMinor { .. }, Precondition::MustNotExist) => {
            Baseline::Exempt(Exemption::FirstMinor)
        }
        (VersionProbe::LaterMinor { predecessor, .. }, Precondition::MustNotExist) => {
            Baseline::PrecedingMinor {
                gts_id: predecessor,
            }
        }
        // Every revision uses the strict edge, including unreachable minor revisions.
        (_, Precondition::Version(_)) => Baseline::CurrentRevision,
    })
}

/// Compare resolved documents through P0's sole comparison entry point.
/// `is_minor_compatible` uses unresolved content and can misclassify levels
/// closed through `$ref`. Distinct side types prevent argument transposition.
///
/// # Errors
/// [`StoreError`] if either side has an unresolvable reference; no verdict is produced.
pub fn backward_comparison(
    store: &GtsStore,
    baseline: BaselineDoc<'_>,
    candidate: CandidateDoc<'_>,
) -> Result<SchemaComparison, StoreError> {
    store.compare_documents(baseline.get(), candidate.get())
}

/// Map a verdict to admission: `None` admits, `Some` gives the refusal reason.
/// The caller must authorize `forced` against the deployment and cross-minor
/// baseline. It waives the check regardless of its verdict (ADR-0004).
#[must_use]
pub const fn refusal(
    verdict: CompatibilityVerdict,
    forced: bool,
) -> Option<AdmissionFailureReason> {
    match verdict {
        CompatibilityVerdict::Compatible => None,
        _ if forced => None,
        // Keep undecidable and incompatible verdicts distinct (ADR-0003, SPEC §16.12).
        CompatibilityVerdict::Incompatible => {
            Some(AdmissionFailureReason::IncompatibleWithBaseline)
        }
        CompatibilityVerdict::Unknown => Some(AdmissionFailureReason::CompatibilityUndecidable),
    }
}

#[cfg(test)]
#[path = "baseline_tests.rs"]
mod baseline_tests;
