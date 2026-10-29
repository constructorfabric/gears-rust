//! The transition orchestrator's transaction adapter (Foundation §3.6 *Attempt Transition* and
//! *Create Transition*; DESIGN §4.1 *The Transition Contract*; ADR-0001/0005/0007).
//!
//! The single engine operation. It composes the delivered collaborators without reimplementing
//! them: the shared PEP (S2-03), the authoritative idempotency gate and durable executions
//! (S2-05), the sealed v3 audit writer (S2-06), step-17 overlap claims (S2-07) and the managed
//! producer enqueue (S2-08). The declarative state table, guard registry and contribution
//! planner are the pure domain modules (`domain::{state_table, guards, contributions,
//! transition}`).
//!
//! Order of every existing-order attempt: boundary-validated request → authorization (point
//! prefetch + PDP; denial audits unresolved evidence, settles nothing) → advisory probe
//! (authorized replay re-resolves no input) → coherent preparation snapshot (draft operations)
//! and slice input resolution outside any transaction → one READ COMMITTED transaction:
//! aggregate lock and authorization-fact recheck → authoritative registry gate (aggregate before
//! registry) → state/version/draft-revision precedence → guards in registration order →
//! stored-target check → step-17 claims → version append → child documents → gate diagnostics
//! → aggregate write → sealed audit → the row's one event → settlement → commit. Business
//! refusals commit as settled outcomes, a composite-gate or overlap refusal writing its gate
//! diagnostics before refusal settlement (M2-2); every other failure aborts everything. A lost
//! commit acknowledgement is reported as unknown, never as a rollback.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bss_orders_sdk::authoring::{Category, OrderHeader, OrderView, SalesPath, VersionSummary};
use bss_orders_sdk::catalog::{OrderState, Reason, Trigger};
use bss_orders_sdk::models::{DraftRevision, IdempotencyKey, OrderVersion, TransitionResult};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_db::secure::{ScopeError, TxConfig};
use toolkit_db::{Db, DbTx};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::authz::{
    Action, Arrangement, ArrangementDelta, AuthzFailure, Caller, Pep, TargetAuthorization,
    decision_class,
};
use crate::domain::audit::{
    ActorIdentities, AdminAttribute, AdminChange, AdminField, AdminTextKey, AttemptEvidence,
    AuditActor, AuditActorClass, AuditTrigger,
};
use crate::domain::contributions::{
    self, AggregateContribution, AggregateFacts, FieldClassification, requires_commercial_attempt,
    requires_receiver_control,
};
use crate::domain::guards::GuardRegistry;
use crate::domain::idempotency::{
    DraftRevisionInput, ExecutionOwner, Fingerprint, FingerprintAxes, FingerprintInput,
    FingerprintTarget, LeaseDuration, PrincipalScope, RegistryKey, RegistryOperation,
    RegistryRuleError, Settlement, StoredResponse,
};
use crate::domain::overlap::{ClaimDirective, ClaimOutcome, ProposedClaims};
use crate::domain::state_table::{ActorRule, Row, StateTable};
use crate::domain::transition::{self, Decision, DraftRevisions, GuardBindings, Inputs};
use crate::infra::events::{self, EventSink, OrderSummary, TxEvents};
use crate::infra::execution::{ReplayTarget, disclose_replay};
use crate::infra::maintenance::scope::TaskGrant;
use crate::infra::maintenance::{MaintenanceTask, TargetScope};
use crate::infra::storage::entity::{self, draft_content, order, order_version};
use crate::infra::storage::repo::audit as writer;
use crate::infra::storage::repo::claims::{ProposalAuthority, transition_tx_config};
use crate::infra::storage::repo::idempotency::{
    self as reg, AttemptInputs, ControlInputs, Durability, DurableExecution, ExecutionTerminal,
    Frozen, Gate, GateRequest, GateTarget, Owned, Probe, Replay, ReplayOutcome,
};
use crate::infra::storage::repo::{LockedOrder, children};
use crate::infra::storage::scoped::{self, AuthorizedLock, AuthorizedRead, PrivateScope};

pub mod documents;
pub mod errors;

pub use documents::{
    ChildDocuments, DiagnosticsContribution, DiagnosticsSettled, DocumentWriter, EventSpec,
};
pub use errors::{Abort, EngineError, FaultPoint};

/// Transaction attempts for transient contention (deadlock/serialization) before giving up.
const TX_ATTEMPTS: u32 = 3;
const SERVICE_SUBJECT: &str = toolkit_gts::gts_id!("cf.core.security.subject_service.v1~");

// ---------------------------------------------------------------------------------------------
// Public request/outcome model

/// The slice's prepared, validated contribution (Foundation step 2, resolved outside any
/// transaction). The engine decides when each part is used.
#[derive(Clone)]
pub struct Prepared {
    /// The request's input resolutions, one per declared port of the row.
    pub inputs: Inputs,
    /// The slice's guard predicates, by registered guard name.
    pub guards: GuardBindings,
    /// The row's contribution to the order aggregate.
    pub aggregate: AggregateContribution,
    /// Resolved overlap claims: exactly submit and amendment carry them (step 17.2).
    pub claims: Option<ProposedClaims>,
    /// Child documents of an admitted transition (step 19).
    pub documents: Option<Arc<dyn DocumentWriter>>,
    /// The row's one event detail (step 24); `None` exactly for eventless rows.
    pub event: Option<Arc<dyn EventSpec>>,
    /// Administrative changes (row 3 only), audited one entry per changed field (D-117).
    pub admin_changes: Vec<AdminChange>,
    /// The reached gate assessment snapshot stored with the response (S3 owns its contents).
    pub assessment: Option<Value>,
    /// The gate's diagnostic run, written by the engine on a composite-gate refusal, an
    /// overlap conflict and an admission, inside the transaction before settlement (M2-2).
    pub diagnostics: Option<Arc<dyn DiagnosticsContribution>>,
    /// The server-reserved line identity a line insertion returns (02 *Author Line* step 6); it
    /// is part of the settled response, so a same-key replay returns the same identity.
    pub line_id: Option<Uuid>,
    /// Frozen commercial assessment identity returned on successful submit.
    pub assessment_id: Option<Uuid>,
}
impl Prepared {
    /// A contribution with no inputs, guards, documents or event.
    #[must_use]
    pub fn new(aggregate: AggregateContribution) -> Self {
        Self {
            inputs: Inputs::new(),
            guards: GuardBindings::new(),
            aggregate,
            claims: None,
            documents: None,
            event: None,
            admin_changes: Vec::new(),
            assessment: None,
            diagnostics: None,
            line_id: None,
            assessment_id: None,
        }
    }
}

/// What the slice sees when resolving its inputs: authorized facts, never a lock.
#[derive(Debug, Clone)]
pub struct PreparationView {
    /// The order's business facts, as authorized.
    pub facts: AggregateFacts,
    /// Draft operations: the revision of the coherent snapshot taken under the aggregate lock.
    pub prepared_draft_revision: Option<i64>,
    /// Draft operations: the working lines of that snapshot.
    pub draft_lines: Vec<draft_content::Model>,
    /// A recovered D-188/D-198 execution's frozen inputs, loaded before any fresh assessment.
    pub frozen: Option<Frozen>,
}

/// Slice input resolution failure that is not an unresolvable guard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareError {
    /// A dependency is temporarily unavailable; the attempt reports unavailable.
    Unavailable,
    /// A slice or contract defect; the attempt fails closed as an integration failure.
    Integration,
}

/// The slice's preparation callback, invoked after authorization and the advisory probe.
#[async_trait]
pub trait Preparation: Send + Sync {
    /// # Errors
    /// A preparation failure; it aborts before any transaction.
    async fn prepare(&self, view: &PreparationView) -> Result<Prepared, PrepareError>;
}

/// An existing-order request (boundary-validated: expected version present and well formed).
#[derive(Debug, Clone)]
pub struct TransitionRequest {
    /// The target order.
    pub order_id: Uuid,
    /// The requested lifecycle trigger.
    pub trigger: Trigger,
    /// Public catalog operation identifier (fingerprint input).
    pub operation: &'static str,
    /// The caller's expected current order version (positive by its type).
    pub expected_version: OrderVersion,
    /// Client `expected_draft_revision` for draft writes/submit (optional for draft-mutate;
    /// nonnegative by its type).
    pub expected_draft_revision: Option<DraftRevision>,
    /// Canonicalised document contribution (fingerprint input).
    pub document: Value,
    /// A proposed resource/payer change: the engine completes it over the authorized current
    /// facts and requires the complete proposed-arrangement decision (08 §4.3).
    pub proposed: Option<ArrangementDelta>,
    /// Separately validated caller explanation (`caller_reason`, D-143).
    pub caller_reason: Option<String>,
    /// The caller's idempotency key.
    pub idempotency_key: IdempotencyKey,
    /// The caller's correlation identifier, if supplied.
    pub correlation_id: Option<Uuid>,
    /// The presented D-188/D-198 owner on a durable continuation.
    pub execution: Option<ExecutionOwner>,
}

impl TransitionRequest {
    /// The same request presenting `owner`, the D-188/D-198 execution it continues (the final
    /// commit of a reserved commercial attempt). Every fingerprint input is unchanged.
    #[must_use]
    pub fn with_execution(mut self, owner: ExecutionOwner) -> Self {
        self.execution = Some(owner);
        self
    }
}

/// A create request (D-105): no target and no expected version.
#[derive(Debug, Clone)]
pub struct CreateRequest {
    /// The GTS order-category identifier of the new draft.
    pub category: String,
    /// The proposed resource, seller and payer tenants.
    pub arrangement: Arrangement,
    /// The governing contract, if any.
    pub contract_id: Option<Uuid>,
    /// Canonicalised document contribution (fingerprint input).
    pub document: Value,
    /// The caller's idempotency key.
    pub idempotency_key: IdempotencyKey,
    /// The caller's correlation identifier, if supplied.
    pub correlation_id: Option<Uuid>,
}

/// A settled or replayed outcome; its response is the immutable snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineOutcome {
    /// Whether the attempt committed, was refused or replayed.
    pub kind: OutcomeKind,
    /// The immutable stored response.
    pub response: StoredResponse,
}
/// How an attempt settled.
#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeKind {
    /// The transition committed.
    Committed {
        /// The committed order.
        order_id: Uuid,
        /// The committed audit entry.
        audit_id: Uuid,
    },
    /// A business refusal settled the key.
    Refused {
        /// The registered refusal reason.
        reason: Reason,
        /// The refusal's audit entry.
        audit_id: Uuid,
    },
    /// A same-key replay of an earlier settlement.
    Replayed {
        /// The replayed settlement's outcome.
        outcome: ReplayOutcome,
        /// The order the replayed record settled on; none for a refused create.
        order_id: Option<Uuid>,
    },
}

/// Engine construction inputs; the compiled registries are validated at startup.
pub struct EngineParts {
    /// The database handle.
    pub db: Db,
    /// The shared policy enforcement point.
    pub pep: Pep,
    /// The bound managed event producer.
    pub sink: EventSink,
    /// Configured identities the audit actor class derives from.
    pub identities: ActorIdentities,
    /// The keyed minimization secret for administrative text.
    pub admin_key: Arc<AdminTextKey>,
    /// The idempotency execution lease.
    pub lease: LeaseDuration,
}

/// The compiled declarations the engine runs (startup-validated, Foundation §3.4 step 1).
#[derive(Debug, Clone)]
pub struct Registries {
    /// The compiled state table.
    pub table: StateTable,
    /// The compiled per-row guard sets.
    pub guards: GuardRegistry,
    /// The compiled field classification.
    pub fields: FieldClassification,
}
impl Registries {
    /// Compile the state table, guard registry and field classification.
    ///
    /// # Errors
    /// Any invalid declaration: the gear refuses to start.
    pub fn compile() -> anyhow::Result<Self> {
        let table = StateTable::registered()
            .map_err(|e| anyhow::anyhow!("bss-orders: state table: {e}"))?;
        let guards = GuardRegistry::registered(&table)
            .map_err(|e| anyhow::anyhow!("bss-orders: guard registry: {e}"))?;
        let fields = FieldClassification::registered()
            .map_err(|e| anyhow::anyhow!("bss-orders: field classes: {e}"))?;
        crate::domain::capture::check_wire_classification(&fields)
            .map_err(|e| anyhow::anyhow!("bss-orders: field classes: {e}"))?;
        Ok(Self {
            table,
            guards,
            fields,
        })
    }
}

