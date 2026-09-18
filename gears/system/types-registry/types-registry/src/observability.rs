//! Admission tracing spans.

use gts::CompatibilityVerdict;
use tracing::{Span, field};
use uuid::Uuid;

use crate::domain::compat::Baseline;
use crate::domain::enums::OperationKind;

/// The label an operation's kind carries.
const fn kind_label(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Registration => "registration",
        OperationKind::Deletion => "deletion",
    }
}

/// The span covering one admission pass over one operation.
#[must_use]
pub fn operation_span(operation_id: Uuid) -> Span {
    tracing::info_span!(
        "types_registry.admission.operation",
        %operation_id,
        kind = field::Empty,
        dry_run = field::Empty,
    )
}

/// Fill in the two fields [`operation_span`] left empty.
pub fn record_operation_facts(span: &Span, kind: OperationKind, dry_run: bool) {
    span.record("kind", kind_label(kind));
    span.record("dry_run", dry_run);
}

/// Span for one candidate. Record binary GTS versions now (ADR-0003);
/// [`record_compat_facts`] fills fields learned during evaluation.
#[must_use]
pub fn unit_span(
    operation_id: Uuid,
    gts_id: &str,
    kind: OperationKind,
    dry_run: bool,
    operation_item_id: i64,
) -> Span {
    tracing::info_span!(
        "types_registry.admission.unit",
        %operation_id,
        gts_id,
        kind = kind_label(kind),
        dry_run,
        operation_item_id,
        gts_spec_version = gts::GTS_SPECIFICATION_VERSION,
        gts_impl_version = gts::GTS_IMPLEMENTATION_VERSION,
        baseline = field::Empty,
        baseline_gts_id = field::Empty,
        baseline_revision = field::Empty,
        compat_verdict = field::Empty,
        // T20: how many live direct dependants blocked a deletion. A count,
        // and on the span rather than in a label: identities are unbounded and
        // the caller may not be entitled to read them.
        blocked_dependents = field::Empty,
    )
}

/// Record how many live direct dependants refused a deletion (T20).
pub fn record_blocked_dependents(span: &Span, blocked: usize) {
    span.record("blocked_dependents", blocked);
}

/// Compatibility facts for the unit span. Domain types keep token mapping here.
/// Unbounded baseline identifiers belong in spans, never metric labels (SPEC §8.6).
#[derive(Clone, Copy, Debug)]
pub struct CompatFacts<'a> {
    /// Selection token: `current_revision`, `preceding_minor`, or `exempt_*`.
    pub baseline: &'a Baseline,
    /// The baseline's identifier, absent where no comparison was owed.
    pub gts_id: Option<&'a str>,
    /// The baseline's revision number, absent for the same reason.
    pub revision: Option<i32>,
    /// The verdict, absent where no comparison ran. An absent verdict beside a
    /// present `baseline` token is exactly how an exemption reads.
    pub verdict: Option<CompatibilityVerdict>,
}

/// Fill compatibility fields for admitted and refused candidates.
pub fn record_compat_facts(span: &Span, facts: CompatFacts<'_>) {
    span.record("baseline", facts.baseline.label());
    if let Some(gts_id) = facts.gts_id {
        span.record("baseline_gts_id", gts_id);
    }
    if let Some(revision) = facts.revision {
        span.record("baseline_revision", revision);
    }
    if let Some(verdict) = facts.verdict {
        span.record("compat_verdict", verdict.as_str());
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "observability_tests.rs"]
mod observability_tests;
