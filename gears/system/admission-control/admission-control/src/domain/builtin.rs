//! Built-in policies: compiled Rego documents selected by resource type
//! pattern and action, evaluated in configuration order. A built-in can only
//! deny or stay silent; it has no way to admit.
//!
//! The Rego `input` is
//! `{ enforcing_gear, action, resource: { type, id, tenant_id }, properties,
//! subject: { id, tenant_id } }` plus the gate-supplied `evaluated_at`.

use std::sync::Arc;
use std::time::Duration;

use admission_control_sdk::AdmissionRequest;
use gts::{GtsId, GtsIdPattern};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_policy_evaluation::{CompiledDocument, CostBound, EvaluationContext};
use uuid::Uuid;

/// Entrypoint rule every built-in policy defines.
pub const BUILTIN_ENTRYPOINT: &str = "deny";

/// One compiled built-in policy.
#[domain_model]
#[derive(Debug)]
pub struct BuiltinPolicy {
    /// Stable identity.
    pub id: String,
    /// Resource-type selectors as configured (GTS patterns; a concrete type id
    /// is a pattern without wildcard).
    pub resource_types: Vec<String>,
    /// Parsed form of `resource_types`.
    pub patterns: Vec<GtsIdPattern>,
    /// Actions the policy applies to; empty means every action.
    pub actions: Vec<String>,
    /// The compiled document.
    pub document: Arc<dyn CompiledDocument>,
}

impl BuiltinPolicy {
    fn applies_to(&self, resource_type: &GtsId, action: &str) -> bool {
        (self.actions.is_empty() || self.actions.iter().any(|a| a == action))
            && self
                .patterns
                .iter()
                .any(|pattern| resource_type.matches_pattern(pattern))
    }
}

/// Result of built-in evaluation. There is deliberately no admitting variant.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinOutcome {
    /// No selected policy denies: proceed to the engine.
    NoProhibition,
    /// The first policy, in configuration order, that denies.
    Prohibited {
        /// Stable identity of the policy.
        policy_id: String,
    },
    /// A policy could not be evaluated, so its denial cannot be ruled out.
    Failed {
        /// The policy that failed.
        policy_id: String,
    },
}

/// The immutable set of built-in policies, in configuration order.
#[domain_model]
#[derive(Debug, Default)]
pub struct BuiltinPolicySet {
    policies: Vec<BuiltinPolicy>,
}

impl BuiltinPolicySet {
    /// A set over `policies`.
    #[must_use]
    pub fn new(policies: Vec<BuiltinPolicy>) -> Self {
        Self { policies }
    }

    /// Number of policies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.policies.len()
    }

    /// Whether the set holds no policy.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Every concrete (non-wildcard) resource type, for registry resolution.
    pub fn concrete_resource_types(&self) -> impl Iterator<Item = &str> {
        self.policies
            .iter()
            .flat_map(|policy| policy.resource_types.iter())
            .map(String::as_str)
            .filter(|selector| !selector.contains('*'))
    }

    /// Evaluates the policies applicable to `request`, each under `timeout`;
    /// the first denial wins. Any evaluation error or non-boolean result is
    /// [`BuiltinOutcome::Failed`].
    #[must_use]
    pub fn evaluate(
        &self,
        request: &AdmissionRequest,
        subject: (Uuid, Uuid),
        evaluated_at: OffsetDateTime,
        timeout: Duration,
    ) -> BuiltinOutcome {
        let Ok(resource_type) = GtsId::try_new(&request.resource_type) else {
            return BuiltinOutcome::NoProhibition;
        };
        let mut applicable = self
            .policies
            .iter()
            .filter(|policy| policy.applies_to(&resource_type, &request.action))
            .peekable();
        let Some(first) = applicable.peek() else {
            return BuiltinOutcome::NoProhibition;
        };
        let failed = |policy: &BuiltinPolicy| BuiltinOutcome::Failed {
            policy_id: policy.id.clone(),
        };
        let Ok(ctx) = EvaluationContext::new(build_input(request, subject), evaluated_at) else {
            return failed(first);
        };
        for policy in applicable {
            match policy.document.evaluate(&ctx, CostBound::new(timeout)) {
                Ok(Value::Bool(true)) => {
                    return BuiltinOutcome::Prohibited {
                        policy_id: policy.id.clone(),
                    };
                }
                Ok(Value::Bool(false) | Value::Null) => {}
                // The backend's message may quote request data: log the
                // policy identity only.
                Ok(_) | Err(_) => {
                    tracing::warn!(policy_id = %policy.id, "built-in policy evaluation failed");
                    return failed(policy);
                }
            }
        }
        BuiltinOutcome::NoProhibition
    }
}

/// Builds the Rego `input` document from the borrowed request and the
/// `(subject id, subject tenant id)` taken from the security context.
#[must_use]
pub fn build_input(request: &AdmissionRequest, subject: (Uuid, Uuid)) -> Value {
    json!({
        "enforcing_gear": request.enforcing_gear,
        "action": request.action,
        "resource": {
            "type": request.resource_type,
            "id": request.resource_id,
            "tenant_id": request.resource_tenant_id,
        },
        "properties": Value::Object(request.properties.clone()),
        "subject": { "id": subject.0, "tenant_id": subject.1 },
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "builtin_tests.rs"]
mod builtin_tests;
