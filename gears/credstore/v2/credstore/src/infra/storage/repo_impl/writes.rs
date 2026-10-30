// Updated: 2026-10-06 by Constructor Tech
//! Write-path repo methods (ADR-0006): `insert_active`, `insert_declared`,
//! `switch_value`, `update_metadata`, `remove_value`, `delete_by_id`.
//!
//! Every pointer switch is one compare-and-set on the row `version`. Each
//! method that learns some store content is dead (a delete, a secret
//! removal, a rotation, a write that lost) runs inside ONE transaction
//! ([`SecretRepoImpl::run_tx`], which retries a definite rollback)
//! together with the cleanup debts
//! it implies (rows of `credstore_store_cleanup`); the two secret-writing commits
//! (`insert_active`, `switch_value`) additionally retire the attempt's write
//! intent first, in the same transaction - see the module docs on
//! [`crate::domain::secret::repo::SecretRepo`].

use credstore_sdk::{SharingMode, StoreKey, TenantId, ValueVersion};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue, ColumnTrait, Condition, DbBackend, DbErr, EntityTrait, ExprTrait, QueryFilter,
    QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DbTx, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::cleanup::{DeadContent, cleanup_tasks};
use crate::domain::secret::model::{
    CleanupDebt, Fallback, IntentCommit, NewDeclaredSecret, NewSecret, SecretRow, WriteAttempt,
};
use crate::infra::storage::entity;
use crate::infra::storage::entity::enums::{DbFallback, DbSharing, DbStatus};
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, TxFuture, entity_to_model, map_scope_err,
};
use crate::infra::storage::repo_impl::intents::{
    delete_expired_intents_tx, delete_intent_tx, lost_write_tasks, record_debts,
};

// ── Creates ─────────────────────────────────────────────────────────────────

/// Create step 3 (tx1): ONE transaction retiring the attempt's intent and
/// inserting the row `active`. The insert is `ON CONFLICT DO NOTHING`: a
/// unique violation would abort a `PostgreSQL` transaction and take the
/// intent deletion with it, but the definite loss must commit that deletion
/// together with the `purge` debt for the attempt's fresh key.
pub(super) async fn insert_active(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    new: &NewSecret,
    attempt: &WriteAttempt,
) -> Result<IntentCommit<()>, DomainError> {
    let am = active_model(new);
    let scope = scope.clone();
    let attempt = attempt.clone();
    let version = new.value_version.clone();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let (scope, am, attempt) = (scope.clone(), am.clone(), attempt.clone());
        let version = version.clone();
        Box::pin(async move { insert_active_tx(tx, &scope, am, &attempt, &version).await })
            as TxFuture<'_, IntentCommit<()>>
    })
    .await
}

/// The row a create inserts `active`.
pub(super) fn active_model(new: &NewSecret) -> entity::secrets::ActiveModel {
    let now = OffsetDateTime::now_utc();
    entity::secrets::ActiveModel {
        id: ActiveValue::Set(new.id),
        tenant_id: ActiveValue::Set(new.tenant_id.0),
        reference: ActiveValue::Set(new.reference.as_ref().to_owned()),
        sharing: ActiveValue::Set(DbSharing::from(new.sharing)),
        owner_id: ActiveValue::Set(new.owner_id.0),
        status: ActiveValue::Set(DbStatus::Active),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(new.secret_type_uuid),
        expires_at: ActiveValue::Set(new.expires_at),
        value_version: ActiveValue::Set(Some(new.value_version.0.clone())),
        fallback: ActiveValue::Set(DbFallback::from(new.fallback)),
    }
}

/// The create's `INSERT`, as `ON CONFLICT DO NOTHING` (no conflict target:
/// whichever unique index the reference hits) so that losing the reference
/// race is an empty result, not an error that would abort the transaction.
pub(super) fn insert_unless_taken(
    am: entity::secrets::ActiveModel,
) -> sea_orm::Insert<entity::secrets::ActiveModel> {
    entity::secrets::Entity::insert(am).on_conflict(OnConflict::new().do_nothing().to_owned())
}

