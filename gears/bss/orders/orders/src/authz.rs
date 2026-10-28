//! Shared Orders policy enforcement point (08 §3.5, §3.6, §4.3, §4.4).
//!
//! One adapter around the platform `PolicyEnforcer` serves REST, the in-process SDK, the engine
//! pre-guard and the read wrapper. It asks the exact registered resource/action, forwards the
//! trusted caller context and the supplied (unvalidated) delegation proof reference, requires
//! constraints, and returns PDP-compiled scopes wrapped in types only this module constructs.
//! Orders never turns an actor class into a grant, never evaluates proof and never falls back to
//! a local evaluator or unrestricted scope. Business guards (immutable axes, acceptance party,
//! admissibility) remain separate domain rules evaluated after authorization.
use crate::domain::audit::{ActorIdentities, AuditActorClass};
use crate::domain::read::ProofDenial;
use crate::gts::permissions::{self, Permission, properties};
use authz_resolver_sdk::models::DenyReason;
use authz_resolver_sdk::pep::{AccessRequest, ConstraintCompileError, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::catalog::{OPERATIONS, Operation, Reason};
use bss_orders_sdk::models::DelegationProofRef;
use std::sync::Arc;
use toolkit_security::SecurityContext;
use toolkit_security::access_scope::{AccessScope, ScopeFilter, ScopeValue};
use uuid::Uuid;

/// Reserved PDP request-context carrier for the supplied delegation proof reference.
///
/// **Interim, pending platform agreement** (`UPSTREAM_REQS` §2.9 items 1–3): the resolver SDK has
/// no caller-evidence field, so the reference travels as a resource property that no Orders
/// resource type advertises as supported. It is therefore never compiled into a scope.
pub const DELEGATION_PROOF_REF: &str = "delegation_proof_ref";
/// PDP deny code for a path that needs delegation when no proof was presented. **Interim.**
pub const DENY_DELEGATION_PROOF_REQUIRED: &str = "delegation_proof_required";
/// PDP deny code for presented proof that is invalid, expired, revoked or wrong-scope. **Interim.**
pub const DENY_DELEGATION_PROOF_INVALID: &str = "delegation_proof_invalid";

const SUBJECT_USER: &str = toolkit_gts::gts_id!("cf.core.security.subject_user.v1~");

/// Closed census of grantable Orders actions (08 §4.3 resource–action catalog).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    OrderCreate,
    OrderWrite,
    OrderSubmit,
    OrderAmend,
    OrderEdit,
    OrderPreview,
    OrderCancel,
    OrderHold,
    OrderResume,
    OrderRead,
    OrderApprovalReflection,
    OrderBeginFulfillment,
    OrderSpawnSignal,
    OrderFulfillmentAcknowledgement,
    OrderWorkflowCancel,
    OrderForceFailUnreconciled,
    AcceptanceRecord,
    AuditRead,
    AuditUnresolvedRead,
}

impl Action {
    pub const ALL: [Self; 19] = [
        Self::OrderCreate,
        Self::OrderWrite,
        Self::OrderSubmit,
        Self::OrderAmend,
        Self::OrderEdit,
        Self::OrderPreview,
        Self::OrderCancel,
        Self::OrderHold,
        Self::OrderResume,
        Self::OrderRead,
        Self::OrderApprovalReflection,
        Self::OrderBeginFulfillment,
        Self::OrderSpawnSignal,
        Self::OrderFulfillmentAcknowledgement,
        Self::OrderWorkflowCancel,
        Self::OrderForceFailUnreconciled,
        Self::AcceptanceRecord,
        Self::AuditRead,
        Self::AuditUnresolvedRead,
    ];

    /// Orders-local logical `(resource, action)` names from 08 §4.3.
    #[must_use]
    pub const fn logical(self) -> (&'static str, &'static str) {
        match self {
            Self::OrderCreate => ("order", "create"),
            Self::OrderWrite => ("order", "write"),
            Self::OrderSubmit => ("order", "submit"),
            Self::OrderAmend => ("order", "amend"),
            Self::OrderEdit => ("order", "edit"),
            Self::OrderPreview => ("order", "preview"),
            Self::OrderCancel => ("order", "cancel"),
            Self::OrderHold => ("order", "hold"),
            Self::OrderResume => ("order", "resume"),
            Self::OrderRead => ("order", "read"),
            Self::OrderApprovalReflection => ("order", "approval-reflection"),
            Self::OrderBeginFulfillment => ("order", "begin-fulfillment"),
            Self::OrderSpawnSignal => ("order", "spawn-signal"),
            Self::OrderFulfillmentAcknowledgement => ("order", "fulfillment-acknowledgement"),
            Self::OrderWorkflowCancel => ("order", "workflow-cancel"),
            Self::OrderForceFailUnreconciled => ("order", "force-fail-unreconciled"),
            Self::AcceptanceRecord => ("acceptance", "record"),
            Self::AuditRead => ("audit", "read"),
            Self::AuditUnresolvedRead => ("audit-unresolved", "read"),
        }
    }

