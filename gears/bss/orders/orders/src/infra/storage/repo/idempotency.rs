//! Authoritative idempotency gate and durable execution ownership (Foundation §3.6/§4.2,
//! D-188, D-198).
//!
//! Lock order is aggregate (a [`LockedOrder`]) before registry before linked attempt/control.
//! Every deadline comes from fresh database wall-clock time (`clock_timestamp()`) read after
//! the registry row lock was obtained. Ordinary claims live only inside one transaction:
//! [`Owned`] borrows that transaction and settles there. D-188/D-198 executions persist an
//! immutable execution ID plus owner token and fencing generation; every continuation across
//! remote calls re-verifies that exact fence under lock ([`verify_owner`]).
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt};
use uuid::Uuid;

use super::{LockedOrder, TransactionRunner, mutable, private};
use crate::domain::idempotency::{
    ExecutionOwner, Fingerprint, GateDecision, LeaseDuration, RecordView, RegistryKey,
    RegistryRuleError, Settlement, StoredResponse, classify, next_fence,
};
use crate::infra::storage::entity::{commercial_attempt, fulfillment_control, idempotency};
use crate::infra::storage::scoped::PrivateScope;

const IN_FLIGHT: &str = "in_flight";
const SETTLED: &str = "settled";
/// Gate rounds: a lost conflict-safe insert re-reads the winner; a winner removed again by
/// concurrent expiry handling gets one more round before failing as contention.
const GATE_ROUNDS: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(transparent)]
    Store(#[from] ScopeError),
    #[error(transparent)]
    Rule(#[from] RegistryRuleError),
    #[error("idempotency record contended")]
    Contended,
    #[error("registry target does not match the operation")]
    TargetRequired,
    #[error("stale or foreign execution owner")]
    StaleOwner,
    #[error("an operational first result cannot be rewritten")]
    FirstResultConflict,
    #[error("commercial version allocation exhausted")]
    CandidateExhausted,
    #[error("a fulfillment control is already pending")]
    ControlPending,
    #[error("settlement does not match the bound target or execution")]
    SettlementMismatch,
    #[error("execution status cannot regress")]
    StatusRegression,
}

/// Whether a new claim creates a durable D-188/D-198 owner identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    Ordinary,
    Durable,
}

/// The authorized request's registry inputs.
#[derive(Debug, Clone, Copy)]
pub struct GateRequest<'r> {
    pub key: &'r RegistryKey,
    pub fingerprint: &'r Fingerprint,
    pub lease: LeaseDuration,
    pub durability: Durability,
    /// The executor's own persisted owner on a D-188/D-198 continuation.
    pub presented: Option<&'r ExecutionOwner>,
}

