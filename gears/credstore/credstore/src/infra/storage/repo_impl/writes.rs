//! Write-path repo methods (ADR-0006): `insert_active`, `insert_declared`,
//! `switch_value`, `update_metadata`, `remove_value`, `delete_by_id`.
//!
//! Every pointer switch is one compare-and-set on the row `version`. Each
//! method that learns some store content is dead (a delete, a secret
//! removal, a rotation, a write that lost) runs inside ONE
//! [`toolkit_db::DBProvider::transaction`] together with the outbox enqueue
//! of the cleanup tasks it implies; the two secret-writing commits
//! (`insert_active`, `switch_value`) additionally retire the attempt's write
//! intent first, in the same transaction - see the module docs on
//! [`crate::domain::secret::repo::SecretRepo`].

use std::sync::Arc;

use credstore_sdk::{DestroySelector, SharingMode, StoreKey, TenantId, ValueVersion};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue, ColumnTrait, Condition, DbErr, EntityTrait, ExprTrait, QueryFilter, QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::outbox::Wake;
use toolkit_db::secure::{
    DBRunner, DbTx, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    CleanupTask, Fallback, IntentCommit, NewDeclaredSecret, NewSecret, SecretRow, SecretStatus,
    WriteAttempt,
};
use crate::infra::outbox::CleanupEnqueuer;
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, TxFuture, entity_to_model, map_scope_err, sharing_to_i16,
};
use crate::infra::storage::repo_impl::intents::{
    delete_intent_tx, enqueue_tasks, lost_write_tasks,
};

// ── Creates ─────────────────────────────────────────────────────────────────

/// Plain `INSERT` of a prepared row on `runner`. `scope_unchecked`: an INSERT
/// cannot subtree-clamp on a row that doesn't exist yet.
async fn insert_row<R: DBRunner + Sync>(
    runner: &R,
    scope: &AccessScope,
    am: entity::secrets::ActiveModel,
) -> Result<(), DomainError> {
    entity::secrets::Entity::insert(am)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .exec(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(())
}

/// Runs `am`'s plain insert. A unique-index conflict maps to `Conflict`
/// through the shared classification ladder: an expired own row still holds
/// the reference, so a create over it is a conflict like any other.
async fn create_row(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    am: entity::secrets::ActiveModel,
) -> Result<(), DomainError> {
    let conn = repo.db.conn()?;
    insert_row(&conn, scope, am).await
}

/// Create step 3 (tx1): ONE transaction retiring the attempt's intent and
/// inserting the row `active`. The insert is `ON CONFLICT DO NOTHING`: a
/// unique violation would abort a `PostgreSQL` transaction and take the
/// intent deletion with it, but the definite loss must commit that deletion
/// together with the purge of the attempt's fresh key.
pub(super) async fn insert_active(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    new: &NewSecret,
    attempt: &WriteAttempt,
) -> Result<IntentCommit<()>, DomainError> {
    let now = OffsetDateTime::now_utc();
    let am = entity::secrets::ActiveModel {
        id: ActiveValue::Set(new.id),
        tenant_id: ActiveValue::Set(new.tenant_id.0),
        reference: ActiveValue::Set(new.reference.as_ref().to_owned()),
        sharing: ActiveValue::Set(sharing_to_i16(new.sharing)),
        owner_id: ActiveValue::Set(new.owner_id.0),
        status: ActiveValue::Set(SecretStatus::Active.as_smallint()),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(new.secret_type_uuid),
        expires_at: ActiveValue::Set(new.expires_at),
        value_version: ActiveValue::Set(Some(new.value_version.0.clone())),
        fallback: ActiveValue::Set(new.fallback.as_smallint()),
    };
    let scope = scope.clone();
    let attempt = attempt.clone();
    let cleanup = Arc::clone(&repo.cleanup);
    let (outcome, wake) = repo
        .db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(
                async move { insert_active_tx(cleanup.as_ref(), tx, &scope, am, &attempt).await },
            ) as TxFuture<'_, (IntentCommit<()>, Wake)>
        })
        .await?;
    wake.fire();
    Ok(outcome)
}

/// The create's `INSERT`, as `ON CONFLICT DO NOTHING` (no conflict target:
/// whichever unique index the reference hits) so that losing the reference
/// race is an empty result, not an error that would abort the transaction.
pub(super) fn insert_unless_taken(
    am: entity::secrets::ActiveModel,
) -> sea_orm::Insert<entity::secrets::ActiveModel> {
    entity::secrets::Entity::insert(am).on_conflict(OnConflict::new().do_nothing().to_owned())
}