pub(super) async fn insert_active_tx(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    am: entity::secrets::ActiveModel,
    attempt: &WriteAttempt,
    version: &ValueVersion,
) -> Result<IntentCommit<()>, DomainError> {
    // The intent first: if it was healed, nothing else may happen.
    if !delete_intent_tx(tx, attempt.attempt_id).await? {
        return Ok(IntentCommit::IntentLost);
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
        Ok(_) => Ok(IntentCommit::Committed {
            value: (),
            debts: Vec::new(),
            healed: 0,
        }),
        // The reference is taken: a definite loss. The fresh record id can
        // never get a row, so the whole key is dead.
        Err(ScopeError::Db(DbErr::RecordNotInserted)) => {
            let tasks = cleanup_tasks(
                DeadContent::LostWrite {
                    key: &attempt.key,
                    version,
                    row_exists: false,
                },
                attempt.destroy_supported,
            );
            let debts = record_debts(tx, tasks).await?;
            Ok(IntentCommit::Lost { debts })
        }
        Err(e) => Err(map_scope_err(e)),
    }
}

/// Create-with-no-value path (ADR-0007, "PUT: whole replace, secret tri-state"): `status = declared`,
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
        sharing: ActiveValue::Set(DbSharing::from(new.sharing)),
        owner_id: ActiveValue::Set(new.owner_id.0),
        status: ActiveValue::Set(DbStatus::Declared),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(new.secret_type_uuid),
        expires_at: ActiveValue::Set(new.expires_at),
        value_version: ActiveValue::Set(None),
        fallback: ActiveValue::Set(DbFallback::from(new.fallback)),
    };
    let conn = repo.db.conn()?;
    // `scope_unchecked`: an INSERT cannot subtree-clamp on a row that doesn't
    // exist yet.
    entity::secrets::Entity::insert(am)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(())
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
    let backend = repo.db.db().backend();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let (scope, attempt) = (scope.clone(), attempt.clone());
        let new_value_version = new_value_version.clone();
        Box::pin(async move {
            switch_value_tx(
                backend,
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
        }) as TxFuture<'_, IntentCommit<SecretRow>>
    })
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update, plus the attempt it retires"
)]
pub(super) async fn switch_value_tx(
    backend: DbBackend,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    id: Uuid,
    expected_version: i64,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    new_value_version: ValueVersion,
    attempt: &WriteAttempt,
) -> Result<IntentCommit<SecretRow>, DomainError> {
    // The intent first: if it was healed, nothing else may happen.
    if !delete_intent_tx(tx, attempt.attempt_id).await? {
        return Ok(IntentCommit::IntentLost);
    }

    let now = OffsetDateTime::now_utc();

    // The compare-and-set: `version` is the one the caller read before its
    // `plugin.put`, so a concurrent change of any kind matches nothing.
    // Accepts either resting status: a `declared` row switches to `active`
    // exactly like an `active` row being rotated (ADR-0008, "Suppression is one
    // request, always").
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::ValueVersion,
            Expr::value(Some(new_value_version.0.clone())),
        )
        .col_expr(
            entity::secrets::Column::Sharing,
            Expr::value(DbSharing::from(sharing)),
        )
        .col_expr(
            entity::secrets::Column::Fallback,
            Expr::value(DbFallback::from(fallback)),
        )
        .col_expr(entity::secrets::Column::ExpiresAt, Expr::value(expires_at))
        .col_expr(
            entity::secrets::Column::Version,
            Expr::col(entity::secrets::Column::Version).add(1_i64),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            entity::secrets::Column::Status,
            Expr::value(DbStatus::Active),
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
        // debt for this attempt's now unreferenced version.
        let tasks = lost_write_tasks(
            tx,
            &attempt.key,
            &new_value_version,
            attempt.destroy_supported,
        )
        .await?;
        let debts = record_debts(tx, tasks).await?;
        return Ok(IntentCommit::Lost { debts });
    }

    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| DomainError::internal("switch_value: row vanished after its own update"))?;
    let row = entity_to_model(row);

    // Every version below the one just committed is dead: destroy by position
    // (safe because the CAS base is the row read before the `put`). This also
    // covers whatever version a crashed writer of an expired intent may have
    // landed: versions are ordered and that writer's version is older.
    let tasks = cleanup_tasks(
        DeadContent::Superseded {
            key: &attempt.key,
            new: &new_value_version,
        },
        attempt.destroy_supported,
    );
    let debts = record_debts(tx, tasks).await?;
    // Heal the record's expired intents (the row read reported some).
    let healed = if attempt.heal_expired_intents {
        delete_expired_intents_tx(tx, backend, &attempt.key).await?
    } else {
        0
    };
    Ok(IntentCommit::Committed {
        value: row,
        debts,
        healed,
    })
}