/// Create never locks a nonexistent aggregate; every other operation presents its locked
/// aggregate, which binds the record's target and establishes aggregate-before-registry order.
pub enum GateTarget<'l, 'a, T: TransactionRunner> {
    Create(&'a T),
    Order(&'l LockedOrder<'a, T>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOrigin {
    /// This transaction's own successful conflict-safe insert.
    Inserted,
    /// An expired record was deleted and replaced by a fresh execution generation.
    ReplacedExpired,
    /// A matching in-flight record whose lease expired was reclaimed under the row lock.
    Reclaimed,
    /// The presenting executor's own live durable marker.
    CurrentOwner,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReplayOutcome {
    Success,
    Refused(String),
}
/// A matching settled record. Disclosure must still be rechecked against current authority.
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    pub response: StoredResponse,
    pub outcome: ReplayOutcome,
    pub order_id: Option<Uuid>,
    pub audit_id: Option<Uuid>,
}
impl Replay {
    fn from_row(row: &idempotency::Model) -> Result<Self, RegistryError> {
        let response = row
            .settled_response
            .as_ref()
            .ok_or(RegistryRuleError::InvalidRecord)
            .and_then(StoredResponse::decode)?;
        let outcome = match (row.outcome.as_deref(), &row.outcome_reason) {
            (Some("success"), _) => ReplayOutcome::Success,
            (Some("refused"), Some(reason)) => ReplayOutcome::Refused(reason.clone()),
            _ => return Err(RegistryRuleError::InvalidRecord.into()),
        };
        Ok(Self {
            response,
            outcome,
            order_id: row.order_id,
            audit_id: row.audit_id,
        })
    }
}

/// Frozen operational evidence of a durable execution.
#[derive(Debug, Clone, PartialEq)]
pub enum Frozen {
    Commercial(commercial_attempt::Model),
    Control(fulfillment_control::Model),
}
impl Frozen {
    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        match self {
            Self::Commercial(a) => matches!(a.status.as_str(), "prepared" | "running"),
            Self::Control(c) => {
                matches!(c.status.as_str(), "prepared" | "awaiting" | "barrier_ready")
            }
        }
    }
    fn owner(&self, execution_id: Uuid) -> ExecutionOwner {
        let (owner_token, fencing_generation) = match self {
            Self::Commercial(a) => (a.owner_token, a.fencing_generation),
            Self::Control(c) => (c.owner_token, c.fencing_generation),
        };
        ExecutionOwner {
            execution_id,
            owner_token,
            fencing_generation,
        }
    }
    fn link(&self) -> ExecutionLink {
        match self {
            Self::Commercial(a) => ExecutionLink::Commercial(a.attempt_id),
            Self::Control(c) => ExecutionLink::Control(c.control_id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionLink {
    Commercial(Uuid),
    Control(Uuid),
}

/// Persisted identity of a durable execution, carried across remote calls. It is evidence of
/// ownership only while [`verify_owner`] still finds this exact fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableExecution {
    operation: String,
    principal_scope: String,
    idempotency_key: String,
    order_id: Uuid,
    owner: ExecutionOwner,
    link: ExecutionLink,
}
impl DurableExecution {
    #[must_use]
    pub fn owner(&self) -> ExecutionOwner {
        self.owner
    }
    #[must_use]
    pub fn link(&self) -> ExecutionLink {
        self.link
    }
    #[must_use]
    pub fn order_id(&self) -> Uuid {
        self.order_id
    }
    /// Fault injection only: a late worker still holding an old generation's identity.
    #[cfg(test)]
    pub(crate) fn stale_for_test(
        key: (&str, &str, &str),
        order_id: Uuid,
        owner: ExecutionOwner,
        link: ExecutionLink,
    ) -> Self {
        Self {
            operation: key.0.to_owned(),
            principal_scope: key.1.to_owned(),
            idempotency_key: key.2.to_owned(),
            order_id,
            owner,
            link,
        }
    }
    fn from_parts(row: &idempotency::Model, frozen: &Frozen) -> Result<Self, RegistryError> {
        Ok(Self {
            operation: row.operation.clone(),
            principal_scope: row.principal_scope.clone(),
            idempotency_key: row.idempotency_key.clone(),
            order_id: row.order_id.ok_or(RegistryRuleError::InvalidRecord)?,
            owner: owner_of(row).ok_or(RegistryRuleError::InvalidRecord)?,
            link: frozen.link(),
        })
    }
}

/// Ownership of a registry record, valid only within the transaction that claimed it.
pub struct Owned<'a, T: TransactionRunner> {
    tx: &'a T,
    scope: AccessScope,
    row: idempotency::Model,
    origin: ClaimOrigin,
    now: OffsetDateTime,
    frozen: Option<Frozen>,
}
impl<T: TransactionRunner> std::fmt::Debug for Owned<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Owned")
            .field("execution_id", &self.row.execution_id)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// Authoritative gate outcome.
#[expect(
    clippy::large_enum_variant,
    reason = "Transaction-scoped value returned once per request, never stored or collected"
)]
pub enum Gate<'a, T: TransactionRunner> {
    Replay(Replay),
    /// Different fingerprint: the caller appends mismatch evidence; the winner is unchanged.
    Mismatch,
    /// Matching live in-flight lease owned by someone else: nothing is settled or stolen.
    StillProcessing,
    Owned(Owned<'a, T>),
}

impl<T: TransactionRunner> std::fmt::Debug for Gate<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Replay(r) => f.debug_tuple("Replay").field(r).finish(),
            Self::Mismatch => f.write_str("Mismatch"),
            Self::StillProcessing => f.write_str("StillProcessing"),
            Self::Owned(o) => f.debug_tuple("Owned").field(o).finish(),
        }
    }
}

impl<T: TransactionRunner> Owned<'_, T> {
    #[must_use]
    pub fn origin(&self) -> ClaimOrigin {
        self.origin
    }
    #[must_use]
    pub fn record(&self) -> &idempotency::Model {
        &self.row
    }
    /// Fresh database time read after the registry lock.
    #[must_use]
    pub fn claimed_at(&self) -> OffsetDateTime {
        self.now
    }
    /// D-188/D-198: the recovered execution and its frozen inputs, loaded before any fresh
    /// assessment. `None` for ordinary claims and not-yet-linked new durable claims.
    pub fn durable(&self) -> Result<Option<(DurableExecution, &Frozen)>, RegistryError> {
        match &self.frozen {
            Some(frozen) => Ok(Some((
                DurableExecution::from_parts(&self.row, frozen)?,
                frozen,
            ))),
            None => Ok(None),
        }
    }
    /// Settle an ordinary claim (success or owned business refusal) in its own transaction.
    /// Linked D-188/D-198 executions settle through [`finish_execution`].
    ///
    /// # Errors
    /// Linked execution, target mismatch or store failure (which aborts the transaction).
    pub async fn settle(self, settlement: Settlement) -> Result<idempotency::Model, RegistryError> {
        if self.row.attempt_id.is_some() || self.row.fulfillment_control_id.is_some() {
            return Err(RegistryError::SettlementMismatch);
        }
        settle_row(self.tx, &self.scope, &self.row, settlement).await
    }
}

