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

use std::collections::HashSet;
use std::mem::discriminant;

use gts::schema_traits::X_GTS_TRAITS_SCHEMA;
use gts::{
    CompatibilityDiagnostic, CompatibilityVerdict, GtsId, GtsIdSegment, GtsStore, SchemaComparison,
    StoreError,
};
use serde_json::{Value, json};
use toolkit_macros::domain_model;

use super::derivation::normalize_dialect;
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
fn backward_comparison(
    store: &GtsStore,
    baseline: BaselineDoc<'_>,
    candidate: CandidateDoc<'_>,
) -> Result<SchemaComparison, StoreError> {
    store.compare_documents(baseline.get(), candidate.get())
}

/// Which of the two contracts could not be compared. The message names the
/// traits side, since the payload's resolution error alone points at the host.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum ComparisonFailure {
    #[error("{0}")]
    Payload(#[source] StoreError),
    #[error("x-gts-traits-schema: {0}")]
    TraitsSchema(#[source] StoreError),
}

/// Whether the candidate's chain still declares `x-gts-traits-schema`. GTS
/// materializes the same dialect-only contract for an explicit `{}` and for no
/// declaration at all, yet refuses every trait value in the latter case, so the
/// withdrawal is carried beside the artifact.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraitsDeclaration {
    /// The candidate's chain declares as much as the baseline's did.
    Kept,
    /// The candidate withdrew the last declaration the baseline's chain had.
    Withdrawn,
}

impl TraitsDeclaration {
    /// Withdrawn when the baseline declared at its own level, the candidate does
    /// not, and no ancestor does. Ancestors are shared by both sides: the same
    /// identifier, or a preceding minor under the same parent prefix.
    pub fn between(
        baseline: BaselineDoc<'_>,
        candidate: CandidateDoc<'_>,
        ancestors_declare: impl FnOnce() -> bool,
    ) -> Self {
        // Trait schemas may appear only at the top level (GTS §9.7.1).
        let declares = |document: &Value| document.get(X_GTS_TRAITS_SCHEMA).is_some();
        if declares(baseline.get()) && !declares(candidate.get()) && !ancestors_declare() {
            Self::Withdrawn
        } else {
            Self::Kept
        }
    }
}

/// The contract of a chain with no traits declaration: only the empty object.
fn no_trait_values() -> Value {
    json!({ "type": "object", "maxProperties": 0 })
}

/// The verdict over both contracts, with each contract's evidence kept apart so
/// a refusal can say which one it is about.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evolution {
    pub verdict: CompatibilityVerdict,
    pub payload: Vec<CompatibilityDiagnostic>,
    pub traits: Vec<CompatibilityDiagnostic>,
}

/// Both the payload and the effective trait values must remain admissible.
///
/// # Errors
/// [`ComparisonFailure`] naming the contract that could not be resolved.
pub fn evolution_comparison(
    store: &GtsStore,
    baseline: BaselineDoc<'_>,
    candidate: CandidateDoc<'_>,
    baseline_traits: BaselineDoc<'_>,
    candidate_traits: CandidateDoc<'_>,
    declaration: TraitsDeclaration,
) -> Result<Evolution, ComparisonFailure> {
    let payload =
        backward_comparison(store, baseline, candidate).map_err(ComparisonFailure::Payload)?;
    let undeclared;
    let candidate_traits = match declaration {
        TraitsDeclaration::Kept => candidate_traits,
        TraitsDeclaration::Withdrawn => {
            undeclared = no_trait_values();
            CandidateDoc::new(&undeclared)
        }
    };
    let (traits_verdict, traits_diagnostics) = traits_comparison(
        store,
        baseline,
        candidate,
        baseline_traits,
        candidate_traits,
    )
    .map_err(ComparisonFailure::TraitsSchema)?;
    Ok(Evolution {
        verdict: all_hold(payload.backward_compatibility(), traits_verdict),
        payload: payload.backward_diagnostics,
        traits: traits_diagnostics,
    })
}

