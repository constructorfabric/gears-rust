//! Admission models: the request an enforcing gear submits, the verdict it
//! receives, the refusal causes, and the refusal event payload.
//!
//! The request and verdict types are plain value types without serde: they
//! cross an in-process boundary only. The refusal event payload and the types
//! it embeds are the exception: they are the wire contract of the audit topic
//! and carry serde derives.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
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
    /// interpretation. Events carry their names only, never their values.
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
    value.len() <= RESOURCE_TYPE_MAX_LEN
        && value.trim() == value
        && gts::GtsId::try_new(value).is_ok_and(|id| id.is_type())
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub enum RefusalCause {
    /// Refused by a built-in platform policy.
    BuiltinPolicy {
        /// Stable identity of the refusing built-in policy.
        policy_id: String,
    },
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
    /// A check could not run. Retryable; an incident, not a policy outcome.
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
            Self::BuiltinPolicy { .. } => reason::BUILTIN_POLICY_REFUSED,
            Self::Policy { .. } => reason::POLICY_REFUSED,
            Self::RequestTooLarge { .. } => reason::REQUEST_TOO_LARGE,
            Self::CouldNotRun { .. } => reason::COULD_NOT_RUN,
        }
    }

    /// `true` only for [`RefusalCause::CouldNotRun`].
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::CouldNotRun { .. })
    }

    /// Projects the cause onto a canonical error.
    ///
    /// | Cause | Category | Where the reason code and identity travel |
    /// |---|---|---|
    /// | `BuiltinPolicy` | `failed_precondition` (400) | violation `type` = `BUILTIN_POLICY_REFUSED`, `subject` = policy id |
    /// | `Policy` | `failed_precondition` (400) | violation `type` = `POLICY_REFUSED`, `subject` = engine reason code |
    /// | `RequestTooLarge` | `out_of_range` (400) | field violation `field` = bound name |
    /// | `CouldNotRun` | `service_unavailable` (503) | detail `COULD_NOT_RUN: <condition>` |
    #[must_use]
    pub fn to_canonical_error(&self) -> AdmissionError {
        match self {
            Self::BuiltinPolicy { policy_id } => AdmissionResourceError::failed_precondition()
                .with_precondition_violation(
                    policy_id.clone(),
                    "operation refused by a built-in platform policy",
                    reason::BUILTIN_POLICY_REFUSED,
                )
                .create(),
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
            Self::CouldNotRun { condition } => AdmissionError::service_unavailable()
                .with_detail(format!("{}: {}", reason::COULD_NOT_RUN, condition.as_str()))
                .create(),
        }
    }
}

/// A caller-submission size bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeBound {
    /// Maximum serialised size of one admission's operation context.
    ContextBytes,
    /// Maximum property count of one admission's operation context.
    PropertyCount,
}

impl SizeBound {
    /// Stable `snake_case` name of the bound.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextBytes => "context_bytes",
            Self::PropertyCount => "property_count",
        }
    }
}

impl fmt::Display for SizeBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a check could not run. Every variant refuses (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCondition {
    /// No engine is selected.
    NoEngine,
    /// The engine could not be reached or is unavailable.
    EngineUnavailable,
    /// The engine did not answer within the engine timeout.
    EngineTimeout,
    /// The engine returned an error.
    EngineError,
    /// A built-in policy failed to evaluate (error or timeout).
    BuiltinPolicyFailure,
    /// Internal error in the gate.
    Internal,
}

impl FailureCondition {
    /// Every condition, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::NoEngine,
        Self::EngineUnavailable,
        Self::EngineTimeout,
        Self::EngineError,
        Self::BuiltinPolicyFailure,
        Self::Internal,
    ];

    /// Stable `snake_case` label, used for metrics and events.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoEngine => "no_engine",
            Self::EngineUnavailable => "engine_unavailable",
            Self::EngineTimeout => "engine_timeout",
            Self::EngineError => "engine_error",
            Self::BuiltinPolicyFailure => "builtin_policy_failure",
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalEventCause {
    /// Refused by a built-in policy (see `builtin_policy_id`).
    BuiltinPolicy,
    /// Refused (or shadow-denied) by tenant policy (see `policy`).
    Policy,
    /// Refused because the request exceeded a size bound.
    RequestTooLarge,
    /// Refused because a check could not run (see `condition`).
    CouldNotRun,
}

impl From<&RefusalCause> for RefusalEventCause {
    fn from(cause: &RefusalCause) -> Self {
        match cause {
            RefusalCause::BuiltinPolicy { .. } => Self::BuiltinPolicy,
            RefusalCause::Policy { .. } => Self::Policy,
            RefusalCause::RequestTooLarge { .. } => Self::RequestTooLarge,
            RefusalCause::CouldNotRun { .. } => Self::CouldNotRun,
        }
    }
}

/// One refusal or shadow finding, as published under
/// [`REFUSAL_EVENT_TYPE`](crate::gts::REFUSAL_EVENT_TYPE): one event per
/// (operation, policy) pair. Property values are never carried, only names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalEvent {
    /// Correlation identifier the gate minted for the operation.
    pub correlation_id: Uuid,
    /// Instant the event was produced (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    /// Calling enforcing gear.
    pub enforcing_gear: String,
    /// Action requested.
    pub action: String,
    /// GTS type identifier of the target resource.
    pub resource_type: String,
    /// Target resource identifier, where the request named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<Uuid>,
    /// Tenant owning the target resource.
    pub resource_tenant_id: Uuid,
    /// Subject, from the security context.
    pub subject_id: Uuid,
    /// Subject's tenant, from the security context.
    pub subject_tenant_id: Uuid,
    /// `false` for a shadow finding (the operation was not refused by it).
    pub enforced: bool,
    /// Refusal cause.
    pub cause: RefusalEventCause,
    /// For a built-in refusal: the policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin_policy_id: Option<String>,
    /// For a could-not-run refusal: the failure condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<FailureCondition>,
    /// For a policy refusal or shadow finding: the responsible document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyReference>,
    /// Names of every supplied property (never values).
    #[serde(default)]
    pub property_names: Vec<String>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
