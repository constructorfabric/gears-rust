//! Test PDPs: a scripted recording double (proves Orders asks the right question and enforces
//! the answer) and a gear-local policy fake that answers from a small list of grants (D-227).
//! Neither is runtime code: the module is compiled only under `cfg(test)`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not \
              `expect`, because not every test in the file unwraps or expects"
)]
use super::{
    Action, Caller, DELEGATION_PROOF_REF, DENY_DELEGATION_PROOF_INVALID,
    DENY_DELEGATION_PROOF_REQUIRED, Pep,
};
use crate::domain::audit::{ActorIdentities, Principal, ServiceRole};
use authz_resolver_sdk::{
    AuthZResolverApi, Constraint, DenyReason, EqPredicate, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, InPredicate, PolicyEnforcer, Predicate,
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

/// One grant of [`FakePdp`]: `subject` may take `actions` on whatever one of `paths` admits.
/// All predicates of a path must hold, and any one path suffices; each predicate is
/// `property IN values`.
pub struct Grant {
    subject: Uuid,
    actions: Vec<Action>,
    paths: Vec<Vec<(String, Vec<Uuid>)>>,
    delegated: bool,
}
impl Grant {
    pub fn new(subject: Uuid, actions: &[Action]) -> Self {
        Self {
            subject,
            actions: actions.to_vec(),
            paths: Vec::new(),
            delegated: false,
        }
    }
    /// Adds one OR path of `property IN values` predicates.
    pub fn path<'p>(mut self, predicates: impl IntoIterator<Item = (&'p str, Vec<Uuid>)>) -> Self {
        self.paths.push(
            predicates
                .into_iter()
                .map(|(property, values)| (property.to_owned(), values))
                .collect(),
        );
        self
    }
    /// The grant applies only when the request carries a delegation proof the PDP accepts.
    pub fn delegated(mut self) -> Self {
        self.delegated = true;
        self
    }
    fn covers(&self, request: &EvaluationRequest) -> bool {
        self.subject == request.subject.id
            && self.actions.iter().any(|action| {
                action.permission().is_ok_and(|p| {
                    p.resource == request.resource.resource_type && p.action == request.action.name
                })
            })
    }
    fn constraints(&self) -> impl Iterator<Item = Constraint> + '_ {
        self.paths.iter().map(|predicates| Constraint {
            predicates: predicates
                .iter()
                .map(|(property, values)| {
                    Predicate::In(InPredicate::new(property.as_str(), values.iter().copied()))
                })
                .collect(),
        })
    }
}

/// A gear-local fake PDP for the in-crate tests (D-227), after Rating's `FakePdp` (PR #5390).
///
/// It answers the questions the Orders PEP asks, deny by default: the grants that name the
/// subject and the exact registered resource/action pair contribute their paths as constraints,
/// and the answer is an allow with all of them (the PEP and the scoped queries enforce them).
/// A delegated grant contributes only when the request's `delegation_proof_ref` is one of the
/// accepted proofs. With nothing contributed the answer is a denial, coded
/// `delegation_proof_required` / `delegation_proof_invalid` when a delegated grant would have
/// applied with a (valid) proof. It never evaluates subject tenant, token scopes or tenant
/// context, and it makes none of the PEP's own checks (finite order IDs, break-glass class).
pub struct FakePdp {
    grants: Vec<Grant>,
    accepted_proofs: Vec<String>,
}
impl FakePdp {
    pub fn new(grants: Vec<Grant>, accepted_proofs: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            grants,
            accepted_proofs: accepted_proofs.iter().map(|p| (*p).to_owned()).collect(),
        })
    }
    fn decide(&self, request: &EvaluationRequest) -> EvaluationResponse {
        let proof = property(request, DELEGATION_PROOF_REF);
        let proof_accepted = proof.is_some_and(|p| self.accepted_proofs.iter().any(|a| a == p));
        let mut constraints = Vec::new();
        let mut needs_proof = false;
        for grant in self.grants.iter().filter(|g| g.covers(request)) {
            if grant.delegated && !proof_accepted {
                needs_proof = true;
            } else {
                constraints.extend(grant.constraints());
            }
        }
        if !constraints.is_empty() {
            allow(constraints)
        } else if needs_proof && proof.is_some() {
            deny(Some(DENY_DELEGATION_PROOF_INVALID))
        } else if needs_proof {
            deny(Some(DENY_DELEGATION_PROOF_REQUIRED))
        } else {
            deny(None)
        }
    }
}
#[async_trait::async_trait]
impl AuthZResolverApi for FakePdp {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(self.decide(&request))
    }
}

/// A PEP over the default test identities: none of the subjects the authz tests use is
/// configured, so their class follows the subject type alone (D-226).
pub fn pep(api: Arc<dyn AuthZResolverApi>) -> Pep {
    pep_with(api, default_identities())
}
/// A PEP over the given identities (the engine tests pass the engine's own).
pub fn pep_with(api: Arc<dyn AuthZResolverApi>, identities: ActorIdentities) -> Pep {
    Pep::new(Arc::new(PolicyEnforcer::new(api)), identities)
}
pub fn default_identities() -> ActorIdentities {
    ActorIdentities::new(
        Some(Principal {
            subject_id: u(900),
            subject_tenant_id: u(901),
        }),
        [(
            ServiceRole::Workflow,
            Principal {
                subject_id: u(902),
                subject_tenant_id: u(901),
            },
        )],
    )
    .expect("valid test identities")
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
