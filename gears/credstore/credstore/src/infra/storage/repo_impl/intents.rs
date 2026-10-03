//! The write-intent journal and its reclaim (ADR-0006), plus the helpers the
//! write transactions in [`super::writes`] share: retiring an intent,
//! deciding what a lost write leaves behind, and enqueuing cleanup tasks in
//! the open transaction.
//!
//! `lease_until` is produced and compared by the DATABASE clock on both
//! backends (`now()` on `PostgreSQL`; an ISO-8601 `strftime` on `SQLite`,
//! where timestamps are TEXT), never bound from the process, so instances
//! with skewed clocks agree on when a lease is over.

use std::collections::HashSet;
use std::time::Duration;

use credstore_sdk::{DestroySelector, StoreKey, TenantId, ValueVersion};
use sea_orm::QuerySelect;
use sea_orm::sea_query::{Expr, LockBehavior, LockType, Query, SelectStatement, SimpleExpr};
use sea_orm::{ColumnTrait, Condition, DbBackend, EntityTrait, ExprTrait, QueryFilter, QueryOrder};
use toolkit_db::outbox::Wake;
use toolkit_db::secure::{DbTx, SecureDeleteExt, SecureEntityExt, secure_insert_from_select};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{CleanupTask, Reclaimed, WriteAttempt};
use crate::infra::outbox::CleanupEnqueuer;
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{SecretRepoImpl, TxFuture, map_scope_err};

/// The database's current instant, in the column's own format.
fn db_now(backend: DbBackend) -> SimpleExpr {
    match backend {
        DbBackend::Sqlite => Expr::cust("strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"),
        _ => Expr::cust("now()"),
    }
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
const INTENT_COLUMNS: [entity::write_intents::Column; 4] = [
    entity::write_intents::Column::AttemptId,
    entity::write_intents::Column::TenantId,
    entity::write_intents::Column::RecordId,
    entity::write_intents::Column::LeaseUntil,
];

/// The row tx0 inserts: the attempt's identity and key, and `lease_until`
/// computed by the database.
pub(super) fn begin_intent_source(
    backend: DbBackend,
    attempt: &WriteAttempt,
    lease: Duration,
) -> SelectStatement {
    Query::select()
        .expr(Expr::value(attempt.attempt_id))
        .expr(Expr::value(attempt.key.tenant_id.0))
        .expr(Expr::value(attempt.key.record_id))
        .expr(db_now_plus(backend, lease))
        .to_owned()
}

/// tx0: `INSERT` the attempt's intent with `lease_until = now() + lease` on
/// the database clock, as ONE statement.
pub(super) async fn begin_write_intent(
    repo: &SecretRepoImpl,
    attempt: &WriteAttempt,
    lease: Duration,
) -> Result<(), DomainError> {
    let backend = repo.db.db().backend();
    let conn = repo.db.conn()?;
    secure_insert_from_select::<entity::write_intents::Entity, _>(
        INTENT_COLUMNS,
        begin_intent_source(backend, attempt, lease),
        &AccessScope::allow_all(),
        &conn,
    )
    .await
    .map_err(map_scope_err)?;
    Ok(())
}

/// The reclaim's batch query: up to `limit` intents whose lease is over,
/// oldest first. `PostgreSQL` locks them `FOR UPDATE SKIP LOCKED`, so
/// concurrent passes take disjoint batches and never wait on each other or
/// on a writer retiring its own intent.
pub(super) fn expired_intents_select(
    backend: DbBackend,
    limit: u64,
) -> sea_orm::Select<entity::write_intents::Entity> {
    entity::write_intents::Entity::find()
        .filter(
            Condition::all()
                .add(Expr::col(entity::write_intents::Column::LeaseUntil).lt(db_now(backend))),
        )
        .order_by_asc(entity::write_intents::Column::LeaseUntil)
        .limit(limit)
        .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
}

/// Best-effort retirement of an intent whose attempt will not `put` (the
/// lease guard fired).
pub(super) async fn drop_write_intent(
    repo: &SecretRepoImpl,
    attempt_id: Uuid,
) -> Result<(), DomainError> {
    let conn = repo.db.conn()?;
    entity::write_intents::Entity::delete_many()
        .filter(Condition::all().add(entity::write_intents::Column::AttemptId.eq(attempt_id)))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(())
}

/// Deletes the attempt's intent inside `tx`. `true` iff it existed (the
/// delete affected exactly one row); `false` means it was reclaimed.
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

/// What a write that lost definitively (or lost its intent) leaves behind
/// for the outbox: `destroy(key, Exactly(version))` when a row with the
/// record id still exists and the plugin supports destroy, nothing when it
/// exists and the plugin does not, `purge(key)` when no row exists (the key
/// can never hold a live value).
pub(super) async fn lost_write_tasks(
    tx: &DbTx<'_>,
    key: &StoreKey,
    version: &ValueVersion,
    destroy_supported: bool,
) -> Result<Vec<CleanupTask>, DomainError> {
    if !row_exists_tx(tx, key.record_id).await? {
        return Ok(vec![CleanupTask::Purge(key.clone())]);
    }
    if destroy_supported {
        return Ok(vec![CleanupTask::Destroy {
            key: key.clone(),
            selector: DestroySelector::Exactly(version.clone()),
        }]);
    }
    Ok(Vec::new())
}

/// Enqueues every task of `tasks` in `tx`; the returned wake is fired after
/// the commit.
pub(super) async fn enqueue_tasks(
    cleanup: &dyn CleanupEnqueuer,
    tx: &DbTx<'_>,
    tasks: &[CleanupTask],
) -> Result<Wake, DomainError> {
    let mut wake = Wake::empty();
    for task in tasks {
        wake += cleanup.enqueue(tx, task).await?;
    }
    Ok(wake)
}

/// Step 5c: ONE transaction enqueuing what the writer's version needs after
/// its intent was reclaimed (see [`lost_write_tasks`]).
pub(super) async fn settle_lost_intent(
    repo: &SecretRepoImpl,
    key: &StoreKey,
    version: &ValueVersion,
    destroy_supported: bool,
) -> Result<Vec<CleanupTask>, DomainError> {
    let key = key.clone();
    let version = version.clone();
    let cleanup = std::sync::Arc::clone(&repo.cleanup);
    let (tasks, wake) = repo
        .db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                let tasks = lost_write_tasks(tx, &key, &version, destroy_supported).await?;
                let wake = enqueue_tasks(cleanup.as_ref(), tx, &tasks).await?;
                Ok((tasks, wake))
            }) as TxFuture<'_, (Vec<CleanupTask>, Wake)>
        })
        .await?;
    wake.fire();
    Ok(tasks)
}

