//! Service implementation for the static `AuthZ` resolver plugin.

use authz_resolver_sdk::{
    Constraint, EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InPredicate,
    Predicate,
};
use modkit_macros::domain_model;
use modkit_security::pep_properties;
use uuid::Uuid;

/// Static `AuthZ` resolver service.
///
/// - Returns `decision: true` with an `in` predicate on `pep_properties::OWNER_TENANT_ID`
///   scoped to the context tenant from the request (for all operations including CREATE).
/// - Denies access (`decision: false`) when no valid tenant can be resolved.
#[domain_model]
#[derive(Default)]
pub struct Service;

impl Service {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Evaluate an authorization request.
    #[must_use]
    #[allow(clippy::unused_self)] // &self reserved for future config/state
    pub fn evaluate(&self, request: &EvaluationRequest) -> EvaluationResponse {
        // Always scope to context tenant (all CRUD operations get constraints)
        let tenant_id = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|t| t.root_id)
            .or_else(|| {
                // Fallback: extract tenant_id from subject properties
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            });

        let Some(tid) = tenant_id else {
            // No tenant resolvable from context or subject — deny access.
            return EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            };
        };

        if tid == Uuid::default() {
            // Nil UUID tenant — deny rather than grant unrestricted access.
            return EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            };
        }

        EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tid],
                    ))],
                }],
                ..Default::default()
            },
        }
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod tests;
