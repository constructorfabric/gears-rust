//! Admission models: the request an enforcing gear submits, the verdict it
//! receives, the refusal causes, and the refusal event payload.
//!
//! The request and verdict types are plain value types without serde: they
//! cross an in-process boundary only. The refusal event payload and the types
//! it embeds are the exception: they are the wire contract of the audit topic
//! and carry serde derives.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AdmissionError, AdmissionResourceError, invalid_request, reason};

/// One intended operation an enforcing gear submits for admission.
///
/// There is no subject field: the subject and its tenant come from the
/// `SecurityContext` passed beside the request. `enforcing_gear`, `action`
/// and `resource_type` are copied into refusal events, so the gate validates
/// them with [`validate_identifier`] and [`validate_resource_type`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionRequest {
    /// Name of the calling enforcing gear.
    pub enforcing_gear: String,
    /// Action the caller intends (for example `create`).
    pub action: String,
    /// GTS type identifier of the target resource.
    pub resource_type: String,
    /// Identifier of the target resource, where the operation names one.
    pub resource_id: Option<Uuid>,
    /// Tenant that owns the target resource.
    pub resource_tenant_id: Uuid,
    /// Caller-supplied operation properties, forwarded to evaluation without
    /// interpretation, nested at most [`PROPERTY_MAX_DEPTH`] levels deep.
    /// Events carry their names only, never their values.
    pub properties: serde_json::Map<String, serde_json::Value>,
}

impl AdmissionRequest {
    /// Request with no resource identifier and no properties.
    #[must_use]
    pub fn new(
        enforcing_gear: impl Into<String>,
        action: impl Into<String>,
        resource_type: impl Into<String>,
        resource_tenant_id: Uuid,
    ) -> Self {
        Self {
            enforcing_gear: enforcing_gear.into(),
            action: action.into(),
            resource_type: resource_type.into(),
            resource_id: None,
            resource_tenant_id,
            properties: serde_json::Map::new(),
        }
    }

    /// Names the target resource.
    #[must_use]
    pub fn with_resource_id(mut self, resource_id: Uuid) -> Self {
        self.resource_id = Some(resource_id);
        self
    }

    /// Adds (or replaces) one operation property.
    #[must_use]
    pub fn with_property(mut self, name: impl Into<String>, value: serde_json::Value) -> Self {
        self.properties.insert(name.into(), value);
        self
    }
}

/// Deepest nesting a property value may have: a property's own value is at
/// depth 1, and each enclosing array or object adds one. A request nested
/// deeper is refused as too large ([`SizeBound::ContextDepth`]) before anything
/// walks the properties recursively.
pub const PROPERTY_MAX_DEPTH: usize = 64;

/// Longest identifier [`validate_identifier`] accepts, in bytes.
pub const IDENTIFIER_MAX_LEN: usize = 128;

/// Checks that `value` is an identifier of the grammar
/// `^[a-z0-9][a-z0-9._:-]{0,127}$`: one to [`IDENTIFIER_MAX_LEN`] ASCII
/// bytes, the first a lowercase letter or digit, the rest lowercase letters,
/// digits, `.`, `_`, `:` or `-`.
///
/// The grammar keeps free text, whitespace, control characters and
/// arbitrary lengths out of the fields the refusal event copies verbatim.
///
/// # Errors
///
/// `invalid_argument` naming `field`, with reason
/// [`INVALID_IDENTIFIER`](reason::INVALID_IDENTIFIER). The error never
/// echoes `value`.
pub fn validate_identifier(field: &'static str, value: &str) -> Result<(), AdmissionError> {
    if is_identifier(value) {
        Ok(())
    } else {
        Err(invalid_request(
            field,
            "must match ^[a-z0-9][a-z0-9._:-]{0,127}$",
            reason::INVALID_IDENTIFIER,
        ))
    }
}

/// Longest resource type [`validate_resource_type`] accepts, in bytes.
pub const RESOURCE_TYPE_MAX_LEN: usize = 256;

/// Checks that `value` is a GTS **type** identifier (it parses as a GTS
/// identifier and ends in `~`), carries no surrounding whitespace, and is at
/// most [`RESOURCE_TYPE_MAX_LEN`] bytes long.
///
/// The length is checked before the identifier is parsed, so an oversized
/// value costs nothing to reject.
///
/// # Errors
///
/// `invalid_argument` naming the field `resource_type`, with reason
/// [`INVALID_IDENTIFIER`](reason::INVALID_IDENTIFIER). The error never
/// echoes `value`.
pub fn validate_resource_type(value: &str) -> Result<(), AdmissionError> {
    if is_resource_type(value) {
        Ok(())
    } else {
        Err(invalid_request(
            "resource_type",
            "must be a GTS type identifier (ending in `~`) of at most 256 bytes",
            reason::INVALID_IDENTIFIER,
        ))
    }
}