async fn insert_active_tx(
    cleanup: &dyn CleanupEnqueuer,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    am: entity::secrets::ActiveModel,
    attempt: &WriteAttempt,
) -> Result<(IntentCommit<()>, Wake), DomainError> {
    // The intent first: if it was reclaimed, nothing else may happen.
    if !delete_intent_tx(tx, attempt.attempt_id).await? {
        return Ok((IntentCommit::IntentLost, Wake::empty()));
    }
    // `scope_unchecked`: an INSERT cannot subtree-clamp on a row that doesn't
    // exist yet.
    let inserted = insert_unless_taken(am)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .exec(tx)
        .await;
    match inserted {
        Ok(_) => Ok((
            IntentCommit::Committed {
                value: (),
                enqueued: Vec::new(),
            },
            Wake::empty(),
        )),
        // The reference is taken: a definite loss. The fresh record id can
        // never get a row, so the whole key is dead.
        Err(ScopeError::Db(DbErr::RecordNotInserted)) => {
            let tasks = vec![CleanupTask::Purge(attempt.key.clone())];
            let wake = enqueue_tasks(cleanup, tx, &tasks).await?;
            Ok((IntentCommit::Lost { enqueued: tasks }, wake))
        }
        Err(e) => Err(map_scope_err(e)),
    }
}

/// Create-with-no-value path (ADR-0004 Amendment B): `status = declared`,
/// `value_version` `NULL`; no plugin call is ever made. A unique-index
/// conflict maps to `DomainError::Conflict` through the same
/// `classify_db_err_to_domain` ladder every other write uses.
pub(super) async fn insert_declared(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    new: &NewDeclaredSecret,
) -> Result<(), DomainError> {
    let now = OffsetDateTime::now_utc();
    let am = entity::secrets::ActiveModel {
        id: ActiveValue::Set(new.id),
        tenant_id: ActiveValue::Set(new.tenant_id.0),
        reference: ActiveValue::Set(new.reference.as_ref().to_owned()),
        sharing: ActiveValue::Set(sharing_to_i16(new.sharing)),
        owner_id: ActiveValue::Set(new.owner_id.0),
        status: ActiveValue::Set(SecretStatus::Declared.as_smallint()),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(new.secret_type_uuid),
        expires_at: ActiveValue::Set(new.expires_at),
        value_version: ActiveValue::Set(None),
        fallback: ActiveValue::Set(new.fallback.as_smallint()),
    };
    create_row(repo, scope, am).await
}

