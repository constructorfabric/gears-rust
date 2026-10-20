// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! The write-intent journal and the cleanup debts (ADR-0006), plus the
//! helpers the write transactions in [`super::writes`] share: retiring an
//! intent, deciding what a lost write leaves behind, recording debts in the
//! open transaction and the heal statements.
//!
//! `lease_until` is produced and compared by the DATABASE clock on both
//! backends (`now()` on `PostgreSQL`; an ISO-8601 `strftime` on `SQLite`,
//! where timestamps are TEXT), never bound from the process, so instances
//! with skewed clocks agree on when a lease is over.
//!
//! Nothing here scans: both tables are read by point lookups on
//! `(tenant_id, record_id)` or `(tenant_id, reference)`, and nothing counts.

use std::collections::HashSet;
use std::time::Duration;

use credstore_sdk::{SecretRef, StoreKey, TenantId, ValueVersion};
use sea_orm::sea_query::{Expr, Query, SelectStatement, SimpleExpr};
use sea_orm::{
    ActiveValue, ColumnTrait, Condition, DbBackend, EntityTrait, ExprTrait, QueryFilter,
};
use toolkit_db::secure::{
    DBRunner, DbTx, SecureDeleteExt, SecureEntityExt, SecureInsertExt, secure_insert_from_select,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::cleanup::{DeadContent, cleanup_tasks};
use crate::domain::secret::model::{CleanupDebt, CleanupTask, HealedCreates, WriteAttempt};
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, TxFuture, db_now_sql, entity_to_debt, map_scope_err, task_to_columns,
};

/// The database's current instant, in the column's own format.
pub(super) fn db_now(backend: DbBackend) -> SimpleExpr {
    Expr::cust(db_now_sql(backend))
}

/// The database's current instant plus `lease`, in the column's own format.
fn db_now_plus(backend: DbBackend, lease: Duration) -> SimpleExpr {
    match backend {
        DbBackend::Sqlite => Expr::cust_with_values(
            "strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
            [format!("+{:.3} seconds", lease.as_secs_f64())],
        ),
        // `PostgreSQL` custom expressions number their values (`$1`); `SQLite`
        // uses `?`.
        _ => Expr::cust_with_values(
            "now() + make_interval(0, 0, 0, 0, 0, 0, $1)",
            [lease.as_secs_f64()],
        ),
    }
}

/// The columns [`begin_intent_source`] fills, in order.
const INTENT_COLUMNS: [entity::write_intents::Column; 5] = [
    entity::write_intents::Column::AttemptId,
    entity::write_intents::Column::TenantId,
    entity::write_intents::Column::RecordId,
    entity::write_intents::Column::Reference,
    entity::write_intents::Column::LeaseUntil,
];

/// The row tx0 inserts: the attempt's identity, key and reference, and
/// `lease_until` computed by the database.
pub(super) fn begin_intent_source(
    backend: DbBackend,
    attempt: &WriteAttempt,
    lease: Duration,
) -> SelectStatement {
    Query::select()
        .expr(Expr::value(attempt.attempt_id))
        .expr(Expr::value(attempt.key.tenant_id.0))
        .expr(Expr::value(attempt.key.record_id))
        .expr(Expr::value(attempt.reference.clone()))
        .expr(db_now_plus(backend, lease))
        .to_owned()
}

/// tx0's statement inside `tx`: `INSERT` the attempt's intent with
/// `lease_until = now() + lease` on the database clock, as ONE statement.
pub(super) async fn begin_intent_tx(
    tx: &DbTx<'_>,
    backend: DbBackend,
    attempt: &WriteAttempt,
    lease: Duration,
) -> Result<(), DomainError> {
    secure_insert_from_select::<entity::write_intents::Entity, _>(
        INTENT_COLUMNS,
        begin_intent_source(backend, attempt, lease),
        &AccessScope::allow_all(),
        tx,
    )
    .await
    .map_err(map_scope_err)?;
    Ok(())
}