async fn settle_row<T: TransactionRunner>(
    tx: &T,
    scope: &AccessScope,
    row: &idempotency::Model,
    settlement: Settlement,
) -> Result<idempotency::Model, RegistryError> {
    let is_create = row.operation == "create";
    let mut proposed = row.clone();
    SETTLED.clone_into(&mut proposed.status);
    proposed.lease_expires_at = None;
    match settlement {
        Settlement::Success {
            order_id,
            audit_id,
            response,
        } => {
            match (is_create, order_id) {
                (true, Some(id)) => proposed.order_id = Some(id),
                (false, None) => {}
                (false, Some(id)) if row.order_id == Some(id) => {}
                _ => return Err(RegistryError::SettlementMismatch),
            }
            proposed.outcome = Some("success".to_owned());
            proposed.audit_id = Some(audit_id);
            proposed.settled_response = Some(response.encode()?);
        }
        Settlement::Refused {
            reason,
            audit_id,
            response,
        } => {
            proposed.outcome = Some("refused".to_owned());
            proposed.outcome_reason = Some(reason.mapping().reason.to_owned());
            proposed.audit_id = Some(audit_id);
            proposed.settled_response = Some(response.encode()?);
        }
    }
    mutable::replace_idempotency(tx, scope, row, proposed.clone()).await?;
    Ok(proposed)
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct DbClock {
    t: time::OffsetDateTime,
}
/// Fresh wall-clock database time (`clock_timestamp()`, not transaction-start `now()`). The
/// aggregate projection always yields one row, whether or not the key exists.
async fn db_now<R: DBRunner>(
    runner: &R,
    scope: &AccessScope,
    key: (&str, &str, &str),
) -> Result<OffsetDateTime, RegistryError> {
    let rows = key_filter(idempotency::Entity::find(), key)
        .secure()
        .scope_with(scope)
        .project_all(runner, |q| {
            q.select_only()
                .column_as(Expr::cust("clock_timestamp()"), "t")
                .column_as(Expr::cust("count(*)"), "n")
                .into_model::<DbClock>()
        })
        .await?;
    rows.first()
        .map(|c| c.t)
        .ok_or(RegistryError::Store(ScopeError::Invalid(
            "database clock unavailable",
        )))
}
/// Fresh database wall-clock time for evidence written outside an owned claim (refusals that
/// settle nothing), read in the caller's transaction through the principal's registry scope.
///
/// # Errors
/// Store failure.
pub async fn database_time<R: DBRunner>(
    runner: &R,
    scope: &PrivateScope,
    key: &RegistryKey,
) -> Result<OffsetDateTime, RegistryError> {
    let (op, key_text) = (key.operation().token(), key.key_text());
    db_now(
        runner,
        scope.access_scope(),
        (op.as_str(), key.principal().as_str(), key_text.as_str()),
    )
    .await
}

fn key_filter(
    query: sea_orm::Select<idempotency::Entity>,
    (operation, principal, key): (&str, &str, &str),
) -> sea_orm::Select<idempotency::Entity> {
    query
        .filter(idempotency::Column::Operation.eq(operation))
        .filter(idempotency::Column::PrincipalScope.eq(principal))
        .filter(idempotency::Column::IdempotencyKey.eq(key))
}

fn owner_of(row: &idempotency::Model) -> Option<ExecutionOwner> {
    row.owner_token.map(|owner_token| ExecutionOwner {
        execution_id: row.execution_id,
        owner_token,
        fencing_generation: row.fencing_generation,
    })
}

impl<T: TransactionRunner> LockedOrder<'_, T> {
    async fn lock_attempt(
        &self,
        attempt_id: Uuid,
    ) -> Result<Option<commercial_attempt::Model>, ScopeError> {
        commercial_attempt::Entity::find_by_id(attempt_id)
            .lock_exclusive()
            .secure()
            .scope_with(&self.child_scope())
            .one(self.tx)
            .await
    }
    async fn lock_control(
        &self,
        control_id: Uuid,
    ) -> Result<Option<fulfillment_control::Model>, ScopeError> {
        fulfillment_control::Entity::find_by_id(control_id)
            .lock_exclusive()
            .secure()
            .scope_with(&self.child_scope())
            .one(self.tx)
            .await
    }
    /// Linked frozen evidence of a marker bound to this aggregate.
    async fn linked(&self, row: &idempotency::Model) -> Result<Option<Frozen>, RegistryError> {
        if row.order_id != Some(self.row.order_id) {
            return Ok(None);
        }
        Ok(match (row.attempt_id, row.fulfillment_control_id) {
            (Some(id), None) => Some(Frozen::Commercial(
                self.lock_attempt(id)
                    .await?
                    .ok_or(RegistryRuleError::InvalidRecord)?,
            )),
            (None, Some(id)) => Some(Frozen::Control(
                self.lock_control(id)
                    .await?
                    .ok_or(RegistryRuleError::InvalidRecord)?,
            )),
            (None, None) => None,
            (Some(_), Some(_)) => return Err(RegistryRuleError::InvalidRecord.into()),
        })
    }
    /// D-188 checked high-water increment under the aggregate lock; exhaustion refuses
    /// before any command and never wraps or reuses a number.
    async fn allocate_candidate_version(&mut self) -> Result<i32, RegistryError> {
        let next = self
            .row
            .version_allocation_high_water
            .checked_add(1)
            .ok_or(RegistryError::CandidateExhausted)?;
        let mut proposed = self.row.clone();
        proposed.version_allocation_high_water = next;
        let scope = self.scope.clone();
        self.replace(&scope, proposed).await?;
        Ok(next)
    }
}