/// Test-only failure injection at the named persistence boundaries.
#[cfg(test)]
#[async_trait]
pub trait FaultHook: Send + Sync {
    async fn at(&self, point: FaultPoint, tx: &DbTx<'_>) -> Result<(), Abort>;
}

struct Inner {
    db: Db,
    pep: Pep,
    sink: EventSink,
    identities: ActorIdentities,
    admin_key: Arc<AdminTextKey>,
    lease: LeaseDuration,
    registries: Registries,
    #[cfg(test)]
    fault: Option<Arc<dyn FaultHook>>,
}

/// The single engine entry point. Cloning shares one configured engine.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------------------------
// Internal attempt context

/// How the attempt locks its aggregate: a PEP decision (callers) or the worker's discovered
/// target (08 §3.5 private capability; never a broad discovery scope).
#[derive(Clone)]
enum TargetLock {
    Authorized(TargetAuthorization),
    Internal(TargetScope),
}
impl TargetLock {
    /// The current-order write scope: the PDP decision's, or the worker's discovered target.
    fn access_scope(&self) -> &toolkit_security::AccessScope {
        match self {
            Self::Authorized(target) => target.access_scope(),
            Self::Internal(target) => target.access_scope(),
        }
    }
}

struct Attempt {
    caller: Caller,
    actor: AuditActor,
    trigger: Trigger,
    order_id: Uuid,
    expected_version: i32,
    revisions_client: Option<i64>,
    prepared_draft_revision: Option<i64>,
    target: TargetLock,
    proposed: Option<crate::authz::ProposedAuthorization>,
    /// Acquiring rows: the caller's order-read scope naming a conflicting holder (D-179).
    disclosure: Option<toolkit_security::AccessScope>,
    key: RegistryKey,
    fingerprint: Fingerprint,
    principal: PrivateScope,
    presented: Option<ExecutionOwner>,
    caller_reason: Option<String>,
    correlation_id: Option<Uuid>,
    prepared: Prepared,
    completed: AtomicBool,
}

/// What a committed transaction produced.
enum TxOutcome {
    Settled(EngineOutcome),
    /// Evidence committed; nothing settled (authorization-fact conflicts, lost access,
    /// mismatch, still-processing).
    Unsettled(Reason),
    /// A matching settled record: disclose after commit.
    Replay(Replay),
}

impl Engine {
    /// Build the engine over its validated collaborators.
    #[must_use]
    pub fn new(parts: EngineParts, registries: Registries) -> Self {
        Self {
            inner: Arc::new(Inner {
                db: parts.db,
                pep: parts.pep,
                sink: parts.sink,
                identities: parts.identities,
                admin_key: parts.admin_key,
                lease: parts.lease,
                registries,
                #[cfg(test)]
                fault: None,
            }),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_fault(&self, hook: Arc<dyn FaultHook>) -> Self {
        let inner = &self.inner;
        Self {
            inner: Arc::new(Inner {
                db: inner.db.clone(),
                pep: inner.pep.clone(),
                sink: inner.sink.clone(),
                identities: inner.identities.clone(),
                admin_key: Arc::clone(&inner.admin_key),
                lease: inner.lease,
                registries: inner.registries.clone(),
                fault: Some(hook),
            }),
        }
    }

    /// The compiled registries (slices read the field classification from here).
    #[must_use]
    pub fn registries(&self) -> &Registries {
        &self.inner.registries
    }

    // -----------------------------------------------------------------------------------------
    // Create branch (D-105)

    /// Create Transition: never loads/locks a nonexistent aggregate, never compares an
    /// existing version, is event-less, and creates nothing on refusal.
    ///
    /// # Errors
    /// Unsettled refusals (authorization denial, mismatch, still-processing) and sanitized
    /// infrastructure outcomes.
    pub async fn create(
        &self,
        caller: &Caller,
        request: CreateRequest,
        preparation: &dyn Preparation,
    ) -> Result<EngineOutcome, EngineError> {
        let inner = &self.inner;
        let actor = classify(&inner.identities, caller.ctx())?;
        let key = registry_key(caller, Trigger::Create, &request.idempotency_key)?;
        let authorized = match inner
            .pep
            .authorize_new_arrangement(caller, Action::OrderCreate, request.arrangement)
            .await
        {
            Ok(authorized) => authorized,
            Err(failure) => {
                return Err(self
                    .record_denial(
                        caller,
                        &actor,
                        Trigger::Create,
                        None,
                        &key,
                        &request,
                        failure,
                    )
                    .await);
            }
        };
        let fingerprint = FingerprintInput {
            operation: "create",
            trigger: Trigger::Create,
            target: FingerprintTarget::Create,
            axes: axes(request.arrangement),
            draft_revision: DraftRevisionInput::NotApplicable,
            contribution: &request.document,
        }
        .fingerprint()
        .map_err(|e| fingerprint_failure("create", Trigger::Create, e))?;
        let principal = PrivateScope::for_principal(key.principal().as_str());
        if let Probe::Replay(replay) = self
            .probe(&key, &fingerprint, None, AbortContext::of(&request))
            .await?
        {
            return self.disclose(caller, ReplayTarget::Create, replay).await;
        }
        let view = PreparationView {
            facts: creation_facts(&request, Uuid::nil()),
            prepared_draft_revision: None,
            draft_lines: Vec::new(),
            frozen: None,
        };
        let prepared = preparation.prepare(&view).await.map_err(prepare_error)?;
        let create = Arc::new(CreateAttempt {
            caller: caller.clone(),
            actor,
            request,
            authorized,
            key,
            fingerprint,
            principal,
            prepared,
            completed: AtomicBool::new(false),
        });
        let inner2 = Arc::clone(&self.inner);
        let attempt = Arc::clone(&create);
        let result = events::transaction(
            &inner.db,
            &inner.sink,
            transition_tx_config(),
            TX_ATTEMPTS,
            Abort::db_err,
            move |tx, _events| {
                let inner = Arc::clone(&inner2);
                let attempt = Arc::clone(&attempt);
                Box::pin(async move {
                    attempt.completed.store(false, Ordering::SeqCst);
                    let out = Box::pin(create_body(&inner, &attempt, tx)).await?;
                    attempt.completed.store(true, Ordering::SeqCst);
                    Ok(out)
                })
            },
        )
        .await;
        let context = AbortContext::of(&create.request);
        self.finish(
            caller,
            ReplayTarget::Create,
            result,
            &create.completed,
            context,
        )
        .await
    }

    // -----------------------------------------------------------------------------------------
    // Existing-order branch

    /// Attempt Transition for an authenticated caller (every trigger except create and the
    /// worker-only expiry triggers).
    ///
    /// # Errors
    /// Unsettled refusals and sanitized infrastructure outcomes; settled refusals are `Ok`.
    pub async fn transition(
        &self,
        caller: &Caller,
        request: TransitionRequest,
        preparation: &dyn Preparation,
    ) -> Result<EngineOutcome, EngineError> {
        let inner = &self.inner;
        // D-105: create runs its dedicated branch and never this existing-order algorithm.
        if request.trigger == Trigger::Create {
            return Err(EngineError::Integration);
        }
        let action = action_for(request.trigger).ok_or(EngineError::Integration)?;
        check_axis_change(&request)?;
        let actor = classify(&inner.identities, caller.ctx())?;
        let key = registry_key(caller, request.trigger, &request.idempotency_key)?;
        let prefetch = scoped::prefetch(&inner.db, request.order_id)
            .await
            .map_err(|e| errors::prefetch_failure(request.trigger.token(), &e))?;
        let target = match inner.pep.authorize_target(caller, action, &prefetch).await {
            Ok(target) => target,
            Err(failure) => {
                return Err(self
                    .record_denial(
                        caller,
                        &actor,
                        request.trigger,
                        Some(request.order_id),
                        &key,
                        &request,
                        failure,
                    )
                    .await);
            }
        };
        // A declared row actor rule must match the caller's decision class; the PEP already
        // refused every non-user class on the break-glass rows from the same class (D-182,
        // D-226), so this is the backstop.
        check_actor_rule(
            &inner.registries.table,
            request.trigger,
            decision_class(&inner.identities, caller.ctx()),
        )?;
        let proposed = match request.proposed {
            Some(delta) => match inner
                .pep
                .authorize_proposed(
                    caller,
                    &target,
                    target.facts().arrangement().with_delta(delta),
                )
                .await
            {
                Ok(proposed) => Some(proposed),
                Err(failure) => {
                    return Err(self
                        .record_denial(
                            caller,
                            &actor,
                            request.trigger,
                            Some(request.order_id),
                            &key,
                            &request,
                            failure,
                        )
                        .await);
                }
            },
            None => None,
        };
        let fingerprint = fingerprint(&request, target.facts().arrangement())?;
        let principal = PrivateScope::for_principal(key.principal().as_str());
        let frozen = match self
            .probe(
                &key,
                &fingerprint,
                Some((request.order_id, &target)),
                AbortContext::of(&request),
            )
            .await?
        {
            Probe::Replay(replay) => {
                return self
                    .disclose(
                        caller,
                        ReplayTarget::Order {
                            action,
                            prefetch: &prefetch,
                        },
                        replay,
                    )
                    .await;
            }
            Probe::Existing(frozen) => Some(frozen),
            // The authoritative gate refuses the mismatch with its evidence.
            Probe::Mismatch | Probe::Miss => None,
        };
        let disclosure = self.disclosure(caller, request.trigger).await?;
        let view = match self
            .snapshot(&target, AbortContext::of(&request), frozen)
            .await?
        {
            Ok(view) => view,
            Err(reason) => {
                return Err(self
                    .record_late_conflict(caller, &actor, &request, &key, reason)
                    .await);
            }
        };
        let prepared = preparation.prepare(&view).await.map_err(prepare_error)?;
        let attempt = Arc::new(Attempt {
            caller: caller.clone(),
            actor,
            trigger: request.trigger,
            order_id: request.order_id,
            expected_version: i32::from(request.expected_version),
            revisions_client: request.expected_draft_revision.map(i64::from),
            prepared_draft_revision: view.prepared_draft_revision,
            target: TargetLock::Authorized(target),
            proposed,
            disclosure,
            key,
            fingerprint,
            principal,
            presented: request.execution,
            caller_reason: request.caller_reason,
            correlation_id: request.correlation_id,
            prepared,
            completed: AtomicBool::new(false),
        });
        let result = self.run(&attempt).await;
        let target = ReplayTarget::Order {
            action,
            prefetch: &prefetch,
        };
        let context = AbortContext::of(&*attempt);
        self.finish(caller, target, result, &attempt.completed, context)
            .await
    }

    /// Internal worker entry (Foundation *Internal worker entry*; 08 §3.5): only the allowlisted
    /// expiry triggers, the configured service actor, and a target scope built from the
    /// discovered row. Every business guard, idempotency rule, audit and outbox obligation
    /// applies; no caller PDP pre-guard runs.
    ///
    /// # Errors
    /// As [`Self::transition`].
    pub async fn transition_internal(
        &self,
        grant: &TaskGrant<'_>,
        target: &TargetScope,
        request: TransitionRequest,
        preparation: &dyn Preparation,
    ) -> Result<EngineOutcome, EngineError> {
        let inner = &self.inner;
        let allowed = matches!(
            (grant.task(), request.trigger),
            (MaintenanceTask::StateExpiry, Trigger::Expire)
                | (MaintenanceTask::DraftAutoVoid, Trigger::AutoVoid)
        );
        let discovered = target.order().ok_or(EngineError::Integration)?;
        if !allowed
            || discovered.order_id() != request.order_id
            || discovered.task() != grant.task()
        {
            return Err(EngineError::Integration);
        }
        let actor_identity = grant.actor();
        let ctx = SecurityContext::builder()
            .subject_id(actor_identity.subject_id())
            .subject_tenant_id(actor_identity.subject_tenant_id())
            .subject_type(SERVICE_SUBJECT)
            .build()
            .map_err(|_| EngineError::Integration)?;
        let caller = Caller::new(ctx, None);
        let actor = classify(&inner.identities, caller.ctx())?;
        check_actor_rule(
            &inner.registries.table,
            request.trigger,
            decision_class(&inner.identities, caller.ctx()),
        )?;
        let key = registry_key(&caller, request.trigger, &request.idempotency_key)?;
        // The target scope pins the discovered persisted axes; a row whose axes changed is not
        // visible through it and the worker never rebinds them.
        let Some(facts) = self
            .internal_facts(target, AbortContext::of(&request))
            .await?
        else {
            return Err(self
                .record_late_conflict(
                    &caller,
                    &actor,
                    &request,
                    &key,
                    Reason::AuthorizationContextChanged,
                )
                .await);
        };
        let current = Arrangement {
            resource_tenant_id: facts.resource_tenant_id,
            seller_tenant_id: facts.seller_tenant_id,
            payer_tenant_id: facts.payer_tenant_id,
        };
        let fingerprint = fingerprint(&request, current)?;
        let principal = PrivateScope::for_principal(key.principal().as_str());
        if let Probe::Replay(replay) = self
            .probe(&key, &fingerprint, None, AbortContext::of(&request))
            .await?
        {
            // A worker replays only its own key's outcome for the discovered target.
            if replay.order_id.is_some_and(|id| id != request.order_id) {
                return Err(EngineError::Integration);
            }
            return Ok(replayed(replay));
        }
        let view = PreparationView {
            facts,
            prepared_draft_revision: None,
            draft_lines: Vec::new(),
            frozen: None,
        };
        let prepared = preparation.prepare(&view).await.map_err(prepare_error)?;
        let attempt = Arc::new(Attempt {
            caller: caller.clone(),
            actor,
            trigger: request.trigger,
            order_id: request.order_id,
            expected_version: i32::from(request.expected_version),
            revisions_client: None,
            prepared_draft_revision: None,
            target: TargetLock::Internal(target.clone()),
            proposed: None,
            disclosure: None,
            key,
            fingerprint,
            principal,
            presented: None,
            caller_reason: request.caller_reason,
            correlation_id: request.correlation_id,
            prepared,
            completed: AtomicBool::new(false),
        });
        let result = self.run(&attempt).await;
        match result {
            Ok(TxOutcome::Settled(outcome)) => Ok(outcome),
            Ok(TxOutcome::Unsettled(reason)) => Err(EngineError::Refused(reason)),
            Ok(TxOutcome::Replay(replay)) => Ok(replayed(replay)),
            Err(abort) => Err(log_abort(
                &abort,
                attempt.completed.load(Ordering::SeqCst),
                AbortContext::of(&*attempt),
            )),
        }
    }

    // -----------------------------------------------------------------------------------------
    // D-188 / D-198 specialized preparation subflows

    /// D-188 "frozen inputs are loaded before any fresh assessment" (E2): what the head of
    /// [`Self::transition`] does for a submit or amendment key (authorization with denial
    /// evidence, the fingerprint over the authorized facts, the read-only registry probe), so a
    /// same-key retry finds its settled outcome or its reserved attempt before the slice assesses
    /// anything.
    ///
    /// - a matching settled record is disclosed after the current-authority recheck and returned
    ///   as [`Recovery::Replay`]; nothing is resolved or written;
    /// - a matching unresolved commercial attempt is resumed through the authoritative gate
    ///   under the aggregate lock ([`Self::prepare_durable`] with a recovery start): a lapsed
    ///   lease is reclaimed with a rotated owner and fence (the only write besides evidence), a
    ///   live one answers the unsettled `still-processing`, and the attempt's frozen inputs are
    ///   returned unchanged as [`Recovery::Existing`]. No candidate is allocated and no attempt
    ///   is created: a key the gate finds without a linked execution rolls back and is
    ///   [`Recovery::Miss`];
    /// - a live record of the key under another fingerprint goes to the same authoritative
    ///   gate, which answers the unsettled `idempotency-mismatch` with its resolved evidence:
    ///   the caller never assesses a request its key already refuses;
    /// - anything else (no record, an expired record) is [`Recovery::Miss`]: the caller assesses
    ///   and reserves, and [`Self::prepare_durable`]'s own recovery stays the race backstop.
    ///
    /// # Errors
    /// As [`Self::prepare_commercial_attempt`]; a trigger other than submit or amendment is an
    /// integration failure.
    pub async fn recover_commercial_attempt(
        &self,
        caller: &Caller,
        request: &TransitionRequest,
    ) -> Result<Recovery, EngineError> {
        if !matches!(request.trigger, Trigger::Submit | Trigger::Amendment) {
            return Err(EngineError::Integration);
        }
        let inner = &self.inner;
        let action = action_for(request.trigger).ok_or(EngineError::Integration)?;
        check_axis_change(request)?;
        let actor = classify(&inner.identities, caller.ctx())?;
        let key = registry_key(caller, request.trigger, &request.idempotency_key)?;
        let prefetch = scoped::prefetch(&inner.db, request.order_id)
            .await
            .map_err(|e| errors::prefetch_failure(request.trigger.token(), &e))?;
        let target = match inner.pep.authorize_target(caller, action, &prefetch).await {
            Ok(target) => target,
            Err(failure) => {
                return Err(self
                    .record_denial(
                        caller,
                        &actor,
                        request.trigger,
                        Some(request.order_id),
                        &key,
                        request,
                        failure,
                    )
                    .await);
            }
        };
        check_actor_rule(
            &inner.registries.table,
            request.trigger,
            decision_class(&inner.identities, caller.ctx()),
        )?;
        let fingerprint = fingerprint(request, target.facts().arrangement())?;
        match self
            .probe(
                &key,
                &fingerprint,
                Some((request.order_id, &target)),
                AbortContext::of(request),
            )
            .await?
        {
            Probe::Replay(replay) => Ok(Recovery::Replay(Box::new(
                self.disclose(
                    caller,
                    ReplayTarget::Order {
                        action,
                        prefetch: &prefetch,
                    },
                    replay,
                )
                .await?,
            ))),
            // A mismatch is answered by the authoritative gate under the lock, with its
            // resolved evidence (`idempotency-mismatch`), before the slice assesses anything.
            Probe::Existing(Frozen::Commercial(_)) | Probe::Mismatch => {
                match self
                    .prepare_durable(caller, request, DurableStart::Recover)
                    .await?
                {
                    Durable::Prepared(PreparedExecution::Execution(execution)) => {
                        Ok(Recovery::Existing(execution))
                    }
                    Durable::Prepared(PreparedExecution::Settled(outcome)) => {
                        Ok(Recovery::Replay(outcome))
                    }
                    Durable::Nothing => Ok(Recovery::Miss),
                }
            }
            // A receiver control under a commercial key is a contract defect.
            Probe::Existing(Frozen::Control(_)) => Err(EngineError::Integration),
            Probe::Miss => Ok(Recovery::Miss),
        }
    }

    /// D-188 step 2: authorize, then durably claim/fence ownership and reserve the candidate in
    /// a short committed transaction before the first Pricing command. Only submit and
    /// amendment use it. A recovered execution is returned with its frozen inputs unchanged.
    ///
    /// `line_requests` builds the canonical per-line Pricing requests from the candidate the
    /// transaction allocates (each request carries it as its order version); it runs once, inside
    /// that transaction, and only for a new execution. Its failure aborts the transaction, so no
    /// candidate, attempt or marker link survives it.
    ///
    /// A new submit execution first rechecks, under the same aggregate lock, that the submit is
    /// admissible from the locked state and that the locked draft revision is both the caller's
    /// `expected_draft_revision` and the attempt's `prepared_draft_revision` (E1): a draft edit
    /// that landed after the slice's read answers the same unsettled refusal the ordinary path
    /// decides (`version-conflict`, `precondition-failed` or `not-admissible`) and the whole
    /// transaction rolls back, so no candidate is allocated and no attempt or claim survives.
    ///
    /// # Errors
    /// Unsettled refusals (authorization, mismatch, still-processing, changed facts, and lost
    /// access as the non-disclosing `order-not-found`, exactly as the ordinary paths; D-114,
    /// D-141) and infrastructure outcomes; a failed `line_requests` is an integration failure. A
    /// settled replay is returned as [`PreparedExecution::Settled`].
    pub async fn prepare_commercial_attempt(
        &self,
        caller: &Caller,
        request: &TransitionRequest,
        inputs: AttemptInputs,
        line_requests: impl FnOnce(i32) -> Result<Value, PrepareError> + Send + 'static,
    ) -> Result<PreparedExecution, EngineError> {
        if !matches!(request.trigger, Trigger::Submit | Trigger::Amendment) {
            return Err(EngineError::Integration);
        }
        self.prepare_durable(
            caller,
            request,
            DurableStart::Commercial(inputs, Box::new(line_requests)),
        )
        .await?
        .prepared()
    }

    /// D-188 step 3's recorder (submit/amendment phase B): authorizes `request`'s target once,
    /// with denial evidence, and returns the handle that records each returned per-line Pricing
    /// result in its own short transaction under the aggregate lock and the attempt's fence
    /// ([`ReceiptRecorder::record`]). Nothing is written here.
    ///
    /// # Errors
    /// Unsettled authorization refusals and outages, as [`Self::transition`]; a trigger other
    /// than submit or amendment is an integration failure.
    pub async fn receipt_recorder(
        &self,
        caller: &Caller,
        request: &TransitionRequest,
    ) -> Result<ReceiptRecorder, EngineError> {
        if !matches!(request.trigger, Trigger::Submit | Trigger::Amendment) {
            return Err(EngineError::Integration);
        }
        let inner = &self.inner;
        let action = action_for(request.trigger).ok_or(EngineError::Integration)?;
        let actor = classify(&inner.identities, caller.ctx())?;
        let key = registry_key(caller, request.trigger, &request.idempotency_key)?;
        let prefetch = scoped::prefetch(&inner.db, request.order_id)
            .await
            .map_err(|e| errors::prefetch_failure(request.trigger.token(), &e))?;
        let target = match inner.pep.authorize_target(caller, action, &prefetch).await {
            Ok(target) => target,
            Err(failure) => {
                return Err(self
                    .record_denial(
                        caller,
                        &actor,
                        request.trigger,
                        Some(request.order_id),
                        &key,
                        request,
                        failure,
                    )
                    .await);
            }
        };
        Ok(ReceiptRecorder {
            engine: self.clone(),
            caller: caller.clone(),
            actor,
            key,
            request: request.clone(),
            target,
        })
    }

    /// D-198 step 1: persist owned receiver-control intent (pending pointer blocks new grants)
    /// before any receiver command. Only post-spawn hold and ordinary terminal operations use
    /// it.
    ///
    /// # Errors
    /// As [`Self::prepare_commercial_attempt`].
    pub async fn prepare_receiver_control(
        &self,
        caller: &Caller,
        request: &TransitionRequest,
        inputs: ControlInputs,
    ) -> Result<PreparedExecution, EngineError> {
        if !matches!(
            request.trigger,
            Trigger::Hold
                | Trigger::Cancel
                | Trigger::CancelWorkflowMediated
                | Trigger::AcknowledgeFailed
        ) {
            return Err(EngineError::Integration);
        }
        self.prepare_durable(caller, request, DurableStart::Control(inputs))
            .await?
            .prepared()
    }

    async fn prepare_durable(
        &self,
        caller: &Caller,
        request: &TransitionRequest,
        start: DurableStart,
    ) -> Result<Durable, EngineError> {
        let inner = &self.inner;
        let action = action_for(request.trigger).ok_or(EngineError::Integration)?;
        check_axis_change(request)?;
        let actor = classify(&inner.identities, caller.ctx())?;
        let key = registry_key(caller, request.trigger, &request.idempotency_key)?;
        let prefetch = scoped::prefetch(&inner.db, request.order_id)
            .await
            .map_err(|e| errors::prefetch_failure(request.trigger.token(), &e))?;
        let target = match inner.pep.authorize_target(caller, action, &prefetch).await {
            Ok(target) => target,
            Err(failure) => {
                return Err(self
                    .record_denial(
                        caller,
                        &actor,
                        request.trigger,
                        Some(request.order_id),
                        &key,
                        request,
                        failure,
                    )
                    .await);
            }
        };
        // The complete proposed arrangement is authorized before any commercial preparation
        // about newly named parties (Foundation step 2).
        if let Some(delta) = request.proposed
            && let Err(failure) = inner
                .pep
                .authorize_proposed(
                    caller,
                    &target,
                    target.facts().arrangement().with_delta(delta),
                )
                .await
        {
            return Err(self
                .record_denial(
                    caller,
                    &actor,
                    request.trigger,
                    Some(request.order_id),
                    &key,
                    request,
                    failure,
                )
                .await);
        }
        let fingerprint = fingerprint(request, target.facts().arrangement())?;
        let principal = PrivateScope::for_principal(key.principal().as_str());
        let lease = inner.lease;
        let presented = request.execution;
        let expected_version = i32::from(request.expected_version);
        let (tgt, k, f, p) = (target.clone(), key.clone(), fingerprint.clone(), principal);
        let (who, trigger, correlation) = (caller.clone(), request.trigger, request.correlation_id);
        let client_revision = request.expected_draft_revision.map(i64::from);
        let order_id = request.order_id;
        let engine = Arc::clone(&self.inner);
        let outcome = inner
            .db
            // The body runs once (`FnOnce`), so the durable start moves in by value.
            .transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                Box::pin(async move {
                    // The same lock-and-recheck mapping as the ordinary paths (`lock`,
                    // `snapshot`): stale facts conflict, lost access stays non-disclosing.
                    let mut locked = match scoped::lock_authorized(tx, &tgt).await? {
                        AuthorizedLock::Locked(locked) => *locked,
                        AuthorizedLock::Stale { current_version } => {
                            return Ok(DurableOutcome::Conflict(transition::stale_refusal(
                                Some(expected_version),
                                current_version,
                                trigger,
                            )));
                        }
                        AuthorizedLock::AccessLost => {
                            return Ok(DurableOutcome::Conflict(Reason::OrderNotFound));
                        }
                    };
                    let request = GateRequest {
                        key: &k,
                        fingerprint: &f,
                        lease,
                        durability: Durability::Durable,
                        presented: presented.as_ref(),
                    };
                    let owned = match reg::resolve(GateTarget::Order(&locked), &p, &request).await?
                    {
                        Gate::Replay(replay) => {
                            return Ok(DurableOutcome::Replay(Box::new(replay)));
                        }
                        gate @ (Gate::Mismatch | Gate::StillProcessing) => {
                            // Resolved evidence on the locked order; the winner is untouched.
                            let reason = if matches!(gate, Gate::Mismatch) {
                                Reason::IdempotencyMismatch
                            } else {
                                Reason::StillProcessing
                            };
                            let t = reg::database_time(tx, &p, &k).await?;
                            let sealed = evidence(&who, actor, &k, correlation, t)?
                                .resolved_refusal(
                                    Uuid::new_v4(),
                                    AuditTrigger::Public(trigger),
                                    &locked.audit_facts()?,
                                    reason,
                                )?;
                            writer::append_resolved_refusal(&locked, sealed).await?;
                            return Ok(DurableOutcome::Refused(reason));
                        }
                        Gate::Owned(owned) => owned,
                    };
                    if let Some((execution, frozen)) = owned.durable()? {
                        return Ok(DurableOutcome::Execution(Box::new((
                            execution,
                            frozen.clone(),
                        ))));
                    }
                    let (execution, frozen) = match start {
                        // E2: a recovery never starts an execution; the fresh claim rolls back.
                        DurableStart::Recover => return Err(Abort::NothingToResume),
                        DurableStart::Commercial(inputs, lines) => {
                            // E1: the draft revision and admissibility under this lock.
                            if trigger == Trigger::Submit {
                                let revisions = DraftRevisions::Draft {
                                    client: client_revision,
                                    prepared: inputs.prepared_draft_revision.ok_or(
                                        Abort::Integration("submit without a draft revision"),
                                    )?,
                                };
                                if let Err(refusal) = transition::admit(
                                    &engine.registries.table,
                                    trigger,
                                    &aggregate_facts(locked.row())?,
                                    expected_version,
                                    revisions,
                                )? {
                                    return Err(Abort::UnreservedRefusal(refusal.reason()));
                                }
                            }
                            // A failed snapshot aborts before the attempt insert; the
                            // rollback releases the candidate and the fresh claim.
                            let (execution, attempt) = reg::begin_commercial_attempt(
                                owned,
                                &mut locked,
                                inputs,
                                |candidate| {
                                    lines(candidate).map_err(|error| {
                                        tracing::error!(
                                            target: "orders.engine",
                                            trigger = trigger.token(),
                                            order_id = %order_id,
                                            correlation_id =
                                                correlation.map(tracing::field::display),
                                            candidate,
                                            error = ?error,
                                            "commercial line requests"
                                        );
                                        Abort::Integration("commercial line requests")
                                    })
                                },
                            )
                            .await?;
                            (execution, Frozen::Commercial(attempt))
                        }
                        DurableStart::Control(inputs) => {
                            let (execution, control) =
                                reg::begin_fulfillment_control(owned, &mut locked, inputs).await?;
                            (execution, Frozen::Control(control))
                        }
                    };
                    Ok::<_, Abort>(DurableOutcome::Execution(Box::new((execution, frozen))))
                })
            })
            .await;
        match outcome {
            Ok(DurableOutcome::Execution(execution)) => {
                Ok(Durable::Prepared(PreparedExecution::Execution(execution)))
            }
            Ok(DurableOutcome::Replay(replay)) => self
                .disclose(
                    caller,
                    ReplayTarget::Order {
                        action,
                        prefetch: &prefetch,
                    },
                    *replay,
                )
                .await
                .map(|outcome| Durable::Prepared(PreparedExecution::Settled(Box::new(outcome)))),
            Ok(DurableOutcome::Refused(reason)) => Err(EngineError::Refused(reason)),
            Ok(DurableOutcome::Conflict(reason)) | Err(Abort::UnreservedRefusal(reason)) => {
                Err(self
                    .record_late_conflict(caller, &actor, request, &key, reason)
                    .await)
            }
            Err(Abort::NothingToResume) => Ok(Durable::Nothing),
            Err(abort) => Err(log_abort(&abort, false, AbortContext::of(request))),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Shared steps

    async fn run(&self, attempt: &Arc<Attempt>) -> Result<TxOutcome, Abort> {
        let inner2 = Arc::clone(&self.inner);
        let attempt2 = Arc::clone(attempt);
        events::transaction(
            &self.inner.db,
            &self.inner.sink,
            transition_tx_config(),
            TX_ATTEMPTS,
            Abort::db_err,
            move |tx, events| {
                let inner = Arc::clone(&inner2);
                let attempt = Arc::clone(&attempt2);
                Box::pin(async move {
                    attempt.completed.store(false, Ordering::SeqCst);
                    let out = Box::pin(existing_body(&inner, &attempt, tx, &events)).await?;
                    attempt.completed.store(true, Ordering::SeqCst);
                    Ok(out)
                })
            },
        )
        .await
    }

    async fn finish(
        &self,
        caller: &Caller,
        target: ReplayTarget<'_>,
        result: Result<TxOutcome, Abort>,
        completed: &AtomicBool,
        context: AbortContext,
    ) -> Result<EngineOutcome, EngineError> {
        match result {
            Ok(TxOutcome::Settled(outcome)) => Ok(outcome),
            Ok(TxOutcome::Unsettled(reason)) => Err(EngineError::Refused(reason)),
            Ok(TxOutcome::Replay(replay)) => self.disclose(caller, target, replay).await,
            Err(abort) => Err(log_abort(&abort, completed.load(Ordering::SeqCst), context)),
        }
    }

    /// Step 17 names a conflicting holder only through the caller's order-read scope (D-179):
    /// the untargeted read scope, i.e. exactly the orders this caller could list. A caller
    /// without read authority is never told which order holds the key, but the transition itself
    /// is unaffected: a read denial is not a refusal of the requested operation. An outage
    /// fails the attempt before any transaction, like every other PDP outage.
    async fn disclosure(
        &self,
        caller: &Caller,
        trigger: Trigger,
    ) -> Result<Option<toolkit_security::AccessScope>, EngineError> {
        if !crate::domain::overlap::ACQUIRING_TRIGGERS.contains(&trigger) {
            return Ok(None);
        }
        match self
            .inner
            .pep
            .authorize_collection(caller, Action::OrderRead)
            .await
        {
            Ok(scope) => Ok(Some(scope.access_scope().clone())),
            Err(AuthzFailure::Refused { .. }) => {
                Ok(Some(toolkit_security::AccessScope::deny_all()))
            }
            Err(AuthzFailure::Unavailable) => Err(EngineError::Unavailable),
            Err(AuthzFailure::Integration) => Err(EngineError::Integration),
        }
    }

    /// Replay only after current disclosure authority is rechecked (Foundation §2.1 step 3).
    async fn disclose(
        &self,
        caller: &Caller,
        target: ReplayTarget<'_>,
        replay: Replay,
    ) -> Result<EngineOutcome, EngineError> {
        let outcome = replay.outcome.clone();
        let order_id = replay.order_id;
        let response = disclose_replay(&self.inner.pep, &self.inner.db, caller, target, replay)
            .await
            .map_err(|f| EngineError::from_authz(&f))?;
        Ok(EngineOutcome {
            kind: OutcomeKind::Replayed { outcome, order_id },
            response,
        })
    }

    /// The read-only registry probe under the key's own principal scope, built here because a
    /// `PrivateScope` is a non-cloneable authority the transaction closure must own.
    async fn probe(
        &self,
        key: &RegistryKey,
        fingerprint: &Fingerprint,
        order: Option<(Uuid, &TargetAuthorization)>,
        context: AbortContext,
    ) -> Result<Probe, EngineError> {
        let (principal, key, fingerprint) = (
            PrivateScope::for_principal(key.principal().as_str()),
            key.clone(),
            fingerprint.clone(),
        );
        let order = order.map(|(id, target)| (id, target.clone()));
        self.inner
            .db
            .transaction_ref_mapped_with_config(TxConfig::read_only(), move |tx| {
                Box::pin(async move {
                    let order = order.as_ref().map(|(id, t)| (*id, t.access_scope()));
                    Ok::<_, Abort>(reg::probe(tx, &principal, &key, &fingerprint, order).await?)
                })
            })
            .await
            .map_err(|abort| log_abort(&abort, false, context))
    }

    /// The preparation view. Draft operations take a coherent snapshot under the aggregate lock
    /// in a short transaction released before any slice call (Foundation step 4, OL-4).
    async fn snapshot(
        &self,
        target: &TargetAuthorization,
        context: AbortContext,
        frozen: Option<Frozen>,
    ) -> Result<Result<PreparationView, Reason>, EngineError> {
        let trigger = context.trigger;
        let target = target.clone();
        let draft = transition::uses_draft_revisions(trigger);
        let config = if draft {
            transition_tx_config()
        } else {
            TxConfig::read_only()
        };
        self.inner
            .db
            .transaction_ref_mapped_with_config(config, move |tx| {
                Box::pin(async move {
                    let row = if draft {
                        match scoped::lock_authorized(tx, &target).await? {
                            AuthorizedLock::Locked(locked) => locked.row().clone(),
                            AuthorizedLock::Stale { current_version } => {
                                return Ok(Err(transition::stale_refusal(
                                    None,
                                    current_version,
                                    trigger,
                                )));
                            }
                            AuthorizedLock::AccessLost => return Ok(Err(Reason::OrderNotFound)),
                        }
                    } else {
                        match scoped::read_authorized(tx, &target).await? {
                            AuthorizedRead::Current(row) => *row,
                            AuthorizedRead::FactsChanged => {
                                return Ok(Err(Reason::AuthorizationContextChanged));
                            }
                            AuthorizedRead::NotVisible => return Ok(Err(Reason::OrderNotFound)),
                        }
                    };
                    let draft_lines = if draft {
                        children::draft_content_for_order(tx, target.access_scope(), row.order_id)
                            .await?
                    } else {
                        Vec::new()
                    };
                    Ok::<_, Abort>(Ok(PreparationView {
                        facts: aggregate_facts(&row)?,
                        prepared_draft_revision: draft.then_some(row.draft_revision),
                        draft_lines,
                        frozen,
                    }))
                })
            })
            .await
            .map_err(|abort| log_abort(&abort, false, context))
    }

    async fn internal_facts(
        &self,
        target: &TargetScope,
        context: AbortContext,
    ) -> Result<Option<AggregateFacts>, EngineError> {
        let target = target.clone();
        self.inner
            .db
            .transaction_ref_mapped_with_config(TxConfig::read_only(), move |tx| {
                Box::pin(async move {
                    let order_id = target
                        .order()
                        .map(crate::infra::maintenance::scope::DiscoveredOrder::order_id)
                        .ok_or(Abort::Integration("worker target"))?;
                    let row = crate::infra::storage::repo::find_order(
                        tx,
                        target.access_scope(),
                        order_id,
                    )
                    .await?;
                    Ok::<_, Abort>(match row {
                        Some(row) => Some(aggregate_facts(&row)?),
                        None => None,
                    })
                })
            })
            .await
            .map_err(|abort| log_abort(&abort, false, context))
    }

    /// Early authorization denial: unresolved refusal evidence under the subject-tenant scope,
    /// no target lookup/lock/enrichment, no registry access (D-98, D-104). Outages and
    /// integration failures write nothing.
    #[expect(
        clippy::too_many_arguments,
        reason = "the denial evidence row needs the caller, actor, trigger, target, key, request \
                  and failure, each recorded as a separate column"
    )]
    async fn record_denial<R: Requested>(
        &self,
        caller: &Caller,
        actor: &AuditActor,
        trigger: Trigger,
        requested: Option<Uuid>,
        key: &RegistryKey,
        request: &R,
        failure: AuthzFailure,
    ) -> EngineError {
        match failure {
            AuthzFailure::Refused { reason, .. } => {
                match self
                    .append_unresolved(
                        caller,
                        actor,
                        trigger,
                        requested,
                        key,
                        request.correlation(),
                        reason,
                    )
                    .await
                {
                    Ok(()) => EngineError::Refused(reason),
                    Err(error) => error,
                }
            }
            AuthzFailure::Unavailable => EngineError::Unavailable,
            AuthzFailure::Integration => EngineError::Integration,
        }
    }