// ── Pointer switch ──────────────────────────────────────────────────────────

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update, plus the attempt it retires"
)]
pub(super) async fn switch_value(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    expected_version: i64,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    new_value_version: ValueVersion,
    attempt: &WriteAttempt,
) -> Result<IntentCommit<SecretRow>, DomainError> {
    let scope = scope.clone();
    let attempt = attempt.clone();
    let cleanup = Arc::clone(&repo.cleanup);
    let (outcome, wake) = repo
        .db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                switch_value_tx(
                    cleanup.as_ref(),
                    tx,
                    &scope,
                    id,
                    expected_version,
                    sharing,
                    fallback,
                    expires_at,
                    new_value_version,
                    &attempt,
                )
                .await
            }) as TxFuture<'_, (IntentCommit<SecretRow>, Wake)>
        })
        .await?;
    wake.fire();
    Ok(outcome)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update, plus the attempt it retires"
)]
async fn switch_value_tx(
    cleanup: &dyn CleanupEnqueuer,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    id: Uuid,
    expected_version: i64,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    new_value_version: ValueVersion,
    attempt: &WriteAttempt,
) -> Result<(IntentCommit<SecretRow>, Wake), DomainError> {
    // The intent first: if it was reclaimed, nothing else may happen.
    if !delete_intent_tx(tx, attempt.attempt_id).await? {
        return Ok((IntentCommit::IntentLost, Wake::empty()));
    }

    let now = OffsetDateTime::now_utc();

    // The compare-and-set: `version` is the one the caller read before its
    // `plugin.put`, so a concurrent change of any kind matches nothing.
    // Accepts either resting status: a `declared` row switches to `active`
    // exactly like an `active` row being rotated (ADR-0004, "Writing a value
    // to a suppressed record is not a conflict").
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::ValueVersion,
            Expr::value(Some(new_value_version.0.clone())),
        )
        .col_expr(
            entity::secrets::Column::Sharing,
            Expr::value(sharing_to_i16(sharing)),
        )
        .col_expr(
            entity::secrets::Column::Fallback,
            Expr::value(fallback.as_smallint()),
        )
        .col_expr(entity::secrets::Column::ExpiresAt, Expr::value(expires_at))
        .col_expr(
            entity::secrets::Column::Version,
            Expr::col(entity::secrets::Column::Version).add(1_i64),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            entity::secrets::Column::Status,
            Expr::value(SecretStatus::Active.as_smallint()),
        )
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Id.eq(id))
                .add(entity::secrets::Column::Version.eq(expected_version)),
        )
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    if rows_affected == 0 {
        // A definite loss. The intent deletion still commits, with the
        // cleanup of this attempt's now unreferenced version.
        let tasks = lost_write_tasks(
            tx,
            &attempt.key,
            &new_value_version,
            attempt.destroy_supported,
        )
        .await?;
        let wake = enqueue_tasks(cleanup, tx, &tasks).await?;
        return Ok((IntentCommit::Lost { enqueued: tasks }, wake));
    }

    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| DomainError::internal("switch_value: row vanished after its own update"))?;
    let row = entity_to_model(row)?;

    // Every version below the one just committed is dead: destroy by position
    // (safe because the CAS base is the row read before the `put`).
    let tasks = if attempt.destroy_supported {
        vec![CleanupTask::Destroy {
            key: attempt.key.clone(),
            selector: DestroySelector::Below(new_value_version),
        }]
    } else {
        Vec::new()
    };
    let wake = enqueue_tasks(cleanup, tx, &tasks).await?;
    Ok((
        IntentCommit::Committed {
            value: row,
            enqueued: tasks,
        },
        wake,
    ))
}

/// Metadata-only update (ADR-0004 `PATCH` with no `value` key): never
/// touches `value_version`/`status`. One transaction,
/// mirroring `switch_value_tx`/`remove_value_tx`: lock + read the row first,
/// then gate the UPDATE on the version just read under that lock.
pub(super) async fn update_metadata(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
) -> Result<Option<SecretRow>, DomainError> {
    let scope = scope.clone();
    repo.db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                update_metadata_tx(
                    tx,
                    &scope,
                    id,
                    expected_version,
                    sharing,
                    fallback,
                    expires_at,
                )
                .await
            }) as TxFuture<'_, Option<SecretRow>>
        })
        .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update"
)]
async fn update_metadata_tx(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
) -> Result<Option<SecretRow>, DomainError> {
    let now = OffsetDateTime::now_utc();

    let current = entity::secrets::Entity::find()
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?;
    let Some(current) = current else {
        return Ok(None);
    };
    if let Some(expected) = expected_version
        && current.version != expected
    {
        return Ok(None);
    }
    let locked_version = current.version;

    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::Sharing,
            Expr::value(sharing_to_i16(sharing)),
        )
        .col_expr(
            entity::secrets::Column::Fallback,
            Expr::value(fallback.as_smallint()),
        )
        .col_expr(entity::secrets::Column::ExpiresAt, Expr::value(expires_at))
        .col_expr(
            entity::secrets::Column::Version,
            Expr::col(entity::secrets::Column::Version).add(1_i64),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Id.eq(id))
                .add(entity::secrets::Column::Version.eq(locked_version)),
        )
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    if rows_affected == 0 {
        return Ok(None);
    }
    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| {
            DomainError::internal("update_metadata: row vanished after its own update")
        })?;
    Some(entity_to_model(row)).transpose()
}