/// Resolve the registry for an authorized request (Foundation §3.6 steps 6-9).
///
/// Retention expiry is applied first at fresh DB time; the fingerprint is compared before
/// settlement or lease state. Absence is claimed with a conflict-safe insert; a lost insert
/// re-reads the winner's locked record rather than assuming ownership.
///
/// # Errors
/// Store failure, invalid record, fence exhaustion or contention; all abort the transaction
/// and never grant ownership.
pub async fn resolve<'a, T: TransactionRunner>(
    target: GateTarget<'_, 'a, T>,
    scope: &PrivateScope,
    request: &GateRequest<'_>,
) -> Result<Gate<'a, T>, RegistryError> {
    let (tx, locked) = match target {
        GateTarget::Create(tx) => (tx, None),
        GateTarget::Order(locked) => (locked.tx, Some(locked)),
    };
    let operation = request.key.operation();
    if operation.is_create() == locked.is_some()
        || (request.durability == Durability::Durable && locked.is_none())
    {
        return Err(RegistryError::TargetRequired);
    }
    let s = scope.access_scope();
    let (op, key_text) = (operation.token(), request.key.key_text());
    let key = (
        op.as_str(),
        request.key.principal().as_str(),
        key_text.as_str(),
    );
    for _ in 0..GATE_ROUNDS {
        let current = private::lock_idempotency(tx, s, key.0, key.1, key.2).await?;
        let now = db_now(tx, s, key).await?;
        let Some(row) = current else {
            let fresh = new_record(request, key, locked.map(|l| l.row.order_id), now)?;
            if private::offer_idempotency(tx, s, fresh.clone()).await? {
                return Ok(owned(tx, s, fresh, ClaimOrigin::Inserted, now, None));
            }
            continue;
        };
        if !matches!(row.status.as_str(), IN_FLIGHT | SETTLED) {
            return Err(RegistryRuleError::InvalidRecord.into());
        }
        // One key binds one aggregate. A record of another order is never replayed/reclaimed;
        // a linked foreign execution is conservatively treated as unresolved.
        let foreign = locked.is_some_and(|l| row.order_id != Some(l.row.order_id));
        let frozen = match locked {
            Some(l) if !foreign => l.linked(&row).await?,
            _ => None,
        };
        let view = RecordView {
            fingerprint: &row.request_fingerprint,
            settled: row.status == SETTLED,
            lease_expires_at: row.lease_expires_at,
            expires_at: row.expires_at,
            unresolved_execution: frozen.as_ref().is_some_and(Frozen::is_unresolved)
                || (foreign && (row.attempt_id.is_some() || row.fulfillment_control_id.is_some())),
            owner: owner_of(&row),
        };
        let decision = classify(view, request.fingerprint, now, request.presented)?;
        if foreign && decision != GateDecision::ReplaceExpired {
            return Ok(Gate::Mismatch);
        }
        return match decision {
            GateDecision::ReplaceExpired => {
                delete_for_replacement(tx, s, &row).await?;
                let fresh = new_record(request, key, locked.map(|l| l.row.order_id), now)?;
                if !private::offer_idempotency(tx, s, fresh.clone()).await? {
                    return Err(RegistryError::Contended);
                }
                Ok(owned(tx, s, fresh, ClaimOrigin::ReplacedExpired, now, None))
            }
            GateDecision::Mismatch => Ok(Gate::Mismatch),
            GateDecision::Replay => Ok(Gate::Replay(Replay::from_row(&row)?)),
            GateDecision::StillProcessing => Ok(Gate::StillProcessing),
            GateDecision::CurrentOwner => {
                // The marker and its linked execution must carry the same live fence.
                if let Some(frozen) = &frozen
                    && Some(frozen.owner(row.execution_id)) != owner_of(&row)
                {
                    return Err(RegistryError::StaleOwner);
                }
                Ok(owned(tx, s, row, ClaimOrigin::CurrentOwner, now, frozen))
            }
            GateDecision::Reclaim => {
                let (row, frozen) = reclaim(tx, s, locked, row, frozen, request.lease, now).await?;
                Ok(owned(tx, s, row, ClaimOrigin::Reclaimed, now, frozen))
            }
        };
    }
    Err(RegistryError::Contended)
}

fn owned<'a, T: TransactionRunner>(
    tx: &'a T,
    scope: &AccessScope,
    row: idempotency::Model,
    origin: ClaimOrigin,
    now: OffsetDateTime,
    frozen: Option<Frozen>,
) -> Gate<'a, T> {
    Gate::Owned(Owned {
        tx,
        scope: scope.clone(),
        row,
        origin,
        now,
        frozen,
    })
}