/// Trait values are objects, validated against the effective traits schema of
/// the type being validated: the host itself, or any descendant inheriting the
/// contract. A self-relative operand such as `/$id` binds to that type, so the
/// contract is compared under the host's identity and with the operand left
/// unbound, standing for a descendant's. Both comparisons must prove inclusion;
/// an unbound operand that cannot be proven fails closed as undecided. A final
/// baseline has no descendants, so only the host reading is owed.
fn traits_comparison(
    store: &GtsStore,
    baseline: BaselineDoc<'_>,
    candidate: CandidateDoc<'_>,
    baseline_traits: BaselineDoc<'_>,
    candidate_traits: CandidateDoc<'_>,
) -> Result<(CompatibilityVerdict, Vec<CompatibilityDiagnostic>), StoreError> {
    let contract = |host: &Value, traits: &Value, as_host: bool| {
        let mut contract = json!({
            "$schema": host["$schema"],
            "type": "object",
            "allOf": [traits],
        });
        if as_host {
            contract["$id"] = host["$id"].clone();
        }
        contract
    };
    let mut verdict = CompatibilityVerdict::Compatible;
    let mut diagnostics: Vec<CompatibilityDiagnostic> = Vec::new();
    let mut reported = HashSet::new();
    if same_contract(baseline, candidate, baseline_traits, candidate_traits) {
        return Ok((verdict, diagnostics));
    }
    let final_baseline =
        baseline.get().get(gts::schema_modifiers::X_GTS_FINAL) == Some(&Value::Bool(true));
    let readings: &[bool] = if final_baseline {
        &[true]
    } else {
        &[true, false]
    };
    for &as_host in readings {
        let comparison = store.compare_documents(
            &contract(baseline.get(), baseline_traits.get(), as_host),
            &contract(candidate.get(), candidate_traits.get(), as_host),
        )?;
        verdict = all_hold(verdict, comparison.backward_compatibility());
        // The readings word one finding differently; report each place and kind once.
        for diagnostic in comparison.backward_diagnostics {
            if reported.insert((diagnostic.path.clone(), discriminant(&diagnostic.finding))) {
                diagnostics.push(diagnostic);
            }
        }
    }
    Ok((verdict, diagnostics))
}

/// An untouched traits contract of the same type admits the same values. GTS
/// cannot always prove that structurally (an `additionalProperties`
/// intersection is never proven), so identity is decided here. The `$id` must
/// match exactly: `/$id` means a different type under a different identifier.
/// Dialects are compared by meaning where they are declared, the host and the
/// root GTS pins on the effective schema; everything else is compared verbatim,
/// since a `$schema` key elsewhere may be literal data.
fn same_contract(
    baseline: BaselineDoc<'_>,
    candidate: CandidateDoc<'_>,
    baseline_traits: BaselineDoc<'_>,
    candidate_traits: CandidateDoc<'_>,
) -> bool {
    let dialect = |schema: &Value| schema["$schema"].as_str().and_then(normalize_dialect);
    let without_dialect = |schema: &Value| {
        let mut schema = schema.clone();
        if let Some(members) = schema.as_object_mut() {
            members.remove("$schema");
        }
        schema
    };
    let (baseline, candidate) = (baseline.get(), candidate.get());
    let (baseline_traits, candidate_traits) = (baseline_traits.get(), candidate_traits.get());
    baseline["$id"] == candidate["$id"]
        && dialect(baseline).is_some()
        && dialect(baseline) == dialect(candidate)
        && dialect(baseline_traits) == dialect(candidate_traits)
        && without_dialect(baseline_traits) == without_dialect(candidate_traits)
}

/// The verdict of obligations that must all hold: one refutation refutes, and
/// an undecided one leaves the whole undecided.
const fn all_hold(left: CompatibilityVerdict, right: CompatibilityVerdict) -> CompatibilityVerdict {
    use CompatibilityVerdict::{Compatible, Incompatible, Unknown};
    match (left, right) {
        (Incompatible, _) | (_, Incompatible) => Incompatible,
        (Unknown, _) | (_, Unknown) => Unknown,
        (Compatible, Compatible) => Compatible,
    }
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