fn is_resource_type(value: &str) -> bool {
    // `GtsTypeId::try_new` trims its input and allows 1 024 bytes, while the
    // value is copied verbatim into events: check both here, length first.
    value.len() <= RESOURCE_TYPE_MAX_LEN
        && value.trim() == value
        && gts::GtsTypeId::try_new(value).is_ok()
}

fn is_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some((first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= IDENTIFIER_MAX_LEN
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && rest.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b':' | b'-')
        })
}

// ---------------------------------------------------------------------------
// Verdict side
// ---------------------------------------------------------------------------

/// The gate's whole answer for one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// The operation may proceed.
    Admitted(Admission),
    /// The operation must not proceed.
    Refused(Refusal),
}

impl Verdict {
    /// `true` only for [`Verdict::Admitted`].
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted(_))
    }

    /// Correlation identifier the gate minted for this operation.
    #[must_use]
    pub fn correlation_id(&self) -> Uuid {
        match self {
            Self::Admitted(a) => a.correlation_id,
            Self::Refused(r) => r.correlation_id,
        }
    }
}

/// An admitted verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// Correlation identifier the gate minted for this operation.
    pub correlation_id: Uuid,
}

/// A refused verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// Why the operation was refused.
    pub cause: RefusalCause,
    /// Correlation identifier the gate minted for this operation.
    pub correlation_id: Uuid,
}

impl Refusal {
    /// Projects the refusal onto a canonical error.
    #[must_use]
    pub fn to_canonical_error(&self) -> AdmissionError {
        self.cause.to_canonical_error()
    }
}

/// Reference to a policy document that denied (or would have denied) an
/// operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyReference {
    /// Bundle holding the document.
    pub bundle_id: Uuid,
    /// Bundle version holding the document.
    pub version_id: Uuid,
    /// The document.
    pub document_id: Uuid,
    /// Document name.
    pub document_name: String,
}

/// Machine-readable refusal cause.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefusalCause {
    /// Refused by tenant policy.
    Policy {
        /// Engine reason code.
        reason_code: String,
        /// Every policy document that denied the operation.
        denials: Vec<PolicyReference>,
    },
    /// The request exceeded a configured size bound.
    RequestTooLarge {
        /// The bound that was exceeded.
        bound: SizeBound,
    },
    /// The engine judged the operation's properties invalid. Not retryable:
    /// the caller must change the request. Why it was judged invalid goes to
    /// operator logs only.
    InvalidRequest,
    /// A check could not run: an incident, not a policy outcome. Retryable,
    /// except for [`FailureCondition::Internal`], which is a defect.
    CouldNotRun {
        /// What prevented the check from running.
        condition: FailureCondition,
    },
}

impl RefusalCause {
    /// Stable reason code of this cause's family.
    #[must_use]
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::Policy { .. } => reason::POLICY_REFUSED,
            Self::RequestTooLarge { .. } => reason::REQUEST_TOO_LARGE,
            Self::InvalidRequest => reason::INVALID_REQUEST,
            Self::CouldNotRun { .. } => reason::COULD_NOT_RUN,
        }
    }

    /// `true` only for [`RefusalCause::CouldNotRun`] with a condition other
    /// than [`FailureCondition::Internal`]: a defect does not go away on
    /// retry.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::CouldNotRun { condition } if *condition != FailureCondition::Internal)
    }

    /// Projects the cause onto a canonical error.
    ///
    /// | Cause | Category | Where the reason code and identity travel |
    /// |---|---|---|
    /// | `Policy` | `failed_precondition` (400) | violation `type` = `POLICY_REFUSED`, `subject` = engine reason code |
    /// | `RequestTooLarge` | `out_of_range` (400) | field violation `field` = bound name |
    /// | `InvalidRequest` | `invalid_argument` (400) | field violation `field` = `properties` |
    /// | `CouldNotRun` (`internal`) | `internal` (500) | detail `COULD_NOT_RUN: internal` |
    /// | `CouldNotRun` (any other) | `service_unavailable` (503) | detail `COULD_NOT_RUN: <condition>` |
    #[must_use]
    pub fn to_canonical_error(&self) -> AdmissionError {
        match self {
            Self::Policy { reason_code, .. } => AdmissionResourceError::failed_precondition()
                .with_precondition_violation(
                    reason_code.clone(),
                    "operation refused by policy",
                    reason::POLICY_REFUSED,
                )
                .create(),
            Self::RequestTooLarge { bound } => AdmissionResourceError::out_of_range(format!(
                "admission request exceeds the {} bound",
                bound.as_str()
            ))
            .with_field_violation(
                bound.as_str(),
                "request exceeds the configured bound; send less rather than retrying",
                reason::REQUEST_TOO_LARGE,
            )
            .create(),
            Self::InvalidRequest => AdmissionResourceError::invalid_argument()
                .with_field_violation(
                    "properties",
                    "the policy engine rejected the operation's properties",
                    reason::INVALID_REQUEST,
                )
                .create(),
            Self::CouldNotRun {
                condition: FailureCondition::Internal,
            } => AdmissionError::internal(format!(
                "{}: {}",
                reason::COULD_NOT_RUN,
                FailureCondition::Internal.as_str()
            ))
            .create(),
            Self::CouldNotRun { condition } => AdmissionError::service_unavailable()
                .with_detail(format!("{}: {}", reason::COULD_NOT_RUN, condition.as_str()))
                .create(),
        }
    }
}

