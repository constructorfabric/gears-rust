//! The bulk Quota envelopes (`features/bulk-quota-crud.md`): every item of one
//! tenant's envelope applied in one transaction, or none, with the envelope's
//! idempotency record.
//!
//! # Lock order
//!
//! Update and deactivate first lock every target Quota row, ascending by
//! `quota_id` and whatever its status (rank 1), each under its own item's
//! scope. The envelope's idempotency stripe follows (rank 2), `NOWAIT`, and a
//! refused stripe retries the whole transaction within the contention budget.
//! The budget bounds that stripe contention only: the Quota rows, and the
//! counter and lease rows each item's single-item body locks afterwards, wait
//! exactly as single-item Quota CRUD does. Create has no Quota row to lock: its
//! stripe comes first, then the capacity rows of its distinct metrics,
//! ascending, which each insert then finds in place.
//!
//! # Order of items
//!
//! Locking in id order never becomes execution order. The prelock keeps what it
//! found per id — a row, or none — and raises nothing; items then run in
//! submission order, so the first failing item is the first in the request,
//! and a lease spanning two Quotas of one deactivation is resolved by the
//! earlier of them.
//!
//! # Record and replay
//!
//! Once the stripe is held the record is read again: a record under the
//! envelope scope is a replay, answered only when every target is still
//! visible under its item's current scope (update, deactivate) or the envelope
//! tenant still lies inside each item's scope (create). The record is keyed by
//! the PDP-authorized envelope tenant, so it is read and written unscoped, as
//! `lookup_idempotency` reads it; every Quota row stays under its item's scope.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use quota_enforcement_sdk::{
    BulkCreateEnvelope, BulkCreated, BulkCreatedItem, BulkDeactivateEnvelope, BulkDeactivated,
    BulkDeactivatedItem, BulkRecord, BulkUpdateEnvelope, BulkUpdated, BulkUpdatedItem,
    IdempotencyWrite, QuotaId, StorageError, TenantId, TransitionOutcome,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError, validate_tenant_in_scope};
use toolkit_security::AccessScope;

use super::{PreparedCreate, SqlQuotaStore, TxError, lift};
use crate::domain::ports::{Actor, StoreError};
use crate::infra::outbox::{AttemptWakes, SettleWakes};
use crate::infra::storage::consumption_store::{DEFAULT_RETENTION_SECS, scope_key_of};
use crate::infra::storage::entity::quota;
use crate::infra::storage::locking::{
    ContentionBudget, ContentionError, contention_budget_of, lock_scopes, scope_lock_not_available,
    with_budget,
};
use crate::infra::storage::quota_mapping::{self, QuotaUpdate, STATUS_ACTIVE};
use crate::infra::storage::repo::{
    RowWait, config_repo, idempotency_repo as idem_repo, lease_repo, quota_repo,
};

/// Attempts at an envelope whose record insert lost a race; the next attempt
/// finds the winner's record under the stripe and replays it.
const RACE_ATTEMPTS: usize = 2;

impl ContentionError for TxError {
    fn is_lock_not_available(&self) -> bool {
        matches!(self, Self::Scope(scope) if scope_lock_not_available(scope))
    }

    fn contention_timeout() -> Self {
        Self::Store(StoreError::ContentionTimeout)
    }

    fn missing_stripe(stripe: i32) -> Self {
        Self::Store(StoreError::Corrupt {
            operation: "lock idempotency stripe",
            detail: format!("idempotency stripe {stripe} is missing"),
        })
    }
}