fn new_record(
    request: &GateRequest<'_>,
    (operation, principal, key): (&str, &str, &str),
    order_id: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<idempotency::Model, RegistryError> {
    let window = request.key.operation().retention().window();
    Ok(idempotency::Model {
        operation: operation.to_owned(),
        principal_scope: principal.to_owned(),
        idempotency_key: key.to_owned(),
        order_id,
        request_fingerprint: request.fingerprint.as_str().to_owned(),
        status: IN_FLIGHT.to_owned(),
        // A fresh generation: reused raw key text never adopts an old execution.
        execution_id: Uuid::new_v4(),
        attempt_id: None,
        fulfillment_control_id: None,
        owner_token: (request.durability == Durability::Durable).then(Uuid::new_v4),
        fencing_generation: 0,
        lease_expires_at: Some(now + request.lease.duration()),
        outcome: None,
        outcome_reason: None,
        audit_id: None,
        settled_response: None,
        created_at: now,
        expires_at: now
            .checked_add(window)
            .ok_or(RegistryRuleError::InvalidRecord)?,
    })
}

/// Narrow request-path expiry deletion of exactly the locked generation; the DB guard still
/// refuses a live window or unresolved execution.
async fn delete_for_replacement<T: TransactionRunner>(
    tx: &T,
    scope: &AccessScope,
    row: &idempotency::Model,
) -> Result<(), RegistryError> {
    let result = idempotency::Entity::delete_many()
        .filter(idempotency::Column::Operation.eq(&row.operation))
        .filter(idempotency::Column::PrincipalScope.eq(&row.principal_scope))
        .filter(idempotency::Column::IdempotencyKey.eq(&row.idempotency_key))
        .filter(idempotency::Column::ExecutionId.eq(row.execution_id))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?;
    if result.rows_affected != 1 {
        return Err(RegistryError::Contended);
    }
    Ok(())
}

/// Replace the lease deadline under the row lock; durable owners also rotate the owner token
/// and checked-increment the fence on the marker and its linked execution together.
async fn reclaim<T: TransactionRunner>(
    tx: &T,
    scope: &AccessScope,
    locked: Option<&LockedOrder<'_, T>>,
    row: idempotency::Model,
    frozen: Option<Frozen>,
    lease: LeaseDuration,
    now: OffsetDateTime,
) -> Result<(idempotency::Model, Option<Frozen>), RegistryError> {
    let lease_until = now + lease.duration();
    let mut proposed = row.clone();
    proposed.lease_expires_at = Some(lease_until);
    if row.owner_token.is_some() {
        proposed.owner_token = Some(Uuid::new_v4());
        proposed.fencing_generation = next_fence(row.fencing_generation)?;
    }
    mutable::replace_idempotency(tx, scope, &row, proposed.clone()).await?;
    let frozen = match (frozen, locked, proposed.owner_token) {
        (None, _, _) => None,
        (Some(Frozen::Commercial(a)), Some(l), Some(token)) => {
            let mut next = a.clone();
            next.owner_token = token;
            next.fencing_generation = proposed.fencing_generation;
            next.lease_until = Some(lease_until);
            l.replace_commercial_attempt(&a, next.clone()).await?;
            Some(Frozen::Commercial(next))
        }
        (Some(Frozen::Control(c)), Some(l), Some(token)) => {
            let mut next = c.clone();
            next.owner_token = token;
            next.fencing_generation = proposed.fencing_generation;
            next.lease_until = Some(lease_until);
            l.replace_fulfillment_control(&c, next.clone()).await?;
            Some(Frozen::Control(next))
        }
        _ => return Err(RegistryRuleError::InvalidRecord.into()),
    };
    Ok((proposed, frozen))
}

/// The advisory pre-resolution probe result.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "Returned once per request before resolution, never stored or collected"
)]
pub enum Probe {
    /// Matching, settled and unexpired at fresh DB time: return after the disclosure recheck,
    /// without resolving guard inputs.
    Replay(Replay),
    /// Matching unresolved D-188/D-198 execution: resume from its frozen inputs, never from a
    /// fresh assessment.
    Existing(Frozen),
    /// Anything else, including mismatch and live leases: the authoritative gate decides.
    Miss,
}

/// Advisory, unlocked registry probe. It runs after authorization and before guard-input
/// resolution; a probe that misses a concurrent settle costs one wasted resolution only.
///
/// `order_scope` is the caller's current authorized order scope; frozen evidence is read only
/// through it and only for the record bound to that same order.
///
/// # Errors
/// Store failure or an invalid stored snapshot.
pub async fn probe(
    runner: &impl DBRunner,
    scope: &PrivateScope,
    key: &RegistryKey,
    fingerprint: &Fingerprint,
    order: Option<(Uuid, &AccessScope)>,
) -> Result<Probe, RegistryError> {
    let s = scope.access_scope();
    let (op, key_text) = (key.operation().token(), key.key_text());
    let k = (op.as_str(), key.principal().as_str(), key_text.as_str());
    let Some(row) = private::find_idempotency(runner, s, k.0, k.1, k.2).await? else {
        return Ok(Probe::Miss);
    };
    let now = db_now(runner, s, k).await?;
    if row.request_fingerprint != fingerprint.as_str()
        || order.is_some_and(|(id, _)| row.order_id != Some(id))
    {
        return Ok(Probe::Miss);
    }
    if row.status == SETTLED {
        // An expired record is never replayed, even before the sweep removes it.
        if row.expires_at <= now {
            return Ok(Probe::Miss);
        }
        return Ok(Probe::Replay(Replay::from_row(&row)?));
    }
    let Some((order_id, order_scope)) = order else {
        return Ok(Probe::Miss);
    };
    let frozen = match (row.attempt_id, row.fulfillment_control_id) {
        (Some(id), None) => {
            super::children::commercial_attempt_for_order(runner, order_scope, order_id)
                .await?
                .into_iter()
                .find(|a| a.attempt_id == id)
                .map(Frozen::Commercial)
        }
        (None, Some(id)) => {
            super::children::fulfillment_control_for_order(runner, order_scope, order_id)
                .await?
                .into_iter()
                .find(|c| c.control_id == id)
                .map(Frozen::Control)
        }
        _ => None,
    };
    Ok(match frozen {
        Some(frozen) if frozen.is_unresolved() => Probe::Existing(frozen),
        _ => Probe::Miss,
    })
}