    /// A late authorization-fact conflict, lost access, mismatch or still-processing found
    /// before the attempt's own transaction: audited, never settled.
    async fn record_late_conflict<R: Requested>(
        &self,
        caller: &Caller,
        actor: &AuditActor,
        request: &R,
        key: &RegistryKey,
        reason: Reason,
    ) -> EngineError {
        match self
            .append_unresolved(
                caller,
                actor,
                request.trigger(),
                request.order(),
                key,
                request.correlation(),
                reason,
            )
            .await
        {
            Ok(()) => EngineError::Refused(reason),
            Err(error) => error,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the unresolved-refusal audit row needs the caller, actor, trigger, target, key, \
                  correlation and reason, each recorded as a separate column"
    )]
    async fn append_unresolved(
        &self,
        caller: &Caller,
        actor: &AuditActor,
        trigger: Trigger,
        requested: Option<Uuid>,
        key: &RegistryKey,
        correlation_id: Option<Uuid>,
        reason: Reason,
    ) -> Result<(), EngineError> {
        let (caller, actor, key) = (caller.clone(), *actor, key.clone());
        let refusal_scope = PrivateScope::for_refusal(&caller);
        let principal = PrivateScope::for_principal(key.principal().as_str());
        let completed = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&completed);
        let result = self
            .inner
            .db
            .transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                Box::pin(async move {
                    let t = reg::database_time(tx, &principal, &key).await?;
                    let evidence = evidence(&caller, actor, &key, correlation_id, t)?;
                    let sealed = evidence.unresolved_refusal(
                        Uuid::new_v4(),
                        AuditTrigger::Public(trigger),
                        requested,
                        reason,
                    )?;
                    writer::append_unresolved_refusal(tx, &refusal_scope, sealed).await?;
                    done.store(true, Ordering::SeqCst);
                    Ok::<_, Abort>(())
                })
            })
            .await;
        let context = AbortContext {
            trigger,
            order_id: requested,
            correlation_id,
        };
        result.map_err(|abort| log_abort(&abort, completed.load(Ordering::SeqCst), context))
    }
}

