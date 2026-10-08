//! Reversible `CanonicalError` item failures (DESIGN §3.3).
//! `FailedPrecondition` violation 0 carries reason/key/message; later violations carry
//! `context.<name>`/value/empty description. Unknown reasons and context round-trip unchanged.
//!
//! [`reason`] defines codes used in storage, errors and metrics;
//! [`AdmissionFailureReason`] provides typed dispatch (ADR 0005 rule 4).

use std::collections::BTreeMap;

use toolkit_canonical_errors::CanonicalError;

use crate::gts::{Resource, TypeResource};
use crate::precondition::{PARENT_NOT_REGISTERED, REGISTRATION_POLICY_PREFIX};

/// Prefix of a context violation's `type`.
const CONTEXT_PREFIX: &str = "context.";

/// Wire `reason` codes of a failed item, one per [`AdmissionFailureReason`] variant.
pub mod reason {
    pub const ACTIVATION_WRITE_SET_EXCEEDED: &str = "activation_write_set_exceeded";
    pub const ALREADY_EXISTS: &str = "already_exists";
    pub const BASELINE_UNRESOLVABLE: &str = "baseline_unresolvable";
    pub const BLOCKED_BY_DEPENDENCY: &str = "blocked_by_dependency";
    pub const BLOCKED_BY_PREDECESSOR: &str = "blocked_by_predecessor";
    pub const COMPATIBILITY_UNDECIDABLE: &str = "compatibility_undecidable";
    pub const DEPENDENCY_DELETED: &str = "dependency_deleted";
    pub const DEPENDENCY_NOT_FOUND: &str = "dependency_not_found";
    pub const DEPENDENT_INVALID: &str = "dependent_invalid";
    pub const DIALECT_CHANGED: &str = "dialect_changed";
    pub const ENTITY_DELETED: &str = "entity_deleted";
    pub const FAMILY_KIND_CONFLICT: &str = "family_kind_conflict";
    pub const HAS_REGISTERED_DEPENDENTS: &str = "has_registered_dependents";
    pub const FAMILY_SHAPE_CONFLICT: &str = "family_shape_conflict";
    pub const INCOMPATIBLE_WITH_BASELINE: &str = "incompatible_with_baseline";
    pub const INSTANCE_OF_MAJOR_ZERO: &str = "instance_of_major_zero";
    pub const INVALID_DOCUMENT: &str = "invalid_document";
    pub const INVALID_IDENTIFIER: &str = "invalid_identifier";
    pub const INVALID_SCHEMA: &str = "invalid_schema";
    pub const INVALID_VALUE: &str = "invalid_value";
    pub const MISSING_PREDECESSOR: &str = "missing_predecessor";
    pub const NOT_ACTIVE: &str = "not_active";
    pub const PRECONDITION_FAILED: &str = "precondition_failed";
    /// Another publisher owns the entity (D18, Phase 9).
    pub const PUBLISHER_MISMATCH: &str = "publisher_mismatch";
    pub const RESOLUTION_CLOSURE_EXCEEDED: &str = "resolution_closure_exceeded";
    pub const RESOLVED_DOCUMENT_TOO_LARGE: &str = "resolved_document_too_large";
    pub const REVALIDATION_EXHAUSTED: &str = "revalidation_exhausted";
    pub const STABLE_DERIVES_FROM_MAJOR_ZERO: &str = "stable_derives_from_major_zero";
    pub const STABLE_REFS_MAJOR_ZERO: &str = "stable_refs_major_zero";
    /// Newer release of the same publisher (D18, Phase 9).
    pub const SUPERSEDED: &str = "superseded";
    pub const SYSTEM_FAILURE: &str = "system_failure";
    pub const UNPARSABLE_PAYLOAD: &str = "unparsable_payload";
    pub const UNREADABLE_VERSION: &str = "unreadable_version";
    pub const UNRECOGNIZED_PAYLOAD: &str = "unrecognized_payload";
}

