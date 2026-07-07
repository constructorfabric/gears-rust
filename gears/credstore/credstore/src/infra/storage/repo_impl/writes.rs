//! Write-path repo methods: `insert_provisioning`, `mark_active`,
//! `mark_deprovisioning`, `touch`, `backfill_fp`, `delete_by_id`,
//! `list_stale_pending`, `list_unfenced`, `reap_by_id`.

use credstore_sdk::SharingMode;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::secure::{SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{NewSecret, SecretRow, SecretStatus};
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, entity_to_model, map_scope_err, sharing_to_i16,
};

pub(super) async fn insert_provisioning(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    new: &NewSecret,
) -> Result<(), DomainError> {
    use sea_orm::ActiveValue;
    let conn = repo.db.conn()?;
    let now = OffsetDateTime::now_utc();
    let am = entity::secrets::ActiveModel {
        id: ActiveValue::Set(new.id),
        tenant_id: ActiveValue::Set(new.tenant_id.0),
        reference: ActiveValue::Set(new.reference.as_ref().to_owned()),
        sharing: ActiveValue::Set(sharing_to_i16(new.sharing)),
        owner_id: ActiveValue::Set(new.owner_id.0),
        status: ActiveValue::Set(SecretStatus::Provisioning.as_smallint()),
        created_at: ActiveValue::Set(now),
        updated_at: ActiveValue::Set(now),
        version: ActiveValue::NotSet,
        secret_type_uuid: ActiveValue::Set(new.secret_type_uuid),
        expires_at: ActiveValue::Set(new.expires_at),
        value_fp: ActiveValue::Set(Some(new.value_fp.clone())),
        fp_key_id: ActiveValue::Set(Some(new.fp_key_id)),
    };
    // scope_unchecked: INSERT cannot subtree-clamp on a row that doesn't exist yet.
    entity::secrets::Entity::insert(am)
        .secure()
        .scope_unchecked(scope)
        .map_err(map_scope_err)?
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;
    Ok(())
}

pub(super) async fn mark_active(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
) -> Result<(), DomainError> {
    let conn = repo.db.conn()?;
    let now = OffsetDateTime::now_utc();
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::Status,
            Expr::value(SecretStatus::Active.as_smallint()),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Id.eq(id))
                .add(entity::secrets::Column::Status.eq(SecretStatus::Provisioning.as_smallint())),
        )
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    if rows_affected == 0 {
        return Err(DomainError::Conflict);
    }
    Ok(())
}

pub(super) async fn mark_deprovisioning(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
) -> Result<bool, DomainError> {
    let conn = repo.db.conn()?;
    let now = OffsetDateTime::now_utc();
    // Stamp updated_at: the deprovisioning-timeout clock the reaper keys off.
    // The version is deliberately left alone so an If-Match retry of the same
    // delete still matches the version the client saw.
    let mut filter = Condition::all()
        .add(entity::secrets::Column::Id.eq(id))
        .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint()));
    if let Some(v) = expected_version {
        filter = filter.add(entity::secrets::Column::Version.eq(v));
    }
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::Status,
            Expr::value(SecretStatus::Deprovisioning.as_smallint()),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .filter(filter)
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    Ok(rows_affected > 0)
}