/// Recovered or newly started D-188/D-198 execution, or the key's settled outcome.
#[derive(Debug)]
pub enum PreparedExecution {
    /// The durable execution and its frozen inputs.
    Execution(Box<(DurableExecution, Frozen)>),
    /// The key's already settled outcome.
    Settled(Box<EngineOutcome>),
}

/// The D-188 step-3 receipt recorder of one authorized submit/amendment request
/// ([`Engine::receipt_recorder`]).
pub struct ReceiptRecorder {
    engine: Engine,
    caller: Caller,
    actor: AuditActor,
    key: RegistryKey,
    request: TransitionRequest,
    target: TargetAuthorization,
}
impl std::fmt::Debug for ReceiptRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiptRecorder")
            .field("order_id", &self.request.order_id)
            .finish_non_exhaustive()
    }
}

/// What one receipt record found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptOutcome {
    /// Recorded, or the identical first result was already recorded (a lost-reply retry).
    Recorded(reg::Recorded),
    /// The execution's fence moved (a reclaim by a later retry, or a settled key): this owner
    /// stops and writes nothing more.
    StaleOwner,
}

impl ReceiptRecorder {
    /// Record `result` as `line_key`'s first result on `execution`'s attempt: one short
    /// transaction that locks the authorized aggregate, verifies the exact owner and fence,
    /// appends the result and renews the owner's lease on the marker and the attempt to
    /// `now + lease` (`reg::record_receipt`). A different first result is never rewritten.
    ///
    /// # Errors
    /// Changed authorization facts and lost access as the unsettled refusals of the ordinary
    /// paths (with unresolved evidence); a conflicting first result, or a `line_key` that is not
    /// one of the attempt's frozen lines, is an integration failure; store failures.
    pub async fn record(
        &self,
        execution: &DurableExecution,
        line_key: &str,
        result: Value,
    ) -> Result<ReceiptOutcome, EngineError> {
        let target = self.target.clone();
        let execution = execution.clone();
        let line_key = line_key.to_owned();
        let (expected_version, trigger) = (
            i32::from(self.request.expected_version),
            self.request.trigger,
        );
        let lease = self.engine.inner.lease;
        let outcome = self
            .engine
            .inner
            .db
            .transaction_ref_mapped_with_config(transition_tx_config(), move |tx| {
                Box::pin(async move {
                    let locked = match scoped::lock_authorized(tx, &target).await? {
                        AuthorizedLock::Locked(locked) => *locked,
                        AuthorizedLock::Stale { current_version } => {
                            return Ok(Err(transition::stale_refusal(
                                Some(expected_version),
                                current_version,
                                trigger,
                            )));
                        }
                        AuthorizedLock::AccessLost => return Ok(Err(Reason::OrderNotFound)),
                    };
                    Ok::<_, Abort>(Ok(
                        match reg::record_receipt(&locked, &execution, &line_key, result, lease)
                            .await
                        {
                            Ok(recorded) => ReceiptOutcome::Recorded(recorded),
                            Err(reg::RegistryError::StaleOwner) => ReceiptOutcome::StaleOwner,
                            Err(reg::RegistryError::FirstResultConflict) => {
                                return Err(Abort::Integration(
                                    "a different first receipt result is already recorded",
                                ));
                            }
                            Err(reg::RegistryError::UnfrozenLine) => {
                                return Err(Abort::Integration(
                                    "a receipt for a line the attempt did not freeze",
                                ));
                            }
                            Err(error) => return Err(error.into()),
                        },
                    ))
                })
            })
            .await;
        match outcome {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(reason)) => Err(self
                .engine
                .record_late_conflict(&self.caller, &self.actor, &self.request, &self.key, reason)
                .await),
            Err(abort) => Err(log_abort(&abort, false, AbortContext::of(&self.request))),
        }
    }
}

