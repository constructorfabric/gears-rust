//! Test PDPs: a scripted recording double (proves Orders asks the right question and enforces
//! the answer) and the real rules provider behind the resolver API. Neither is runtime code.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::{Caller, Pep};
use authz_resolver_sdk::{
    AuthZResolverApi, AuthZResolverPluginClient, Constraint, DenyReason, EqPredicate,
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InPredicate, PolicyEnforcer,
    Predicate,
};
use parking_lot::Mutex;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

pub type Script = dyn Fn(&EvaluationRequest) -> Option<EvaluationResponse> + Send + Sync;

/// Records every evaluation; `None` from the script is a provider outage.
pub struct ScriptedPdp {
    script: Box<Script>,
    pub requests: Mutex<Vec<EvaluationRequest>>,
}
impl ScriptedPdp {
    pub fn new(
        script: impl Fn(&EvaluationRequest) -> Option<EvaluationResponse> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            script: Box::new(script),
            requests: Mutex::new(Vec::new()),
        })
    }
    pub fn calls(&self) -> Vec<(String, Option<Uuid>)> {
        self.requests
            .lock()
            .iter()
            .map(|r| (r.action.name.clone(), r.resource.id))
            .collect()
    }
}
#[async_trait::async_trait]
impl AuthZResolverApi for ScriptedPdp {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let response = (self.script)(&request);
        self.requests.lock().push(request);
        response.ok_or_else(|| CanonicalError::service_unavailable().create())
    }
}

/// The real rules provider, reached through its plugin client like the resolver does.
pub struct RulesProvider(Arc<dyn AuthZResolverPluginClient>);
impl RulesProvider {
    pub fn from_policy(policy: serde_json::Value) -> Arc<Self> {
        let config: rules_authz_plugin::config::RulesAuthZPluginConfig =
            serde_json::from_value(policy).unwrap();
        Arc::new(Self(Arc::new(
            rules_authz_plugin::domain::Service::from_config(&config).unwrap(),
        )))
    }
}
#[async_trait::async_trait]
impl AuthZResolverApi for RulesProvider {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.0
            .evaluate(request)
            .await
            .map_err(|_| CanonicalError::service_unavailable().create())
    }
}

pub fn pep(api: Arc<dyn AuthZResolverApi>) -> Pep {
    Pep::new(Arc::new(PolicyEnforcer::new(api)))
}
pub fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
pub fn eq(property: &str, value: Uuid) -> Predicate {
    Predicate::Eq(EqPredicate::new(property, value))
}
pub fn is_in(property: &str, values: &[Uuid]) -> Predicate {
    Predicate::In(InPredicate::new(property, values.iter().copied()))
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
pub fn deny(code: Option<&str>) -> EvaluationResponse {
    EvaluationResponse {
        decision: false,
        context: EvaluationResponseContext {
            constraints: vec![],
            deny_reason: code.map(|c| DenyReason {
                error_code: c.to_owned(),
                details: None,
            }),
        },
    }
}
pub const USER: &str = toolkit_gts::gts_id!("cf.core.security.subject_user.v1~");
pub const SERVICE: &str = toolkit_gts::gts_id!("cf.core.security.subject_service.v1~");
pub fn ctx(subject: u128, tenant: u128, kind: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(u(subject))
        .subject_tenant_id(u(tenant))
        .subject_type(kind)
        .build()
        .unwrap()
}
pub fn user(subject: u128, tenant: u128) -> Caller {
    Caller::new(ctx(subject, tenant, USER), None)
}
pub fn service(subject: u128, tenant: u128) -> Caller {
    Caller::new(ctx(subject, tenant, SERVICE), None)
}
pub fn with_proof(caller: &Caller, proof: &str) -> Caller {
    Caller::new(
        caller.ctx().clone(),
        Some(proof.to_owned().try_into().unwrap()),
    )
}
pub fn property<'r>(request: &'r EvaluationRequest, name: &str) -> Option<&'r str> {
    request
        .resource
        .properties
        .get(name)
        .and_then(serde_json::Value::as_str)
}