/// Context names the SDK reads. Others are carried as they arrive.
pub mod context {
    pub const DEPENDENCY_ID: &str = "dependency_id";
    pub const DEPENDENCY_KIND: &str = "dependency_kind";
    pub const DIAGNOSTIC_CODE: &str = "diagnostic_code";
    pub const STORED_VERSION: &str = "stored_version";
    pub const OFFERED_VERSION: &str = "offered_version";
    pub const STORED_PUBLISHER: &str = "stored_publisher";
    pub const OFFERED_PUBLISHER: &str = "offered_publisher";
}

/// Wire values of [`context::DEPENDENCY_KIND`], one per [`DependencyKind`] variant.
pub mod dependency_kind {
    /// The derivation base of a Type Schema.
    pub const BASE: &str = "base";
    /// The Type Schema an Instance conforms to.
    pub const CONFORMING_TYPE: &str = "conforming_type";
    /// A schema `$ref` target.
    pub const REF: &str = "ref";
}

/// Typed view of [`context::DEPENDENCY_KIND`] (DESIGN §3.3: an open set).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    /// See [`dependency_kind::BASE`].
    Base,
    /// See [`dependency_kind::CONFORMING_TYPE`].
    ConformingType,
    /// See [`dependency_kind::REF`].
    Ref,
    /// A value this build does not know, preserved as it arrived.
    Unknown(String),
}

impl DependencyKind {
    /// Project a wire value; anything unfamiliar becomes [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(value: &str) -> Self {
        match value {
            dependency_kind::BASE => Self::Base,
            dependency_kind::CONFORMING_TYPE => Self::ConformingType,
            dependency_kind::REF => Self::Ref,
            unknown => Self::Unknown(unknown.to_owned()),
        }
    }

    /// Render back to the wire value. Inverse of [`Self::from_wire`].
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Base => dependency_kind::BASE,
            Self::ConformingType => dependency_kind::CONFORMING_TYPE,
            Self::Ref => dependency_kind::REF,
            Self::Unknown(value) => value,
        }
    }
}