/// What [`Engine::recover_commercial_attempt`] found for a submit or amendment key (E2).
#[derive(Debug)]
pub enum Recovery {
    /// The key's settled outcome, disclosed after the current-authority recheck; boxed like
    /// `Existing`, so the enum stays small.
    Replay(Box<EngineOutcome>),
    /// The key's unresolved commercial attempt under a live fence, with its frozen inputs.
    Existing(Box<(DurableExecution, Frozen)>),
    /// No settled outcome and no attempt: the slice assesses and reserves.
    Miss,
}

/// [`Engine::prepare_durable`]'s answer: the execution or settled replay, or nothing to resume.
enum Durable {
    Prepared(PreparedExecution),
    /// A recovery start found no linked execution (only [`DurableStart::Recover`]).
    Nothing,
}
impl Durable {
    /// A starting preparation always yields an execution or a replay.
    fn prepared(self) -> Result<PreparedExecution, EngineError> {
        match self {
            Self::Prepared(prepared) => Ok(prepared),
            Self::Nothing => Err(EngineError::Integration),
        }
    }
}

/// Builds a commercial attempt's canonical line requests from its allocated candidate.
type LineRequests = Box<dyn FnOnce(i32) -> Result<Value, PrepareError> + Send>;

enum DurableStart {
    Commercial(AttemptInputs, LineRequests),
    Control(ControlInputs),
    /// Resume an existing execution only (E2); a key without one rolls back.
    Recover,
}
enum DurableOutcome {
    Execution(Box<(DurableExecution, Frozen)>),
    Replay(Box<Replay>),
    Refused(Reason),
    /// A late fact conflict (`version-conflict`/`authorization-context-changed`) or lost access
    /// (`order-not-found`) at the lock: unresolved evidence only, never a settlement.
    Conflict(Reason),
}

/// The request facts the evidence writers need from either request type.
trait Requested {
    fn trigger(&self) -> Trigger;
    fn order(&self) -> Option<Uuid>;
    fn correlation(&self) -> Option<Uuid>;
}
impl Requested for TransitionRequest {
    fn trigger(&self) -> Trigger {
        self.trigger
    }
    fn order(&self) -> Option<Uuid> {
        Some(self.order_id)
    }
    fn correlation(&self) -> Option<Uuid> {
        self.correlation_id
    }
}
impl Requested for Attempt {
    fn trigger(&self) -> Trigger {
        self.trigger
    }
    fn order(&self) -> Option<Uuid> {
        Some(self.order_id)
    }
    fn correlation(&self) -> Option<Uuid> {
        self.correlation_id
    }
}
impl Requested for CreateRequest {
    fn trigger(&self) -> Trigger {
        Trigger::Create
    }
    fn order(&self) -> Option<Uuid> {
        None
    }
    fn correlation(&self) -> Option<Uuid> {
        self.correlation_id
    }
}

impl EngineError {
    fn from_authz(failure: &AuthzFailure) -> Self {
        match failure {
            AuthzFailure::Refused { reason, .. } => Self::Refused(*reason),
            AuthzFailure::Unavailable => Self::Unavailable,
            AuthzFailure::Integration => Self::Integration,
        }
    }
}

/// The request identifiers the central abort log carries. Never the idempotency key, the
/// principal scope, the proof reference or a body.
#[derive(Debug, Clone, Copy)]
struct AbortContext {
    trigger: Trigger,
    order_id: Option<Uuid>,
    correlation_id: Option<Uuid>,
}
impl AbortContext {
    fn of(request: &impl Requested) -> Self {
        Self {
            trigger: request.trigger(),
            order_id: request.order(),
            correlation_id: request.correlation(),
        }
    }
}

/// The single log line of every aborted attempt: the identifiers as fields, the abort cause as
/// the `error` field and a fixed message.
fn log_abort(abort: &Abort, body_completed: bool, context: AbortContext) -> EngineError {
    let error = errors::terminate(abort, body_completed);
    if let EngineError::NotDelivered(capability) = error {
        errors::log_not_delivered(capability);
        return error;
    }
    tracing::error!(
        target: "orders.engine",
        trigger = context.trigger.token(),
        order_id = context.order_id.map(tracing::field::display),
        correlation_id = context.correlation_id.map(tracing::field::display),
        body_completed,
        outcome = ?error,
        error = %abort,
        "transition attempt aborted"
    );
    error
}

fn prepare_error(error: PrepareError) -> EngineError {
    match error {
        PrepareError::Unavailable => EngineError::Unavailable,
        PrepareError::Integration => EngineError::Integration,
    }
}

fn replayed(replay: Replay) -> EngineOutcome {
    EngineOutcome {
        kind: OutcomeKind::Replayed {
            outcome: replay.outcome,
            order_id: replay.order_id,
        },
        response: replay.response,
    }
}

/// The grantable action for a caller-entered trigger. Expiry triggers have none: only the
/// private worker capability enters them.
#[must_use]
pub fn action_for(trigger: Trigger) -> Option<Action> {
    Some(match trigger {
        Trigger::Create => Action::OrderCreate,
        Trigger::DraftMutate => Action::OrderWrite,
        Trigger::AdministrativeEdit => Action::OrderEdit,
        Trigger::Submit => Action::OrderSubmit,
        Trigger::Cancel => Action::OrderCancel,
        Trigger::ReflectApprovalRequired
        | Trigger::ReflectApprovalNotRequired
        | Trigger::ReflectApprovalGranted
        | Trigger::ReflectApprovalDenied => Action::OrderApprovalReflection,
        Trigger::BeginFulfillment => Action::OrderBeginFulfillment,
        Trigger::ReportSpawnSignal => Action::OrderSpawnSignal,
        Trigger::AcknowledgeCompleted | Trigger::AcknowledgeFailed => {
            Action::OrderFulfillmentAcknowledgement
        }
        Trigger::CancelWorkflowMediated => Action::OrderWorkflowCancel,
        Trigger::Amendment => Action::OrderAmend,
        Trigger::Hold => Action::OrderHold,
        Trigger::Resume => Action::OrderResume,
        Trigger::RecordAcceptance => Action::AcceptanceRecord,
        Trigger::ForceFailUnreconciled => Action::OrderForceFailUnreconciled,
        Trigger::Expire | Trigger::AutoVoid => return None,
    })
}

fn classify(
    identities: &ActorIdentities,
    ctx: &SecurityContext,
) -> Result<AuditActor, EngineError> {
    identities
        .classify(ctx)
        .map_err(|_| EngineError::Integration)
}

fn registry_key(
    caller: &Caller,
    trigger: Trigger,
    key: &IdempotencyKey,
) -> Result<RegistryKey, EngineError> {
    let principal =
        PrincipalScope::from_context(caller.ctx()).map_err(|_| EngineError::Integration)?;
    Ok(RegistryKey::new(
        RegistryOperation::Trigger(trigger),
        principal,
        key.clone(),
    ))
}

/// The backstop for the declared row actor rule, on the same [`decision_class`] the PEP decides
/// with (D-226). The `System` rule never goes through the PEP, so only this check enforces it.
fn check_actor_rule(
    table: &StateTable,
    trigger: Trigger,
    class: AuditActorClass,
) -> Result<(), EngineError> {
    let declared = table
        .rows()
        .iter()
        .filter(|r| r.trigger == trigger)
        .map(|r| r.actor)
        .find(|a| *a != ActorRule::Authorized);
    let ok = match declared {
        None | Some(ActorRule::Authorized) => true,
        Some(ActorRule::System) => class == AuditActorClass::System,
        Some(ActorRule::User) => class == AuditActorClass::User,
    };
    if ok {
        Ok(())
    } else {
        // A caller the PEP admitted cannot reach this on a user-only row (same class, D-226);
        // a system-only row reached by a non-system actor is an integration fault, logged with
        // the rule and class, never the principal (D-182).
        tracing::error!(
            target: "orders.engine.integration",
            trigger = %trigger.token(),
            declared_rule = ?declared,
            actor_class = class.token(),
            "the caller's configured actor class does not satisfy the row's declared actor rule"
        );
        Err(EngineError::Integration)
    }
}

fn axes(a: Arrangement) -> FingerprintAxes {
    FingerprintAxes {
        seller_tenant_id: a.seller_tenant_id,
        resource_tenant_id: a.resource_tenant_id,
        payer_tenant_id: a.payer_tenant_id,
    }
}

/// Only `draft-mutate` (resource/payer) and `amendment` (payer) carry a proposed arrangement;
/// any other proposal is refused before authorization, a read or a write.
fn check_axis_change(request: &TransitionRequest) -> Result<(), EngineError> {
    match request.proposed {
        Some(delta)
            if !transition::axis_change_admitted(
                request.trigger,
                delta.resource_tenant_id.is_some(),
                delta.payer_tenant_id.is_some(),
            ) =>
        {
            tracing::error!(
                target: "orders.engine.integration",
                trigger = %request.trigger.token(),
                "a proposed tenant-axis change on a trigger that cannot change it"
            );
            Err(EngineError::Integration)
        }
        _ => Ok(()),
    }
}

/// The request fingerprint (Foundation §4.2). The tenant axes "in force" for a request that
/// proposes a resource/payer change are the arrangement it establishes: the current facts
/// completed with its delta, exactly as authorized. Hashing the pre-change axes instead would turn
/// the same-key retry of a committed axis edit into `idempotency-mismatch`, since the retry reads
/// the arrangement that edit wrote.
fn fingerprint(
    request: &TransitionRequest,
    current: Arrangement,
) -> Result<Fingerprint, EngineError> {
    let in_force = request
        .proposed
        .map_or(current, |delta| current.with_delta(delta));
    FingerprintInput {
        operation: request.operation,
        trigger: request.trigger,
        target: FingerprintTarget::Order {
            order_id: request.order_id,
            expected_version: i32::from(request.expected_version),
        },
        axes: axes(in_force),
        draft_revision: match (
            transition::uses_draft_revisions(request.trigger),
            request.expected_draft_revision.map(i64::from),
        ) {
            (true, Some(n)) => DraftRevisionInput::Expected(n),
            _ => DraftRevisionInput::NotApplicable,
        },
        contribution: &request.document,
    }
    .fingerprint()
    .map_err(|e| fingerprint_failure(request.operation, request.trigger, e))
}

/// A fingerprint rule violation is a slice/engine contract defect (an unregistered operation, a
/// mismatched target or a non-canonical document): fail closed, with the rule in the log.
fn fingerprint_failure(operation: &str, trigger: Trigger, error: RegistryRuleError) -> EngineError {
    tracing::error!(
        target: "orders.engine.integration",
        operation,
        trigger = %trigger.token(),
        error = %error,
        "request fingerprint refused by the registry rules"
    );
    EngineError::Integration
}

fn evidence(
    caller: &Caller,
    actor: AuditActor,
    key: &RegistryKey,
    correlation_id: Option<Uuid>,
    t: OffsetDateTime,
) -> Result<AttemptEvidence, Abort> {
    Ok(AttemptEvidence::new(
        actor,
        caller.supplied_proof(),
        key.key(),
        correlation_id,
        t,
    )?)
}

fn parse_state(token: &str) -> Result<OrderState, Abort> {
    Ok(crate::domain::audit::parse_state(token)?)
}

/// Domain facts of a locked/read aggregate row.
fn aggregate_facts(row: &order::Model) -> Result<AggregateFacts, Abort> {
    Ok(AggregateFacts {
        order_id: row.order_id,
        state: parse_state(&row.state)?,
        state_entered_at: row.state_entered_at,
        current_version: row.current_version,
        version_allocation_high_water: row.version_allocation_high_water,
        draft_revision: row.draft_revision,
        pre_hold_state: row.pre_hold_state.as_deref().map(parse_state).transpose()?,
        resume_count: row.resume_count,
        amendment_count: row.amendment_count,
        fulfillment_control_generation: row.fulfillment_control_generation,
        fulfillment_control_pending: row.fulfillment_control_pending,
        spawn_signal_at: row.spawn_signal_at,
        authorization_failure_tolerated_at: row.authorization_failure_tolerated_at,
        compensation_evidence: row.compensation_evidence.clone(),
        resource_tenant_id: row.resource_tenant_id,
        seller_tenant_id: row.seller_tenant_id,
        payer_tenant_id: row.payer_tenant_id,
        category: row.category.clone(),
        contract_id: row.contract_id,
    })
}

