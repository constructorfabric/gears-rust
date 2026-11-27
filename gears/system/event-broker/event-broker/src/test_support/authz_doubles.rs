//! `AllowAllAuthZ`: a permissive-by-default `AuthZResolverApi` double for
//! `EventBrokerHarness`. It allows every request, so a test that doesn't
//! exercise authorization needs no setup; a test of a denial builds a
//! narrower double instead (`DenyingAuthZ`, `PolicyAuthZ`).
//! Tenant scope is enforced via the same `AuthZResolverApi`/
//! `PolicyEnforcer` seam (`domain/authz.rs::tenant_authorized` on the
//! delivery side, the tenant-scope check in `domain/ingest.rs` on publish),
//! not a separate `tenant-resolver-sdk` double - there is no second client
//! type to fake here.
//!
//! Every double that allows answers the same constraint, through
//! `owner_tenant_constraints`, so a caller that requires constraints sees
//! what tr-authz would answer; `PolicyAuthZ`'s consume scope adds its
//! event-type predicate to it.

use std::collections::{BTreeSet, HashMap, HashSet};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, BarrierMode, Constraint, EvaluationRequest, EvaluationResponse,
    EvaluationResponseContext, InPredicate, Predicate,
};
use event_broker_sdk::gts::{CONSUMER_GROUP_RESOURCE_TYPE, REQUEST_RESOURCE_TYPE};
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::streaming::scope::{EVENT_TYPE, EVENT_TYPES};

/// Grants every `evaluate` call - the stand-in for "no PEP configured".
///
/// An allow carries the constraint `owner_tenant_constraints` answers: one
/// `In(owner_tenant_id, [root_id])` when the request has a `root_id` and
/// declares `owner_tenant_id` as a supported property, as tr-authz does, and
/// none otherwise. That lets a caller that sets `require_constraints(true)`
/// compile an answer instead of failing with `ConstraintsRequiredButAbsent`.
///
/// Callers that decide on `EvaluationResponse::decision`/`context.deny_reason`
/// alone are unaffected: every `PolicyEnforcer::access_scope_with` call in the
/// gear sets `require_constraints(false)` and drops the compiled scope, so the
/// constraint is compiled and ignored.
pub struct AllowAllAuthZ;

#[async_trait]
impl AuthZResolverApi for AllowAllAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: owner_tenant_constraints(&request),
                ..Default::default()
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// Denies `evaluate` whenever `deny_if` returns `true` for the request being
/// checked, otherwise allows - lets a single test deny exactly the specific
/// `event_type:produce`/`topic:consume`/`event_type:consume`/tenant-scope
/// check it cares about (inspect `request.action.name` and
/// `request.resource.properties`) while every other check on the same
/// harness keeps its default allow.
///
/// A deny carries no constraints. An allow answers like [`AllowAllAuthZ`].
pub struct DenyingAuthZ<F> {
    pub deny_if: F,
}

#[async_trait]
impl<F> AuthZResolverApi for DenyingAuthZ<F>
where
    F: Fn(&EvaluationRequest) -> bool + Send + Sync,
{
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        if (self.deny_if)(&request) {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: owner_tenant_constraints(&request),
                ..Default::default()
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// The constraints every allowing double answers: one
/// `In(owner_tenant_id, [root_id])` when the request has a `root_id` in its
/// tenant context and declares `owner_tenant_id` as a supported property, as
/// tr-authz does, and none otherwise. A request that declares no owner
/// property cannot compile an `owner_tenant_id` predicate, so it gets none.
fn owner_tenant_constraints(request: &EvaluationRequest) -> Vec<Constraint> {
    let owner_declared = request
        .context
        .supported_properties
        .iter()
        .any(|property| property == pep_properties::OWNER_TENANT_ID);
    let root_id = request
        .context
        .tenant_context
        .as_ref()
        .and_then(|tenant_context| tenant_context.root_id);
    match root_id {
        Some(root_id) if owner_declared => vec![Constraint {
            predicates: vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [root_id],
            ))],
        }],
        _ => Vec::new(),
    }
}