    fn from_logical(resource: &str, action: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.logical() == (resource, action))
    }

    /// The registered permission; startup validation proves every action has one.
    ///
    /// # Errors
    /// Returns an integration failure for an unregistered pair (impossible after startup).
    pub fn permission(self) -> Result<&'static Permission, AuthzFailure> {
        let (resource, action) = self.logical();
        permissions::permission(resource, action).ok_or(AuthzFailure::Integration)
    }

    /// The registered resource descriptor shared by catalog and enforcement.
    ///
    /// # Errors
    /// Returns an integration failure for an undeclared resource.
    pub fn resource_type(self) -> Result<&'static ResourceType, AuthzFailure> {
        permissions::resource_type(self.logical().0).ok_or(AuthzFailure::Integration)
    }

    /// Service-only Workflow seam grants (08 §4.3), including the internal rebuild entry.
    #[must_use]
    pub const fn is_workflow_seam(self) -> bool {
        matches!(
            self,
            Self::OrderApprovalReflection
                | Self::OrderBeginFulfillment
                | Self::OrderSpawnSignal
                | Self::OrderFulfillmentAcknowledgement
                | Self::OrderWorkflowCancel
        )
    }
}

/// The internal D-201 `replace-fulfillment-grant` continuation is authorized as
/// `order × spawn-signal` on all axes; it adds no public route or action.
pub const REPLACE_FULFILLMENT_GRANT_ACTION: Action = Action::OrderSpawnSignal;

/// PATCH trigger selected only from the request's field classes, before any state read (02 §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchClass {
    /// Any commercial field selects `draft-mutate` → `order × write`.
    Commercial,
    /// Administrative fields only select `administrative-edit` → `order × edit`.
    Administrative,
}

/// Census failure: an operation without one exact declaration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CensusError {
    #[error("Orders operation has no permission declaration")]
    Undeclared,
    #[error("PATCH requires a field-class trigger")]
    MissingPatchClass,
    #[error("only PATCH operations take a field-class trigger")]
    UnexpectedPatchClass,
}

/// The catalog's action encoding for the PATCH operations, whose action the request's field
/// classes select (`order × write` or `order × edit`, 02 §4.3). Only this module parses it.
pub const FIELD_CLASS_ACTION: &str = "field_class:write|edit";

/// The field-class arguments a catalog operation takes: both PATCH classes for the field-class
/// encoding, `None` alone for every other operation.
#[must_use]
pub fn patch_classes(op: &Operation) -> &'static [Option<PatchClass>] {
    if op.action == FIELD_CLASS_ACTION {
        &[
            Some(PatchClass::Commercial),
            Some(PatchClass::Administrative),
        ]
    } else {
        &[None]
    }
}

/// Every action a catalog operation can resolve to, one per [`patch_classes`] entry; each item
/// is a census failure for an undeclared operation.
pub fn logical_actions(op: &Operation) -> impl Iterator<Item = Result<Action, CensusError>> + '_ {
    patch_classes(op)
        .iter()
        .map(move |class| operation_action(op, *class))
}

/// Map one catalog operation to its exact action.
///
/// # Errors
/// Returns a census failure for an undeclared operation or a PATCH without its field class.
pub fn operation_action(op: &Operation, patch: Option<PatchClass>) -> Result<Action, CensusError> {
    if op.action == FIELD_CLASS_ACTION {
        return match patch {
            Some(PatchClass::Commercial) => Ok(Action::OrderWrite),
            Some(PatchClass::Administrative) => Ok(Action::OrderEdit),
            None => Err(CensusError::MissingPatchClass),
        };
    }
    if patch.is_some() {
        return Err(CensusError::UnexpectedPatchClass);
    }
    Action::from_logical(op.resource, op.action).ok_or(CensusError::Undeclared)
}