/// The proposed creation as facts for the create preparation view (no order exists yet).
fn creation_facts(request: &CreateRequest, order_id: Uuid) -> AggregateFacts {
    AggregateFacts {
        order_id,
        state: OrderState::Draft,
        state_entered_at: OffsetDateTime::UNIX_EPOCH,
        current_version: 1,
        version_allocation_high_water: 1,
        draft_revision: 0,
        pre_hold_state: None,
        resume_count: 0,
        amendment_count: 0,
        fulfillment_control_generation: 0,
        fulfillment_control_pending: None,
        spawn_signal_at: None,
        authorization_failure_tolerated_at: None,
        compensation_evidence: None,
        resource_tenant_id: request.arrangement.resource_tenant_id,
        seller_tenant_id: request.arrangement.seller_tenant_id,
        payer_tenant_id: request.arrangement.payer_tenant_id,
        category: request.category.clone(),
        contract_id: request.contract_id,
    }
}

/// The aggregate row carrying the planned post-state; untouched columns keep their values,
/// so column-level immutable grants are preserved by the conditional update.
fn post_model(row: &order::Model, after: &AggregateFacts) -> order::Model {
    let mut next = row.clone();
    after.state.token().clone_into(&mut next.state);
    next.state_entered_at = after.state_entered_at;
    next.current_version = after.current_version;
    next.draft_revision = after.draft_revision;
    next.pre_hold_state = after.pre_hold_state.map(|s| s.token().to_owned());
    next.resume_count = after.resume_count;
    next.amendment_count = after.amendment_count;
    next.fulfillment_control_generation = after.fulfillment_control_generation;
    next.spawn_signal_at = after.spawn_signal_at;
    next.authorization_failure_tolerated_at = after.authorization_failure_tolerated_at;
    next.compensation_evidence
        .clone_from(&after.compensation_evidence);
    next.payer_tenant_id = after.payer_tenant_id;
    next.resource_tenant_id = after.resource_tenant_id;
    next.category.clone_from(&after.category);
    next.contract_id = after.contract_id;
    next
}

fn version_token(version: i32) -> Result<OrderVersion, Abort> {
    OrderVersion::try_from(i64::from(version)).map_err(|_| Abort::Integration("version"))
}

fn json_value<T: serde::Serialize>(value: &T) -> Result<Value, Abort> {
    serde_json::to_value(value).map_err(|_| Abort::Integration("response serialization"))
}

/// The settled success snapshot: the committed post-state, `ETag` = returned version.
fn success_response(
    trigger: Trigger,
    after: &AggregateFacts,
    line_id: Option<Uuid>,
    assessment: Option<Value>,
    assessment_id: Option<Uuid>,
) -> Result<StoredResponse, Abort> {
    let body = TransitionResult {
        order_id: after.order_id,
        state: after.state,
        version: version_token(after.current_version)?,
        draft_revision: transition::uses_draft_revisions(trigger)
            .then(|| DraftRevision::try_from(after.draft_revision))
            .transpose()
            .map_err(|_| Abort::Integration("draft revision"))?,
        request_audit_id: None,
        line_id,
        assessment_id,
    };
    let headers = BTreeMap::from([("etag".to_owned(), format!("\"{}\"", after.current_version))]);
    Ok(StoredResponse::new(
        200,
        json_value(&body)?,
        headers,
        assessment,
    )?)
}

/// The settled refusal snapshot: the canonical Problem with only permitted context data, built
/// by the REST error gear (D-219) and stored with its registered wire status.
fn refusal_response(
    reason: Reason,
    data: Value,
    assessment: Option<Value>,
) -> Result<StoredResponse, Abort> {
    let problem = crate::api::rest::error::settled_refusal_problem(reason, data);
    Ok(StoredResponse::new(
        crate::api::rest::wire::reason_status(reason),
        json_value(&problem)?,
        BTreeMap::new(),
        assessment,
    )?)
}

#[cfg_attr(
    not(test),
    expect(
        clippy::unused_async,
        reason = "the only await is the cfg(test) fault hook; production keeps the async \
                  signature so every call site is identical in both builds"
    )
)]
async fn fault(inner: &Inner, point: FaultPoint, tx: &DbTx<'_>) -> Result<(), Abort> {
    #[cfg(test)]
    if let Some(hook) = &inner.fault {
        return hook.at(point, tx).await;
    }
    let _ = (inner, point, tx);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Transaction bodies

struct CreateAttempt {
    caller: Caller,
    actor: AuditActor,
    request: CreateRequest,
    authorized: crate::authz::ProposedAuthorization,
    key: RegistryKey,
    fingerprint: Fingerprint,
    principal: PrivateScope,
    prepared: Prepared,
    completed: AtomicBool,
}

/// Create Transition steps 4-7 in one transaction.
async fn create_body<'a>(
    inner: &Inner,
    a: &CreateAttempt,
    tx: &'a DbTx<'a>,
) -> Result<TxOutcome, Abort> {
    let request = GateRequest {
        key: &a.key,
        fingerprint: &a.fingerprint,
        lease: inner.lease,
        durability: Durability::Ordinary,
        presented: None,
    };
    let refusal_scope = PrivateScope::for_refusal(&a.caller);
    let unresolved = |reason: Reason, t: OffsetDateTime| -> Result<_, Abort> {
        let evidence = evidence(&a.caller, a.actor, &a.key, a.request.correlation_id, t)?;
        Ok(evidence.unresolved_refusal(
            Uuid::new_v4(),
            AuditTrigger::Public(Trigger::Create),
            None,
            reason,
        )?)
    };
    let owned = match reg::resolve(GateTarget::Create(tx), &a.principal, &request).await? {
        Gate::Replay(replay) => return Ok(TxOutcome::Replay(replay)),
        gate @ (Gate::Mismatch | Gate::StillProcessing) => {
            let reason = if matches!(gate, Gate::Mismatch) {
                Reason::IdempotencyMismatch
            } else {
                Reason::StillProcessing
            };
            let t = reg::database_time(tx, &a.principal, &a.key).await?;
            writer::append_unresolved_refusal(tx, &refusal_scope, unresolved(reason, t)?).await?;
            fault(inner, FaultPoint::Audit, tx).await?;
            return Ok(TxOutcome::Unsettled(reason));
        }
        Gate::Owned(owned) => owned,
    };
    fault(inner, FaultPoint::Gate, tx).await?;
    let t = owned.claimed_at();
    let registries = &inner.registries;
    let facts = creation_facts(&a.request, Uuid::nil());
    let decision = transition::decide_create(
        &registries.table,
        &registries.guards,
        &facts,
        &a.prepared.guards,
        &a.prepared.inputs,
        t,
    )?;
    if let Some(transition::CreateRefusal { reason, assessment }) = decision {
        // No aggregate, version or placeholder: unresolved evidence, settled with NULL order.
        let sealed =
            writer::append_unresolved_refusal(tx, &refusal_scope, unresolved(reason, t)?).await?;
        fault(inner, FaultPoint::Audit, tx).await?;
        let audit_id = sealed.row().audit_id;
        let response = refusal_response(
            reason,
            json!({}),
            assessment.then(|| a.prepared.assessment.clone()).flatten(),
        )?;
        owned
            .settle(Settlement::Refused {
                reason,
                audit_id,
                response: response.clone(),
            })
            .await?;
        fault(inner, FaultPoint::Settle, tx).await?;
        return Ok(TxOutcome::Settled(EngineOutcome {
            kind: OutcomeKind::Refused { reason, audit_id },
            response,
        }));
    }
    Box::pin(create_aggregate(inner, a, tx, owned)).await
}

/// Create steps 6-7 after ownership and guard acceptance: identity, number, aggregate, empty
/// version 1, child documents, committed create audit and settlement. Event-less.
async fn create_aggregate<'a>(
    inner: &Inner,
    a: &CreateAttempt,
    tx: &'a DbTx<'a>,
    owned: Owned<'a, DbTx<'a>>,
) -> Result<TxOutcome, Abort> {
    let t = owned.claimed_at();
    let execution_id = owned.record().execution_id;
    let order_id = Uuid::new_v4();
    let number = order_number();
    let arrangement = a.request.arrangement;
    let initial = order::Model {
        order_id,
        order_number: number.clone(),
        category: a.request.category.clone(),
        resource_tenant_id: arrangement.resource_tenant_id,
        audit_tenant_id: arrangement.resource_tenant_id,
        payer_tenant_id: arrangement.payer_tenant_id,
        seller_tenant_id: arrangement.seller_tenant_id,
        initiating_actor: a.actor.reference(),
        // D-106/D-140: partner_placed iff the allowed create carried a proof reference.
        sales_path: if a.caller.supplied_proof().is_some() {
            SalesPath::PartnerPlaced
        } else {
            SalesPath::SelfService
        }
        .token()
        .to_owned(),
        contract_id: a.request.contract_id,
        state: OrderState::Draft.token().to_owned(),
        state_entered_at: t,
        current_version: 1,
        version_allocation_high_water: 1,
        draft_revision: 0,
        pre_hold_state: None,
        resume_count: 0,
        amendment_count: 0,
        fulfillment_control_generation: 0,
        fulfillment_control_pending: None,
        spawn_signal_at: None,
        authorization_failure_tolerated_at: None,
        compensation_evidence: None,
        audit_sequence: 0,
        created_at: t,
    };
    let inserted = scoped::insert_authorized_order(tx, &a.authorized, initial).await?;
    let mut locked = LockedOrder::acquire(tx, a.authorized.access_scope(), &inserted).await?;
    locked
        .insert_order_version(order_version::Model {
            order_id,
            version: 1,
            supersedes_version: None,
            market_currency: None,
            market_region: None,
            payer_tenant_id: arrangement.payer_tenant_id,
            category: a.request.category.clone(),
            contract_id: a.request.contract_id,
            actor: a.actor.reference(),
            actor_tenant_id: a.actor.subject_tenant_id(),
            reason: Trigger::Create.token().to_owned(),
            amendment_reason: None,
            created_at: t,
        })
        .await?;
    fault(inner, FaultPoint::Version, tx).await?;
    let audit_id = Uuid::new_v4();
    if let Some(documents) = &a.prepared.documents {
        documents
            .write(&ChildDocuments::new(&locked, t, audit_id, execution_id))
            .await?;
    }
    fault(inner, FaultPoint::Documents, tx).await?;
    let evidence = evidence(&a.caller, a.actor, &a.key, a.request.correlation_id, t)?;
    let pending = evidence.committed_create(audit_id, &locked.audit_facts()?)?;
    writer::append_committed(&mut locked, pending).await?;
    fault(inner, FaultPoint::Audit, tx).await?;
    // Create is event-less (row 1); an event detail here is a contract defect.
    if a.prepared.event.is_some() {
        return Err(Abort::Integration("create is event-less"));
    }
    let body = json_value(&created_view(&inserted)?)?;
    // `Location` is not stored: only the REST adapter knows the public path (a gateway prefix
    // included), and it adds the header to the first response and every replay alike.
    let headers = BTreeMap::from([("etag".to_owned(), "\"1\"".to_owned())]);
    let response = StoredResponse::new(201, body, headers, None)?;
    owned
        .settle(Settlement::Success {
            order_id: Some(order_id),
            audit_id,
            response: response.clone(),
        })
        .await?;
    fault(inner, FaultPoint::Settle, tx).await?;
    Ok(TxOutcome::Settled(EngineOutcome {
        kind: OutcomeKind::Committed { order_id, audit_id },
        response,
    }))
}

/// The 201 body of a create (`models.json` `OrderView`): the committed empty draft at version 1
/// with draft revision 0 and no lines, pin or total (02 §2.1 step 4; D-206 `snake_case`).
fn created_view(row: &order::Model) -> Result<OrderView, Abort> {
    let category = Category::parse(&row.category).ok_or(Abort::Integration("category"))?;
    Ok(OrderView {
        resolved_totals: None,
        order: OrderHeader {
            order_id: row.order_id,
            order_number: row.order_number.clone(),
            state: parse_state(&row.state)?,
            category,
            resource_tenant_id: row.resource_tenant_id,
            seller_tenant_id: row.seller_tenant_id,
            payer_tenant_id: row.payer_tenant_id,
            contract_id: row.contract_id,
            sales_path: SalesPath::from_token(&row.sales_path)
                .ok_or(Abort::Integration("sales path"))?,
            created_at: row.created_at,
            state_entered_at: row.state_entered_at,
        },
        version: VersionSummary {
            version: version_token(row.current_version)?,
            supersedes_version: None,
            created_at: row.created_at,
        },
        lines: Vec::new(),
        draft_revision: Some(
            DraftRevision::try_from(row.draft_revision)
                .map_err(|_| Abort::Integration("draft revision"))?,
        ),
    })
}

/// Human-readable, seller-unique order number. Uniqueness, not gaplessness, is promised; a
/// collision aborts through the `(seller_tenant_id, order_number)` unique index.
fn order_number() -> String {
    let id = Uuid::new_v4();
    let (high, _) = id.as_u64_pair();
    format!("ORD-{high:016X}")
}

/// Lock the attempt's aggregate: PEP-authorized (with fact recheck) or worker-discovered.
async fn lock<'a>(
    attempt: &Attempt,
    tx: &'a DbTx<'a>,
) -> Result<Result<LockedOrder<'a, DbTx<'a>>, Reason>, Abort> {
    match &attempt.target {
        TargetLock::Authorized(target) => Ok(match scoped::lock_authorized(tx, target).await? {
            AuthorizedLock::Locked(locked) => Ok(*locked),
            AuthorizedLock::Stale { current_version } => Err(transition::stale_refusal(
                Some(attempt.expected_version),
                current_version,
                attempt.trigger,
            )),
            AuthorizedLock::AccessLost => Err(Reason::OrderNotFound),
        }),
        TargetLock::Internal(target) => {
            match LockedOrder::lock_current(tx, target.access_scope(), attempt.order_id).await? {
                Some(locked) => Ok(Ok(locked)),
                // The discovered tenant properties changed: a worker never rebinds axes.
                None => Ok(Err(Reason::AuthorizationContextChanged)),
            }
        }
    }
}