/// One tenant of a [`PolicyAuthZ`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tenant {
    id: Uuid,
    parent: Option<Uuid>,
    self_managed: bool,
}

impl Tenant {
    /// The root of the tree: no parent.
    #[must_use]
    pub fn root(id: Uuid) -> Self {
        Self {
            id,
            parent: None,
            self_managed: false,
        }
    }

    /// A tenant under `parent` that its ancestors reach through barriers.
    #[must_use]
    pub fn child(id: Uuid, parent: Uuid) -> Self {
        Self {
            id,
            parent: Some(parent),
            self_managed: false,
        }
    }

    /// A tenant under `parent` that is a barrier: an ancestor reaches it, or
    /// anything below it, only with barriers ignored.
    #[must_use]
    pub fn self_managed_child(id: Uuid, parent: Uuid) -> Self {
        Self {
            id,
            parent: Some(parent),
            self_managed: true,
        }
    }
}

/// One right of a [`PolicyAuthZ`]: `subject` may perform `action` on
/// `resource_type`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Grant {
    subject: Uuid,
    resource_type: String,
    action: String,
}

impl Grant {
    #[must_use]
    pub fn new(subject: Uuid, resource_type: &str, action: &str) -> Self {
        Self {
            subject,
            resource_type: resource_type.to_owned(),
            action: action.to_owned(),
        }
    }
}

/// A tenant tree [`PolicyAuthZ`] refuses to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyAuthZError {
    /// Two tenants share an id.
    #[error("tenant {0} appears twice in the tree")]
    DuplicateTenant(Uuid),
    /// A tenant names a parent that is not in the tree.
    #[error("tenant {tenant} names parent {parent}, which is not in the tree")]
    UnknownParent { tenant: Uuid, parent: Uuid },
}

/// An action-aware `AuthZResolverApi` double for the publish rules of
/// `docs/DESIGN.md` §3.3, the consume rules of JOIN and stream open, and the
/// consumer-group rules (create, the group check, management). It
/// decides on the action, the resource type and the tenant tree, where
/// [`DenyingAuthZ`] only sees the request.
///
/// The rules, per request. The caller's tenant is `subject.properties.tenant_id`.
/// `reaches(anchor, candidate, barrier)` means the candidate is in the closed
/// subtree rooted at the anchor: reflexive, like tr-authz's `is_in_subtree`.
/// Otherwise it walks the candidate's parents up to the anchor. With
/// `Respect` it is false when the candidate is self-managed or a
/// self-managed tenant lies strictly between the two (the semantics of
/// `TenantResolverClient::is_ancestor`); with `Ignore` barriers are skipped.
/// A tenant that is not in the tree is reached by nobody.
///
/// Publish. The target is `context.tenant_context.root_id`, and a request
/// without one is denied.
/// - The tenant check (`produce` on the request resource type) allows when
///   the caller's tenant reaches the target under the request's barrier mode.
/// - The barrier right (`produce_ignore_barrier`) allows when the subject
///   holds that grant on the resource type **and** the caller's tenant
///   reaches the target with barriers ignored: the grant does not widen
///   reach, and reach does not stand in for the grant. tr-authz still
///   requires reach for the barrier right, even ignoring barriers.
/// - The type check (`produce` on any other resource type) allows when the
///   subject holds the `produce` grant on the resource type. It never applies
///   tenant reach to `owner_tenant_id`: the type check decides the type grant
///   only.
///
/// Consume. A topic's resource type is its id, so a grant on a topic names
/// the topic id.
/// - The tenant check (`consume` on the request resource type) allows when
///   the caller's tenant reaches the target. The target is `root_id` under
///   the request's barrier mode when the request sends one, as the check that
///   classifies a refused consume scope does; otherwise it is the request's
///   `owner_tenant_id` property under `Respect`, as a gate with no tenant
///   context sends it. It is the one rule that needs no tenant context.
/// - The barrier right (`consume_ignore_barrier`) allows when the subject
///   holds that grant on the topic **and** the caller's tenant reaches
///   `root_id` with barriers ignored. The grant decides: a root with no
///   self-managed tenant below it does not stand in for it.
/// - The consume scope (`consume` on any other resource type) allows when
///   the subject holds the `consume` grant on the topic **and** the caller's
///   tenant reaches `root_id` under the request's barrier mode. With an
///   event-type rule for the subject and topic ([`Self::with_event_types`]),
///   the allow adds `In(event_type, ..)` listing the rule's types that are
///   among the request's `event_types` candidates; when the request sends no
///   candidates it adds no such predicate, and when none of the rule's types
///   is a candidate it denies rather than answer an empty `in`.
///
/// Consumer groups. The anonymous group family's resource type is
/// `gts.cf.core.events.consumer_group.v1~`, and a named group's is its own id,
/// which starts with it.
/// - The group check (`consume`), the management check (`manage`) and group
///   creation (`define`) allow when the subject holds that grant on the
///   resource type **and** the caller's tenant reaches `root_id` under the
///   request's barrier mode. A request without a root is denied.
///
/// Every other request is denied. Subject-type and `owner_id` rules are not
/// modelled.
///
/// An allow answers the constraints of `owner_tenant_constraints`, like
/// [`AllowAllAuthZ`], with the consume scope's event-type predicate added to
/// the same constraint, or as its own constraint when there is none. A deny
/// carries none.
#[derive(Debug, Clone)]
pub struct PolicyAuthZ {
    tenants: HashMap<Uuid, Tenant>,
    grants: HashSet<Grant>,
    /// Per (subject, topic): the only event types the subject may consume.
    event_types: HashMap<(Uuid, String), BTreeSet<String>>,
}

