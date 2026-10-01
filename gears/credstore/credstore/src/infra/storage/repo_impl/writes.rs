//! Write-path repo methods (ADR-0006): `insert_active`, `insert_declared`,
//! `switch_value`, `update_metadata`, `remove_value`, `delete_by_id`.
//!
//! Every pointer switch is one compare-and-set on the row `version`; a delete
//! runs inside ONE
//! [`toolkit_db::DBProvider::transaction`] together with the outbox enqueue of
//! the key purge - see the module docs on
//! [`crate::domain::secret::repo::SecretRepo`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use credstore_sdk::{SharingMode, StoreKey, ValueVersion};
use sea_orm::ExprTrait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, DbTx, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    Fallback, NewDeclaredSecret, NewSecret, SecretRow, SecretStatus,
};
use crate::infra::outbox::PurgeEnqueuer;
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, entity_to_model, map_scope_err, sharing_to_i16,
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

pub(super) async fn insert_active(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    new: &NewSecret,
) -> Result<(), DomainError> {
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
    create_row(repo, scope, am).await
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
    reason = "one CAS with every field it may update"
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
) -> Result<Option<SecretRow>, DomainError> {
    let scope = scope.clone();
    repo.db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                switch_value_tx(
                    tx,
                    &scope,
                    id,
                    expected_version,
                    sharing,
                    fallback,
                    expires_at,
                    new_value_version,
                )
                .await
            })
                as Pin<Box<dyn Future<Output = Result<Option<SecretRow>, DomainError>> + Send + '_>>
        })
        .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update"
)]
async fn switch_value_tx(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    id: Uuid,
    expected_version: i64,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    new_value_version: ValueVersion,
) -> Result<Option<SecretRow>, DomainError> {
    let now = OffsetDateTime::now_utc();

    // The compare-and-set: `version` is the one the caller read before its
    // `plugin.put`, so a concurrent change of any kind matches nothing.
    // Accepts either resting status: a `declared` row switches to `active`
    // exactly like an `active` row being rotated (ADR-0004, "Writing a value
    // to a suppressed record is not a conflict").
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::ValueVersion,
            Expr::value(Some(new_value_version.0)),
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
        return Ok(None);
    }

    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(tx)
        .await
        .map_err(map_scope_err)?
        .ok_or_else(|| DomainError::internal("switch_value: row vanished after its own update"))?;
    Some(entity_to_model(row)).transpose()
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
            })
                as Pin<Box<dyn Future<Output = Result<Option<SecretRow>, DomainError>> + Send + '_>>
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

/// Secret removal (ADR-0004 `PATCH {"secret": null}`): one compare-and-set -
/// nulls the pointer, moves the row to `declared`, applies the merged
/// metadata, and returns the value version the row held so the caller can
/// best-effort `destroy` it. Never touches the store.
pub(super) async fn remove_value(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
) -> Result<Option<(SecretRow, Option<ValueVersion>)>, DomainError> {
    let scope = scope.clone();
    repo.db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                remove_value_tx(
                    tx,
                    &scope,
                    id,
                    expected_version,
                    sharing,
                    fallback,
                    expires_at,
                )
                .await
            })
                as Pin<
                    Box<
                        dyn Future<
                                Output = Result<
                                    Option<(SecretRow, Option<ValueVersion>)>,
                                    DomainError,
                                >,
                            > + Send
                            + '_,
                    >,
                >
        })
        .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "one CAS with every field it may update"
)]
async fn remove_value_tx(
    tx: &DbTx<'_>,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
) -> Result<Option<(SecretRow, Option<ValueVersion>)>, DomainError> {
    let now = OffsetDateTime::now_utc();

    // Lock + read so the value version we hand back for destroy is exactly
    // the one this transaction nulls, atomically.
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
    let old_value_version = current.value_version.clone();
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
    let row = entity_to_model(row)?;
    Ok(Some((row, old_value_version.map(ValueVersion))))
}

pub(super) async fn delete_by_id(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    key: &StoreKey,
    expected_version: Option<i64>,
) -> Result<(), DomainError> {
    let scope = scope.clone();
    let key = key.clone();
    let purge = Arc::clone(&repo.purge);
    let wake = repo
        .db
        .transaction(move |tx: &DbTx<'_>| {
            Box::pin(async move {
                delete_row_tx(purge.as_ref(), tx, &scope, &key, expected_version).await
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<toolkit_db::outbox::Wake, DomainError>>
                            + Send
                            + '_,
                    >,
                >
        })
        .await?;
    // Committed: wake the sequencer.
    wake.fire();
    Ok(())
}

/// One transaction: `DELETE` the row (CAS on `expected_version` when given;
/// 0 rows affected is `NotFound`) and enqueue the key purge.
async fn delete_row_tx(
    purge: &dyn PurgeEnqueuer,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    key: &StoreKey,
    expected_version: Option<i64>,
) -> Result<toolkit_db::outbox::Wake, DomainError> {
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
    purge.enqueue_purge(tx, key).await
}