/// Attempt Transition steps 5-27 in one transaction.
async fn existing_body<'a>(
    inner: &Inner,
    a: &Attempt,
    tx: &'a DbTx<'a>,
    events: &TxEvents,
) -> Result<TxOutcome, Abort> {
    let mut locked = match lock(a, tx).await? {
        Ok(locked) => locked,
        Err(reason) => {
            // Late fact conflict / lost access: evidence only, never a settlement (§3.6).
            let t = reg::database_time(tx, &a.principal, &a.key).await?;
            let sealed = evidence(&a.caller, a.actor, &a.key, a.correlation_id, t)?
                .unresolved_refusal(
                    Uuid::new_v4(),
                    AuditTrigger::Public(a.trigger),
                    Some(a.order_id),
                    reason,
                )?;
            writer::append_unresolved_refusal(tx, &PrivateScope::for_refusal(&a.caller), sealed)
                .await?;
            fault(inner, FaultPoint::Audit, tx).await?;
            return Ok(TxOutcome::Unsettled(reason));
        }
    };
    let request = GateRequest {
        key: &a.key,
        fingerprint: &a.fingerprint,
        lease: inner.lease,
        durability: if a.presented.is_some() {
            Durability::Durable
        } else {
            Durability::Ordinary
        },
        presented: a.presented.as_ref(),
    };
    let owned = match reg::resolve(GateTarget::Order(&locked), &a.principal, &request).await? {
        Gate::Replay(replay) => return Ok(TxOutcome::Replay(replay)),
        Gate::Mismatch => {
            return refuse_unsettled(inner, a, &locked, Reason::IdempotencyMismatch).await;
        }
        Gate::StillProcessing => {
            return refuse_unsettled(inner, a, &locked, Reason::StillProcessing).await;
        }
        Gate::Owned(owned) => owned,
    };
    fault(inner, FaultPoint::Gate, tx).await?;
    let t = owned.claimed_at();
    let facts = aggregate_facts(locked.row())?;
    let durable = owned.durable()?.map(|(e, f)| (e, f.clone()));
    let revisions = if transition::uses_draft_revisions(a.trigger) {
        transition::DraftRevisions::Draft {
            client: a.revisions_client,
            prepared: a
                .prepared_draft_revision
                .ok_or(Abort::Integration("draft snapshot"))?,
        }
    } else {
        DraftRevisions::NotApplicable
    };
    // Row 3: the change guard and the per-field audit compare the named values with those
    // stored at this locked read, never with the preparation snapshot (04 §3.6 step 2).
    let administrative = if a.trigger == Trigger::AdministrativeEdit {
        locked_administrative(&locked, &a.prepared.admin_changes).await?
    } else {
        Vec::new()
    };
    let registries = &inner.registries;
    let decision = transition::decide_with(
        &registries.table,
        &registries.guards,
        a.trigger,
        &facts,
        a.expected_version,
        revisions,
        &a.prepared.guards,
        &a.prepared.inputs,
        t,
        &administrative,
    )?;
    let execution_id = owned.record().execution_id;
    let settle = Settle {
        owned,
        durable,
        execution_id,
    };
    let row = match decision {
        Decision::Admitted(row) => row.clone(),
        Decision::Engine(refusal) => {
            let data = match refusal {
                transition::EngineRefusal::NotAdmissible { state, trigger } => json!({
                    "state": state.token(),
                    "trigger": trigger.token(),
                }),
                transition::EngineRefusal::VersionConflict {
                    current_version,
                    draft_revision,
                }
                | transition::EngineRefusal::PreconditionFailed {
                    current_version,
                    draft_revision,
                } => match draft_revision {
                    Some(revision) => {
                        json!({"current_version": current_version, "draft_revision": revision})
                    }
                    None => json!({"current_version": current_version}),
                },
                transition::EngineRefusal::ResumeTargetMissing => json!({}),
            };
            let refusal = Refusal {
                overlap_conflict: None,
                reason: refusal.reason(),
                data,
                reach: GateReach::NotReached,
            };
            return Box::pin(refuse_settled(inner, a, &mut locked, settle, t, refusal)).await;
        }
        Decision::Guard {
            reason, assessment, ..
        }
        | Decision::Unevaluable {
            reason, assessment, ..
        } => {
            // D-229: a composite-gate refusal (the only guard whose `assessment` is set) carries
            // the authorized gate report in `context.data`; every other guard carries none.
            let refusal = Refusal {
                overlap_conflict: None,
                reason,
                data: json!({}),
                reach: if assessment {
                    GateReach::Reported
                } else {
                    GateReach::NotReached
                },
            };
            return Box::pin(refuse_settled(inner, a, &mut locked, settle, t, refusal)).await;
        }
    };
    Box::pin(admit(
        inner,
        a,
        &mut locked,
        settle,
        &row,
        &facts,
        t,
        events,
        &administrative,
    ))
    .await
}

/// Registry ownership and, for D-188/D-198, the recovered durable execution.
struct Settle<'a> {
    owned: Owned<'a, DbTx<'a>>,
    durable: Option<(DurableExecution, Frozen)>,
    execution_id: Uuid,
}
impl<'a> Settle<'a> {
    async fn settle(
        self,
        locked: &mut LockedOrder<'a, DbTx<'a>>,
        settlement: Settlement,
    ) -> Result<(), Abort> {
        match self.durable {
            Some((execution, _)) => {
                let terminal = if matches!(settlement, Settlement::Success { .. }) {
                    ExecutionTerminal::Completed
                } else {
                    ExecutionTerminal::Refused
                };
                reg::finish_execution(locked, &execution, terminal, settlement).await?;
            }
            None => {
                self.owned.settle(settlement).await?;
            }
        }
        Ok(())
    }
}

/// Mismatch / still-processing: resolved evidence on the locked order, no settlement.
async fn refuse_unsettled<'a>(
    inner: &Inner,
    a: &Attempt,
    locked: &LockedOrder<'a, DbTx<'a>>,
    reason: Reason,
) -> Result<TxOutcome, Abort> {
    let tx = locked.transaction();
    let t = reg::database_time(tx, &a.principal, &a.key).await?;
    let sealed = evidence(&a.caller, a.actor, &a.key, a.correlation_id, t)?.resolved_refusal(
        Uuid::new_v4(),
        AuditTrigger::Public(a.trigger),
        &locked.audit_facts()?,
        reason,
    )?;
    writer::append_resolved_refusal(locked, sealed).await?;
    fault(inner, FaultPoint::Audit, tx).await?;
    Ok(TxOutcome::Unsettled(reason))
}

/// How far a refused attempt got through the composite gate (M2-2, D-229).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateReach {
    /// An engine-only or non-gate guard refusal: no diagnostics, no report.
    NotReached,
    /// The gate assessment was reached but did not refuse (the step-17 overlap conflict): the
    /// run's diagnostics are written and the snapshot keeps the report, but
    /// `context.data.assessment` stays absent.
    Assessed,
    /// The composite gate itself refused: diagnostics are written and `context.data.assessment`
    /// carries the authorized gate report (D-229 item 2).
    Reported,
}

/// A business refusal: its registered reason, permitted Problem context, and how far it got
/// through the composite gate (D-229).
struct Refusal {
    overlap_conflict: Option<crate::domain::overlap::OverlapConflict>,
    reason: Reason,
    data: Value,
    reach: GateReach,
}

/// An owned business refusal: resolved audit (with the D-201 force-request observation where
/// it applies), settled with that audit, committed. No business write precedes it.
async fn refuse_settled<'a>(
    inner: &Inner,
    a: &Attempt,
    locked: &mut LockedOrder<'a, DbTx<'a>>,
    settle: Settle<'a>,
    t: OffsetDateTime,
    refusal: Refusal,
) -> Result<TxOutcome, Abort> {
    check_refusal_owner(settle.durable.as_ref(), a)?;
    let Refusal {
        reason,
        mut data,
        reach,
        overlap_conflict,
    } = refusal;
    let assessed = reach != GateReach::NotReached;
    let tx = locked.transaction();
    let audit_id = Uuid::new_v4();
    // M2-2: a reached assessment (composite gate, overlap conflict) persists its diagnostic run
    // before the refusal audit and settlement; engine-only refusals never write one.
    if assessed {
        step_diagnostics(
            a,
            tx,
            a.order_id,
            None,
            audit_id,
            t,
            overlap_conflict.as_ref(),
        )
        .await?;
    }
    let sealed = evidence(&a.caller, a.actor, &a.key, a.correlation_id, t)?.resolved_refusal(
        audit_id,
        AuditTrigger::Public(a.trigger),
        &locked.audit_facts()?,
        reason,
    )?;
    writer::append_resolved_refusal(locked, sealed).await?;
    fault(inner, FaultPoint::Audit, tx).await?;
    // The refused force request is the two-person request reference (D-182, D-201).
    if reason == Reason::SecondApproverRequired
        && let Some(map) = data.as_object_mut()
    {
        map.insert("request_audit_id".to_owned(), json!(audit_id));
    }
    let reached = if let (Some(conflict), Some(diagnostics)) =
        (overlap_conflict.as_ref(), a.prepared.diagnostics.as_ref())
    {
        diagnostics.collision_report(conflict)?
    } else if assessed {
        a.prepared.assessment.clone()
    } else {
        None
    };
    // D-229: the report is part of the stored Problem, so the first answer and every same-key
    // replay carry it in `context.data.assessment`.
    if reach == GateReach::Reported
        && let Some(gate_report) = &reached
        && let Some(map) = data.as_object_mut()
    {
        map.insert("assessment".to_owned(), gate_report.clone());
    }
    let response = refusal_response(reason, data, reached)?;
    settle
        .settle(
            locked,
            Settlement::Refused {
                reason,
                audit_id,
                response: response.clone(),
            },
        )
        .await?;
    fault(inner, FaultPoint::Settle, tx).await?;
    Ok(TxOutcome::Settled(EngineOutcome {
        kind: OutcomeKind::Refused { reason, audit_id },
        response,
    }))
}

/// Steps 14-27 for an admitted row.
#[expect(
    clippy::too_many_arguments,
    reason = "steps 14-27 share the locked aggregate, settlement, row, facts, transition time, \
              event sink and administrative changes of one transaction"
)]
async fn admit<'a>(
    inner: &Inner,
    a: &Attempt,
    locked: &mut LockedOrder<'a, DbTx<'a>>,
    settle: Settle<'a>,
    row: &Row,
    facts: &AggregateFacts,
    t: OffsetDateTime,
    events: &TxEvents,
    administrative: &[AdminChange],
) -> Result<TxOutcome, Abort> {
    let tx = locked.transaction();
    check_subflow(settle.durable.as_ref(), a, row, facts)?;
    let effects = contributions::plan(row, facts, &a.prepared.aggregate)?;
    check_documents(a, row, &effects)?;
    let after = post_facts(a, facts, &effects, t)?;
    let before_audit = locked.audit_facts()?;
    let post = post_model(locked.row(), &after);
    let write_scope = write_scope(a);
    // Step 17 (every row): terminal release, untouched, or replacement; before any version or
    // document write, and nothing takes a claim afterwards.
    if let Some(conflict) = step_claims(a, locked, row, &effects, &post, &write_scope).await? {
        fault(inner, FaultPoint::Claims, tx).await?;
        let refusal = conflict_refusal(&conflict);
        return Box::pin(refuse_settled(inner, a, locked, settle, t, refusal)).await;
    }
    fault(inner, FaultPoint::Claims, tx).await?;
    step_version(a, locked, row, facts, &effects, t).await?;
    fault(inner, FaultPoint::Version, tx).await?;
    // Step 19: child documents through the bounded writer (never the aggregate). The audit
    // entry step 22 appends is allocated now so a D-198 grant can link it.
    let audit_id = Uuid::new_v4();
    if let Some(documents) = &a.prepared.documents {
        documents
            .write(&ChildDocuments::new(
                locked,
                t,
                audit_id,
                settle.execution_id,
            ))
            .await?;
    }
    fault(inner, FaultPoint::Documents, tx).await?;
    // M2-2: the gate's diagnostic run names the version this transition admits (the appended
    // candidate, else the locked current version), after the version append its FK needs.
    let admitted = effects
        .version
        .map_or(locked.row().current_version, |step| step.candidate);
    step_diagnostics(a, tx, facts.order_id, Some(admitted), audit_id, t, None).await?;
    step_aggregate(a, locked, post, &write_scope).await?;
    fault(inner, FaultPoint::Aggregate, tx).await?;
    let audit_id = step_audit(
        inner,
        a,
        locked,
        row,
        &before_audit,
        audit_id,
        t,
        administrative,
    )
    .await?;
    fault(inner, FaultPoint::Audit, tx).await?;
    step_event(a, locked, row, &after, t, events).await?;
    fault(inner, FaultPoint::Enqueue, tx).await?;
    let response = success_response(
        row.trigger,
        &after,
        a.prepared.line_id,
        a.prepared.assessment.clone(),
        a.prepared.assessment_id,
    )?;
    settle
        .settle(
            locked,
            Settlement::Success {
                order_id: None,
                audit_id,
                response: response.clone(),
            },
        )
        .await?;
    fault(inner, FaultPoint::Settle, tx).await?;
    Ok(TxOutcome::Settled(EngineOutcome {
        kind: OutcomeKind::Committed {
            order_id: facts.order_id,
            audit_id,
        },
        response,
    }))
}

/// The current-order write scope: the authorized proposal's when the arrangement changes, the
/// target's otherwise.
fn write_scope(a: &Attempt) -> toolkit_security::AccessScope {
    a.proposed.as_ref().map_or_else(
        || a.target.access_scope().clone(),
        |p| p.access_scope().clone(),
    )
}