pub(super) async fn touch(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    sharing: SharingMode,
    expected_version: Option<i64>,
    expires_at: Option<OffsetDateTime>,
    value_fp: Vec<u8>,
) -> Result<Option<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    let now = OffsetDateTime::now_utc();
    // Atomic, id-keyed: version = version + 1, set sharing, stamp updated_at,
    // and re-stamp the value fingerprint — the fp travels in the SAME UPDATE
    // as the sharing label, so a fingerprint match on read transitively
    // proves value and metadata came from one writer (the fence invariant).
    // Keyed by id (the row found by find_for_write) so it needs no sharing-class
    // filter and works for both private (sharing unchanged) and non-private
    // (tenant<->shared) rows. When expected_version is set, the bump is gated on
    // version = expected so a stale optimistic-lock write commits 0 rows.
    let mut filter = Condition::all()
        .add(entity::secrets::Column::Id.eq(id))
        .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint()));
    if let Some(v) = expected_version {
        filter = filter.add(entity::secrets::Column::Version.eq(v));
    }
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::Sharing,
            Expr::value(sharing_to_i16(sharing)),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .col_expr(
            entity::secrets::Column::Version,
            Expr::col(entity::secrets::Column::Version).add(1_i64),
        )
        // Whole-value replace: the new expiry (or its absence) wins.
        .col_expr(entity::secrets::Column::ExpiresAt, Expr::value(expires_at))
        .col_expr(
            entity::secrets::Column::ValueFp,
            Expr::value(Some(value_fp)),
        )
        .col_expr(
            entity::secrets::Column::FpKeyId,
            Expr::value(Some(crate::domain::secret::fence::CURRENT_FENCE_KEY_ID)),
        )
        .filter(filter)
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    if rows_affected == 0 {
        return Ok(None);
    }
    // Re-read the updated row by id.
    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(entity::secrets::Column::Id.eq(id)))
        .one(&conn)
        .await
        .map_err(map_scope_err)?;
    row.map(entity_to_model).transpose()
}

/// Stamp the fence fingerprint onto a row that has none (out-of-band seeded):
/// CAS on `value_fp IS NULL`, so a concurrent PUT that already stamped wins
/// (0 rows → `false`, a no-op for the caller). Deliberately does NOT bump
/// `version` or `updated_at` — nothing client-visible changed, the caller's
/// `ETag` must stay stable. Unscoped (system-side heal, like the reaper).
pub(super) async fn backfill_fp(
    repo: &SecretRepoImpl,
    id: Uuid,
    value_fp: Vec<u8>,
    fp_key_id: i16,
) -> Result<bool, DomainError> {
    let conn = repo.db.conn()?;
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::ValueFp,
            Expr::value(Some(value_fp)),
        )
        .col_expr(
            entity::secrets::Column::FpKeyId,
            Expr::value(Some(fp_key_id)),
        )
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Id.eq(id))
                .add(entity::secrets::Column::ValueFp.is_null()),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    Ok(rows_affected > 0)
}

/// Active rows still missing a fence fingerprint (out-of-band seeded and not
/// yet read), bounded batch for the reaper's backfill sweep. Unscoped.
pub(super) async fn list_unfenced(
    repo: &SecretRepoImpl,
    limit: u64,
) -> Result<Vec<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    let rows = entity::secrets::Entity::find()
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint()))
                .add(entity::secrets::Column::ValueFp.is_null()),
        )
        // Oldest-first for a fair, deterministic sweep across ticks (same
        // reasoning as list_stale_pending).
        .order_by(entity::secrets::Column::UpdatedAt, Order::Asc)
        .limit(limit)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    rows.into_iter().map(entity_to_model).collect()
}

pub(super) async fn delete_by_id(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    expected_version: Option<i64>,
) -> Result<(), DomainError> {
    let conn = repo.db.conn()?;
    let mut filter = Condition::all().add(entity::secrets::Column::Id.eq(id));
    if let Some(v) = expected_version {
        filter = filter.add(entity::secrets::Column::Version.eq(v));
    }
    let rows_affected = entity::secrets::Entity::delete_many()
        .filter(filter)
        .secure()
        .scope_with(scope)
        .exec(&conn)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    if rows_affected == 0 {
        return Err(DomainError::NotFound);
    }
    Ok(())
}

