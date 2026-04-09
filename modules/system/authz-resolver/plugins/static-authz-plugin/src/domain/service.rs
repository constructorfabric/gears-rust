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
            // No tenant resolvable from context or subject - deny access.
            return EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            };
        };

        if tid == Uuid::default() {
            // Nil UUID tenant - deny rather than grant unrestricted access.
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
mod tests {
    use super::*;
    use authz_resolver_sdk::pep::IntoPropertyValue;
    use authz_resolver_sdk::{Action, EvaluationRequestContext, Resource, Subject, TenantContext};
    use std::collections::HashMap;

    fn make_request(require_constraints: bool, tenant_id: Option<Uuid>) -> EvaluationRequest {
        let mut subject_properties = HashMap::new();
        subject_properties.insert(
            "tenant_id".to_owned(),
            serde_json::Value::String("22222222-2222-2222-2222-222222222222".to_owned()),
        );

        EvaluationRequest {
            subject: Subject {
                id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
                subject_type: None,
                properties: subject_properties,
            },
            action: Action {
                name: "list".to_owned(),
            },
            resource: Resource {
                resource_type: "gts.x.core.users.user.v1~".to_owned(),
                id: None,
                properties: HashMap::new(),
            },
            context: EvaluationRequestContext {
                tenant_context: tenant_id.map(|id| TenantContext {
                    root_id: Some(id),
                    ..TenantContext::default()
                }),
                token_scopes: vec!["*".to_owned()],
                require_constraints,
                capabilities: vec![],
                supported_properties: vec![],
                bearer_token: None,
            },
        }
    }

    #[test]
    fn list_operation_with_tenant_context() {
        let tenant_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let service = Service::new();
        let response = service.evaluate(&make_request(true, Some(tenant_id)));

        assert!(response.decision);
        assert_eq!(response.context.constraints.len(), 1);

        let constraint = &response.context.constraints[0];
        assert_eq!(constraint.predicates.len(), 1);

        match &constraint.predicates[0] {
            Predicate::In(in_pred) => {
                assert_eq!(in_pred.property, pep_properties::OWNER_TENANT_ID);
                assert_eq!(in_pred.values, vec![tenant_id.into_filter_value()]);
            }
            other => panic!("Expected In predicate, got: {other:?}"),
        }
    }

    #[test]
    fn list_operation_without_tenant_falls_back_to_subject_properties() {
        let service = Service::new();
        let response = service.evaluate(&make_request(true, None));

        // Falls back to subject.properties["tenant_id"]
        assert!(response.decision);
        assert_eq!(response.context.constraints.len(), 1);

        match &response.context.constraints[0].predicates[0] {
            Predicate::In(in_pred) => {
                assert_eq!(
                    in_pred.values,
                    vec![
                        Uuid::parse_str("22222222-2222-2222-2222-222222222222")
                            .unwrap()
                            .into_filter_value()
                    ]
                );
            }
            other => panic!("Expected In predicate, got: {other:?}"),
        }
    }

    #[test]
    fn nil_tenant_is_denied() {
        let service = Service::new();
        let response = service.evaluate(&make_request(true, Some(Uuid::default())));

        assert!(!response.decision);
        assert!(response.context.constraints.is_empty());
    }

    #[test]
    fn missing_tenant_context_and_subject_property_is_denied() {
        let request = EvaluationRequest {
            subject: Subject {
                id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
                subject_type: None,
                properties: HashMap::new(), // no tenant_id property
            },
            action: Action {
                name: "list".to_owned(),
            },
            resource: Resource {
                resource_type: "gts.x.core.users.user.v1~".to_owned(),
                id: None,
                properties: HashMap::new(),
            },
            context: EvaluationRequestContext {
                tenant_context: None,
                token_scopes: vec!["*".to_owned()],
                require_constraints: true,
                capabilities: vec![],
                supported_properties: vec![],
                bearer_token: None,
            },
        };

        let service = Service::new();
        let response = service.evaluate(&request);

        assert!(!response.decision);
        assert!(response.context.constraints.is_empty());
    }
}