impl std::fmt::Display for DependencyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// A candidate refusal, or a code preserved from another service version.
///
/// Typed view of [`reason`]. Exhaustive on purpose: [`Self::Unknown`] carries any code this
/// build does not know, so a newer registry never breaks an older consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// A new base, conforming type or schema reference names a tombstone;
    /// deletion removes an entity from the valid targets (PRD §5).
    DependencyDeleted,
    /// A required base, conforming type or schema reference is absent.
    DependencyNotFound,
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
    /// Another publisher owns the entity (D18); emitted from Phase 9.
    PublisherMismatch,
    ResolutionClosureExceeded,
    ResolvedDocumentTooLarge,
    RevalidationExhausted,
    /// ADR-0015 quarantine: a stable candidate's immediate derivation base names
    /// a major-0 entity.
    StableDerivesFromMajorZero,
    /// ADR-0015: a stable candidate `$ref`s a major-0 entity.
    StableRefsMajorZero,
    /// A newer release of the same publisher owns the entity (D18); emitted from Phase 9.
    Superseded,
    /// The system failed, not the candidate; `error_code` names the cause.
    SystemFailure,
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
            reason::ACTIVATION_WRITE_SET_EXCEEDED => Self::ActivationWriteSetExceeded,
            reason::ALREADY_EXISTS => Self::AlreadyExists,
            reason::BASELINE_UNRESOLVABLE => Self::BaselineUnresolvable,
            reason::BLOCKED_BY_DEPENDENCY => Self::BlockedByDependency,
            reason::BLOCKED_BY_PREDECESSOR => Self::BlockedByPredecessor,
            reason::COMPATIBILITY_UNDECIDABLE => Self::CompatibilityUndecidable,
            reason::DEPENDENCY_DELETED => Self::DependencyDeleted,
            reason::DEPENDENCY_NOT_FOUND => Self::DependencyNotFound,
            reason::DEPENDENT_INVALID => Self::DependentInvalid,
            reason::DIALECT_CHANGED => Self::DialectChanged,
            reason::ENTITY_DELETED => Self::EntityDeleted,
            reason::FAMILY_KIND_CONFLICT => Self::FamilyKindConflict,
            reason::HAS_REGISTERED_DEPENDENTS => Self::HasRegisteredDependents,
            reason::FAMILY_SHAPE_CONFLICT => Self::FamilyShapeConflict,
            reason::INCOMPATIBLE_WITH_BASELINE => Self::IncompatibleWithBaseline,
            reason::INSTANCE_OF_MAJOR_ZERO => Self::InstanceOfMajorZero,
            reason::INVALID_DOCUMENT => Self::InvalidDocument,
            reason::INVALID_IDENTIFIER => Self::InvalidIdentifier,
            reason::INVALID_SCHEMA => Self::InvalidSchema,
            reason::INVALID_VALUE => Self::InvalidValue,
            reason::MISSING_PREDECESSOR => Self::MissingPredecessor,
            reason::NOT_ACTIVE => Self::NotActive,
            reason::PRECONDITION_FAILED => Self::PreconditionFailed,
            reason::PUBLISHER_MISMATCH => Self::PublisherMismatch,
            reason::RESOLUTION_CLOSURE_EXCEEDED => Self::ResolutionClosureExceeded,
            reason::RESOLVED_DOCUMENT_TOO_LARGE => Self::ResolvedDocumentTooLarge,
            reason::REVALIDATION_EXHAUSTED => Self::RevalidationExhausted,
            reason::STABLE_DERIVES_FROM_MAJOR_ZERO => Self::StableDerivesFromMajorZero,
            reason::STABLE_REFS_MAJOR_ZERO => Self::StableRefsMajorZero,
            reason::SUPERSEDED => Self::Superseded,
            reason::SYSTEM_FAILURE => Self::SystemFailure,
            reason::UNPARSABLE_PAYLOAD => Self::UnparsablePayload,
            reason::UNREADABLE_VERSION => Self::UnreadableVersion,
            reason::UNRECOGNIZED_PAYLOAD => Self::UnrecognizedPayload,
            unknown => Self::Unknown(unknown.to_owned()),
        }
    }

    /// The stable code persisted in error payloads and returned to clients.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Unknown(code) => code,
            known => known.metric_label(),
        }
    }

    /// A bounded metric label: unknown codes share the single `other` series.
    #[must_use]
    pub const fn metric_label(&self) -> &'static str {
        match self {
            Self::ActivationWriteSetExceeded => reason::ACTIVATION_WRITE_SET_EXCEEDED,
            Self::AlreadyExists => reason::ALREADY_EXISTS,
            Self::BaselineUnresolvable => reason::BASELINE_UNRESOLVABLE,
            Self::BlockedByDependency => reason::BLOCKED_BY_DEPENDENCY,
            Self::BlockedByPredecessor => reason::BLOCKED_BY_PREDECESSOR,
            Self::CompatibilityUndecidable => reason::COMPATIBILITY_UNDECIDABLE,
            Self::DependencyDeleted => reason::DEPENDENCY_DELETED,
            Self::DependencyNotFound => reason::DEPENDENCY_NOT_FOUND,
            Self::DependentInvalid => reason::DEPENDENT_INVALID,
            Self::DialectChanged => reason::DIALECT_CHANGED,
            Self::EntityDeleted => reason::ENTITY_DELETED,
            Self::FamilyKindConflict => reason::FAMILY_KIND_CONFLICT,
            Self::HasRegisteredDependents => reason::HAS_REGISTERED_DEPENDENTS,
            Self::FamilyShapeConflict => reason::FAMILY_SHAPE_CONFLICT,
            Self::IncompatibleWithBaseline => reason::INCOMPATIBLE_WITH_BASELINE,
            Self::InstanceOfMajorZero => reason::INSTANCE_OF_MAJOR_ZERO,
            Self::InvalidDocument => reason::INVALID_DOCUMENT,
            Self::InvalidIdentifier => reason::INVALID_IDENTIFIER,
            Self::InvalidSchema => reason::INVALID_SCHEMA,
            Self::InvalidValue => reason::INVALID_VALUE,
            Self::MissingPredecessor => reason::MISSING_PREDECESSOR,
            Self::NotActive => reason::NOT_ACTIVE,
            Self::PreconditionFailed => reason::PRECONDITION_FAILED,
            Self::PublisherMismatch => reason::PUBLISHER_MISMATCH,
            Self::ResolutionClosureExceeded => reason::RESOLUTION_CLOSURE_EXCEEDED,
            Self::ResolvedDocumentTooLarge => reason::RESOLVED_DOCUMENT_TOO_LARGE,
            Self::RevalidationExhausted => reason::REVALIDATION_EXHAUSTED,
            Self::StableDerivesFromMajorZero => reason::STABLE_DERIVES_FROM_MAJOR_ZERO,
            Self::StableRefsMajorZero => reason::STABLE_REFS_MAJOR_ZERO,
            Self::Superseded => reason::SUPERSEDED,
            Self::SystemFailure => reason::SYSTEM_FAILURE,
            Self::UnparsablePayload => reason::UNPARSABLE_PAYLOAD,
            Self::UnreadableVersion => reason::UNREADABLE_VERSION,
            Self::UnrecognizedPayload => reason::UNRECOGNIZED_PAYLOAD,
            Self::Unknown(_) => "other",
        }
    }
}