impl PolicyAuthZ {
    /// A policy over `tenants` that holds no grants yet.
    ///
    /// # Errors
    ///
    /// [`PolicyAuthZError::DuplicateTenant`] when two tenants share an id;
    /// [`PolicyAuthZError::UnknownParent`] when a tenant's parent is not in
    /// the tree.
    pub fn new(tenants: impl IntoIterator<Item = Tenant>) -> Result<Self, PolicyAuthZError> {
        let mut by_id = HashMap::new();
        for tenant in tenants {
            if by_id.insert(tenant.id, tenant).is_some() {
                return Err(PolicyAuthZError::DuplicateTenant(tenant.id));
            }
        }
        for tenant in by_id.values() {
            if let Some(parent) = tenant.parent
                && !by_id.contains_key(&parent)
            {
                return Err(PolicyAuthZError::UnknownParent {
                    tenant: tenant.id,
                    parent,
                });
            }
        }
        Ok(Self {
            tenants: by_id,
            grants: HashSet::new(),
            event_types: HashMap::new(),
        })
    }

    /// This policy with `grant` added.
    #[must_use]
    pub fn with_grant(mut self, grant: Grant) -> Self {
        self.grants.insert(grant);
        self
    }

    /// This policy with an event-type rule added: on `topic`, `subject`
    /// consumes only the event types in `types`.
    #[must_use]
    pub fn with_event_types<'a>(
        mut self,
        subject: Uuid,
        topic: &str,
        types: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        self.event_types
            .entry((subject, topic.to_owned()))
            .or_default()
            .extend(types.into_iter().map(str::to_owned));
        self
    }

    fn holds(&self, subject: Uuid, resource_type: &str, action: &str) -> bool {
        self.grants
            .contains(&Grant::new(subject, resource_type, action))
    }

    /// Whether `candidate` is in the closed subtree rooted at `anchor`.
    fn reaches(&self, anchor: Uuid, candidate: Uuid, barrier: BarrierMode) -> bool {
        if anchor == candidate {
            return true;
        }
        let mut current = candidate;
        // The walk climbs one parent per step, so a tree of `n` tenants ends
        // within `n` steps; the bound only keeps a cyclic tree from looping.
        for _ in 0..self.tenants.len() {
            let Some(tenant) = self.tenants.get(&current) else {
                return false;
            };
            if barrier == BarrierMode::Respect && tenant.self_managed {
                return false;
            }
            let Some(parent) = tenant.parent else {
                return false;
            };
            if parent == anchor {
                return true;
            }
            current = parent;
        }
        false
    }

    /// The tenant context's `root_id` and barrier mode; `None` when the
    /// request sends no root.
    fn root(request: &EvaluationRequest) -> Option<(Uuid, BarrierMode)> {
        let tenant_context = request.context.tenant_context.as_ref()?;
        Some((tenant_context.root_id?, tenant_context.barrier_mode))
    }

    /// The constraints an allow answers, or `None` for a deny.
    fn decide(&self, request: &EvaluationRequest) -> Option<Vec<Constraint>> {
        let caller_tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())?;
        let subject = request.subject.id;
        let resource_type = request.resource.resource_type.as_str();
        let allowed = match (request.action.name.as_str(), resource_type) {
            ("produce", REQUEST_RESOURCE_TYPE) => {
                let (target, barrier) = Self::root(request)?;
                self.reaches(caller_tenant, target, barrier)
            }
            ("produce_ignore_barrier", _) => {
                let (target, _) = Self::root(request)?;
                self.holds(subject, resource_type, "produce_ignore_barrier")
                    && self.reaches(caller_tenant, target, BarrierMode::Ignore)
            }
            ("produce", _) => {
                Self::root(request)?;
                self.holds(subject, resource_type, "produce")
            }
            ("consume", REQUEST_RESOURCE_TYPE) => {
                let (target, barrier) = match Self::root(request) {
                    Some(root) => root,
                    None => (
                        request
                            .resource
                            .properties
                            .get(pep_properties::OWNER_TENANT_ID)
                            .and_then(Value::as_str)
                            .and_then(|value| Uuid::parse_str(value).ok())?,
                        BarrierMode::Respect,
                    ),
                };
                self.reaches(caller_tenant, target, barrier)
            }
            ("consume_ignore_barrier", _) => {
                let (target, _) = Self::root(request)?;
                self.holds(subject, resource_type, "consume_ignore_barrier")
                    && self.reaches(caller_tenant, target, BarrierMode::Ignore)
            }
            (action @ ("consume" | "manage" | "define"), _)
                if resource_type.starts_with(CONSUMER_GROUP_RESOURCE_TYPE) =>
            {
                let (target, barrier) = Self::root(request)?;
                self.holds(subject, resource_type, action)
                    && self.reaches(caller_tenant, target, barrier)
            }
            ("consume", _) => {
                let (target, barrier) = Self::root(request)?;
                if !(self.holds(subject, resource_type, "consume")
                    && self.reaches(caller_tenant, target, barrier))
                {
                    return None;
                }
                return self.consume_scope(subject, request);
            }
            _ => false,
        };
        allowed.then(|| owner_tenant_constraints(request))
    }

    /// The constraints an allowed consume scope answers: the owner tenant
    /// constraint, with the subject's event-type rule for the topic, if any,
    /// narrowed to the request's candidates. `None` (a deny) when the rule
    /// and the candidates do not meet, or the candidates are not a list of
    /// type ids.
    fn consume_scope(&self, subject: Uuid, request: &EvaluationRequest) -> Option<Vec<Constraint>> {
        let mut constraints = owner_tenant_constraints(request);
        let Some(rule) = self
            .event_types
            .get(&(subject, request.resource.resource_type.clone()))
        else {
            return Some(constraints);
        };
        let Some(candidates) = request.resource.properties.get(EVENT_TYPES) else {
            return Some(constraints);
        };
        let candidates: Vec<&str> = candidates
            .as_array()?
            .iter()
            .map(Value::as_str)
            .collect::<Option<_>>()?;
        let types: Vec<&str> = rule
            .iter()
            .map(String::as_str)
            .filter(|event_type| candidates.contains(event_type))
            .collect();
        if types.is_empty() {
            return None;
        }
        let predicate = Predicate::In(InPredicate::new(EVENT_TYPE, types));
        match constraints.first_mut() {
            Some(constraint) => constraint.predicates.push(predicate),
            None => constraints.push(Constraint {
                predicates: vec![predicate],
            }),
        }
        Some(constraints)
    }
}

#[async_trait]
impl AuthZResolverApi for PolicyAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let Some(constraints) = self.decide(&request) else {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            });
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

#[cfg(test)]
#[path = "authz_doubles_tests.rs"]
mod tests;