/// An item's failure as the envelope's: a failure that belongs to the item is
/// lifted and attributed to it; one of the envelope as a whole — the backend,
/// the outbox, a lost race, contention — passes through unchanged.
fn at_item(operation: &'static str, index: usize, error: TxError) -> TxError {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-catch
    // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-rollback
    // @cpt-begin:cpt-cf-quota-enforcement-state-bulk-envelope:p2:inst-qbs-rollback
    // The caller returns this error from the transaction body, so the whole
    // envelope rolls back with it and no record is written.
    match error {
        TxError::Db(_)
        | TxError::Scope(ScopeError::Db(_))
        | TxError::Enqueue(_)
        | TxError::Raced
        | TxError::Store(
            StoreError::Unavailable { .. }
            | StoreError::ContentionTimeout
            | StoreError::IdempotencyPayloadMismatch
            | StoreError::BulkItem { .. },
        ) => error,
        other => TxError::Store(lift(operation, other).at_item(index)),
    }
    // @cpt-end:cpt-cf-quota-enforcement-state-bulk-envelope:p2:inst-qbs-rollback
    // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-rollback
    // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-catch
}

/// The unscoped view the envelope's record is read and written under.
fn record_scope() -> AccessScope {
    AccessScope::allow_all()
}

/// The stored outcome under `write`, or the payload mismatch when its key
/// carried other items. `None` when there is no record.
async fn stored_outcome<T: DeserializeOwned>(
    tx: &impl DBRunner,
    write: &IdempotencyWrite,
    now: OffsetDateTime,
) -> Result<Option<T>, TxError> {
    let key = scope_key_of(&write.scope);
    let Some(row) = idem_repo::find(tx, &record_scope(), &key, now, None).await? else {
        return Ok(None);
    };
    if row.payload_hash != write.payload_hash.as_bytes() {
        return Err(StoreError::IdempotencyPayloadMismatch.into());
    }
    serde_json::from_str::<BulkRecord<T>>(&row.decision_blob)
        .map(|record| Some(record.outcome))
        .map_err(|error| {
            StoreError::Corrupt {
                operation: "read bulk record",
                detail: error.to_string(),
            }
            .into()
        })
}

/// Now plus the longest idempotency retention of any of `metrics`.
async fn retention_deadline<'a>(
    tx: &impl DBRunner,
    tenant: TenantId,
    metrics: impl IntoIterator<Item = &'a str>,
    now: OffsetDateTime,
) -> Result<OffsetDateTime, TxError> {
    let tenant = tenant.as_uuid().to_string();
    let mut deadline = now;
    for metric in metrics {
        let seconds = config_repo::read_idempotency_retention(tx, &tenant, metric)
            .await?
            .unwrap_or(DEFAULT_RETENTION_SECS)
            .max(0);
        deadline = deadline.max(now + time::Duration::seconds(seconds));
    }
    Ok(deadline)
}

/// Record the committed `outcome` under the envelope key, in the transaction.
async fn record(
    tx: &impl DBRunner,
    write: &IdempotencyWrite,
    outcome: &impl Serialize,
    now: OffsetDateTime,
    expires_at: OffsetDateTime,
) -> Result<(), TxError> {
    let key = scope_key_of(&write.scope);
    let scope = record_scope();
    idem_repo::delete_expired_at_key(tx, &scope, &key, now).await?;
    let blob = serde_json::to_string(&BulkRecord::new(outcome)).map_err(|error| {
        TxError::Store(StoreError::Corrupt {
            operation: "write bulk record",
            detail: error.to_string(),
        })
    })?;
    let new = idem_repo::NewRecord {
        key,
        payload_hash: write.payload_hash.as_bytes(),
        decision_blob: blob,
        applied_entries: None,
        attribution_hash: None,
        engine_id: None,
        policy_id: None,
        policy_version: None,
        created_at: now,
        expires_at,
    };
    match idem_repo::insert(tx, &scope, &new).await? {
        idem_repo::Inserted::Yes => Ok(()),
        idem_repo::Inserted::Raced => Err(TxError::Raced),
    }
}