/// Whether the operation names a target order (list, create and preview do not).
#[must_use]
pub fn is_targeted(op: &Operation) -> bool {
    crate::api::rest::wire::route(op.id).is_some_and(|route| route.path.contains("{orderId}"))
}

/// Startup census: every catalog operation and the internal continuation map to a registered
/// permission and resource descriptor; every action except operational unresolved-audit read
/// is reached by some public operation.
///
/// # Errors
/// Returns an error so initialization fails on an undeclared operation or drifted catalog.
pub fn validate_census() -> anyhow::Result<()> {
    let mut reached = std::collections::BTreeSet::new();
    for op in OPERATIONS {
        for action in logical_actions(op) {
            let action = action.map_err(|e| anyhow::anyhow!("bss-orders: {} {e}", op.id))?;
            let (resource, logical) = action.logical();
            action.permission().map_err(|_| {
                anyhow::anyhow!(
                    "bss-orders: {} has no registered {resource}.{logical} permission",
                    op.id
                )
            })?;
            action.resource_type().map_err(|_| {
                anyhow::anyhow!("bss-orders: {} uses undeclared resource {resource}", op.id)
            })?;
            reached.insert(action);
        }
    }
    reached.insert(REPLACE_FULFILLMENT_GRANT_ACTION);
    reached.insert(Action::AuditUnresolvedRead);
    anyhow::ensure!(
        reached.len() == Action::ALL.len() && permissions::PERMISSIONS.len() == Action::ALL.len(),
        "bss-orders: permission census drift"
    );
    Ok(())
}

/// The D-202 credential check every supplied proof reference passes before any PDP, registry or
/// store work, whichever adapter (REST header or SDK call metadata) carried it.
///
/// # Errors
/// `request-invalid` for a credential-bearing proof reference.
pub fn check_proof_reference(proof: &DelegationProofRef) -> Result<(), OrdersError> {
    crate::domain::audit::validate_proof_reference(proof)
        .map(|_| ())
        .map_err(|_| OrdersError::Refused(Reason::RequestInvalid))
}

/// The caller's actor class for every authorization decision: the PEP's user-only and
/// finite-ID rules and the engine's declared row actor rule (D-226, PR #5259 #188).
///
/// The configured identities decide (D-115): the configured worker actor is `System`, a
/// configured service principal is `Service`. A subject they leave as `User` stays `User` only
/// when its authenticated subject type is the platform user type; any other type, including an
/// absent one, is `Service`, so an unconfigured service token keeps the stricter service rules.
/// The subject type can only tighten the class, never loosen it. The audit row still records
/// [`ActorIdentities::classify`]'s class.
///
/// A user-typed subject configured as a service or system principal is a deployment
/// misconfiguration: it is decided by its configured class and logged with the class only,
/// never the principal (D-182).
#[must_use]
pub fn decision_class(identities: &ActorIdentities, ctx: &SecurityContext) -> AuditActorClass {
    let user_typed = ctx.subject_type() == Some(SUBJECT_USER);
    match identities.configured_class(ctx) {
        AuditActorClass::User if !user_typed => AuditActorClass::Service,
        AuditActorClass::User => AuditActorClass::User,
        class => {
            if user_typed {
                tracing::warn!(
                    target: "orders.authz",
                    actor_class = class.token(),
                    "bss-orders: a user-typed subject is configured as a non-user principal; \
                     the configured class decides"
                );
            }
            class
        }
    }
}

/// The trusted caller: authenticated `SecurityContext` plus the supplied proof reference.
///
/// Built only from the authenticated context the gateway/SDK caller supplies; never from body
/// actor fields or an order's stored seller.
#[derive(Clone)]
pub struct Caller {
    ctx: SecurityContext,
    proof: Option<DelegationProofRef>,
}
impl Caller {
    #[must_use]
    pub fn new(ctx: SecurityContext, proof: Option<DelegationProofRef>) -> Self {
        Self { ctx, proof }
    }
    /// The caller of an in-process SDK call: the typed call metadata's proof reference (`CallMeta`
    /// or `CreateMeta`) passes the same D-202 credential check as the REST header before any PDP,
    /// registry or store work. Shared by the REST handlers and the local SDK client, so neither
    /// adapter depends on the other.
    ///
    /// # Errors
    /// `request-invalid` for a credential-bearing proof reference (D-202).
    pub fn from_meta(
        ctx: &SecurityContext,
        proof: Option<&DelegationProofRef>,
    ) -> Result<Self, OrdersError> {
        if let Some(proof) = proof {
            check_proof_reference(proof)?;
        }
        Ok(Self::new(ctx.clone(), proof.cloned()))
    }
    #[must_use]
    pub fn ctx(&self) -> &SecurityContext {
        &self.ctx
    }
    /// The supplied reference; evidence rows record it as *supplied*, never as verified.
    #[must_use]
    pub fn supplied_proof(&self) -> Option<&DelegationProofRef> {
        self.proof.as_ref()
    }
}
impl std::fmt::Debug for Caller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caller")
            .field("subject_id", &self.ctx.subject_id())
            .field("subject_tenant_id", &self.ctx.subject_tenant_id())
            .field("proof", &self.proof)
            .finish_non_exhaustive()
    }
}

