//! Error types of the durable-execution library.
//!
//! [`Error`] is returned by every runtime operation. Match on [`Error::kind`] to
//! choose recovery and use [`Error::code`] as the stable machine-readable value.
//! [`DefinitionError`] reports invalid workflow definitions before any storage call.
//!
//! The library does not depend on the platform error model. A gear that exposes runs
//! through REST or gRPC maps [`ErrorKind`] to its own canonical errors and resource
//! types, the way gears map `toolkit_db` errors.
use std::error::Error as StdError;
use std::sync::Arc;

use crate::error::domain::DomainError;
use crate::store::StoreError;
use crate::{Resource, RunId};

/// Stable machine-readable codes returned by [`Error::code`].
pub mod codes;
/// Internal failures that operations branch on before they become an [`Error`].
pub mod domain;

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
/// Definition helpers run in-process before any storage call, so they keep this
/// narrow type. Convert with `?` into [`Error`] (kind [`ErrorKind::InvalidArgument`]).
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

/// Recovery category of an [`Error`].
///
/// Open taxonomy: new kinds may be added in minor releases, so keep a fallback arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A request field or the workflow definition is invalid. Fix the input.
    InvalidArgument,
    /// The run or definition does not exist or is not visible to the caller.
    NotFound,
    /// A definition or request key already exists with different content.
    AlreadyExists,
    /// Authorization rejected the operation.
    PermissionDenied,
    /// The current state forbids the operation, for example a run that is not
    /// finished, a definition with a different fingerprint, or scheduling on an
    /// engine without delivery. Do not retry unchanged.
    FailedPrecondition,
    /// A concurrent update or a lost lease. Re-read state and retry.
    Aborted,
    /// Storage, the queue or authorization is temporarily unavailable. Retry with backoff.
    Unavailable,
    /// A broken invariant or undecodable saved data. Not retryable.
    Internal,
}

/// Failure of a durable-execution operation.
///
/// The display text is for humans and may change. Use [`Self::kind`], [`Self::code`],
/// [`Self::field`], [`Self::run_id`] and [`Self::definition`] in code.
/// [`std::error::Error::source`] returns the underlying cause when one is known, such
/// as a storage or authorization failure; equality ignores it.
#[derive(Clone)]
pub struct Error {
    domain: DomainError,
    source: Option<Arc<dyn StdError + Send + Sync>>,
}

