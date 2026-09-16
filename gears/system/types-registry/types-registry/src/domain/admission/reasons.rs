//! Stable admission failure codes used in stored outcomes and metrics.

use toolkit_macros::domain_model;

/// A candidate refusal, or a code preserved from another service version.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdmissionFailureReason {
    ActivationWriteSetExceeded,
    AlreadyExists,
    /// The baseline's own references no longer resolve, so no comparison could be
    /// performed. Distinct from an undecidable one: the check never ran.
    BaselineUnresolvable,
    /// A selected in-batch dependency — an authored `$ref`, the derivation base
    /// or an Instance's conforming type — did not reach a successful outcome.
    BlockedByDependency,
    /// The preceding minor of a minor-bearing candidate was submitted in the same
    /// batch and failed, so the implicit `vM.(n-1)~ -> vM.n~` edge never closed.
    BlockedByPredecessor,
    /// `compare_documents` returned `Unknown`, distinct from an incompatible verdict.
    CompatibilityUndecidable,
    DependentInvalid,
    /// The declared dialect differs from the major's pinned dialect (ADR-0014).
    DialectChanged,
    EntityDeleted,
    FamilyKindConflict,
    /// Deleting this entity would strand a live direct registered dependant.
    /// The refusal reports how many; never which ones.
    HasRegisteredDependents,
    FamilyShapeConflict,
    /// `Valid(baseline) ⊆ Valid(candidate)` does not hold (ADR-0003).
    IncompatibleWithBaseline,
    /// ADR-0015: a registered Instance cannot conform to a major-0 Type Schema.
    InstanceOfMajorZero,
    InvalidDocument,
    InvalidIdentifier,
    InvalidSchema,
    InvalidValue,
    MissingPredecessor,
    /// The deletion target is not `ACTIVE`. Distinct from
    /// [`Self::EntityDeleted`], which says the entity a *revision* wanted is
    /// gone: this one says the deletion has nothing left to do, and a second
    /// attempt must never read as "retry with a newer version".
    NotActive,
    PreconditionFailed,
    ResolutionClosureExceeded,
    ResolvedDocumentTooLarge,
    RevalidationExhausted,
    /// ADR-0015 quarantine: a stable candidate's immediate derivation base names
    /// a major-0 entity.
    StableDerivesFromMajorZero,
    /// ADR-0015: a stable candidate `$ref`s a major-0 entity.
    StableRefsMajorZero,
    UnparsablePayload,
    UnreadableVersion,
    UnrecognizedPayload,
    /// Preserve an unrecognized stored code without adding a metric label.
    Unknown(String),
}

impl AdmissionFailureReason {
    /// Restore a typed reason while retaining codes from other service versions.
    #[must_use]
    pub fn from_wire(code: &str) -> Self {
        match code {
            "activation_write_set_exceeded" => Self::ActivationWriteSetExceeded,
            "already_exists" => Self::AlreadyExists,
            "baseline_unresolvable" => Self::BaselineUnresolvable,
            "blocked_by_dependency" => Self::BlockedByDependency,
            "blocked_by_predecessor" => Self::BlockedByPredecessor,
            "compatibility_undecidable" => Self::CompatibilityUndecidable,
            "dependent_invalid" => Self::DependentInvalid,
            "dialect_changed" => Self::DialectChanged,
            "entity_deleted" => Self::EntityDeleted,
            "family_kind_conflict" => Self::FamilyKindConflict,
            "has_registered_dependents" => Self::HasRegisteredDependents,
            "family_shape_conflict" => Self::FamilyShapeConflict,
            "incompatible_with_baseline" => Self::IncompatibleWithBaseline,
            "instance_of_major_zero" => Self::InstanceOfMajorZero,
            "invalid_document" => Self::InvalidDocument,
            "invalid_identifier" => Self::InvalidIdentifier,
            "invalid_schema" => Self::InvalidSchema,
            "invalid_value" => Self::InvalidValue,
            "missing_predecessor" => Self::MissingPredecessor,
            "not_active" => Self::NotActive,
            "precondition_failed" => Self::PreconditionFailed,
            "resolution_closure_exceeded" => Self::ResolutionClosureExceeded,
            "resolved_document_too_large" => Self::ResolvedDocumentTooLarge,
            "revalidation_exhausted" => Self::RevalidationExhausted,
            "stable_derives_from_major_zero" => Self::StableDerivesFromMajorZero,
            "stable_refs_major_zero" => Self::StableRefsMajorZero,
            "unparsable_payload" => Self::UnparsablePayload,
            "unreadable_version" => Self::UnreadableVersion,
            "unrecognized_payload" => Self::UnrecognizedPayload,
            unknown => Self::Unknown(unknown.to_owned()),
        }
    }

    /// The stable code persisted in error payloads and returned to clients.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unknown(code) => code,
            known => known.metric_label(),
        }
    }

    /// A bounded metric label: unknown codes share the single `other` series.
    #[must_use]
    pub const fn metric_label(&self) -> &'static str {
        match self {
            Self::ActivationWriteSetExceeded => "activation_write_set_exceeded",
            Self::AlreadyExists => "already_exists",
            Self::BaselineUnresolvable => "baseline_unresolvable",
            Self::BlockedByDependency => "blocked_by_dependency",
            Self::BlockedByPredecessor => "blocked_by_predecessor",
            Self::CompatibilityUndecidable => "compatibility_undecidable",
            Self::DependentInvalid => "dependent_invalid",
            Self::DialectChanged => "dialect_changed",
            Self::EntityDeleted => "entity_deleted",
            Self::FamilyKindConflict => "family_kind_conflict",
            Self::HasRegisteredDependents => "has_registered_dependents",
            Self::FamilyShapeConflict => "family_shape_conflict",
            Self::IncompatibleWithBaseline => "incompatible_with_baseline",
            Self::InstanceOfMajorZero => "instance_of_major_zero",
            Self::InvalidDocument => "invalid_document",
            Self::InvalidIdentifier => "invalid_identifier",
            Self::InvalidSchema => "invalid_schema",
            Self::InvalidValue => "invalid_value",
            Self::MissingPredecessor => "missing_predecessor",
            Self::NotActive => "not_active",
            Self::PreconditionFailed => "precondition_failed",
            Self::ResolutionClosureExceeded => "resolution_closure_exceeded",
            Self::ResolvedDocumentTooLarge => "resolved_document_too_large",
            Self::RevalidationExhausted => "revalidation_exhausted",
            Self::StableDerivesFromMajorZero => "stable_derives_from_major_zero",
            Self::StableRefsMajorZero => "stable_refs_major_zero",
            Self::UnparsablePayload => "unparsable_payload",
            Self::UnreadableVersion => "unreadable_version",
            Self::UnrecognizedPayload => "unrecognized_payload",
            Self::Unknown(_) => "other",
        }
    }
}

impl std::fmt::Display for AdmissionFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "reasons_tests.rs"]
mod reasons_tests;
