use toolkit_canonical_errors::{CanonicalError, resource_error};

/// Rule violated by a workflow definition or contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum DefinitionErrorKind {
    /// The name is not a bounded lowercase identifier ending in `.v<positive version>`.
    #[error(
        "name must be a lowercase identifier of at most 160 bytes ending in .v<positive version>"
    )]
    InvalidName,
    /// The definition has no activities or more than the supported maximum.
    #[error("a definition requires 1..=128 activities")]
    ActivityCount,
    /// An activity ID is empty, too long or uses unsupported characters.
    #[error("activity id must be 1..=80 bytes of [a-z0-9_-]")]
    InvalidActivityId,
    /// Two activities share one ID.
    #[error("duplicate activity id")]
    DuplicateActivityId,
    /// A timeout is zero or longer than one day.
    #[error("timeout must be positive and at most one day")]
    InvalidTimeout,
    /// A retry budget or backoff is out of range.
    #[error("retry policy requires at least one attempt and delays within (0, 1 day]")]
    InvalidRetryPolicy,
    /// A compensation timeout or retry policy is out of range.
    #[error("compensation policy has an invalid timeout or retry policy")]
    InvalidCompensationPolicy,
    /// Fan-out limits are out of range.
    #[error("fan-out requires max_items in 1..=1024 and concurrency in 1..=128")]
    InvalidFanOut,
    /// A parallel stage has fewer than two branches.
    #[error("a parallel stage requires at least two branches")]
    InvalidParallelStage,
    /// Stages do not list every activity once, in order, with matching policies.
    #[error("stages must list every activity once in declaration order")]
    InvalidStages,
    /// The data flow reads a missing, current or later checkpoint.
    #[error("data flow reads a missing or later checkpoint")]
    InvalidDataFlow,
    /// A dependency belongs to another workflow or is not an earlier checkpoint.
    #[error("dependency must be an earlier checkpoint of the same workflow")]
    InvalidDependency,
    /// A checkpoint reference was used before its workflow was built.
    #[error("checkpoint workflow is not built")]
    NotFinalized,
    /// The builder was finalized twice.
    #[error("workflow is already built")]
    AlreadyFinalized,
    /// The worker binds fewer handlers than its contract declares.
    #[error("worker handlers are incomplete")]
    IncompleteBindings,
    /// The contract cannot be serialized.
    #[error("contract cannot be encoded")]
    Encoding,
}

/// Validation failure of a workflow definition or contract.
///
/// Definition helpers run in-process before any trait call, so they keep this
/// narrow type (ADR-0005 "Non-Canonical Methods"). Convert with `?` or
/// [`CanonicalError::from`] to get the canonical `InvalidArgument`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid execution definition: {kind}{}", step.as_ref().map(|s| format!(" (step `{s}`)")).unwrap_or_default())]
pub struct DefinitionError {
    kind: DefinitionErrorKind,
    step: Option<String>,
}
impl DefinitionError {
    /// A violation of `kind` that is not tied to one step.
    #[must_use]
    pub fn new(kind: DefinitionErrorKind) -> Self {
        Self { kind, step: None }
    }
    /// Attribute the violation to the step with ID `step`.
    #[must_use]
    pub fn for_step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }
    /// The violated rule.
    #[must_use]
    pub fn kind(&self) -> DefinitionErrorKind {
        self.kind
    }
    /// ID of the offending step, when the violation belongs to one step.
    #[must_use]
    pub fn step(&self) -> Option<&str> {
        self.step.as_deref()
    }
}

// Literal MUST equal `crate::gts::DEFINITION_RESOURCE_TYPE`.
#[resource_error(gts_id!("cf.durable_execution.execution.definition.v1~"))]
struct DefinitionResource;

/// `FailedPrecondition` for a checkpoint reference of a different contract.
pub fn definition_mismatch() -> CanonicalError {
    DefinitionResource::failed_precondition()
        .with_precondition_violation(
            "fingerprint",
            "checkpoint belongs to a different workflow contract",
            crate::reason::DEFINITION_MISMATCH,
        )
        .create()
}

impl From<DefinitionError> for CanonicalError {
    fn from(e: DefinitionError) -> Self {
        let field = e
            .step
            .as_ref()
            .map_or_else(|| "definition".to_owned(), |s| format!("activities.{s}"));
        DefinitionResource::invalid_argument()
            .with_field_violation(field, e.kind.to_string(), crate::reason::INVALID_DEFINITION)
            .create()
    }
}

#[resource_error(gts_id!("cf.durable_execution.execution.run.v1~"))]
struct RunResource;

/// `InvalidArgument` for a malformed run request field.
pub fn invalid_input(field: &str, message: &str) -> CanonicalError {
    RunResource::invalid_argument()
        .with_field_violation(field, message, crate::reason::INVALID_FORMAT)
        .create()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/error_tests.rs"]
mod tests;