/// A proposed draft axis change (02 §3.6 *Edit Order*): the resource and/or payer values a
/// request names. The seller is fixed from creation (D-119) and is never part of a delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArrangementDelta {
    pub resource_tenant_id: Option<Uuid>,
    pub payer_tenant_id: Option<Uuid>,
}

/// The three business axes of one current or proposed arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "the fields are the normative column and PDP property names (08 §4.3), which all \
              end in `_tenant_id`"
)]
pub struct Arrangement {
    pub resource_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
}
impl Arrangement {
    /// Proposed values from stored facts plus a validated delta; omitted axes keep stored values.
    #[must_use]
    pub fn with_delta(self, delta: ArrangementDelta) -> Self {
        Self {
            resource_tenant_id: delta.resource_tenant_id.unwrap_or(self.resource_tenant_id),
            seller_tenant_id: self.seller_tenant_id,
            payer_tenant_id: delta.payer_tenant_id.unwrap_or(self.payer_tenant_id),
        }
    }
}

/// Minimal facts from the approved point-read prefetch; inputs to PDP, never a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationFacts {
    order_id: Uuid,
    arrangement: Arrangement,
}
impl AuthorizationFacts {
    /// Only the storage prefetch adapter supplies facts.
    pub(crate) fn observed(order_id: Uuid, arrangement: Arrangement) -> Self {
        Self {
            order_id,
            arrangement,
        }
    }
    #[must_use]
    pub fn order_id(&self) -> Uuid {
        self.order_id
    }
    #[must_use]
    pub fn arrangement(&self) -> Arrangement {
        self.arrangement
    }
}

/// Result of the point prefetch for a requested target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prefetch {
    Found(AuthorizationFacts),
    Missing(Uuid),
}
impl Prefetch {
    #[must_use]
    pub fn order_id(&self) -> Uuid {
        match self {
            Self::Found(facts) => facts.order_id,
            Self::Missing(id) => *id,
        }
    }
}

/// Non-disclosing authorization outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthzFailure {
    /// Registered refusal; `proof` keeps the classified reason for operational evidence only.
    Refused {
        reason: Reason,
        proof: Option<ProofDenial>,
    },
    /// PDP timeout/outage on any required call: sanitized retryable 503.
    Unavailable,
    /// Missing/invalid constraints or catalog drift: fail closed, not a permission denial.
    Integration,
}
impl AuthzFailure {
    fn refused(reason: Reason, proof: Option<ProofDenial>) -> Self {
        Self::Refused { reason, proof }
    }
}
impl From<AuthzFailure> for OrdersError {
    fn from(value: AuthzFailure) -> Self {
        match value {
            AuthzFailure::Refused { reason, .. } => Self::Refused(reason),
            AuthzFailure::Unavailable => Self::Unavailable,
            AuthzFailure::Integration => Self::Integration,
        }
    }
}

/// PDP-compiled scope for an untargeted collection read.
#[derive(Debug, Clone)]
pub struct CollectionScope {
    action: Action,
    scope: AccessScope,
}
impl CollectionScope {
    #[must_use]
    pub fn action(&self) -> Action {
        self.action
    }
    /// The PDP-compiled row scope the collection query runs under.
    pub(crate) fn access_scope(&self) -> &AccessScope {
        &self.scope
    }
}