impl Error {
    /// Recovery category.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match &self.domain {
            DomainError::InvalidDefinition(_) | DomainError::InvalidRequest { .. } => {
                ErrorKind::InvalidArgument
            }
            DomainError::RunNotFound(_) | DomainError::DefinitionNotFound(_) => ErrorKind::NotFound,
            DomainError::DefinitionConflict(_) | DomainError::IdempotencyConflict(_) => {
                ErrorKind::AlreadyExists
            }
            DomainError::Forbidden | DomainError::DefinitionForbidden => {
                ErrorKind::PermissionDenied
            }
            DomainError::InvalidState(_)
            | DomainError::DefinitionInvalidState(_)
            | DomainError::DefinitionInactive
            | DomainError::DefinitionMismatch
            | DomainError::DeliveryDisabled => ErrorKind::FailedPrecondition,
            DomainError::LeaseLost
            | DomainError::ConcurrentUpdate
            | DomainError::DefinitionConcurrentUpdate => ErrorKind::Aborted,
            DomainError::Unavailable => ErrorKind::Unavailable,
            DomainError::Internal(_) => ErrorKind::Internal,
        }
    }

    /// Stable machine-readable code, one of the constants in [`codes`].
    #[must_use]
    pub fn code(&self) -> &'static str {
        match &self.domain {
            DomainError::Forbidden | DomainError::DefinitionForbidden => {
                codes::EXECUTION_ACCESS_DENIED
            }
            DomainError::RunNotFound(_) => codes::RUN_NOT_FOUND,
            DomainError::DefinitionNotFound(_) => codes::DEFINITION_NOT_FOUND,
            DomainError::LeaseLost => codes::LEASE_LOST,
            DomainError::ConcurrentUpdate | DomainError::DefinitionConcurrentUpdate => {
                codes::CONCURRENT_UPDATE
            }
            DomainError::InvalidState(_) | DomainError::DefinitionInvalidState(_) => {
                codes::INVALID_STATE
            }
            DomainError::DefinitionInactive => codes::DEFINITION_INACTIVE,
            DomainError::DefinitionMismatch => codes::DEFINITION_MISMATCH,
            DomainError::DeliveryDisabled => codes::DELIVERY_DISABLED,
            DomainError::DefinitionConflict(_) => codes::DEFINITION_CONFLICT,
            DomainError::IdempotencyConflict(_) => codes::REQUEST_KEY_CONFLICT,
            DomainError::InvalidDefinition(_) => codes::INVALID_DEFINITION,
            DomainError::InvalidRequest { reason, .. } => reason,
            DomainError::Unavailable => codes::UNAVAILABLE,
            DomainError::Internal(_) => codes::INTERNAL,
        }
    }

    /// Whether the same call may succeed later: [`ErrorKind::Aborted`] after
    /// re-reading state, [`ErrorKind::Unavailable`] after a backoff.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self.kind(), ErrorKind::Aborted | ErrorKind::Unavailable)
    }

    /// Offending request field for [`ErrorKind::InvalidArgument`].
    ///
    /// Definition failures report `definition`, or `activities.<step>` for a step.
    #[must_use]
    pub fn field(&self) -> Option<String> {
        match &self.domain {
            DomainError::InvalidRequest { field, .. } => Some((*field).to_owned()),
            DomainError::InvalidDefinition(error) => Some(error.step().map_or_else(
                || "definition".to_owned(),
                |step| format!("activities.{step}"),
            )),
            _ => None,
        }
    }

    /// Run the failure refers to, when it names one.
    #[must_use]
    pub fn run_id(&self) -> Option<RunId> {
        match &self.domain {
            DomainError::RunNotFound(id) => Some(*id),
            _ => None,
        }
    }

    /// Definition name the failure refers to, when it names one.
    #[must_use]
    pub fn definition(&self) -> Option<&str> {
        match &self.domain {
            DomainError::DefinitionNotFound(name) | DomainError::DefinitionConflict(name) => {
                Some(name)
            }
            _ => None,
        }
    }

    /// Request key (`idempotency_key` or `coalescing_key`) reused with different input.
    #[must_use]
    pub fn request_key(&self) -> Option<&'static str> {
        match &self.domain {
            DomainError::IdempotencyConflict(key) => Some(key),
            _ => None,
        }
    }

    /// Resource the failure concerns, to map it onto the gear's own resource types.
    /// `None` for failures of the engine itself (`Unavailable`, `Internal`, delivery).
    #[must_use]
    pub fn resource(&self) -> Option<Resource> {
        match &self.domain {
            DomainError::DefinitionForbidden
            | DomainError::DefinitionNotFound(_)
            | DomainError::DefinitionConcurrentUpdate
            | DomainError::DefinitionInvalidState(_)
            | DomainError::DefinitionInactive
            | DomainError::DefinitionMismatch
            | DomainError::DefinitionConflict(_)
            | DomainError::InvalidDefinition(_) => Some(Resource::Definition),
            DomainError::Forbidden
            | DomainError::RunNotFound(_)
            | DomainError::LeaseLost
            | DomainError::ConcurrentUpdate
            | DomainError::InvalidState(_)
            | DomainError::IdempotencyConflict(_)
            | DomainError::InvalidRequest { .. } => Some(Resource::Run),
            DomainError::DeliveryDisabled | DomainError::Unavailable | DomainError::Internal(_) => {
                None
            }
        }
    }

    /// Attach the underlying cause.
    pub(crate) fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Arc::new(source));
        self
    }

    /// `InvalidArgument` for a malformed request field.
    pub(crate) fn invalid_input(field: &'static str, message: &'static str) -> Self {
        DomainError::InvalidRequest {
            field,
            reason: codes::INVALID_FORMAT,
            message,
        }
        .into()
    }

    /// A saved result or checkpoint does not decode as the requested type.
    pub(crate) fn undecodable(checkpoint: bool, cause: serde_json::Error) -> Self {
        Self::from(DomainError::Internal(if checkpoint {
            "saved checkpoint does not match the requested payload type"
        } else {
            "saved workflow result does not match the requested payload type"
        }))
        .with_source(cause)
    }

    /// The domain failure, for internal dispatch.
    pub(crate) fn domain(&self) -> &DomainError {
        &self.domain
    }
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind())
            .field("code", &self.code())
            .field("message", &self.domain.to_string())
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.domain, f)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.domain == other.domain
    }
}

impl Eq for Error {}

impl From<DomainError> for Error {
    fn from(domain: DomainError) -> Self {
        if let DomainError::Internal(invariant) = &domain {
            tracing::error!(invariant, "durable execution internal error");
        }
        Self {
            domain,
            source: None,
        }
    }
}

impl From<StoreError> for Error {
    /// Classify through the store mapping, which also logs, and keep the store
    /// failure as the cause.
    fn from(error: StoreError) -> Self {
        if let StoreError::Domain(domain) = error {
            return domain.into();
        }
        let source = Arc::new(error);
        Self {
            source: Some(source.clone()),
            ..Self::from(DomainError::from_store(&source))
        }
    }
}

impl Error {
    /// A store failure of a catalog operation: classified on the definition
    /// resource, keeping the store failure as the cause.
    pub(crate) fn for_definition_store(error: StoreError) -> Self {
        let mut error = Self::from(error);
        error.domain = error.domain.for_definition();
        error
    }
}

impl From<DefinitionError> for Error {
    fn from(error: DefinitionError) -> Self {
        DomainError::InvalidDefinition(error).into()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/definition_error_tests.rs"]
mod tests;
