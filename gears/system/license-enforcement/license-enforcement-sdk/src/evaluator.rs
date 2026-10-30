//! The evaluator contract (DESIGN §3.1, §3.3, ADR-0003).
//!
//! The evaluator decides which grants satisfy a check. It is a pure function:
//! it reads nothing, calls nothing, and writes nothing. The binary links one
//! evaluator crate, which registers itself with [`register_evaluator!`](crate::register_evaluator); the
//! gear takes it with [`linked_evaluator`].

use std::sync::Arc;

use gts::GtsTypeId;
use license_resolver_sdk::LicenseCheckRequest;
use time::OffsetDateTime;
use toolkit_canonical_errors::FieldViolation;

use crate::link::{LinkError, exactly_one};
use crate::models::{Conditions, Grant};
use crate::reason::DenialReason;

/// One binding filter. The store reads the live grants whose binding type
/// matches `type_pattern` (by GTS derivation) and whose binding id is
/// `binding_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BindingFilter {
    /// GTS type pattern. A type that derives from it matches.
    pub type_pattern: GtsTypeId,
    /// Binding id.
    pub binding_id: String,
}

impl BindingFilter {
    /// A filter on a type pattern and a binding id.
    #[must_use]
    pub fn new(type_pattern: GtsTypeId, binding_id: impl Into<String>) -> Self {
        Self {
            type_pattern,
            binding_id: binding_id.into(),
        }
    }
}

/// Result of [`GrantEvaluatorV1::evaluate`] for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Evaluation {
    /// The candidate passes.
    Pass,
    /// The candidate passes in its grace period.
    PassInGrace,
    /// The candidate fails.
    Deny(EvaluatorDenial),
}

/// Why [`GrantEvaluatorV1::evaluate`] denied a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EvaluatorDenial {
    /// The root or the candidate awaits acceptance.
    AwaitingAcceptance,
    /// The root or the candidate is suspended.
    Suspended,
    /// The root or the candidate is terminated.
    Terminated,
    /// The window has not started.
    NotYetValid,
    /// The window ended and there is no grace period.
    Expired,
    /// The grace period ended.
    GraceExpired,
    /// A condition is false, or its attribute is absent.
    ConditionUnmet,
    /// Condition evaluation exceeded the per-check budget.
    ConditionBudgetExceeded,
}

impl From<EvaluatorDenial> for DenialReason {
    fn from(denial: EvaluatorDenial) -> Self {
        match denial {
            EvaluatorDenial::AwaitingAcceptance => Self::AwaitingAcceptance,
            EvaluatorDenial::Suspended => Self::Suspended,
            EvaluatorDenial::Terminated => Self::Terminated,
            EvaluatorDenial::NotYetValid => Self::NotYetValid,
            EvaluatorDenial::Expired => Self::Expired,
            EvaluatorDenial::GraceExpired => Self::GraceExpired,
            EvaluatorDenial::ConditionUnmet => Self::ConditionUnmet,
            EvaluatorDenial::ConditionBudgetExceeded => Self::ConditionBudgetExceeded,
        }
    }
}

/// Rules cannot be evaluated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EvaluationError {
    /// Evaluation exceeded the budget in evaluation steps.
    #[error("evaluation budget exceeded")]
    BudgetExceeded,
    /// The rules are not in the evaluator's form.
    #[error("malformed rules: {0}")]
    Malformed(String),
}

/// The evaluator contract, version 1.
pub trait GrantEvaluatorV1: Send + Sync {
    /// Type ids that the evaluator names. The gear checks at init that each one
    /// is registered.
    fn named_types(&self) -> Vec<GtsTypeId>;

    /// Ordered binding filters for a request. The order sets the candidate
    /// order.
    fn candidate_filter(&self, request: &LicenseCheckRequest) -> Vec<BindingFilter>;

    /// Evaluate one candidate and its root at `instant`: states, validity
    /// window, grace, and conditions against the request `metadata`.
    fn evaluate(
        &self,
        request: &LicenseCheckRequest,
        candidate: &Grant,
        root: &Grant,
        instant: OffsetDateTime,
    ) -> Evaluation;

    /// Violations of conditions or recipient rules against the JSON schema of
    /// the object that they address. Empty when valid.
    fn validate(&self, rules: &Conditions, schema: &serde_json::Value) -> Vec<FieldViolation>;

    /// Evaluate conditions or recipient rules against an object, for example
    /// the recipient projection at `delegate`.
    ///
    /// # Errors
    ///
    /// [`EvaluationError`] when the rules cannot be evaluated.
    fn matches(
        &self,
        rules: &Conditions,
        object: &serde_json::Value,
    ) -> Result<bool, EvaluationError>;
}

/// Link-time registration of an evaluator. Create it with
/// [`register_evaluator!`](crate::register_evaluator).
pub struct EvaluatorRegistration {
    /// Builds the evaluator.
    pub factory: fn() -> Arc<dyn GrantEvaluatorV1>,
}

inventory::collect!(EvaluatorRegistration);

/// Register the evaluator of this crate at link time.
///
/// ```ignore
/// license_enforcement_sdk::register_evaluator!(|| std::sync::Arc::new(MyEvaluator));
/// ```
#[macro_export]
macro_rules! register_evaluator {
    ($factory:expr) => {
        $crate::__private::inventory::submit! {
            $crate::EvaluatorRegistration { factory: $factory }
        }
    };
}

/// The one evaluator that the binary links.
///
/// # Errors
///
/// [`LinkError`] when the binary links no evaluator or more than one.
pub fn linked_evaluator() -> Result<Arc<dyn GrantEvaluatorV1>, LinkError> {
    exactly_one(inventory::iter::<EvaluatorRegistration>).map(|r| (r.factory)())
}
