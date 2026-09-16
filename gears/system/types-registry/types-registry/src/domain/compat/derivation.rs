//! Major-0 quarantine (ADR-0015) and dialect pinning (ADR-0014).
//!
//! Quarantine rejects a stable candidate's direct derivation, `$ref`, or
//! conformance edge to major 0. Unstable candidates may depend on stable types.
//! Direct checks suffice by induction: an intermediate stable type must pass
//! the same rule. No managed entities predate the enforcing release.
//!
//! The dialect pin runs before comparison, whose instance sets require shared
//! semantics. P0 admits one dialect; P2 extends uniformity checks to the closure.
//! Both rules derive their inputs from identifiers and retained documents.

use gts::{GtsId, GtsIdSegment};
use serde_json::Value;
use thiserror::Error;
use toolkit_macros::domain_model;

use super::sides::{BaselineDoc, CandidateDoc};
use crate::domain::admission::AdmissionFailureReason;
use crate::domain::dependency::DependencyEdge;
use crate::domain::enums::DependencyKind;

/// Canonical Draft-07 spelling and aliases (ADR-0014, SPEC §8.1 step 5).
/// Shared by acceptance and [`dialect_pin`].
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
const DRAFT_07_SPELLINGS: [&str; 4] = [
    "http://json-schema.org/draft-07/schema#",
    "http://json-schema.org/draft-07/schema",
    "https://json-schema.org/draft-07/schema#",
    "https://json-schema.org/draft-07/schema",
];

/// The canonical form of a recognized dialect spelling, or `None` for a value
/// outside the admissible set.
pub fn normalize_dialect(declared: &str) -> Option<&'static str> {
    DRAFT_07_SPELLINGS.contains(&declared).then_some(DRAFT_07)
}

/// A quarantined edge, carrying its target for the refusal message (ADR-0015).
#[domain_model]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("'{gts_id}' is stable, so it may not {} major-0 '{target}' (ADR-0015)", .kind.quarantine_verb())]
pub struct QuarantineBreach {
    pub gts_id: String,
    pub kind: DependencyKind,
    pub target: String,
}

impl QuarantineBreach {
    /// Distinct refusal reasons for derivation, reference, and conformance edges.
    #[must_use]
    pub fn reason(&self) -> AdmissionFailureReason {
        match self.kind {
            DependencyKind::Derivation => AdmissionFailureReason::StableDerivesFromMajorZero,
            DependencyKind::SchemaRef => AdmissionFailureReason::StableRefsMajorZero,
            DependencyKind::InstanceOf => AdmissionFailureReason::InstanceOfMajorZero,
        }
    }
}

/// Reject stable candidates that derive from, `$ref`, or conform to major 0.
/// Uses [`extract_edges`](crate::domain::dependency::extract_edges), shared with
/// the dependency graph. Target identifiers suffice; no documents are loaded.
/// `x-gts-ref` creates no dependency and is absent from these edges.
///
/// An unreadable candidate major is treated as stable; baseline selection rejects
/// it earlier. Unreadable target majors are left to resolution validation.
///
/// # Errors
/// [`QuarantineBreach`] for the first offending edge in extraction order
/// (sorted and deduplicated).
pub fn quarantine(id: &GtsId, edges: &[DependencyEdge]) -> Result<(), QuarantineBreach> {
    if major_of(id) == Some(0) {
        return Ok(());
    }
    match edges.iter().find(|edge| names_major_zero(&edge.target)) {
        None => Ok(()),
        Some(edge) => Err(QuarantineBreach {
            gts_id: id.id().to_owned(),
            kind: edge.kind,
            target: edge.target.clone(),
        }),
    }
}

/// The major of an identifier's last segment, which is the segment the profile
/// lives in: a `v0~` prefix marks the *base* as unstable, not the candidate.
fn major_of(id: &GtsId) -> Option<u32> {
    id.segments().last().and_then(GtsIdSegment::ver_major_opt)
}

/// Whether the target's last segment names major 0. Unparsable targets are
/// left to validation rather than reported as quarantine breaches.
fn names_major_zero(target: &str) -> bool {
    GtsId::try_new(target)
        .ok()
        .and_then(|id| major_of(&id))
        .is_some_and(|major| major == 0)
}

/// Declared dialect mismatch (ADR-0014). `None` preserves a missing declaration
/// in diagnostics rather than substituting a default.
#[domain_model]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(
    "the dialect pinned by this major is {}, and the candidate declares {} (ADR-0014)",
    .pinned.as_deref().unwrap_or("absent"),
    .declared.as_deref().unwrap_or("absent")
)]
pub struct DialectDrift {
    /// The baseline's declared dialect, verbatim as authored.
    pub pinned: Option<String>,
    /// The candidate's declared dialect, verbatim as authored.
    pub declared: Option<String>,
}

impl DialectDrift {
    /// Dedicated dialect refusal code, independent of the particular pair (P16).
    pub const REASON: AdmissionFailureReason = AdmissionFailureReason::DialectChanged;
}

/// Pin the dialect across a major's revisions and minor versions (ADR-0014).
/// A new major starts its own pin. Normalize recognized aliases; compare
/// unrecognized spellings verbatim. P0 acceptance admits only Draft-07, but
/// stored baselines may come from builds with a different admissible set.
///
/// Run before comparison to report `dialect_changed` rather than the library's
/// `Unknown` verdict. The library still rejects equivalent Draft-07 spellings:
/// <https://github.com/GlobalTypeSystem/gts-rust/issues/120>.
/// `tests/quarantine_test.rs` records that limitation.
///
/// # Errors
/// [`DialectDrift`] carrying both declared values.
pub fn dialect_pin(
    baseline: BaselineDoc<'_>,
    candidate: CandidateDoc<'_>,
) -> Result<(), DialectDrift> {
    let pinned = declared_dialect(baseline.get());
    let declared = declared_dialect(candidate.get());
    if let (Some(pinned), Some(declared)) = (pinned, declared)
        && canonical(pinned) == canonical(declared)
    {
        return Ok(());
    }
    Err(DialectDrift {
        pinned: pinned.map(str::to_owned),
        declared: declared.map(str::to_owned),
    })
}

/// A document's top-level `$schema`, when it declares a non-empty one.
fn declared_dialect(document: &Value) -> Option<&str> {
    document
        .get("$schema")
        .and_then(Value::as_str)
        .filter(|declared| !declared.is_empty())
}

/// The canonical spelling of a recognized dialect, or the value as authored.
fn canonical(declared: &str) -> &str {
    normalize_dialect(declared).unwrap_or(declared)
}

#[cfg(test)]
#[path = "derivation_tests.rs"]
mod derivation_tests;
