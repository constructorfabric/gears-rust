//! Recorded PDP responses test PEP/SQL enforcement, not deployment policy decisions.
use authz_resolver_sdk::{
    AuthZResolverApi, Constraint, EqPredicate, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, PolicyEnforcer, Predicate,
};
use parking_lot::Mutex;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

pub fn eq(property: &str, value: Uuid) -> Predicate {
    Predicate::Eq(EqPredicate::new(property, value))
}
pub fn path(predicates: Vec<Predicate>) -> Constraint {
    Constraint { predicates }
}
pub fn allow(constraints: Vec<Constraint>) -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints,
            deny_reason: None,
        },
    }
}
#[derive(Clone)]
pub struct Recorded {
    pub response: Option<EvaluationResponse>,
    pub requests: Arc<Mutex<Vec<EvaluationRequest>>>,
}
impl Recorded {
    pub fn new(response: Option<EvaluationResponse>) -> Self {
        Self {
            response,
            requests: Arc::new(Mutex::new(vec![])),
        }
    }
    pub fn enforcer(&self) -> PolicyEnforcer {
        PolicyEnforcer::new(Arc::new(self.clone()))
    }
}
#[async_trait::async_trait]
impl AuthZResolverApi for Recorded {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.requests.lock().push(req);
        self.response
            .clone()
            .ok_or_else(|| CanonicalError::service_unavailable().create())
    }
}
/// Real bundled provider, called through the exact resolver SDK contract.
pub struct BundledStatic;
#[async_trait::async_trait]
impl AuthZResolverApi for BundledStatic {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(static_authz_plugin::domain::Service::new().evaluate(&req))
    }
}
#[allow(clippy::expect_used)] // Fixed test identity, never used by runtime code.
pub fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(10))
        .subject_tenant_id(Uuid::from_u128(11))
        .subject_type(toolkit_gts::gts_id!("cf.core.security.subject_user.v1~"))
        .build()
        .expect("fixed caller fixture")
}
#[allow(clippy::expect_used)] // Malformed recorded fixtures must fail the test.
pub async fn scope(constraints: Vec<Constraint>) -> AccessScope {
    Recorded::new(Some(allow(constraints)))
        .enforcer()
        .access_scope(&caller(), &bss_orders_lifecycle::gts::ORDER, "read", None)
        .await
        .expect("valid recorded scope")
}