/// tx0: announces the attempt in a transaction of its own.
pub(super) async fn begin_write_intent(
    repo: &SecretRepoImpl,
    attempt: &WriteAttempt,
    lease: Duration,
) -> Result<(), DomainError> {
    let backend = repo.db.db().backend();
    let attempt = attempt.clone();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let attempt = attempt.clone();
        Box::pin(async move { begin_intent_tx(tx, backend, &attempt, lease).await })
            as TxFuture<'_, ()>
    })
    .await
}

/// Deletes the attempt's intent inside `tx`. `true` iff it existed (the
/// delete affected exactly one row); `false` means it was healed.
pub(super) async fn delete_intent_tx(tx: &DbTx<'_>, attempt_id: Uuid) -> Result<bool, DomainError> {
    let rows_affected = entity::write_intents::Entity::delete_many()
        .filter(Condition::all().add(entity::write_intents::Column::AttemptId.eq(attempt_id)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(tx)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    Ok(rows_affected == 1)
}

/// Heal of a live record, inside the commit transaction of its next secret
/// write: deletes the record's intents whose lease is over (the condition is
/// re-checked in the DELETE, so an intent renewed or retired since the row
/// was read is left alone) and returns how many it removed. The write's own
/// intent is already gone.
pub(super) async fn delete_expired_intents_tx(
    tx: &DbTx<'_>,
    backend: DbBackend,
    key: &StoreKey,
) -> Result<u64, DomainError> {
    Ok(entity::write_intents::Entity::delete_many()
        .filter(
            Condition::all()
                .add(entity::write_intents::Column::TenantId.eq(key.tenant_id.0))
                .add(entity::write_intents::Column::RecordId.eq(key.record_id))
                .add(Expr::col(entity::write_intents::Column::LeaseUntil).lt(db_now(backend))),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(tx)
        .await
        .map_err(map_scope_err)?
        .rows_affected)
}

/// `SELECT … LIMIT 1` for the row with `record_id` (never a `COUNT`).
pub(super) async fn row_exists_tx(tx: &DbTx<'_>, record_id: Uuid) -> Result<bool, DomainError> {
    Ok(entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(record_id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .is_some())
}

/// What a write that lost definitively (or lost its intent) leaves behind as
/// debts: the row-existence fact this transaction can see, fed to the one
/// cleanup rule ([`cleanup_tasks`], `DeadContent::LostWrite`).
pub(super) async fn lost_write_tasks(
    tx: &DbTx<'_>,
    key: &StoreKey,
    version: &ValueVersion,
    destroy_supported: bool,
) -> Result<Vec<CleanupTask>, DomainError> {
    let row_exists = row_exists_tx(tx, key.record_id).await?;
    Ok(cleanup_tasks(
        DeadContent::LostWrite {
            key,
            version,
            row_exists,
        },
        destroy_supported,
    ))
}

/// Records every task of `tasks` as a debt row in `tx` (the commit of `tx`
/// makes them durable together with whatever made the store content dead).
pub(super) async fn record_debts(
    tx: &DbTx<'_>,
    tasks: Vec<CleanupTask>,
) -> Result<Vec<CleanupDebt>, DomainError> {
    let mut debts = Vec::with_capacity(tasks.len());
    for task in tasks {
        let id = Uuid::new_v4();
        let (op, selector, version) = task_to_columns(&task);
        let key = task.key();
        // `created_at` is left to the column default: the database clock.
        let am = entity::store_cleanup::ActiveModel {
            id: ActiveValue::Set(id),
            tenant_id: ActiveValue::Set(key.tenant_id.0),
            record_id: ActiveValue::Set(key.record_id),
            op: ActiveValue::Set(op),
            selector: ActiveValue::Set(selector),
            version: ActiveValue::Set(version),
            created_at: ActiveValue::NotSet,
        };
        entity::store_cleanup::Entity::insert(am)
            .secure()
            .scope_unchecked(&AccessScope::allow_all())
            .map_err(map_scope_err)?
            .exec(tx)
            .await
            .map_err(map_scope_err)?;
        debts.push(CleanupDebt { id, task });
    }
    Ok(debts)
}

/// The expired intents of `(tenant, reference)` whose record id has no row:
/// what a failed create leaves behind. A plain read; it finds nothing on the
/// usual path and then nothing is written.
async fn expired_orphan_intents(
    runner: &impl DBRunner,
    backend: DbBackend,
    tenant: TenantId,
    reference: &SecretRef,
) -> Result<Vec<entity::write_intents::Model>, DomainError> {
    entity::write_intents::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(entity::write_intents::Column::TenantId.eq(tenant.0))
                .add(entity::write_intents::Column::Reference.eq(reference.as_ref()))
                .add(Expr::col(entity::write_intents::Column::LeaseUntil).lt(db_now(backend)))
                .add(Expr::cust(
                    "NOT EXISTS (SELECT 1 FROM credstore_secrets s \
                     WHERE s.id = credstore_write_intents.record_id)",
                )),
        )
        .all(runner)
        .await
        .map_err(map_scope_err)
}

/// Failed-create heal: finds the expired intents of the reference whose
/// record has no row and, in ONE transaction, deletes each (the expiry is
/// re-checked in the DELETE; an intent another instance already took affects
/// zero rows and is skipped) and records a `purge` debt for each distinct
/// key.
pub(super) async fn heal_failed_creates(
    repo: &SecretRepoImpl,
    tenant: TenantId,
    reference: &SecretRef,
) -> Result<HealedCreates, DomainError> {
    let backend = repo.db.db().backend();
    let conn = repo.db.conn()?;
    let candidates = expired_orphan_intents(&conn, backend, tenant, reference).await?;
    if candidates.is_empty() {
        return Ok(HealedCreates::default());
    }
    repo.run_tx(move |tx: &DbTx<'_>| {
        let candidates = candidates.clone();
        Box::pin(async move {
            let mut healed = 0_u64;
            let mut keys = Vec::new();
            let mut seen = HashSet::new();
            for intent in candidates {
                let rows_affected = entity::write_intents::Entity::delete_many()
                    .filter(
                        Condition::all()
                            .add(entity::write_intents::Column::AttemptId.eq(intent.attempt_id))
                            .add(
                                Expr::col(entity::write_intents::Column::LeaseUntil)
                                    .lt(db_now(backend)),
                            ),
                    )
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?
                    .rows_affected;
                if rows_affected == 0 {
                    continue;
                }
                healed += rows_affected;
                if seen.insert(intent.record_id) {
                    keys.push(StoreKey::new(TenantId(intent.tenant_id), intent.record_id));
                }
            }
            let tasks = keys
                .iter()
                .flat_map(|key| cleanup_tasks(DeadContent::DeadKey { key }, false))
                .collect();
            let debts = record_debts(tx, tasks).await?;
            Ok(HealedCreates {
                intents: healed,
                debts,
            })
        }) as TxFuture<'_, HealedCreates>
    })
    .await
}

/// The pending debts of the record `key`.
pub(super) async fn pending_debts(
    repo: &SecretRepoImpl,
    key: &StoreKey,
) -> Result<Vec<CleanupDebt>, DomainError> {
    let conn = repo.db.conn()?;
    entity::store_cleanup::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(entity::store_cleanup::Column::TenantId.eq(key.tenant_id.0))
                .add(entity::store_cleanup::Column::RecordId.eq(key.record_id)),
        )
        .all(&conn)
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .map(entity_to_debt)
        .collect()
}

/// Deletes the debt row `id`; an already-deleted row affects zero rows.
pub(super) async fn delete_debt(repo: &SecretRepoImpl, id: Uuid) -> Result<(), DomainError> {
    repo.run_tx(move |tx: &DbTx<'_>| {
        Box::pin(async move {
            entity::store_cleanup::Entity::delete_many()
                .filter(Condition::all().add(entity::store_cleanup::Column::Id.eq(id)))
                .secure()
                .scope_with(&AccessScope::allow_all())
                .exec(tx)
                .await
                .map_err(map_scope_err)?;
            Ok(())
        }) as TxFuture<'_, ()>
    })
    .await
}