impl std::fmt::Display for AdmissionFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// One item's failure as the registry records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionFailure {
    pub reason: AdmissionFailureReason,
    pub message: String,
    pub context: BTreeMap<String, String>,
}

impl AdmissionFailure {
    /// A failure with no context.
    #[must_use]
    pub fn new(reason: AdmissionFailureReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
            context: BTreeMap::new(),
        }
    }

    /// Adds one context entry.
    #[must_use]
    pub fn with_context(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(name.into(), value.into());
        self
    }

    /// The context entry `name`, if present.
    #[must_use]
    pub fn context(&self, name: &str) -> Option<&str> {
        self.context.get(name).map(String::as_str)
    }

    /// The [`context::DEPENDENCY_KIND`] entry, typed; `None` only when it is absent.
    #[must_use]
    pub fn dependency_kind(&self) -> Option<DependencyKind> {
        self.context(context::DEPENDENCY_KIND)
            .map(DependencyKind::from_wire)
    }

    /// Encodes this failure of the item `key` (its canonical spelling).
    #[must_use]
    pub fn into_canonical(self, key: &str) -> CanonicalError {
        let mut builder = TypeResource::failed_precondition()
            .with_resource(key)
            .with_precondition_violation(key, self.message, self.reason.as_wire());
        for (name, value) in self.context {
            builder =
                builder.with_precondition_violation(value, "", format!("{CONTEXT_PREFIX}{name}"));
        }
        builder.create()
    }

    /// Decodes exactly what [`Self::into_canonical`] produces; `None` for any other shape.
    ///
    /// Requires the entity resource type, a primary violation naming that resource,
    /// and distinct context names with empty descriptions. Other precondition shapes
    /// (parent or policy refusals) are rejected.
    #[must_use]
    pub fn from_canonical(error: &CanonicalError) -> Option<Self> {
        let CanonicalError::FailedPrecondition {
            ctx,
            resource_type,
            resource_name,
            ..
        } = error
        else {
            return None;
        };
        if Resource::from_wire(resource_type.as_deref()?) != Resource::Entity {
            return None;
        }
        let (primary, rest) = ctx.violations.split_first()?;
        if Some(primary.subject.as_str()) != resource_name.as_deref()
            || primary.type_ == PARENT_NOT_REGISTERED
            || primary.type_.starts_with(REGISTRATION_POLICY_PREFIX)
        {
            return None;
        }
        let mut failure = Self::new(
            AdmissionFailureReason::from_wire(&primary.type_),
            &primary.description,
        );
        for violation in rest {
            let name = violation.type_.strip_prefix(CONTEXT_PREFIX)?;
            if !violation.description.is_empty()
                || failure
                    .context
                    .insert(name.to_owned(), violation.subject.clone())
                    .is_some()
            {
                return None;
            }
        }
        Some(failure)
    }
}

#[cfg(test)]
#[path = "item_failure_tests.rs"]
mod item_failure_tests;