/// Secret removal (ADR-0004 `PATCH {"secret": null}`): ONE transaction - the
/// compare-and-set that nulls the pointer and moves the row to `declared`
/// (applying the merged metadata), plus the enqueue of `destroy(Below(old))`
/// and `destroy(Exactly(old))` for the value version the row held. Never
/// touches the store inline.
#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update, plus the plugin's destroy capability"
)]
pub(super) async fn remove_value(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    destroy_supported: bool,
) -> Result<Option<(SecretRow, Vec<CleanupTask>)>, DomainError> {
    let scope = scope.clone();
    let cleanup = Arc::clone(&repo.cleanup);
    let (removed, wake) = repo
        .db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                remove_value_tx(
                    cleanup.as_ref(),
                    tx,
                    &scope,
                    id,
                    expected_version,
                    sharing,
                    fallback,
                    expires_at,
                    destroy_supported,
                )
                .await
            }) as TxFuture<'_, (Option<(SecretRow, Vec<CleanupTask>)>, Wake)>
        })
        .await?;
    wake.fire();
    Ok(removed)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update, plus the plugin's destroy capability"
)]
async fn remove_value_tx(
    cleanup: &dyn CleanupEnqueuer,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    destroy_supported: bool,
) -> Result<(Option<(SecretRow, Vec<CleanupTask>)>, Wake), DomainError> {
    let now = OffsetDateTime::now_utc();

    // Lock + read so the value version we destroy is exactly the one this
    // transaction nulls, atomically.
    let current = entity::secrets::Entity::find()
        .lock_exclusive()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?;
    let Some(current) = current else {
        return Ok((None, Wake::empty()));
    };
    if let Some(expected) = expected_version
        && current.version != expected
    {
        return Ok((None, Wake::empty()));
    }
    let old_value_version = current.value_version.clone().map(ValueVersion);
    let key = StoreKey::new(TenantId(current.tenant_id), current.id);
    let locked_version = current.version;

    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::ValueVersion,
            Expr::value::<Option<String>>(None),
        )
        .col_expr(
            entity::secrets::Column::Status,
            Expr::value(SecretStatus::Declared.as_smallint()),
        )
        .col_expr(
            entity::secrets::Column::Sharing,
            Expr::value(sharing_to_i16(sharing)),
        )
        .col_expr(
            entity::secrets::Column::Fallback,
            Expr::value(fallback.as_smallint()),
        )
        .col_expr(entity::secrets::Column::ExpiresAt, Expr::value(expires_at))
        .col_expr(
            entity::secrets::Column::Version,
            Expr::col(entity::secrets::Column::Version).add(1_i64),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Id.eq(id))
                .add(entity::secrets::Column::Version.eq(locked_version)),
        )
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    if rows_affected == 0 {
        // Belt-and-braces over `lock_exclusive` (a no-op on SQLite).
        return Ok((None, Wake::empty()));
    }

    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| DomainError::internal("remove_value: row vanished after its own update"))?;
    let row = entity_to_model(row)?;

    // `destroy`, never `delete_key`: a concurrent writer may already have put
    // a newer version under the same key. Below first, then the version
    // itself (`Below` is exclusive).
    let tasks = match old_value_version {
        Some(old) if destroy_supported => vec![
            CleanupTask::Destroy {
                key: key.clone(),
                selector: DestroySelector::Below(old.clone()),
            },
            CleanupTask::Destroy {
                key,
                selector: DestroySelector::Exactly(old),
            },
        ],
        _ => Vec::new(),
    };
    let wake = enqueue_tasks(cleanup, tx, &tasks).await?;
    Ok((Some((row, tasks)), wake))
}

pub(super) async fn delete_by_id(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    key: &StoreKey,
    expected_version: Option<i64>,
) -> Result<(), DomainError> {
    let scope = scope.clone();
    let key = key.clone();
    let cleanup = Arc::clone(&repo.cleanup);
    let wake = repo
        .db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                delete_row_tx(cleanup.as_ref(), tx, &scope, &key, expected_version).await
            }) as TxFuture<'_, Wake>
        })
        .await?;
    // Committed: wake the sequencer.
    wake.fire();
    Ok(())
}

/// One transaction: `DELETE` the row (CAS on `expected_version` when given;
/// 0 rows affected is `NotFound`) and enqueue the key purge.
async fn delete_row_tx(
    cleanup: &dyn CleanupEnqueuer,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    key: &StoreKey,
    expected_version: Option<i64>,
) -> Result<Wake, DomainError> {
    let mut filter = Condition::all().add(entity::secrets::Column::Id.eq(key.record_id));
    if let Some(v) = expected_version {
        filter = filter.add(entity::secrets::Column::Version.eq(v));
    }
    let rows_affected = entity::secrets::Entity::delete_many()
        .filter(filter)
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    if rows_affected == 0 {
        return Err(DomainError::NotFound);
    }
    cleanup.enqueue(tx, &CleanupTask::Purge(key.clone())).await
}