/// Lock every target row ascending by id, each under its item's scope and
/// whatever its status, and keep what was found per id. Nothing is raised
/// here: whether a row is missing, foreign or deactivated is decided per item,
/// in submission order.
async fn prelock(
    tx: &impl DBRunner,
    targets: &[(QuotaId, &AccessScope)],
) -> Result<HashMap<QuotaId, quota::Model>, TxError> {
    let mut ordered: BTreeMap<uuid::Uuid, &AccessScope> = BTreeMap::new();
    for (id, scope) in targets {
        ordered.entry(id.as_uuid()).or_insert(*scope);
    }
    let mut rows = HashMap::with_capacity(ordered.len());
    for (id, scope) in ordered {
        if let Some(row) = quota_repo::find_by_id(tx, scope, id, Some(RowWait::Wait)).await? {
            rows.insert(QuotaId::from(id), row);
        }
    }
    Ok(rows)
}

/// The locked row of `id` when it belongs to `tenant`; a row of another
/// tenant is not found, never reported as foreign.
fn target_of(
    rows: &HashMap<QuotaId, quota::Model>,
    tenant: TenantId,
    id: QuotaId,
) -> Result<&quota::Model, StoreError> {
    rows.get(&id)
        .filter(|row| row.tenant_id == tenant.as_uuid())
        .ok_or(StoreError::QuotaNotFound { id })
}

/// The locked active row of `id`: not found, deactivated, or the row.
fn active_target(
    rows: &HashMap<QuotaId, quota::Model>,
    tenant: TenantId,
    id: QuotaId,
) -> Result<quota::Model, StoreError> {
    let row = target_of(rows, tenant, id)?;
    if row.status != STATUS_ACTIVE {
        return Err(StoreError::QuotaDeactivated { id });
    }
    Ok(row.clone())
}

/// A replay is answered only when every target is still visible to its item.
fn check_replay_targets(
    rows: &HashMap<QuotaId, quota::Model>,
    tenant: TenantId,
    ids: impl IntoIterator<Item = QuotaId>,
) -> Result<(), StoreError> {
    for (index, id) in ids.into_iter().enumerate() {
        target_of(rows, tenant, id).map_err(|error| error.at_item(index))?;
    }
    Ok(())
}

