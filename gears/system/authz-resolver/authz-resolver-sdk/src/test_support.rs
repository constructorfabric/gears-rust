//! Authorization fixtures shared by consumer tests (`test-util` feature).

use uuid::Uuid;

/// A permitting PDP with an empty tenant branch and an optional live OR branch.
///
/// Tests empty query results, refused permission gates, and live alternatives.
pub struct EmptyScopePdp {
    /// Include a live branch for the request subject's tenant.
    pub live: bool,
}
#[async_trait::async_trait]
impl crate::AuthZResolverApi for EmptyScopePdp {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: crate::EvaluationRequest,
    ) -> Result<crate::EvaluationResponse, toolkit::api::canonical_prelude::CanonicalError> {
        use crate::constraints::{Constraint, InPredicate, Predicate};
        let mut constraints = vec![Constraint {
            predicates: vec![Predicate::In(InPredicate::new(
                toolkit_security::pep_properties::OWNER_TENANT_ID,
                Vec::<Uuid>::new(),
            ))],
        }];
        if self.live {
            let tenant = request
                .subject
                .properties
                .get("tenant_id")
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok())
                .unwrap_or(Uuid::nil());
            constraints.push(Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    toolkit_security::pep_properties::OWNER_TENANT_ID,
                    [tenant],
                ))],
            });
        }
        Ok(crate::EvaluationResponse {
            decision: true,
            context: crate::EvaluationResponseContext {
                constraints,
                deny_reason: None,
            },
        })
    }
}
