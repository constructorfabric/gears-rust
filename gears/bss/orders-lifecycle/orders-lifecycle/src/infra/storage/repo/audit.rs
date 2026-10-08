//! Transactional audit writer (S2-06): the only production path into
//! `bss_orders__transition_audit`.
//!
//! Every append takes the caller's sealed transaction, so the audit row, the aggregate counter
//! and the business mutation commit or roll back together (DESIGN §4.4; Pricing's scoped
//! same-transaction insert is the precedent for persistence only, not for sealing). Committed
//! entries allocate the next contiguous `audit_sequence` from the locked aggregate (never a
//! database sequence and never the sparse commercial version) and link to genesis or the stored
//! predecessor digest. Refusals take no sequence and no aggregate lock beyond what the caller
//! already holds; unresolved refusals perform no target lookup at all (D-98).
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt};
use uuid::Uuid;

use super::{LockedOrder, TransactionRunner, private};
use crate::domain::audit::{
    AuditError, AuditRow, DIGEST_LEN, Digest, ForceRequestObservation, OrderFacts,
    PendingCommitted, SealedAudit, genesis, parse_state, state_token,
};
use crate::infra::storage::entity::{order, transition_audit};
use crate::infra::storage::scoped::PrivateScope;

/// Audit append failure. Every variant aborts the caller's transition transaction.
#[derive(Debug, thiserror::Error)]
pub enum AuditStoreError {
    #[error(transparent)]
    Rule(#[from] AuditError),
    #[error("audit store: {0}")]
    Store(#[from] ScopeError),
    /// The stored predecessor needed to extend the chain is missing or malformed: never invent
    /// a predecessor, repair, or restart the chain.
    #[error("committed audit predecessor missing or malformed at sequence {0}")]
    ChainBroken(i64),
    /// The entry was built for another aggregate, namespace or observed state.
    #[error("audit entry does not match the locked aggregate")]
    Foreign,
}

/// Facts of a locked (or freshly inserted) aggregate row.
///
/// # Errors
/// `Shape` for an unregistered stored state.
pub fn order_facts(row: &order::Model) -> Result<OrderFacts, AuditError> {
    Ok(OrderFacts {
        order_id: row.order_id,
        audit_tenant_id: row.audit_tenant_id,
        resource_tenant_id: row.resource_tenant_id,
        state: parse_state(&row.state)?,
        version: row.current_version,
        audit_sequence: row.audit_sequence,
    })
}
impl<T: TransactionRunner> LockedOrder<'_, T> {
    /// Audit facts observed under this transaction's row lock.
    ///
    /// # Errors
    /// `Shape` for an unregistered stored state.
    pub(crate) fn audit_facts(&self) -> Result<OrderFacts, AuditError> {
        order_facts(self.row())
    }
}

/// Persisted model of a sealed entry; every column, no defaults.
#[must_use]
pub fn to_model(sealed: SealedAudit) -> transition_audit::Model {
    let (row, entry_hash) = sealed.into_parts();
    let AuditRow {
        hash_version,
        audit_id,
        audit_tenant_id,
        subject_tenant_id,
        resource_tenant_id,
        order_id,
        requested_order_ref,
        sequence,
        from_state,
        to_state,
        trigger,
        outcome,
        actor,
        actor_class,
        delegation_proof_ref,
        reason,
        changed_field,
        prior_value,
        new_value,
        idempotency_key,
        correlation_id,
        version,
        created_at,
        prev_hash,
        caller_reason,
        force_request_observation,
    } = row;
    transition_audit::Model {
        audit_id,
        hash_version,
        audit_tenant_id,
        subject_tenant_id,
        resource_tenant_id,
        order_id,
        requested_order_ref,
        sequence,
        prev_hash,
        entry_hash: entry_hash.to_vec(),
        from_state,
        to_state,
        trigger,
        outcome,
        actor,
        actor_class,
        delegation_proof_ref,
        reason,
        caller_reason,
        force_request_observation: force_request_observation
            .as_ref()
            .map(ForceRequestObservation::to_json),
        changed_field,
        prior_value,
        new_value,
        idempotency_key,
        correlation_id,
        version,
        created_at,
    }
}

/// Stored row and its stored digest, for verification. Nothing is normalized or repaired.
///
/// # Errors
/// `Shape` for a malformed stored observation object.
pub fn from_model(model: &transition_audit::Model) -> Result<(AuditRow, Vec<u8>), AuditError> {
    let transition_audit::Model {
        audit_id,
        hash_version,
        audit_tenant_id,
        subject_tenant_id,
        resource_tenant_id,
        order_id,
        requested_order_ref,
        sequence,
        prev_hash,
        entry_hash,
        from_state,
        to_state,
        trigger,
        outcome,
        actor,
        actor_class,
        delegation_proof_ref,
        reason,
        caller_reason,
        force_request_observation,
        changed_field,
        prior_value,
        new_value,
        idempotency_key,
        correlation_id,
        version,
        created_at,
    } = model.clone();
    let row = AuditRow {
        hash_version,
        audit_id,
        audit_tenant_id,
        subject_tenant_id,
        resource_tenant_id,
        order_id,
        requested_order_ref,
        sequence,
        from_state,
        to_state,
        trigger,
        outcome,
        actor,
        actor_class,
        delegation_proof_ref,
        reason,
        changed_field,
        prior_value,
        new_value,
        idempotency_key,
        correlation_id,
        version,
        created_at,
        prev_hash,
        caller_reason,
        force_request_observation: force_request_observation
            .as_ref()
            .map(ForceRequestObservation::from_json)
            .transpose()?,
    };
    Ok((row, entry_hash))
}

fn observed_matches(row: &AuditRow, facts: &OrderFacts) -> bool {
    row.order_id == Some(facts.order_id)
        && row.audit_tenant_id == Some(facts.audit_tenant_id)
        && row.resource_tenant_id == Some(facts.resource_tenant_id)
        && row.to_state.as_deref() == Some(state_token(facts.state).as_str())
        && row.version == Some(facts.version)
}

async fn predecessor(
    runner: &impl TransactionRunner,
    scope: &AccessScope,
    order_id: Uuid,
    audit_tenant_id: Uuid,
    sequence: i64,
) -> Result<Digest, AuditStoreError> {
    if sequence == 1 {
        return Ok(genesis(audit_tenant_id, order_id));
    }
    let previous = sequence - 1;
    let row = transition_audit::Entity::find()
        .filter(transition_audit::Column::OrderId.eq(order_id))
        .filter(transition_audit::Column::Sequence.eq(previous))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await?
        .ok_or(AuditStoreError::ChainBroken(previous))?;
    if row.outcome != "committed"
        || row.audit_tenant_id != Some(audit_tenant_id)
        || row.entry_hash.len() != DIGEST_LEN
    {
        return Err(AuditStoreError::ChainBroken(previous));
    }
    let mut digest = [0u8; DIGEST_LEN];
    digest.copy_from_slice(&row.entry_hash);
    Ok(digest)
}

/// Append one committed entry on the locked aggregate, after the business mutation has been
/// applied to it in this transaction. Allocates `audit_sequence + 1`, links genesis (sequence 1)
/// or the stored predecessor digest, inserts the sealed row and advances the counter.
///
/// # Errors
/// Entry/aggregate mismatch, exhausted counter, broken predecessor, scope or store failure. The
/// caller must abort its transaction: an unaudited transition is not a permitted outcome.
pub async fn append_committed<T: TransactionRunner>(
    locked: &mut LockedOrder<'_, T>,
    pending: PendingCommitted,
) -> Result<SealedAudit, AuditStoreError> {
    let facts = locked.audit_facts()?;
    let scope = PrivateScope::for_locked_order(locked);
    // Namespace is the immutable aggregate field frozen at create, never the resource tenant.
    if pending.order_id() != Some(facts.order_id)
        || pending.audit_tenant_id() != Some(facts.audit_tenant_id)
    {
        return Err(AuditStoreError::Foreign);
    }
    let sequence = facts
        .audit_sequence
        .checked_add(1)
        .ok_or(AuditError::SequenceExhausted)?;
    let prev = predecessor(
        locked.tx,
        scope.access_scope(),
        facts.order_id,
        facts.audit_tenant_id,
        sequence,
    )
    .await?;
    let sealed = pending.seal(sequence, prev)?;
    if !observed_matches(sealed.row(), &facts) {
        return Err(AuditStoreError::Foreign);
    }
    private::insert_transition_audit(locked.tx, scope.access_scope(), to_model(sealed.clone()))
        .await?;
    locked.advance_audit_sequence(sequence).await?;
    Ok(sealed)
}

/// Append several committed entries (one per changed administrative field, D-117) at
/// consecutive sequences; the caller settles idempotency with the last.
///
/// # Errors
/// As [`append_committed`]; nothing is partially kept once the caller aborts.
pub async fn append_committed_all<T: TransactionRunner>(
    locked: &mut LockedOrder<'_, T>,
    pending: Vec<PendingCommitted>,
) -> Result<Vec<SealedAudit>, AuditStoreError> {
    let mut out = Vec::with_capacity(pending.len());
    for entry in pending {
        out.push(append_committed(locked, entry).await?);
    }
    Ok(out)
}

/// Append a resolved business refusal observed on the locked aggregate. It takes no sequence
/// and does not advance the counter.
///
/// # Errors
/// Entry/aggregate mismatch, scope or store failure.
pub async fn append_resolved_refusal<T: TransactionRunner>(
    locked: &LockedOrder<'_, T>,
    sealed: SealedAudit,
) -> Result<SealedAudit, AuditStoreError> {
    let facts = locked.audit_facts()?;
    let row = sealed.row();
    if row.outcome != "refused"
        || !observed_matches(row, &facts)
        || row.from_state != row.to_state
        || row
            .force_request_observation
            .as_ref()
            .is_some_and(|o| o.audit_sequence != facts.audit_sequence)
    {
        return Err(AuditStoreError::Foreign);
    }
    let scope = PrivateScope::for_locked_order(locked);
    private::insert_transition_audit(locked.tx, scope.access_scope(), to_model(sealed.clone()))
        .await?;
    Ok(sealed)
}

/// Append an unresolved refusal (early authorization denial, unknown target, refused create)
/// under the authenticated subject-tenant scope (D-104). No aggregate lookup, lock or
/// enrichment happens here; the caller must not have performed one either.
///
/// # Errors
/// A resolved entry, a subject-tenant scope mismatch, or a store failure.
pub async fn append_unresolved_refusal<T: TransactionRunner>(
    tx: &T,
    scope: &PrivateScope,
    sealed: SealedAudit,
) -> Result<SealedAudit, AuditStoreError> {
    let row = sealed.row();
    if row.outcome != "refused" || row.order_id.is_some() {
        return Err(AuditStoreError::Foreign);
    }
    private::insert_transition_audit(tx, scope.access_scope(), to_model(sealed.clone())).await?;
    Ok(sealed)
}

/// Maximum committed-chain page for verification.
pub const MAX_CHAIN_PAGE: u64 = 500;

/// One ascending page of an order's committed chain after `after_sequence`, for the read-only
/// verifier (S2-11 runs it under the verifier role). Refusals are excluded.
///
/// # Errors
/// Invalid page size, malformed stored observation, scope or store failure.
pub async fn committed_chain_page(
    runner: &impl DBRunner,
    scope: &AccessScope,
    order_id: Uuid,
    after_sequence: i64,
    limit: u64,
) -> Result<Vec<(AuditRow, Vec<u8>)>, AuditStoreError> {
    if limit == 0 || limit > MAX_CHAIN_PAGE {
        return Err(ScopeError::Invalid("chain page size must be 1..=500").into());
    }
    let rows = transition_audit::Entity::find()
        .filter(transition_audit::Column::OrderId.eq(order_id))
        .filter(transition_audit::Column::Outcome.eq("committed"))
        .filter(transition_audit::Column::Sequence.gt(after_sequence))
        .order_by_asc(transition_audit::Column::Sequence)
        .limit(limit)
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?;
    Ok(rows.iter().map(from_model).collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------------------------
// Namespace reconciliation and checkpoint reads (S2-11; DESIGN D-100). Every read takes the
// discovered audit-namespace target (D-184): the aggregate and audit stores are scoped by the
// immutable `audit_tenant_id`, the checkpoint stores by their namespace tenant column. Nothing
// here writes; the checkpoint append goes through `repo::private`.

use crate::infra::maintenance::TargetScope;
use crate::infra::storage::entity::{audit_checkpoint, audit_checkpoint_member};
use sea_orm::sea_query::{Expr, ExprTrait, IntoCondition};
use sea_orm::{JoinType, RelationTrait};

/// Page bound for namespace inventory, member and checkpoint reads (bounded memory per pass).
pub const MAX_NAMESPACE_PAGE: u64 = 500;

fn namespace_scopes(target: &TargetScope) -> Result<(AccessScope, &AccessScope), AuditStoreError> {
    let evidence = target
        .namespace_evidence_scope()
        .ok_or(ScopeError::Invalid("not an audit namespace target"))?;
    Ok((evidence, target.access_scope()))
}
fn page(limit: u64) -> Result<u64, AuditStoreError> {
    if limit == 0 || limit > MAX_NAMESPACE_PAGE {
        return Err(ScopeError::Invalid("namespace page size must be 1..=500").into());
    }
    Ok(limit)
}

/// One live order of the namespace with its aggregate audit counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct NamespaceOrder {
    pub order_id: Uuid,
    pub audit_sequence: i64,
}

/// Live orders of the namespace in ascending binary `order_id` order after the `after` keyset
/// cursor. PostgreSQL orders `uuid` bytewise, which is the D-100 member order.
///
/// # Errors
/// Non-namespace target, invalid page, scope or store failure.
pub async fn namespace_orders_page(
    runner: &impl DBRunner,
    target: &TargetScope,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<NamespaceOrder>, AuditStoreError> {
    let (evidence, _) = namespace_scopes(target)?;
    let limit = page(limit)?;
    let mut query = order::Entity::find();
    if let Some(after) = after {
        query = query.filter(order::Column::OrderId.gt(after));
    }
    Ok(query
        .order_by_asc(order::Column::OrderId)
        .limit(limit)
        .secure()
        .scope_with(&evidence)
        .project_all(runner, |q| {
            q.select_only()
                .column(order::Column::OrderId)
                .column(order::Column::AuditSequence)
                .into_model::<NamespaceOrder>()
        })
        .await?)
}

/// How many live orders of the namespace carry committed evidence (`audit_sequence > 0`):
/// the member count a checkpoint of this snapshot must reach exactly.
///
/// # Errors
/// Non-namespace target, scope or store failure.
pub async fn namespace_member_count(
    runner: &impl DBRunner,
    target: &TargetScope,
) -> Result<u64, AuditStoreError> {
    let (evidence, _) = namespace_scopes(target)?;
    Ok(order::Entity::find()
        .filter(order::Column::AuditSequence.gt(0))
        .secure()
        .scope_with(&evidence)
        .count(runner)
        .await?)
}

/// A stored committed chain position.
#[derive(Debug, Clone, PartialEq, Eq, sea_orm::FromQueryResult)]
pub struct ChainHeadRow {
    pub order_id: Uuid,
    pub sequence: i64,
    pub entry_hash: Vec<u8>,
}

/// The committed rows at exactly the given `(order_id, sequence)` positions of this namespace
/// (one batched tuple lookup through the `(order_id, sequence)` unique index).
///
/// # Errors
/// Non-namespace target, more positions than a page, scope or store failure.
pub async fn chain_heads(
    runner: &impl DBRunner,
    target: &TargetScope,
    positions: &[(Uuid, i64)],
) -> Result<Vec<ChainHeadRow>, AuditStoreError> {
    let (evidence, _) = namespace_scopes(target)?;
    if positions.is_empty() {
        return Ok(Vec::new());
    }
    page(u64::try_from(positions.len()).unwrap_or(u64::MAX))?;
    let tuples = positions
        .iter()
        .map(|(order_id, sequence)| (*order_id, *sequence))
        .collect::<Vec<_>>();
    Ok(transition_audit::Entity::find()
        .filter(transition_audit::Column::Outcome.eq("committed"))
        .filter(
            Expr::tuple([
                Expr::col(transition_audit::Column::OrderId),
                Expr::col(transition_audit::Column::Sequence),
            ])
            .in_tuples(tuples),
        )
        .order_by_asc(transition_audit::Column::OrderId)
        .secure()
        .scope_with(&evidence)
        .project_all(runner, |q| {
            q.select_only()
                .column(transition_audit::Column::OrderId)
                .column(transition_audit::Column::Sequence)
                .column(transition_audit::Column::EntryHash)
                .into_model::<ChainHeadRow>()
        })
        .await?)
}

/// Committed rows of the given orders whose sequence exceeds their aggregate counter: a
/// counter rolled back below its chain, or evidence appended outside the sealed writer. The
/// join probes each order's `(order_id, sequence)` index range above its counter, so an intact
/// namespace returns nothing without reading its chains.
///
/// # Errors
/// Non-namespace target, more orders than a page, scope or store failure.
pub async fn overflow_rows(
    runner: &impl DBRunner,
    target: &TargetScope,
    orders: &[Uuid],
) -> Result<Vec<NamespaceOrder>, AuditStoreError> {
    let (evidence, _) = namespace_scopes(target)?;
    if orders.is_empty() {
        return Ok(Vec::new());
    }
    page(u64::try_from(orders.len()).unwrap_or(u64::MAX))?;
    Ok(transition_audit::Entity::find()
        .join(
            JoinType::InnerJoin,
            transition_audit::Relation::Order
                .def()
                .on_condition(|audit, aggregate| {
                    Expr::col((audit, transition_audit::Column::Sequence))
                        .gt(Expr::col((aggregate, order::Column::AuditSequence)))
                        .into_condition()
                }),
        )
        .filter(transition_audit::Column::Outcome.eq("committed"))
        .filter(transition_audit::Column::OrderId.is_in(orders.iter().copied()))
        .order_by_asc(transition_audit::Column::OrderId)
        .order_by_asc(transition_audit::Column::Sequence)
        .secure()
        .scope_with(&evidence)
        .and_scope_for::<order::Entity>(&evidence)
        .project_all(runner, |q| {
            q.select_only()
                .column(transition_audit::Column::OrderId)
                .column_as(transition_audit::Column::Sequence, "audit_sequence")
                .into_model::<NamespaceOrder>()
        })
        .await?)
}

/// The stored digest of the committed entry at one position, for prior-prefix reconciliation.
///
/// # Errors
/// Non-namespace target, scope or store failure.
pub async fn entry_hash_at(
    runner: &impl DBRunner,
    target: &TargetScope,
    order_id: Uuid,
    sequence: i64,
) -> Result<Option<Vec<u8>>, AuditStoreError> {
    Ok(chain_heads(runner, target, &[(order_id, sequence)])
        .await?
        .into_iter()
        .next()
        .map(|row| row.entry_hash))
}

/// The namespace's latest checkpoint header, if any.
///
/// # Errors
/// Non-namespace target, scope or store failure.
pub async fn latest_checkpoint(
    runner: &impl DBRunner,
    target: &TargetScope,
) -> Result<Option<audit_checkpoint::Model>, AuditStoreError> {
    let (_, namespace) = namespace_scopes(target)?;
    Ok(audit_checkpoint::Entity::find()
        .order_by_desc(audit_checkpoint::Column::CheckpointSequence)
        .limit(1)
        .secure()
        .scope_with(namespace)
        .one(runner)
        .await?)
}

/// Checkpoint headers after `after_sequence`, ascending.
///
/// # Errors
/// Non-namespace target, invalid page, scope or store failure.
pub async fn checkpoints_page(
    runner: &impl DBRunner,
    target: &TargetScope,
    after_sequence: i64,
    limit: u64,
) -> Result<Vec<audit_checkpoint::Model>, AuditStoreError> {
    let (_, namespace) = namespace_scopes(target)?;
    let limit = page(limit)?;
    Ok(audit_checkpoint::Entity::find()
        .filter(audit_checkpoint::Column::CheckpointSequence.gt(after_sequence))
        .order_by_asc(audit_checkpoint::Column::CheckpointSequence)
        .limit(limit)
        .secure()
        .scope_with(namespace)
        .all(runner)
        .await?)
}

/// Members of one checkpoint in ascending binary `order_id` order after the keyset cursor.
///
/// # Errors
/// Non-namespace target, invalid page, scope or store failure.
pub async fn checkpoint_members_page(
    runner: &impl DBRunner,
    target: &TargetScope,
    checkpoint_sequence: i64,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<audit_checkpoint_member::Model>, AuditStoreError> {
    let (_, namespace) = namespace_scopes(target)?;
    let limit = page(limit)?;
    let mut query = audit_checkpoint_member::Entity::find()
        .filter(audit_checkpoint_member::Column::CheckpointSequence.eq(checkpoint_sequence));
    if let Some(after) = after {
        query = query.filter(audit_checkpoint_member::Column::OrderId.gt(after));
    }
    Ok(query
        .order_by_asc(audit_checkpoint_member::Column::OrderId)
        .limit(limit)
        .secure()
        .scope_with(namespace)
        .all(runner)
        .await?)
}

/// Every checkpoint's recorded position of one order, ascending by checkpoint sequence, so the
/// verifier can compare each recorded prefix with the recomputed chain.
///
/// # Errors
/// Non-namespace target, invalid page, scope or store failure.
pub async fn members_for_order(
    runner: &impl DBRunner,
    target: &TargetScope,
    order_id: Uuid,
    after_sequence: i64,
    limit: u64,
) -> Result<Vec<audit_checkpoint_member::Model>, AuditStoreError> {
    let (_, namespace) = namespace_scopes(target)?;
    let limit = page(limit)?;
    Ok(audit_checkpoint_member::Entity::find()
        .filter(audit_checkpoint_member::Column::OrderId.eq(order_id))
        .filter(audit_checkpoint_member::Column::CheckpointSequence.gt(after_sequence))
        .order_by_asc(audit_checkpoint_member::Column::CheckpointSequence)
        .limit(limit)
        .secure()
        .scope_with(namespace)
        .all(runner)
        .await?)
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ClockRow {
    observed_at: time::OffsetDateTime,
}

/// Fresh database wall-clock time through the namespace's checkpoint scope: the checkpoint's
/// `captured_at` comes from the database, not a replica clock. One row even for an empty table.
///
/// # Errors
/// Non-namespace target, scope or store failure.
pub async fn namespace_clock(
    runner: &impl DBRunner,
    target: &TargetScope,
) -> Result<time::OffsetDateTime, AuditStoreError> {
    let (_, namespace) = namespace_scopes(target)?;
    let rows = audit_checkpoint::Entity::find()
        .secure()
        .scope_with(namespace)
        .project_all(runner, |q| {
            q.select_only()
                .column_as(Expr::cust("clock_timestamp()"), "observed_at")
                .column_as(Expr::cust("count(*)"), "n")
                .into_model::<ClockRow>()
        })
        .await?;
    rows.first()
        .map(|r| r.observed_at)
        .ok_or_else(|| ScopeError::Invalid("database clock unavailable").into())
}