/// Immutable D-188 inputs captured before the first Pricing command.
#[derive(Debug, Clone)]
pub struct AttemptInputs {
    pub prepared_draft_revision: Option<i64>,
    pub proposed_arrangement: serde_json::Value,
    pub authorization_fact_fingerprint: String,
    pub original_principal: serde_json::Value,
    pub proof_reference: Option<String>,
    pub date_policy_basis: serde_json::Value,
    pub commercial_subject_id: Uuid,
    pub commercial_subject_type: String,
    pub commercial_subject_tenant_id: Uuid,
}

fn require_new_durable<T: TransactionRunner>(
    owned: &Owned<'_, T>,
    locked: &LockedOrder<'_, T>,
) -> Result<ExecutionOwner, RegistryError> {
    // A reclaimed durable marker that was never linked (no candidate, no remote command) is
    // recoverable: its rotated owner/fence starts the execution instead of stranding the key
    // until retention expiry (Foundation §4.2: a durable expired marker stays reclaimable).
    let fresh = match owned.origin {
        ClaimOrigin::Inserted | ClaimOrigin::ReplacedExpired => true,
        ClaimOrigin::Reclaimed => owned.frozen.is_none(),
        ClaimOrigin::CurrentOwner => false,
    };
    if !fresh
        || !std::ptr::eq(owned.tx, locked.tx)
        || owned.row.order_id != Some(locked.row.order_id)
        || owned.row.attempt_id.is_some()
        || owned.row.fulfillment_control_id.is_some()
    {
        return Err(RegistryError::SettlementMismatch);
    }
    owner_of(&owned.row).ok_or(RegistryError::SettlementMismatch)
}

/// D-188 step 2 for a new execution: atomically allocate the candidate, stamp it into the
/// exact per-line request snapshots, insert the attempt and link it to the claimed marker.
/// The caller commits this short transaction before the first remote command.
///
/// # Errors
/// Not a fresh durable claim of this aggregate, candidate exhaustion or store failure.
pub async fn begin_commercial_attempt<'a, T: TransactionRunner>(
    owned: Owned<'a, T>,
    locked: &mut LockedOrder<'a, T>,
    inputs: AttemptInputs,
    line_requests: impl FnOnce(i32) -> serde_json::Value,
) -> Result<(DurableExecution, commercial_attempt::Model), RegistryError> {
    let claim_owner = require_new_durable(&owned, locked)?;
    let previous = locked.row.current_version;
    let candidate = locked.allocate_candidate_version().await?;
    let attempt = commercial_attempt::Model {
        attempt_id: Uuid::new_v4(),
        order_id: locked.row.order_id,
        candidate_version: candidate,
        previous_committed_version: previous,
        idempotency_execution_id: claim_owner.execution_id,
        operation: owned.row.operation.clone(),
        principal_scope: owned.row.principal_scope.clone(),
        request_fingerprint: owned.row.request_fingerprint.clone(),
        prepared_draft_revision: inputs.prepared_draft_revision,
        proposed_arrangement: inputs.proposed_arrangement,
        authorization_fact_fingerprint: inputs.authorization_fact_fingerprint,
        original_principal: inputs.original_principal,
        proof_reference: inputs.proof_reference,
        line_requests: line_requests(candidate),
        date_policy_basis: inputs.date_policy_basis,
        commercial_subject_id: inputs.commercial_subject_id,
        commercial_subject_type: inputs.commercial_subject_type,
        commercial_subject_tenant_id: inputs.commercial_subject_tenant_id,
        status: "prepared".to_owned(),
        owner_token: claim_owner.owner_token,
        fencing_generation: claim_owner.fencing_generation,
        lease_until: owned.row.lease_expires_at,
        receipt_results: serde_json::json!({}),
        created_at: owned.now,
        terminal_at: None,
    };
    let attempt = locked.insert_commercial_attempt(attempt).await?;
    let mut marker = owned.row.clone();
    marker.attempt_id = Some(attempt.attempt_id);
    mutable::replace_idempotency(owned.tx, &owned.scope, &owned.row, marker.clone()).await?;
    let frozen = Frozen::Commercial(attempt.clone());
    Ok((DurableExecution::from_parts(&marker, &frozen)?, attempt))
}

