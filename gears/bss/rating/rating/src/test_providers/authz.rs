//! `FakePdp`: an `AuthZResolverApi` double. By default it grants every subject its own tenant
//! (`owner_tenant_id IN [subject tenant]`, the shape `TenantMode::RootOnly` asks for); a test can
//! deny an action, put a tenant outside every grant, make the PDP unavailable, or make an allow
//! carry no constraint or one the PEP cannot compile.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    DenyReason, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};
use uuid::Uuid;

/// What an allowed decision carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AllowShape {
    /// `owner_tenant_id IN [subject tenant]`.
    #[default]
    OwnTenant,
    /// No constraint at all: an unconstrained allow.
    NoConstraints,
    /// A constraint on a property the PEP does not know, which does not compile.
    UnknownProperty,
}

/// The subject one evaluation was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedSubject {
    /// The subject id.
    pub id: Uuid,
    /// The subject type.
    pub subject_type: Option<String>,
    /// The subject's home tenant.
    pub tenant_id: Option<Uuid>,
}

/// A programmable PDP.
#[derive(Default)]
pub struct FakePdp {
    denied_actions: Mutex<HashSet<(String, String)>>,
    outside_tenants: Mutex<HashSet<Uuid>>,
    unavailable: AtomicBool,
    allow_shape: Mutex<AllowShape>,
    subjects: Mutex<Vec<EvaluatedSubject>>,
    evaluations: AtomicUsize,
}

impl FakePdp {
    /// A PDP that grants every subject its own tenant.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A `PolicyEnforcer` over this PDP.
    #[must_use]
    pub fn enforcer(self: &Arc<Self>) -> PolicyEnforcer {
        PolicyEnforcer::new(Arc::clone(self) as Arc<dyn AuthZResolverApi>)
    }

    /// Denies `action` on `resource_type` to everyone.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    pub fn deny(&self, resource_type: &str, action: &str) {
        self.denied_actions
            .lock()
            .expect("fake PDP lock")
            .insert((resource_type.to_owned(), action.to_owned()));
    }

    /// Denies every request of a subject whose home tenant is `tenant`.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    pub fn put_outside_grant(&self, tenant: Uuid) {
        self.outside_tenants
            .lock()
            .expect("fake PDP lock")
            .insert(tenant);
    }

    /// Makes every evaluation fail as an unreachable PDP does.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    /// Makes every allowed decision carry `shape`.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    pub fn set_allow_shape(&self, shape: AllowShape) {
        *self.allow_shape.lock().expect("fake PDP lock") = shape;
    }

    /// The evaluations answered so far.
    #[must_use]
    pub fn evaluations(&self) -> usize {
        self.evaluations.load(Ordering::SeqCst)
    }

    /// The subjects evaluated so far, in order.
    ///
    /// # Panics
    ///
    /// If the lock is poisoned.
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    #[must_use]
    pub fn subjects(&self) -> Vec<EvaluatedSubject> {
        self.subjects.lock().expect("fake PDP lock").clone()
    }
}

#[async_trait]
impl AuthZResolverApi for FakePdp {
    #[expect(
        clippy::expect_used,
        reason = "a test double: a poisoned lock means a test already panicked"
    )]
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.evaluations.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(CanonicalError::service_unavailable().create());
        }
        let tenant = req
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        self.subjects
            .lock()
            .expect("fake PDP lock")
            .push(EvaluatedSubject {
                id: req.subject.id,
                subject_type: req.subject.subject_type.clone(),
                tenant_id: tenant,
            });
        let shape = *self.allow_shape.lock().expect("fake PDP lock");
        let denied = self
            .denied_actions
            .lock()
            .expect("fake PDP lock")
            .contains(&(req.resource.resource_type, req.action.name));
        let outside = tenant.is_none_or(|t| {
            self.outside_tenants
                .lock()
                .expect("fake PDP lock")
                .contains(&t)
        });
        match tenant {
            Some(tenant) if !denied && !outside => Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: match shape {
                        AllowShape::OwnTenant => vec![Constraint {
                            predicates: vec![Predicate::In(InPredicate::new(
                                pep_properties::OWNER_TENANT_ID,
                                vec![tenant],
                            ))],
                        }],
                        AllowShape::NoConstraints => Vec::new(),
                        AllowShape::UnknownProperty => vec![Constraint {
                            predicates: vec![Predicate::In(InPredicate::new(
                                "no_such_property",
                                vec![tenant],
                            ))],
                        }],
                    },
                    deny_reason: None,
                },
            }),
            _ => Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: Some(DenyReason {
                        error_code: "fake_pdp_denied".to_owned(),
                        details: None,
                    }),
                },
            }),
        }
    }
}