/// Reclaim, in ONE transaction: delete up to `limit` expired intents
/// (`PostgreSQL`: `FOR UPDATE SKIP LOCKED`, so concurrent passes take
/// disjoint batches) and enqueue `purge(key)` for each whose record has no
/// row. An intent whose row exists enqueues nothing.
pub(super) async fn reclaim_expired(
    repo: &SecretRepoImpl,
    limit: u64,
) -> Result<Reclaimed, DomainError> {
    let backend = repo.db.db().backend();
    let cleanup = std::sync::Arc::clone(&repo.cleanup);
    let (reclaimed, wake) = repo
        .db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                let expired = expired_intents_select(backend, limit)
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .all(tx)
                    .await
                    .map_err(map_scope_err)?;
                if expired.is_empty() {
                    return Ok((Reclaimed::default(), Wake::empty()));
                }

                entity::write_intents::Entity::delete_many()
                    .filter(
                        Condition::all().add(
                            entity::write_intents::Column::AttemptId
                                .is_in(expired.iter().map(|i| i.attempt_id)),
                        ),
                    )
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?;

                // One query for the whole batch: which of the records have a
                // row. Never a COUNT.
                let live: HashSet<Uuid> = entity::secrets::Entity::find()
                    .secure()
                    .scope_with(&AccessScope::allow_all())
                    .filter(Condition::all().add(
                        entity::secrets::Column::Id.is_in(expired.iter().map(|i| i.record_id)),
                    ))
                    .all(tx)
                    .await
                    .map_err(map_scope_err)?
                    .into_iter()
                    .map(|row| row.id)
                    .collect();

                let tasks: Vec<CleanupTask> = expired
                    .iter()
                    .filter(|i| !live.contains(&i.record_id))
                    .map(|i| CleanupTask::Purge(StoreKey::new(TenantId(i.tenant_id), i.record_id)))
                    .collect();
                let wake = enqueue_tasks(cleanup.as_ref(), tx, &tasks).await?;
                Ok((
                    Reclaimed {
                        intents: expired.len() as u64,
                        enqueued: tasks,
                    },
                    wake,
                ))
            }) as TxFuture<'_, (Reclaimed, Wake)>
        })
        .await?;
    wake.fire();
    Ok(reclaimed)
}