/// Immutable D-198 control intent and receiver inventory.
#[derive(Debug, Clone)]
pub struct ControlInputs {
    pub expected_version: i32,
    pub fulfillment_attempt_id: Uuid,
    pub generation: i64,
    pub original_actor: serde_json::Value,
    pub proof_reference: Option<String>,
    pub authorization_fact_fingerprint: String,
    pub roster: serde_json::Value,
    pub roster_digest: String,
    pub receiver_commands: serde_json::Value,
}

/// D-198 step 1: persist owned control intent, set the pending pointer (blocking new grants)
/// and link the marker. Public state, audit and event are untouched.
///
/// # Errors
/// Another control pending, not a fresh durable claim, or store failure.
pub async fn begin_fulfillment_control<'a, T: TransactionRunner>(
    owned: Owned<'a, T>,
    locked: &mut LockedOrder<'a, T>,
    inputs: ControlInputs,
) -> Result<(DurableExecution, fulfillment_control::Model), RegistryError> {
    let claim_owner = require_new_durable(&owned, locked)?;
    if locked.row.fulfillment_control_pending.is_some() {
        return Err(RegistryError::ControlPending);
    }
    let control = fulfillment_control::Model {
        control_id: Uuid::new_v4(),
        order_id: locked.row.order_id,
        idempotency_execution_id: claim_owner.execution_id,
        operation: owned.row.operation.clone(),
        request_fingerprint: owned.row.request_fingerprint.clone(),
        expected_version: inputs.expected_version,
        fulfillment_attempt_id: inputs.fulfillment_attempt_id,
        generation: inputs.generation,
        original_actor: inputs.original_actor,
        proof_reference: inputs.proof_reference,
        authorization_fact_fingerprint: inputs.authorization_fact_fingerprint,
        roster: inputs.roster,
        roster_digest: inputs.roster_digest,
        created_at: owned.now,
        status: "prepared".to_owned(),
        owner_token: claim_owner.owner_token,
        fencing_generation: claim_owner.fencing_generation,
        lease_until: owned.row.lease_expires_at,
        receiver_commands: inputs.receiver_commands,
        receiver_evidence: serde_json::json!({}),
        error_classification: None,
        terminal_at: None,
    };
    let control = locked.insert_fulfillment_control(control).await?;
    let mut order = locked.row.clone();
    order.fulfillment_control_pending = Some(control.control_id);
    let order_scope = locked.scope.clone();
    locked.replace(&order_scope, order).await?;
    let mut marker = owned.row.clone();
    marker.fulfillment_control_id = Some(control.control_id);
    mutable::replace_idempotency(owned.tx, &owned.scope, &owned.row, marker.clone()).await?;
    let frozen = Frozen::Control(control.clone());
    Ok((DurableExecution::from_parts(&marker, &frozen)?, control))
}

/// Final/continuation ownership check: the marker and its linked execution still carry this
/// exact execution, owner token and fence and remain unresolved. Returns the locked marker and
/// the frozen evidence; a stale worker gets [`RegistryError::StaleOwner`] and writes nothing.
///
/// # Errors
/// Stale/foreign owner or store failure.
pub async fn verify_owner<T: TransactionRunner>(
    locked: &LockedOrder<'_, T>,
    execution: &DurableExecution,
) -> Result<(idempotency::Model, Frozen), RegistryError> {
    if execution.order_id != locked.row.order_id {
        return Err(RegistryError::StaleOwner);
    }
    let scope = PrivateScope::for_principal(&execution.principal_scope);
    let marker = private::lock_idempotency(
        locked.tx,
        scope.access_scope(),
        &execution.operation,
        &execution.principal_scope,
        &execution.idempotency_key,
    )
    .await?
    .ok_or(RegistryError::StaleOwner)?;
    if marker.status != IN_FLIGHT || owner_of(&marker) != Some(execution.owner) {
        return Err(RegistryError::StaleOwner);
    }
    let frozen = locked
        .linked(&marker)
        .await?
        .ok_or(RegistryError::StaleOwner)?;
    if frozen.link() != execution.link
        || frozen.owner(marker.execution_id) != execution.owner
        || !frozen.is_unresolved()
    {
        return Err(RegistryError::StaleOwner);
    }
    Ok((marker, frozen))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    New,
    /// The identical first result was already recorded (lost-response replay).
    AlreadyRecorded,
}

/// D-188 step 3: record one returned per-line receipt under the current fence. Immutable
/// first results: an identical repeat is accepted, a different value is refused.
///
/// # Errors
/// Stale owner, conflicting first result or store failure.
pub async fn record_receipt<T: TransactionRunner>(
    locked: &LockedOrder<'_, T>,
    execution: &DurableExecution,
    line_key: &str,
    result: serde_json::Value,
) -> Result<Recorded, RegistryError> {
    let (_, frozen) = verify_owner(locked, execution).await?;
    let Frozen::Commercial(attempt) = frozen else {
        return Err(RegistryError::StaleOwner);
    };
    match attempt.receipt_results.get(line_key) {
        Some(existing) if *existing == result => return Ok(Recorded::AlreadyRecorded),
        Some(_) => return Err(RegistryError::FirstResultConflict),
        None => {}
    }
    let mut next = attempt.clone();
    let results = next
        .receipt_results
        .as_object_mut()
        .ok_or(RegistryRuleError::InvalidRecord)?;
    results.insert(line_key.to_owned(), result);
    "running".clone_into(&mut next.status);
    locked.replace_commercial_attempt(&attempt, next).await?;
    Ok(Recorded::New)
}