// @cpt-algo:cpt-cf-quota-enforcement-algo-bulk-envelope:p2
// @cpt-state:cpt-cf-quota-enforcement-state-bulk-envelope:p2
// @cpt-dod:cpt-cf-quota-enforcement-dod-bulk-atomicity:p2
// @cpt-dod:cpt-cf-quota-enforcement-dod-bulk-deactivate-cascade:p2
impl SqlQuotaStore {
    /// The contention budget of the envelope: the strictest of `metrics`, or
    /// the platform default when there is none.
    async fn envelope_budget<'a>(
        &self,
        operation: &'static str,
        metrics: impl IntoIterator<Item = &'a str>,
    ) -> Result<ContentionBudget, StoreError> {
        let lift_budget = |error: StorageError| {
            tracing::warn!(target: "qe.storage", operation, error = %error, "contention budget read failed");
            StoreError::Unavailable { operation }
        };
        let mut budget: Option<ContentionBudget> = None;
        for metric in metrics {
            let one = contention_budget_of(&self.db, Some(metric))
                .await
                .map_err(lift_budget)?;
            budget = Some(budget.map_or(one, |budget| budget.stricter(one)));
        }
        match budget {
            Some(budget) => Ok(budget),
            None => contention_budget_of(&self.db, None)
                .await
                .map_err(lift_budget),
        }
    }

    /// The metrics of the targets visible under their items' scopes, read
    /// before the transaction, for the envelope's contention budget. A
    /// Quota's metric never changes, so the read takes no lock; a target it
    /// does not find adds no metric and fails under the locks as not found.
    async fn target_metrics(
        &self,
        operation: &'static str,
        targets: &[(QuotaId, &AccessScope)],
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let conn = self.conn(operation)?;
        let mut metrics = std::collections::BTreeSet::new();
        for (id, scope) in targets.iter().copied() {
            if let Some(row) = quota_repo::find_by_id(&conn, scope, id.as_uuid(), None)
                .await
                .map_err(|error| lift(operation, error.into()))?
            {
                metrics.insert(row.metric);
            }
        }
        Ok(metrics)
    }

    /// Run `attempt` as one envelope: retried on a refused stripe within the
    /// budget, and once more when its record insert lost a race.
    async fn run_envelope<T, F, Fut>(
        &self,
        operation: &'static str,
        budget: ContentionBudget,
        mut attempt: F,
    ) -> Result<T, StoreError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, TxError>>,
    {
        for _ in 0..RACE_ATTEMPTS {
            match with_budget(budget, &mut attempt).await {
                Err(TxError::Raced) => {}
                other => return other.map_err(|error| lift(operation, error)),
            }
        }
        Err(lift(operation, TxError::Raced))
    }

    /// See [`QuotaStore::bulk_create_quotas`](crate::domain::ports::QuotaStore::bulk_create_quotas).
    pub(super) async fn bulk_create(
        &self,
        actor: &Actor,
        envelope: &BulkCreateEnvelope,
    ) -> Result<TransitionOutcome<BulkCreated>, StoreError> {
        const OPERATION: &str = "bulk create quotas";
        let tenant = envelope.tenant_id;
        let now = self.now();
        let mut prepared = Vec::with_capacity(envelope.items.len());
        for (index, item) in envelope.items.iter().enumerate() {
            if item.draft.tenant_id != tenant
                || validate_tenant_in_scope(tenant.as_uuid(), &item.scope).is_err()
            {
                return Err(StoreError::SubjectOutOfScope.at_item(index));
            }
            prepared.push(
                PreparedCreate::of(&item.draft, &item.events, now)
                    .map_err(|error| lift(OPERATION, error).at_item(index))?,
            );
        }
        // Each metric's capacity row, ensured once, ascending, under the scope
        // of the first item that names it.
        let mut metrics: BTreeMap<String, AccessScope> = BTreeMap::new();
        for item in &envelope.items {
            metrics
                .entry(item.draft.metric.as_str().to_owned())
                .or_insert_with(|| item.scope.clone());
        }
        let budget = self
            .envelope_budget(OPERATION, metrics.keys().map(String::as_str))
            .await?;
        let owned = Arc::new((envelope.clone(), prepared, metrics, actor.clone()));
        self.run_envelope(OPERATION, budget, || {
            let owned = Arc::clone(&owned);
            let wakes = AttemptWakes::begin(&self.enqueuer);
            let enqueuer = wakes.enqueuer();
            self.db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let (envelope, prepared, metrics, actor) = &*owned;
                        lock_scopes::<TxError>(tx, &[&envelope.idempotency.scope]).await?;
                        if let Some(stored) =
                            stored_outcome::<BulkCreated>(tx, &envelope.idempotency, now).await?
                        {
                            for (index, item) in envelope.items.iter().enumerate() {
                                validate_tenant_in_scope(tenant.as_uuid(), &item.scope)
                                    .map_err(|_| StoreError::SubjectOutOfScope.at_item(index))?;
                            }
                            return Ok(TransitionOutcome::NoOp(stored));
                        }
                        for (metric, scope) in metrics {
                            lease_repo::ensure_capacity_row(
                                tx,
                                scope,
                                tenant.as_uuid(),
                                metric,
                                now,
                            )
                            .await?;
                        }
                        let mut items = Vec::with_capacity(envelope.items.len());
                        for (index, (item, prepared)) in
                            envelope.items.iter().zip(prepared).enumerate()
                        {
                            let quota_id = prepared.id;
                            Self::insert_prepared(
                                tx,
                                &item.scope,
                                actor,
                                prepared.clone(),
                                now,
                                &*enqueuer,
                            )
                            .await
                            .map_err(|error| at_item(OPERATION, index, error))?;
                            items.push(BulkCreatedItem {
                                index,
                                idempotency_key: item.idempotency_key.clone(),
                                quota_id,
                            });
                        }
                        let outcome = BulkCreated { items };
                        let expires_at =
                            retention_deadline(tx, tenant, metrics.keys().map(String::as_str), now)
                                .await?;
                        record(tx, &envelope.idempotency, &outcome, now, expires_at).await?;
                        Ok(TransitionOutcome::Applied(outcome))
                    })
                })
                .settling(wakes)
        })
        .await
    }

    /// See [`QuotaStore::bulk_update_quotas`](crate::domain::ports::QuotaStore::bulk_update_quotas).
    pub(super) async fn bulk_update(
        &self,
        actor: &Actor,
        envelope: &BulkUpdateEnvelope,
    ) -> Result<TransitionOutcome<BulkUpdated>, StoreError> {
        const OPERATION: &str = "bulk update quotas";
        let tenant = envelope.tenant_id;
        let mut updates = Vec::with_capacity(envelope.items.len());
        for (index, item) in envelope.items.iter().enumerate() {
            updates.push(
                quota_mapping::patch_to_update(&item.patch)
                    .map_err(|error| lift(OPERATION, error.into()).at_item(index))?,
            );
        }
        let metrics = self
            .target_metrics(
                OPERATION,
                &envelope
                    .items
                    .iter()
                    .map(|item| (item.quota_id, &item.scope))
                    .collect::<Vec<_>>(),
            )
            .await?;
        let budget = self
            .envelope_budget(OPERATION, metrics.iter().map(String::as_str))
            .await?;
        let owned: Arc<(BulkUpdateEnvelope, Vec<QuotaUpdate>, Actor)> =
            Arc::new((envelope.clone(), updates, actor.clone()));
        let clock = Arc::clone(&self.clock);
        self.run_envelope(OPERATION, budget, || {
            let owned = Arc::clone(&owned);
            let clock = Arc::clone(&clock);
            let wakes = AttemptWakes::begin(&self.enqueuer);
            let enqueuer = wakes.enqueuer();
            self.db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let (envelope, updates, actor) = &*owned;
                        let targets: Vec<(QuotaId, &AccessScope)> = envelope
                            .items
                            .iter()
                            .map(|item| (item.quota_id, &item.scope))
                            .collect();
                        let rows = prelock(tx, &targets).await?;
                        lock_scopes::<TxError>(tx, &[&envelope.idempotency.scope]).await?;
                        let now = clock();
                        if let Some(stored) =
                            stored_outcome::<BulkUpdated>(tx, &envelope.idempotency, now).await?
                        {
                            check_replay_targets(
                                &rows,
                                tenant,
                                envelope.items.iter().map(|item| item.quota_id),
                            )?;
                            return Ok(TransitionOutcome::NoOp(stored));
                        }
                        let mut items = Vec::with_capacity(envelope.items.len());
                        let mut metrics = Vec::with_capacity(envelope.items.len());
                        for (index, (item, update)) in
                            envelope.items.iter().zip(updates).enumerate()
                        {
                            let row = active_target(&rows, tenant, item.quota_id)
                                .map_err(|error| error.at_item(index))?;
                            metrics.push(row.metric.clone());
                            let patched = Self::update_locked(
                                tx,
                                &item.scope,
                                actor,
                                row,
                                update,
                                &item.events,
                                &clock,
                                &*enqueuer,
                            )
                            .await
                            .map_err(|error| at_item(OPERATION, index, error))?;
                            items.push(BulkUpdatedItem {
                                index,
                                idempotency_key: item.idempotency_key.clone(),
                                quota_id: item.quota_id,
                                record_version: patched.record_version,
                            });
                        }
                        let outcome = BulkUpdated { items };
                        let expires_at =
                            retention_deadline(tx, tenant, metrics.iter().map(String::as_str), now)
                                .await?;
                        record(tx, &envelope.idempotency, &outcome, now, expires_at).await?;
                        Ok(TransitionOutcome::Applied(outcome))
                    })
                })
                .settling(wakes)
        })
        .await
    }

    /// See [`QuotaStore::bulk_deactivate_quotas`](crate::domain::ports::QuotaStore::bulk_deactivate_quotas).
    pub(super) async fn bulk_deactivate(
        &self,
        actor: &Actor,
        envelope: &BulkDeactivateEnvelope,
    ) -> Result<TransitionOutcome<BulkDeactivated>, StoreError> {
        const OPERATION: &str = "bulk deactivate quotas";
        let tenant = envelope.tenant_id;
        let now = self.now();
        let metrics = self
            .target_metrics(
                OPERATION,
                &envelope
                    .items
                    .iter()
                    .map(|item| (item.quota_id, &item.scope))
                    .collect::<Vec<_>>(),
            )
            .await?;
        let budget = self
            .envelope_budget(OPERATION, metrics.iter().map(String::as_str))
            .await?;
        let owned: Arc<(BulkDeactivateEnvelope, Actor)> =
            Arc::new((envelope.clone(), actor.clone()));
        self.run_envelope(OPERATION, budget, || {
            let owned = Arc::clone(&owned);
            let wakes = AttemptWakes::begin(&self.enqueuer);
            let enqueuer = wakes.enqueuer();
            self.db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let (envelope, actor) = &*owned;
                        let targets: Vec<(QuotaId, &AccessScope)> = envelope
                            .items
                            .iter()
                            .map(|item| (item.quota_id, &item.scope))
                            .collect();
                        let rows = prelock(tx, &targets).await?;
                        lock_scopes::<TxError>(tx, &[&envelope.idempotency.scope]).await?;
                        if let Some(stored) =
                            stored_outcome::<BulkDeactivated>(tx, &envelope.idempotency, now)
                                .await?
                        {
                            check_replay_targets(
                                &rows,
                                tenant,
                                envelope.items.iter().map(|item| item.quota_id),
                            )?;
                            return Ok(TransitionOutcome::NoOp(stored));
                        }
                        // @cpt-begin:cpt-cf-quota-enforcement-state-bulk-envelope:p2:inst-qbs-enter
                        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-apply
                        let mut items = Vec::with_capacity(envelope.items.len());
                        let mut metrics = Vec::with_capacity(envelope.items.len());
                        for (index, item) in envelope.items.iter().enumerate() {
                            let row = active_target(&rows, tenant, item.quota_id)
                                .map_err(|error| error.at_item(index))?;
                            metrics.push(row.metric.clone());
                            let outcome = Self::deactivate_locked(
                                tx,
                                &item.scope,
                                actor,
                                row,
                                &item.events,
                                now,
                                &*enqueuer,
                            )
                            .await
                            .map_err(|error| at_item(OPERATION, index, error))?;
                            items.push(BulkDeactivatedItem {
                                index,
                                idempotency_key: item.idempotency_key.clone(),
                                quota_id: item.quota_id,
                                resolved_leases: outcome.resolved_leases,
                            });
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-apply
                        // @cpt-end:cpt-cf-quota-enforcement-state-bulk-envelope:p2:inst-qbs-enter
                        // @cpt-begin:cpt-cf-quota-enforcement-state-bulk-envelope:p2:inst-qbs-commit
                        // @cpt-begin:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-return
                        let outcome = BulkDeactivated { items };
                        let expires_at =
                            retention_deadline(tx, tenant, metrics.iter().map(String::as_str), now)
                                .await?;
                        record(tx, &envelope.idempotency, &outcome, now, expires_at).await?;
                        Ok(TransitionOutcome::Applied(outcome))
                        // @cpt-end:cpt-cf-quota-enforcement-algo-bulk-envelope:p2:inst-qbe-return
                        // @cpt-end:cpt-cf-quota-enforcement-state-bulk-envelope:p2:inst-qbs-commit
                    })
                })
                .settling(wakes)
        })
        .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "quota_bulk_tests.rs"]
mod quota_bulk_tests;