/// Metadata-only update (ADR-0007, "PATCH: merge, never creates", no `secret` key): never
/// touches `value_version`/`status`. One transaction: the compare-and-set
/// (on `expected_version` when given) and the read of the post-write row.
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
    repo.run_tx(move |tx: &DbTx<'_>| {
        let scope = scope.clone();
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

    let mut filter = Condition::all().add(entity::secrets::Column::Id.eq(id));
    if let Some(expected) = expected_version {
        filter = filter.add(entity::secrets::Column::Version.eq(expected));
    }

    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::Sharing,
            Expr::value(DbSharing::from(sharing)),
        )
        .col_expr(
            entity::secrets::Column::Fallback,
            Expr::value(DbFallback::from(fallback)),
        )
        .col_expr(entity::secrets::Column::ExpiresAt, Expr::value(expires_at))
        .col_expr(
            entity::secrets::Column::Version,
            Expr::col(entity::secrets::Column::Version).add(1_i64),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .filter(filter)
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    // The version moved, the row is gone, or the scope excludes it.
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
    Ok(Some(entity_to_model(row)))
}

/// Secret removal (ADR-0007, "PATCH: merge, never creates", `{"secret": null}`): ONE transaction - the
/// compare-and-set that nulls the pointer and moves the row to `declared`
/// (applying the merged metadata), plus the debts `destroy(Below(old))` and
/// `destroy(Exactly(old))` for the value version the row held. Never touches
/// the store.
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
) -> Result<Option<(SecretRow, Vec<CleanupDebt>)>, DomainError> {
    let scope = scope.clone();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let scope = scope.clone();
        Box::pin(async move {
            remove_value_tx(
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
        }) as TxFuture<'_, Option<(SecretRow, Vec<CleanupDebt>)>>
    })
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update, plus the plugin's destroy capability"
)]
async fn remove_value_tx(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    destroy_supported: bool,
) -> Result<Option<(SecretRow, Vec<CleanupDebt>)>, DomainError> {
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
        return Ok(None);
    };
    if let Some(expected) = expected_version
        && current.version != expected
    {
        return Ok(None);
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
            Expr::value(DbStatus::Declared),
        )
        .col_expr(
            entity::secrets::Column::Sharing,
            Expr::value(DbSharing::from(sharing)),
        )
        .col_expr(
            entity::secrets::Column::Fallback,
            Expr::value(DbFallback::from(fallback)),
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
        // `lock_exclusive` is a no-op on SQLite, so a concurrent change can
        // still reach this CAS there.
        return Ok(None);
    }

    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| DomainError::internal("remove_value: row vanished after its own update"))?;
    let row = entity_to_model(row);

    // `destroy`, never `delete_key`: a concurrent writer may already have put
    // a newer version under the same key. Below first, then the version
    // itself (`Below` is exclusive).
    let tasks = match &old_value_version {
        Some(old) => cleanup_tasks(DeadContent::Removed { key: &key, old }, destroy_supported),
        None => Vec::new(),
    };
    let debts = record_debts(tx, tasks).await?;
    Ok(Some((row, debts)))
}

pub(super) async fn delete_by_id(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    key: &StoreKey,
    expected_version: Option<i64>,
) -> Result<Vec<CleanupDebt>, DomainError> {
    let scope = scope.clone();
    let key = key.clone();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let (scope, key) = (scope.clone(), key.clone());
        Box::pin(async move { delete_row_tx(tx, &scope, &key, expected_version).await })
            as TxFuture<'_, Vec<CleanupDebt>>
    })
    .await
}

/// One transaction: `DELETE` the row (CAS on `expected_version` when given;
/// 0 rows affected is `NotFound`) and record the key purge.
pub(super) async fn delete_row_tx(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    key: &StoreKey,
    expected_version: Option<i64>,
) -> Result<Vec<CleanupDebt>, DomainError> {
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
    let tasks = cleanup_tasks(DeadContent::DeadKey { key }, false);
    record_debts(tx, tasks).await
}