/// Cutoff instant `older_than_secs` ago, saturating to "never reap" for absurd
/// configs. A u64 beyond `i64::MAX` would wrap to a negative duration and match
/// rows from the future; equally, subtracting a duration large enough to leave
/// the representable `OffsetDateTime` range panics. We therefore clamp the
/// offset so the subtraction always lands on a valid instant: any cutoff at or
/// before the min representable date means nothing is ever old enough to reap,
/// which is the safe bound.
fn cutoff(older_than_secs: u64) -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    let secs = i64::try_from(older_than_secs).unwrap_or(i64::MAX);
    now.checked_sub(time::Duration::seconds(secs))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

pub(super) async fn list_stale_pending(
    repo: &SecretRepoImpl,
    provisioning_older_than_secs: u64,
    deprovisioning_older_than_secs: u64,
    limit: u64,
) -> Result<Vec<SecretRow>, DomainError> {
    let conn = repo.db.conn()?;
    // Both arms key off updated_at (idx_credstore_pending): provisioning rows
    // are never updated after insert, so updated_at equals created_at there.
    let rows = entity::secrets::Entity::find()
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(
                            entity::secrets::Column::Status
                                .eq(SecretStatus::Provisioning.as_smallint()),
                        )
                        .add(
                            entity::secrets::Column::UpdatedAt
                                .lt(cutoff(provisioning_older_than_secs)),
                        ),
                )
                .add(
                    Condition::all()
                        .add(
                            entity::secrets::Column::Status
                                .eq(SecretStatus::Deprovisioning.as_smallint()),
                        )
                        .add(
                            entity::secrets::Column::UpdatedAt
                                .lt(cutoff(deprovisioning_older_than_secs)),
                        ),
                ),
        )
        // Oldest-first, so a batch of persistently-failing deprovisioning rows
        // (kept each tick until their backend delete finally succeeds) cannot
        // starve newer stale rows: without a deterministic order the DB may
        // return the same physical-order rows every tick, and once ≥ `limit`
        // of them wedge, no other stale row is ever reaped.
        .order_by(entity::secrets::Column::UpdatedAt, Order::Asc)
        .limit(limit)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .map_err(map_scope_err)?;
    rows.into_iter().map(entity_to_model).collect()
}

pub(super) async fn mark_expired_deprovisioning(repo: &SecretRepoImpl) -> Result<u64, DomainError> {
    let conn = repo.db.conn()?;
    let now = OffsetDateTime::now_utc();
    // Expired active rows enter the ordinary deprovisioning saga: invisible
    // to resolution immediately, name held until backend cleanup completes
    // via the pending sweep. Uses idx_credstore_expiry.
    let rows_affected = entity::secrets::Entity::update_many()
        .col_expr(
            entity::secrets::Column::Status,
            Expr::value(SecretStatus::Deprovisioning.as_smallint()),
        )
        .col_expr(entity::secrets::Column::UpdatedAt, Expr::value(now))
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Status.eq(SecretStatus::Active.as_smallint()))
                .add(entity::secrets::Column::ExpiresAt.is_not_null())
                .add(entity::secrets::Column::ExpiresAt.lte(now)),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    Ok(rows_affected)
}

pub(super) async fn reap_by_id(
    repo: &SecretRepoImpl,
    id: Uuid,
    expected: SecretStatus,
) -> Result<bool, DomainError> {
    let conn = repo.db.conn()?;
    // Status-gated: the reaper observed this row in `expected` status, but a
    // concurrent saga may have moved it on since (most importantly a slow
    // create's `mark_active` flipping `Provisioning → Active`). Guarding the
    // delete on the observed status makes it mutually exclusive with that
    // transition — 0 rows means the row is no longer ours to reap, so the live
    // secret (and its backend value) is left intact. `false` (already gone or
    // moved on) is benign; the caller reports it as "not reaped".
    let rows_affected = entity::secrets::Entity::delete_many()
        .filter(
            Condition::all()
                .add(entity::secrets::Column::Id.eq(id))
                .add(entity::secrets::Column::Status.eq(expected.as_smallint())),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .map_err(map_scope_err)?
        .rows_affected;
    Ok(rows_affected > 0)
}