/// Authorization of the requested action on the current order facts.
#[derive(Debug, Clone)]
pub struct TargetAuthorization {
    action: Action,
    facts: AuthorizationFacts,
    scope: AccessScope,
}
impl TargetAuthorization {
    #[must_use]
    pub fn action(&self) -> Action {
        self.action
    }
    #[must_use]
    pub fn facts(&self) -> &AuthorizationFacts {
        &self.facts
    }
    /// The PDP-compiled scope the authorized order read or lock runs under.
    pub(crate) fn access_scope(&self) -> &AccessScope {
        &self.scope
    }
}

/// Authorization of a complete proposed arrangement (create, or an axis change).
#[derive(Debug, Clone)]
pub struct ProposedAuthorization {
    action: Action,
    order_id: Option<Uuid>,
    arrangement: Arrangement,
    scope: AccessScope,
}
impl ProposedAuthorization {
    #[must_use]
    pub fn action(&self) -> Action {
        self.action
    }
    #[must_use]
    pub fn order_id(&self) -> Option<Uuid> {
        self.order_id
    }
    #[must_use]
    pub fn arrangement(&self) -> Arrangement {
        self.arrangement
    }
    /// The PDP-compiled scope the proposed arrangement was authorized under.
    pub(crate) fn access_scope(&self) -> &AccessScope {
        &self.scope
    }
}

enum Raw {
    Allowed(AccessScope),
    Denied(Option<ProofDenial>),
    Unavailable,
    Integration,
}

/// The one shared Orders PEP.
#[derive(Clone)]
pub struct Pep {
    enforcer: Arc<PolicyEnforcer>,
    /// The configured identities [`decision_class`] reads (D-226).
    identities: ActorIdentities,
}
impl std::fmt::Debug for Pep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pep").finish_non_exhaustive()
    }
}

impl Pep {
    #[must_use]
    pub fn new(enforcer: Arc<PolicyEnforcer>, identities: ActorIdentities) -> Self {
        Self {
            enforcer,
            identities,
        }
    }

    /// Untargeted collection read (list); filters and cursors only narrow the returned scope.
    ///
    /// # Errors
    /// Untargeted denial reasons, sanitized outage or integration failure.
    pub async fn authorize_collection(
        &self,
        caller: &Caller,
        action: Action,
    ) -> Result<CollectionScope, AuthzFailure> {
        // Only the list operation is an untargeted read; a mutation's scope must never become a
        // collection read scope (unresolved audit has its own optional-branch entry).
        if action != Action::OrderRead {
            return Err(AuthzFailure::Integration);
        }
        match self.decide(caller, action, None, None).await {
            Raw::Allowed(scope) => Ok(CollectionScope { action, scope }),
            raw => Err(untargeted_failure(&raw)),
        }
    }

    /// Optional operational unresolved-audit branch: a denial omits those rows (`None`); an
    /// outage still fails the whole audit response.
    ///
    /// # Errors
    /// Sanitized outage or integration failure.
    pub async fn authorize_unresolved_audit(
        &self,
        caller: &Caller,
    ) -> Result<Option<CollectionScope>, AuthzFailure> {
        match self
            .decide(caller, Action::AuditUnresolvedRead, None, None)
            .await
        {
            Raw::Allowed(scope) => Ok(Some(CollectionScope {
                action: Action::AuditUnresolvedRead,
                scope,
            })),
            Raw::Denied(_) => Ok(None),
            Raw::Unavailable => Err(AuthzFailure::Unavailable),
            Raw::Integration => Err(AuthzFailure::Integration),
        }
    }

    /// Untargeted authorization of a complete proposed arrangement: `order × create` or
    /// `order × preview`. The returned scope must admit the full proposed model before any
    /// registry probe or commercial read. The payer relationship is a business guard (D-215), not
    /// part of this decision.
    ///
    /// # Errors
    /// Untargeted denial reasons, sanitized outage or integration failure.
    pub async fn authorize_new_arrangement(
        &self,
        caller: &Caller,
        action: Action,
        proposed: Arrangement,
    ) -> Result<ProposedAuthorization, AuthzFailure> {
        if !matches!(action, Action::OrderCreate | Action::OrderPreview) {
            return Err(AuthzFailure::Integration);
        }
        match self.decide(caller, action, None, Some(proposed)).await {
            Raw::Allowed(scope) if admits(&scope, None, proposed) => Ok(ProposedAuthorization {
                action,
                order_id: None,
                arrangement: proposed,
                scope,
            }),
            // Constraints that exclude the proposed arrangement authorize other arrangements only.
            Raw::Allowed(_) => Err(AuthzFailure::refused(
                Reason::OperationNotPermittedForActor,
                None,
            )),
            raw => Err(untargeted_failure(&raw)),
        }
    }