/// D-198 step 2-4 progress: append receiver commands/evidence under the current fence and
/// optionally advance the nonterminal status (never regress). Existing entries are immutable.
///
/// # Errors
/// Stale owner, conflicting first entry, invalid status or store failure.
pub async fn record_control_progress<T: TransactionRunner>(
    locked: &LockedOrder<'_, T>,
    execution: &DurableExecution,
    commands: &[(String, serde_json::Value)],
    evidence: &[(String, serde_json::Value)],
    status: Option<&str>,
) -> Result<fulfillment_control::Model, RegistryError> {
    let (_, frozen) = verify_owner(locked, execution).await?;
    let Frozen::Control(control) = frozen else {
        return Err(RegistryError::StaleOwner);
    };
    let mut next = control.clone();
    for (map, entries) in [
        (&mut next.receiver_commands, commands),
        (&mut next.receiver_evidence, evidence),
    ] {
        let map = map
            .as_object_mut()
            .ok_or(RegistryRuleError::InvalidRecord)?;
        for (key, value) in entries {
            match map.get(key) {
                Some(existing) if existing == value => {}
                Some(_) => return Err(RegistryError::FirstResultConflict),
                None => {
                    map.insert(key.clone(), value.clone());
                }
            }
        }
    }
    if let Some(status) = status {
        // Nonterminal progress only advances; terminal states belong to `finish_execution`.
        let rank = |s: &str| match s {
            "prepared" => Some(0),
            "awaiting" => Some(1),
            "barrier_ready" => Some(2),
            _ => None,
        };
        match (rank(&control.status), rank(status)) {
            (Some(from), Some(to)) if to >= from => status.clone_into(&mut next.status),
            (Some(_), Some(_)) => return Err(RegistryError::StatusRegression),
            _ => return Err(RegistryRuleError::InvalidRecord.into()),
        }
    }
    locked
        .replace_fulfillment_control(&control, next.clone())
        .await?;
    Ok(next)
}

/// Terminal outcome of a durable execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionTerminal {
    /// Attempt `committed` / control `settled`; settles the marker as success.
    Completed,
    /// Business refusal; settles the marker as the owned refusal.
    Refused,
    /// Conditional abandonment; settles the authorized refusal, candidate stays burned.
    Abandoned,
}

/// D-188 step 5-6 / D-198 step 5 final settlement: under aggregate then registry/execution
/// locks, verify the exact owner and fence, mark the execution terminal, conditionally clear
/// a matching control pointer and settle the marker in this one transaction.
///
/// # Errors
/// Stale owner, mismatched settlement or store failure.
pub async fn finish_execution<T: TransactionRunner>(
    locked: &mut LockedOrder<'_, T>,
    execution: &DurableExecution,
    terminal: ExecutionTerminal,
    settlement: Settlement,
) -> Result<idempotency::Model, RegistryError> {
    let (marker, frozen) = verify_owner(locked, execution).await?;
    if matches!(settlement, Settlement::Success { .. })
        != (terminal == ExecutionTerminal::Completed)
    {
        return Err(RegistryError::SettlementMismatch);
    }
    let scope = PrivateScope::for_principal(&execution.principal_scope);
    let now = db_now(
        locked.tx,
        scope.access_scope(),
        (
            &marker.operation,
            &marker.principal_scope,
            &marker.idempotency_key,
        ),
    )
    .await?;
    match frozen {
        Frozen::Commercial(attempt) => {
            let mut next = attempt.clone();
            match terminal {
                ExecutionTerminal::Completed => "committed",
                ExecutionTerminal::Refused => "refused",
                ExecutionTerminal::Abandoned => "abandoned",
            }
            .clone_into(&mut next.status);
            next.lease_until = None;
            next.terminal_at = Some(now);
            locked.replace_commercial_attempt(&attempt, next).await?;
        }
        Frozen::Control(control) => {
            let mut next = control.clone();
            match terminal {
                ExecutionTerminal::Completed => "settled",
                ExecutionTerminal::Refused => "refused",
                ExecutionTerminal::Abandoned => "abandoned",
            }
            .clone_into(&mut next.status);
            next.lease_until = None;
            next.terminal_at = Some(now);
            locked.replace_fulfillment_control(&control, next).await?;
            // A stale worker cannot clear a newer control's pointer.
            if locked.row.fulfillment_control_pending == Some(control.control_id) {
                let mut order = locked.row.clone();
                order.fulfillment_control_pending = None;
                let order_scope = locked.scope.clone();
                locked.replace(&order_scope, order).await?;
            }
        }
    }
    settle_row(locked.tx, scope.access_scope(), &marker, settlement).await
}