/// M2-2: the slice's gate diagnostic run, if any, on the transition's transaction. A write
/// failure aborts the attempt like any other storage failure. The subject identity and the
/// subject-tenant scope come only from the caller's `SecurityContext` (DESIGN
/// `#contract-03-3-7`), never from the slice.
async fn step_diagnostics(
    a: &Attempt,
    tx: &DbTx<'_>,
    order_id: Uuid,
    version: Option<i32>,
    audit_id: Uuid,
    t: OffsetDateTime,
    overlap_conflict: Option<&crate::domain::overlap::OverlapConflict>,
) -> Result<(), Abort> {
    if let Some(diagnostics) = &a.prepared.diagnostics {
        let ctx = a.caller.ctx();
        let subject_tenant_id = ctx.subject_tenant_id();
        let settled = DiagnosticsSettled {
            overlap_conflict: overlap_conflict.cloned(),
            order_id,
            version,
            audit_id,
            transition_time: t,
            subject_tenant_id,
            subject_id: ctx.subject_id().to_string(),
        };
        let scope = toolkit_security::AccessScope::for_tenant(subject_tenant_id);
        diagnostics.write(tx, &scope, &settled).await?;
    }
    Ok(())
}

/// The settled step-17.5 refusal: blocked tuples name a conflicting order only where the
/// caller's read scope can see it (D-179); the reached assessment rides along.
fn conflict_refusal(conflict: &crate::domain::overlap::OverlapConflict) -> Refusal {
    let blocked: Vec<Value> = conflict
        .blocked
        .iter()
        .map(|b| {
            json!({
                "overlap_scope_key": b.tuple.overlap_scope_key.as_str(),
                "conflicting_order_id": b.visible_holder,
            })
        })
        .collect();
    Refusal {
        overlap_conflict: Some(conflict.clone()),
        reason: crate::domain::overlap::OverlapConflict::REASON,
        data: json!({ "blocked": blocked }),
        reach: GateReach::Assessed,
    }
}

/// Child-document contracts: a generation increment needs its D-198 grant contribution; row 3
/// and only row 3 carries administrative changes.
fn check_documents(a: &Attempt, row: &Row, effects: &contributions::Effects) -> Result<(), Abort> {
    if effects.issues_grant() && a.prepared.documents.is_none() {
        return Err(Abort::NotDelivered(
            "D-198 dispatch grant contribution (S5-02/S5-04)",
        ));
    }
    if (row.trigger == Trigger::AdministrativeEdit) == a.prepared.admin_changes.is_empty() {
        return Err(Abort::Integration(
            "administrative changes belong to row 3 exactly",
        ));
    }
    Ok(())
}

/// Steps 18.2-21: the engine alone writes the aggregate, through the authorized proposal when
/// the arrangement changes and the current-order scope otherwise.
async fn step_aggregate<'a>(
    a: &Attempt,
    locked: &mut LockedOrder<'a, DbTx<'a>>,
    post: order::Model,
    write_scope: &toolkit_security::AccessScope,
) -> Result<(), Abort> {
    if post == *locked.row() {
        return Ok(());
    }
    match &a.proposed {
        Some(proposed) => scoped::apply_authorized_arrangement(locked, proposed, post).await?,
        None => locked.replace(write_scope, post).await?,
    }
    Ok(())
}

/// E3: a refusal never settles over a recovered commercial attempt unless the preparation is the
/// attempt's own commercial one (its `Version` contribution names the reserved candidate). A
/// non-commercial preparation (submit's pre-reservation refusal path) that meets a reclaimed
/// `prepared`/`running` attempt would otherwise mark it refused and burn its candidate on an
/// assessment the attempt never froze; it fails closed instead, writing nothing.
fn check_refusal_owner(
    durable: Option<&(DurableExecution, Frozen)>,
    a: &Attempt,
) -> Result<(), Abort> {
    match durable {
        Some((_, frozen @ Frozen::Commercial(attempt))) if frozen.is_unresolved() => {
            match &a.prepared.aggregate {
                AggregateContribution::Version(v) if v.candidate == attempt.candidate_version => {
                    Ok(())
                }
                _ => Err(Abort::Integration(
                    "a refusal over a recovered commercial attempt from a non-commercial preparation",
                )),
            }
        }
        _ => Ok(()),
    }
}

/// D-188/D-198 are specialized subflows of exactly their rows, never blanket exceptions.
fn check_subflow(
    durable: Option<&(DurableExecution, Frozen)>,
    a: &Attempt,
    row: &Row,
    facts: &AggregateFacts,
) -> Result<(), Abort> {
    let commercial = requires_commercial_attempt(row);
    let control = requires_receiver_control(row, facts);
    match (durable, commercial, control) {
        (Some((_, Frozen::Commercial(attempt))), true, _) => match &a.prepared.aggregate {
            AggregateContribution::Version(v) if v.candidate == attempt.candidate_version => Ok(()),
            _ => Err(Abort::Integration(
                "version contribution is not the reserved candidate",
            )),
        },
        (Some((_, Frozen::Control(_))), false, true) | (None, false, false) => Ok(()),
        (None, true, _) => Err(Abort::NotDelivered("D-188 commercial attempt required")),
        (None, false, true) => Err(Abort::NotDelivered("D-198 receiver control required")),
        _ => Err(Abort::Integration(
            "durable execution does not belong to this row",
        )),
    }
}

/// The complete planned post-state, including an authorized proposed arrangement (draft axis
/// change, amendment payer) and the versioning row's commercial header.
fn post_facts(
    a: &Attempt,
    facts: &AggregateFacts,
    effects: &contributions::Effects,
    t: OffsetDateTime,
) -> Result<AggregateFacts, Abort> {
    let mut after = contributions::apply(facts, effects, t);
    if let Some(proposed) = &a.proposed {
        let arrangement = proposed.arrangement();
        after.resource_tenant_id = arrangement.resource_tenant_id;
        after.payer_tenant_id = arrangement.payer_tenant_id;
    }
    if let AggregateContribution::DraftEdit(header) = &a.prepared.aggregate {
        header.apply_to(&mut after);
    }
    if let AggregateContribution::Version(v) = &a.prepared.aggregate {
        if v.payer_tenant_id != after.payer_tenant_id {
            return Err(Abort::Integration(
                "version payer is not the authorized arrangement",
            ));
        }
        after.category.clone_from(&v.category);
        after.contract_id = v.contract_id;
    }
    Ok(after)
}

/// Step 17: `Some` is the authoritative collision (provisional claims already released).
async fn step_claims<'a>(
    a: &Attempt,
    locked: &LockedOrder<'a, DbTx<'a>>,
    row: &Row,
    effects: &contributions::Effects,
    post: &order::Model,
    write_scope: &toolkit_security::AccessScope,
) -> Result<Option<crate::domain::overlap::OverlapConflict>, Abort> {
    let directive =
        ClaimDirective::for_transition(row.trigger, effects.target, a.prepared.claims.clone())?;
    let authority = match (&directive, &a.disclosure, effects.version) {
        (ClaimDirective::Replace(_), Some(disclosure), Some(step)) => Some(ProposalAuthority {
            proposed: post,
            proposed_scope: write_scope,
            disclosure_scope: disclosure,
            proposed_version: step.candidate,
        }),
        (ClaimDirective::Replace(_), _, _) => {
            return Err(Abort::Integration(
                "claim replacement needs disclosure scope and candidate",
            ));
        }
        _ => None,
    };
    Ok(
        match locked
            .maintain_claims(&directive, authority.as_ref())
            .await?
        {
            ClaimOutcome::Conflict(conflict) => Some(conflict),
            _ => None,
        },
    )
}

/// Step 18: the reserved candidate, superseding the outgoing current version.
async fn step_version<'a>(
    a: &Attempt,
    locked: &LockedOrder<'a, DbTx<'a>>,
    row: &Row,
    facts: &AggregateFacts,
    effects: &contributions::Effects,
    t: OffsetDateTime,
) -> Result<(), Abort> {
    if let (Some(step), AggregateContribution::Version(v)) =
        (effects.version, &a.prepared.aggregate)
    {
        locked
            .insert_order_version(order_version::Model {
                order_id: facts.order_id,
                version: step.candidate,
                supersedes_version: Some(step.supersedes),
                market_currency: v.market_currency.clone(),
                market_region: v.market_region.clone(),
                payer_tenant_id: v.payer_tenant_id,
                category: v.category.clone(),
                contract_id: v.contract_id,
                actor: a.actor.reference(),
                actor_tenant_id: a.actor.subject_tenant_id(),
                reason: row.trigger.token().to_owned(),
                amendment_reason: v.amendment_reason.clone(),
                created_at: t,
            })
            .await?;
    }
    Ok(())
}

/// Step 22: one committed entry, or one per changed administrative field (D-117), after the
/// aggregate write; returns the entry the settlement references (the last one).
#[expect(
    clippy::too_many_arguments,
    reason = "the audit step reads the locked aggregate, row, before-facts, audit id, transition \
              time and administrative changes of the same transaction"
)]
async fn step_audit<'a>(
    inner: &Inner,
    a: &Attempt,
    locked: &mut LockedOrder<'a, DbTx<'a>>,
    row: &Row,
    before: &crate::domain::audit::OrderFacts,
    audit_id: Uuid,
    t: OffsetDateTime,
    administrative: &[AdminChange],
) -> Result<Uuid, Abort> {
    let evidence = evidence(&a.caller, a.actor, &a.key, a.correlation_id, t)?;
    if row.trigger == Trigger::AdministrativeEdit {
        let mut first = Some(audit_id);
        let pending = evidence.administrative_edits(
            &inner.admin_key,
            &locked.audit_facts()?,
            administrative,
            || first.take().unwrap_or_else(Uuid::new_v4),
        )?;
        let sealed = writer::append_committed_all(locked, pending).await?;
        return sealed
            .last()
            .map(|s| s.row().audit_id)
            .ok_or(Abort::Integration("administrative edit audited nothing"));
    }
    let pending = evidence.committed_transition(
        audit_id,
        AuditTrigger::Public(row.trigger),
        before,
        &locked.audit_facts()?,
        a.caller_reason.as_deref(),
    )?;
    writer::append_committed(locked, pending).await?;
    Ok(audit_id)
}

/// Step 24: exactly the row's declared event, built from the committed post-state.
async fn step_event<'a>(
    a: &Attempt,
    locked: &LockedOrder<'a, DbTx<'a>>,
    row: &Row,
    after: &AggregateFacts,
    t: OffsetDateTime,
    events: &TxEvents,
) -> Result<(), Abort> {
    match (row.event, &a.prepared.event) {
        (Some(kind), Some(spec)) if spec.kind() == kind => {
            let summary = summary(locked, after, t, a.correlation_id).await?;
            spec.enqueue(documents::EventEnqueue::new(events, locked, summary))
                .await?;
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(Abort::Integration(
            "event detail does not match the row's declaration",
        )),
    }
}

/// The order's administrative row read under the aggregate lock this transaction holds.
pub(super) async fn locked_order_admin<'a>(
    locked: &LockedOrder<'a, DbTx<'a>>,
) -> Result<Option<entity::order_admin::Model>, ScopeError> {
    let id = locked.row().order_id;
    Ok(children::order_admin_for_order(
        locked.transaction(),
        &toolkit_security::AccessScope::for_resources(vec![id]),
        id,
    )
    .await?
    .into_iter()
    .next())
}

/// The order's line administrative rows read under the aggregate lock this transaction holds.
pub(super) async fn locked_line_admin<'a>(
    locked: &LockedOrder<'a, DbTx<'a>>,
) -> Result<Vec<entity::order_line_admin::Model>, ScopeError> {
    let id = locked.row().order_id;
    children::order_line_admin_for_order(
        locked.transaction(),
        &toolkit_security::AccessScope::for_resources(vec![id]),
        id,
    )
    .await
}

/// Row 3's named changes reconciled with the administrative values stored at this locked read.
async fn locked_administrative<'a>(
    locked: &LockedOrder<'a, DbTx<'a>>,
    named: &[AdminChange],
) -> Result<Vec<AdminChange>, Abort> {
    let order = locked_order_admin(locked).await?;
    let lines = locked_line_admin(locked).await?;
    let value = |attribute: AdminAttribute,
                 external: &Option<String>,
                 label: &Option<String>,
                 notes: &Option<String>| match attribute {
        AdminAttribute::ExternalReference => external.clone(),
        AdminAttribute::DisplayLabel => label.clone(),
        AdminAttribute::InternalNotes => notes.clone(),
    };
    Ok(contributions::reconcile_administrative(
        named,
        |field| match field {
            AdminField::Order(attribute) => order.as_ref().and_then(|row| {
                value(
                    attribute,
                    &row.external_reference,
                    &row.display_label,
                    &row.internal_notes,
                )
            }),
            AdminField::Line(line_id, attribute) => lines
                .iter()
                .find(|row| row.line_id == line_id)
                .and_then(|row| {
                    value(
                        attribute,
                        &row.external_reference,
                        &row.display_label,
                        &row.internal_notes,
                    )
                }),
        },
    ))
}

/// The §4.4 common summary of the committed post-state.
async fn summary<'a>(
    locked: &LockedOrder<'a, DbTx<'a>>,
    after: &AggregateFacts,
    t: OffsetDateTime,
    correlation_id: Option<Uuid>,
) -> Result<OrderSummary, Abort> {
    let row = locked.row();
    let external_reference = children::order_admin_for_order(
        locked.transaction(),
        &toolkit_security::AccessScope::for_resources(vec![row.order_id]),
        row.order_id,
    )
    .await?
    .into_iter()
    .next()
    .and_then(|admin| admin.external_reference);
    Ok(OrderSummary {
        order_id: row.order_id,
        order_version: row.current_version,
        occurred_at: t,
        correlation_id: correlation_id.unwrap_or_else(Uuid::new_v4),
        category: row.category.clone(),
        state: after.state,
        resource_tenant_id: row.resource_tenant_id,
        seller_tenant_id: row.seller_tenant_id,
        payer_tenant_id: row.payer_tenant_id,
        contract_id: row.contract_id,
        external_reference,
    })
}