    /// Targeted decision with the exact hidden/missing call pattern (08 §3.6 item 2, D-68,
    /// D-114, D-141). Both arms make the same PDP calls; the follow-up `order × read` is made
    /// on every denial of a non-read action, discarded on the missing arm.
    ///
    /// # Errors
    /// `order-not-found` / `operation-not-permitted-for-actor`, sanitized outage on either call,
    /// or an integration failure.
    pub async fn authorize_target(
        &self,
        caller: &Caller,
        action: Action,
        prefetch: &Prefetch,
    ) -> Result<TargetAuthorization, AuthzFailure> {
        let order_id = prefetch.order_id();
        let current = match prefetch {
            Prefetch::Found(facts) => Some(facts.arrangement),
            Prefetch::Missing(_) => None,
        };
        let first = self.decide(caller, action, Some(order_id), current).await;
        let denied_proof = match first {
            Raw::Unavailable => return Err(AuthzFailure::Unavailable),
            Raw::Integration => return Err(AuthzFailure::Integration),
            Raw::Allowed(scope) => match prefetch {
                Prefetch::Found(facts) if admits(&scope, Some(order_id), facts.arrangement) => {
                    return self.admit_user_only(caller, action, facts, scope).await;
                }
                // Missing arm, or constraints that exclude the target: the allow is discarded and
                // the request takes the same follow-up as a denial, so a constraint-only answer
                // neither skips the follow-up nor treats a hidden target as authorized.
                Prefetch::Found(_) | Prefetch::Missing(_) => None,
            },
            Raw::Denied(proof) => proof,
        };
        self.targeted_denial(caller, action, order_id, current, denied_proof)
            .await
    }

    /// Second decision for an axis change on an existing order: the same action against the
    /// complete proposed arrangement (no payer condition, D-215). Both decisions are required;
    /// the proposed side never authorizes the existing order and vice versa.
    ///
    /// # Errors
    /// Targeted denial mapping (follow-up read on current facts), outage or integration failure.
    pub async fn authorize_proposed(
        &self,
        caller: &Caller,
        current: &TargetAuthorization,
        proposed: Arrangement,
    ) -> Result<ProposedAuthorization, AuthzFailure> {
        let order_id = current.facts.order_id;
        // D-119: the seller is fixed after create, so a seller change is never a proposed
        // arrangement; the capture/versioning guard refuses such a request `tenant-axis-immutable`
        // before any proposed-side decision. Reaching here with one is an integration defect.
        if proposed.seller_tenant_id != current.facts.arrangement.seller_tenant_id {
            tracing::error!(
                target: "orders.authz.integration",
                "proposed arrangement changes the immutable seller axis"
            );
            return Err(AuthzFailure::Integration);
        }
        match self
            .decide(caller, current.action, Some(order_id), Some(proposed))
            .await
        {
            Raw::Allowed(scope) if admits(&scope, Some(order_id), proposed) => {
                Ok(ProposedAuthorization {
                    action: current.action,
                    order_id: Some(order_id),
                    arrangement: proposed,
                    scope,
                })
            }
            // Constraints that exclude the proposal are a proposed-side denial.
            Raw::Allowed(_) => {
                self.targeted_denial(
                    caller,
                    current.action,
                    order_id,
                    Some(current.facts.arrangement),
                    None,
                )
                .await
            }
            Raw::Denied(proof) => {
                self.targeted_denial(
                    caller,
                    current.action,
                    order_id,
                    Some(current.facts.arrangement),
                    proof,
                )
                .await
            }
            Raw::Unavailable => Err(AuthzFailure::Unavailable),
            Raw::Integration => Err(AuthzFailure::Integration),
        }
    }

    /// Replay never discloses a stored outcome on old authority: recheck the operation's action
    /// (or `order × read` for a create's result) against current facts before returning it.
    ///
    /// # Errors
    /// The same targeted mapping as [`Self::authorize_target`].
    pub async fn recheck_replay_disclosure(
        &self,
        caller: &Caller,
        action: Action,
        prefetch: &Prefetch,
    ) -> Result<TargetAuthorization, AuthzFailure> {
        let action = if matches!(action, Action::OrderCreate) {
            Action::OrderRead
        } else {
            action
        };
        self.authorize_target(caller, action, prefetch).await
    }