/// A caller-submission size bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SizeBound {
    /// Maximum serialised size of one admission's operation context.
    ContextBytes,
    /// Maximum property count of one admission's operation context.
    PropertyCount,
    /// Maximum nesting depth of one admission's operation context
    /// ([`PROPERTY_MAX_DEPTH`]).
    ContextDepth,
}

impl SizeBound {
    /// Stable `snake_case` name of the bound.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextBytes => "context_bytes",
            Self::PropertyCount => "property_count",
            Self::ContextDepth => "context_depth",
        }
    }
}

impl fmt::Display for SizeBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a check could not run. Every variant refuses (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureCondition {
    /// No engine is selected.
    NoEngine,
    /// The engine could not be reached or is unavailable.
    EngineUnavailable,
    /// The engine did not answer within the engine timeout.
    EngineTimeout,
    /// The engine returned an error.
    EngineError,
    /// A defect in the gate or in the gate–engine contract: the engine could
    /// not process what the gate sent, or answered outside the gate's bounds.
    /// Not retryable.
    Internal,
}

impl FailureCondition {
    /// Stable `snake_case` label, used for metrics and events.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoEngine => "no_engine",
            Self::EngineUnavailable => "engine_unavailable",
            Self::EngineTimeout => "engine_timeout",
            Self::EngineError => "engine_error",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for FailureCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Refusal event (published on the audit topic)
// ---------------------------------------------------------------------------

/// Refusal cause as carried by the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RefusalEventCause {
    /// Refused (or shadow-denied) by tenant policy (see `policy`).
    Policy,
    /// Refused because the request exceeded a size bound.
    RequestTooLarge,
    /// Refused because the engine judged the operation's properties invalid.
    InvalidRequest,
    /// Refused because a check could not run (see `condition`).
    CouldNotRun,
}

impl RefusalEventCause {
    /// Stable `snake_case` label, as serialized; also the `cause` label of
    /// the gate's verdict metric.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::RequestTooLarge => "request_too_large",
            Self::InvalidRequest => "invalid_request",
            Self::CouldNotRun => "could_not_run",
        }
    }
}

impl From<&RefusalCause> for RefusalEventCause {
    fn from(cause: &RefusalCause) -> Self {
        match cause {
            RefusalCause::Policy { .. } => Self::Policy,
            RefusalCause::RequestTooLarge { .. } => Self::RequestTooLarge,
            RefusalCause::InvalidRequest => Self::InvalidRequest,
            RefusalCause::CouldNotRun { .. } => Self::CouldNotRun,
        }
    }
}

/// One refusal or shadow finding, as published under
/// [`REFUSAL_EVENT_TYPE`](crate::gts::REFUSAL_EVENT_TYPE): one event per
/// (operation, policy) pair. Property values are never carried, only names.
///
/// This is the event's `data`. The correlation identifier, the instant and the
/// resource tenant travel in the broker envelope (`subject`, `occurred_at`,
/// `tenant_id`), so they are not repeated here.
///
/// The event type's `data` schema is generated from this type. Optional
/// fields are omitted rather than `null`, so their schemas name the inner
/// type and never admit `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefusalEvent {
    /// Calling enforcing gear.
    pub enforcing_gear: String,
    /// Action requested.
    pub action: String,
    /// GTS type identifier of the target resource.
    pub resource_type: String,
    /// Target resource identifier, where the request named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Uuid")]
    pub resource_id: Option<Uuid>,
    /// Subject, from the security context.
    pub subject_id: Uuid,
    /// Subject's tenant, from the security context.
    pub subject_tenant_id: Uuid,
    /// `false` for a shadow finding (the operation was not refused by it).
    pub enforced: bool,
    /// Refusal cause.
    pub cause: RefusalEventCause,
    /// For a could-not-run refusal: the failure condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "FailureCondition")]
    pub condition: Option<FailureCondition>,
    /// For a policy refusal or shadow finding: the responsible document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "PolicyReference")]
    pub policy: Option<PolicyReference>,
    /// Names of every supplied property (never values).
    #[serde(default)]
    pub property_names: Vec<String>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
