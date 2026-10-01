//! Policy engine plugin contract (unstable).
//!
//! An engine registers its implementation in `ClientHub` scoped by its GTS
//! instance identifier, an instance of
//! [`AdmissionEnginePluginSpecV1`](crate::gts::AdmissionEnginePluginSpecV1).
//! Success is an [`EngineResult`] (permit or deny), failure is an
//! [`EngineFailure`]; a denial and a failure are never confused.

use std::fmt;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::models::{AdmissionRequest, FailureCondition, PolicyReference};

/// Plugin trait a policy engine implements.
///
/// The `SecurityContext` is the caller's, propagated unchanged: the engine
/// derives the subject from it, and the request carries no identity.
#[async_trait]
pub trait AdmissionEnginePluginClientV1: Send + Sync {
    /// Evaluates one gated operation.
    ///
    /// # Errors
    ///
    /// [`EngineFailure`] when the engine cannot answer; the gate refuses with
    /// the could-not-run cause.
    async fn evaluate(
        &self,
        ctx: &SecurityContext,
        request: &EngineRequest,
    ) -> Result<EngineResult, EngineFailure>;
}

/// What the gate sends the engine for one gated operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineRequest {
    /// Correlation identifier minted by the gate for this operation.
    pub correlation_id: Uuid,
    /// Calling enforcing gear.
    pub enforcing_gear: String,
    /// Action requested.
    pub action: String,
    /// GTS type identifier of the target resource.
    pub resource_type: String,
    /// Target resource identifier, where the request named one.
    pub resource_id: Option<Uuid>,
    /// Tenant owning the target resource.
    pub resource_tenant_id: Uuid,
    /// Caller-supplied operation properties, forwarded unexamined.
    pub properties: serde_json::Map<String, serde_json::Value>,
}

impl EngineRequest {
    /// Builds the engine request for `request`, stamped with the gate-minted
    /// correlation identifier.
    #[must_use]
    pub fn from_admission(request: &AdmissionRequest, correlation_id: Uuid) -> Self {
        Self {
            correlation_id,
            enforcing_gear: request.enforcing_gear.clone(),
            action: request.action.clone(),
            resource_type: request.resource_type.clone(),
            resource_id: request.resource_id,
            resource_tenant_id: request.resource_tenant_id,
            properties: request.properties.clone(),
        }
    }
}

/// The success side of an engine call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineResult {
    /// The engine permits the operation.
    Permit {
        /// Non-enforcing policies that would have denied.
        shadow_denials: Vec<PolicyReference>,
    },
    /// The engine denies the operation.
    Deny {
        /// Machine-readable reason code.
        reason_code: String,
        /// Every enforcing policy document that denied.
        denials: Vec<PolicyReference>,
        /// Non-enforcing policies that would have denied.
        shadow_denials: Vec<PolicyReference>,
    },
}

/// Failure condition an engine reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineFailureCondition {
    /// The engine is transiently unable to answer.
    Unavailable,
    /// The engine ran out of time.
    Timeout,
    /// Unexpected engine error.
    Internal,
    /// The engine judged the request malformed.
    InvalidRequest,
}

impl EngineFailureCondition {
    /// Stable `snake_case` label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
            Self::InvalidRequest => "invalid_request",
        }
    }

    /// The could-not-run condition the gate refuses with.
    #[must_use]
    pub fn failure_condition(self) -> FailureCondition {
        match self {
            Self::Unavailable => FailureCondition::EngineUnavailable,
            Self::Timeout => FailureCondition::EngineTimeout,
            Self::Internal | Self::InvalidRequest => FailureCondition::EngineError,
        }
    }
}

impl fmt::Display for EngineFailureCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The failure side of an engine call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineFailure {
    /// What went wrong.
    pub condition: EngineFailureCondition,
    /// Diagnostic detail for operator logs only; never returned to a caller
    /// or published.
    pub detail: String,
}

impl EngineFailure {
    /// Failure with the given condition.
    #[must_use]
    pub fn new(condition: EngineFailureCondition, detail: impl Into<String>) -> Self {
        Self {
            condition,
            detail: detail.into(),
        }
    }

    /// [`EngineFailureCondition::Unavailable`].
    #[must_use]
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::new(EngineFailureCondition::Unavailable, detail)
    }

    /// [`EngineFailureCondition::Timeout`].
    #[must_use]
    pub fn timeout(detail: impl Into<String>) -> Self {
        Self::new(EngineFailureCondition::Timeout, detail)
    }

    /// [`EngineFailureCondition::Internal`].
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(EngineFailureCondition::Internal, detail)
    }

    /// [`EngineFailureCondition::InvalidRequest`].
    #[must_use]
    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::new(EngineFailureCondition::InvalidRequest, detail)
    }
}

impl fmt::Display for EngineFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine failure ({}): {}", self.condition, self.detail)
    }
}

impl std::error::Error for EngineFailure {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "plugin_api_tests.rs"]
mod plugin_api_tests;