    async fn admit_user_only(
        &self,
        caller: &Caller,
        action: Action,
        facts: &AuthorizationFacts,
        scope: AccessScope,
    ) -> Result<TargetAuthorization, AuthzFailure> {
        // D-182: the break-glass grant is honored for user principals only; a service principal
        // holding it is treated as a denial on the same follow-up call pattern.
        if action == Action::OrderForceFailUnreconciled
            && decision_class(&self.identities, caller.ctx()) != AuditActorClass::User
        {
            return self
                .targeted_denial(
                    caller,
                    action,
                    facts.order_id,
                    Some(facts.arrangement),
                    None,
                )
                .await;
        }
        Ok(TargetAuthorization {
            action,
            facts: facts.clone(),
            scope,
        })
    }

    async fn targeted_denial<T>(
        &self,
        caller: &Caller,
        action: Action,
        order_id: Uuid,
        current: Option<Arrangement>,
        proof: Option<ProofDenial>,
    ) -> Result<T, AuthzFailure> {
        if let Some(kind) = proof {
            // The action and target tie the denial to a request; the subject id, the proof
            // reference and the principal scope stay out of the log (D-44).
            tracing::warn!(
                target: "orders.authz.proof",
                action = ?action,
                order_id = %order_id,
                subject_tenant_id = %caller.ctx.subject_tenant_id(),
                invalid = kind == ProofDenial::Invalid,
                "delegation proof denial on targeted request"
            );
        }
        if action == Action::OrderRead {
            return Err(AuthzFailure::refused(Reason::OrderNotFound, proof));
        }
        let follow = self
            .decide(caller, Action::OrderRead, Some(order_id), current)
            .await;
        let readable = match follow {
            Raw::Unavailable => return Err(AuthzFailure::Unavailable),
            Raw::Integration => return Err(AuthzFailure::Integration),
            // 403 only when the follow-up read admits this target's current facts; a read scope
            // that excludes it must not confirm the target exists.
            Raw::Allowed(scope) => current.is_some_and(|a| admits(&scope, Some(order_id), a)),
            Raw::Denied(_) => false,
        };
        let reason = if readable && proof.is_none() {
            Reason::OperationNotPermittedForActor
        } else {
            Reason::OrderNotFound
        };
        Err(AuthzFailure::refused(reason, proof))
    }

    async fn decide(
        &self,
        caller: &Caller,
        action: Action,
        order_id: Option<Uuid>,
        arrangement: Option<Arrangement>,
    ) -> Raw {
        let (Ok(permission), Ok(resource)) = (action.permission(), action.resource_type()) else {
            return Raw::Integration;
        };
        let request = access_request(caller, order_id, arrangement);
        let result = self
            .enforcer
            .access_scope_with(&caller.ctx, resource, permission.action, order_id, &request)
            .await;
        let finite_ids = requires_finite_ids(&self.identities, caller, action);
        classify(finite_ids, permission.action, result)
    }
}

fn access_request(
    caller: &Caller,
    order_id: Option<Uuid>,
    arrangement: Option<Arrangement>,
) -> AccessRequest {
    let mut request = AccessRequest::new().require_constraints(true);
    if let Some(id) = order_id {
        request = request.resource_property(properties::ID, id);
    }
    if let Some(a) = arrangement {
        request = request
            .resource_property(properties::RESOURCE_TENANT_ID, a.resource_tenant_id)
            .resource_property(properties::SELLER_TENANT_ID, a.seller_tenant_id)
            .resource_property(properties::PAYER_TENANT_ID, a.payer_tenant_id);
    }
    if let Some(proof) = &caller.proof {
        request = request.resource_property(DELEGATION_PROOF_REF, proof.as_str());
    }
    request
}

fn classify(
    requires_finite_ids: bool,
    pdp_action: &str,
    result: Result<AccessScope, EnforcerError>,
) -> Raw {
    match result {
        Ok(scope) if requires_finite_ids && !finite_order_ids(&scope) => integration(
            pdp_action,
            "service decision lacks a finite order-ID path restriction",
        ),
        Ok(scope) => Raw::Allowed(scope),
        Err(EnforcerError::Denied { deny_reason }) => {
            Raw::Denied(classify_proof(deny_reason.as_ref()))
        }
        Err(EnforcerError::EvaluationFailed(error)) => unavailable(pdp_action, &error),
        Err(EnforcerError::CompileFailed(error)) => compile_failed(pdp_action, &error),
    }
}

/// Sanitized operational signal; no payload, proof, tenant or commercial details.
fn integration(pdp_action: &str, why: &'static str) -> Raw {
    tracing::error!(target: "orders.authz.integration", action = pdp_action, why);
    Raw::Integration
}

/// The PDP constraints did not compile: logs which way they failed and, for a capability
/// violation, the predicate and capability names. The aggregate reason text of the other
/// variants embeds constraint values (tenant and resource ids), so it is never logged.
fn compile_failed(pdp_action: &str, error: &ConstraintCompileError) -> Raw {
    let (kind, predicate, missing) = match error {
        ConstraintCompileError::ConstraintsRequiredButAbsent => ("constraints_absent", "", &[][..]),
        ConstraintCompileError::AllConstraintsFailed { .. } => {
            ("all_constraints_failed", "", &[][..])
        }
        ConstraintCompileError::UnadvertisedCapabilities { predicate, missing } => {
            ("unadvertised_capabilities", *predicate, missing.as_slice())
        }
        _ => ("other", "", &[][..]),
    };
    tracing::error!(
        target: "orders.authz.integration",
        action = pdp_action,
        kind,
        predicate,
        missing = ?missing,
        "PDP constraints missing or invalid"
    );
    Raw::Integration
}

/// The PDP evaluation failed: logs the canonical category and status only, never the detail
/// text, proof reference, tenant or request body.
fn unavailable(pdp_action: &str, error: &toolkit_canonical_errors::CanonicalError) -> Raw {
    tracing::warn!(
        target: "orders.authz.unavailable",
        action = pdp_action,
        category = error.gts_type(),
        status = error.status_code(),
        "PDP unavailable"
    );
    Raw::Unavailable
}

/// Enforce the PDP's constraints on the known facts exactly as the scoped query would; never a
/// local permission decision (Orders adds no condition of its own).
fn admits(scope: &AccessScope, order_id: Option<Uuid>, arrangement: Arrangement) -> bool {
    crate::infra::storage::scoped::scope_admits(scope, order_id, arrangement)
}

fn untargeted_failure(raw: &Raw) -> AuthzFailure {
    match raw {
        Raw::Denied(Some(ProofDenial::Required)) => {
            AuthzFailure::refused(Reason::DelegationProofRequired, Some(ProofDenial::Required))
        }
        Raw::Denied(Some(ProofDenial::Invalid)) => {
            AuthzFailure::refused(Reason::DelegationProofInvalid, Some(ProofDenial::Invalid))
        }
        Raw::Denied(None) => AuthzFailure::refused(Reason::OperationNotPermittedForActor, None),
        Raw::Unavailable => AuthzFailure::Unavailable,
        Raw::Allowed(_) | Raw::Integration => AuthzFailure::Integration,
    }
}

fn classify_proof(reason: Option<&DenyReason>) -> Option<ProofDenial> {
    match reason.map(|r| r.error_code.as_str()) {
        Some(DENY_DELEGATION_PROOF_REQUIRED) => Some(ProofDenial::Required),
        Some(DENY_DELEGATION_PROOF_INVALID) => Some(ProofDenial::Invalid),
        _ => None,
    }
}

/// Workflow seam actions and every service-principal decision need a finite explicit order-ID
/// set in **every** OR path (08 §4.3); one bounded branch cannot make an unbounded one safe.
/// Every non-user class of [`decision_class`] is held to it (D-226).
fn requires_finite_ids(identities: &ActorIdentities, caller: &Caller, action: Action) -> bool {
    action.is_workflow_seam() || decision_class(identities, caller.ctx()) != AuditActorClass::User
}

/// Shape check only — never a local permission decision.
#[must_use]
pub fn finite_order_ids(scope: &AccessScope) -> bool {
    !scope.is_unconstrained()
        && !scope.constraints().is_empty()
        && scope.constraints().iter().all(|path| {
            path.filters().iter().any(|filter| match filter {
                ScopeFilter::Eq(eq) => {
                    eq.property() == properties::ID && matches!(eq.value(), ScopeValue::Uuid(_))
                }
                ScopeFilter::In(set) => {
                    set.property() == properties::ID
                        && !set.values().is_empty()
                        && set
                            .values()
                            .iter()
                            .all(|v| matches!(v, ScopeValue::Uuid(_)))
                }
                _ => false,
            })
        })
}

#[cfg(test)]
pub(crate) mod test_pdp;
#[cfg(test)]
mod tests;
