//! Repository for `multipart_uploads` and `multipart_upload_parts`.
//!
//! No `tenant_id` column, so all queries use `AccessScope::allow_all()`; the tenant
//! boundary is enforced through the parent `files` row before a session is created.

use sea_orm::sea_query::LockType;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession, MultipartUploadState};
use crate::infra::storage::entity::multipart_upload::{
    ActiveModel as UploadActiveModel, Column as UploadColumn, Entity as UploadEntity,
    Model as UploadModel,
};
use crate::infra::storage::entity::multipart_upload_part::{
    ActiveModel as PartActiveModel, Column as PartColumn, Entity as PartEntity, Model as PartModel,
};

/// Repository for multipart upload sessions and their parts.
#[derive(Clone, Default)]
pub struct MultipartRepo;

impl MultipartRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Insert a new multipart upload session row.
    #[allow(clippy::too_many_arguments)]
    pub async fn create<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        backend_id: &str,
        backend_path: &str,
        declared_mime: &str,
        declared_size: u64,
        part_size: u64,
        auto_bind: bool,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let declared_size_i64 = i64::try_from(declared_size)
            .map_err(|_| DomainError::validation("declared_size", "declared_size overflows i64"))?;
        let part_size_i64 = i64::try_from(part_size)
            .map_err(|_| DomainError::validation("part_size", "part_size overflows i64"))?;
        let am = UploadActiveModel {
            upload_id: Set(upload_id),
            file_id: Set(file_id),
            version_id: Set(version_id),
            backend_upload_handle: Set(backend_upload_handle.to_owned()),
            state: Set("in_progress".to_owned()),
            declared_mime: Set(declared_mime.to_owned()),
            mime_validated: Set(false),
            declared_size: Set(declared_size_i64),
            part_size: Set(part_size_i64),
            auto_bind: Set(auto_bind),
            lease_until: Set(None),
            lease_owner: Set(None),
            complete_result: Set(None),
            backend_id: Set(Some(backend_id.to_owned())),
            backend_path: Set(Some(backend_path.to_owned())),
            created_at: Set(now),
            expires_at: Set(expires_at),
        };
        secure_insert::<UploadEntity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Lock a `multipart_uploads` row (`SELECT ... FOR UPDATE`) and return its `state`, or `None`.
    /// Call only as the transaction's FIRST statement (parent-before-children ordering, see
    /// `FileRepo::lock_for_update`), so finalize cannot commit against a session the sweep aborted.
    pub async fn lock_session_state<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        let found = UploadEntity::find()
            .filter(UploadColumn::UploadId.eq(upload_id))
            .lock(LockType::Update)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(found.map(|m| m.state))
    }

    /// Fetch a multipart upload session by `upload_id`.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
    ) -> Result<Option<MultipartUploadSession>, DomainError> {
        let found = UploadEntity::find()
            .filter(UploadColumn::UploadId.eq(upload_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(conn)
            .await
            .map_err(DomainError::from)?;
        found.map(session_from_model).transpose()
    }

    /// Compare-and-set the session `state`: transitions only if currently
    /// `expected_state`. Returns `false` on a stale transition (e.g. a
    /// `complete`/`abort` race).
    ///
    /// `mime_validated`, when `Some`, is set in the same UPDATE (`complete` passes
    /// `true` after sniffing; `abort` passes `None`).
    pub async fn update_state<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        expected_state: &str,
        new_state: &str,
        mime_validated: Option<bool>,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let mut update =
            UploadEntity::update_many().col_expr(UploadColumn::State, Expr::value(new_state));
        if let Some(validated) = mime_validated {
            update = update.col_expr(UploadColumn::MimeValidated, Expr::value(validated));
        }
        let res = update
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::UploadId.eq(upload_id))
                    .add(UploadColumn::State.eq(expected_state)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(res.rows_affected > 0)
    }

    /// Acquire (or take over) the completion lease with one conditional UPDATE from `in_progress`
    /// or an EXPIRED `completing`; `false` = live lease elsewhere, terminal, or expired.
    /// Fenced by `expires_at > now` in the CAS itself: the service's snapshot check cannot see a
    /// session that expired since.
    pub async fn acquire_complete_lease<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = UploadEntity::update_many()
            .col_expr(UploadColumn::State, Expr::value("completing"))
            .col_expr(UploadColumn::LeaseUntil, Expr::value(lease_until))
            .col_expr(UploadColumn::LeaseOwner, Expr::value(owner))
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::UploadId.eq(upload_id))
                    .add(UploadColumn::ExpiresAt.gt(now))
                    .add(claimable(now)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(res.rows_affected > 0)
    }

    /// Release a held lease back to `in_progress` (assembly failed). Scoped to `owner` so a
    /// takeover's lease is not clobbered by the crashed original.
    pub async fn release_complete_lease<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        owner: &str,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = clear_lease(
            UploadEntity::update_many().col_expr(UploadColumn::State, Expr::value("in_progress")),
        )
        .filter(
            sea_orm::Condition::all()
                .add(UploadColumn::UploadId.eq(upload_id))
                .add(UploadColumn::State.eq("completing"))
                .add(UploadColumn::LeaseOwner.eq(owner)),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(conn)
        .await
        .map_err(DomainError::from)?;
        Ok(res.rows_affected > 0)
    }

    /// Abort a `completing` session whose lease has EXPIRED: CAS `lease_until < now` -> `aborted`.
    pub async fn abort_expired_completing<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = clear_lease(
            UploadEntity::update_many().col_expr(UploadColumn::State, Expr::value("aborted")),
        )
        .filter(
            sea_orm::Condition::all()
                .add(UploadColumn::UploadId.eq(upload_id))
                .add(UploadColumn::State.eq("completing"))
                .add(UploadColumn::LeaseUntil.lt(now)),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(conn)
        .await
        .map_err(DomainError::from)?;
        Ok(res.rows_affected > 0)
    }

    /// Terminal transition `completing -> completed`, persisting the snapshot and clearing the lease.
    ///
    /// `expected_owner`: when `Some`, the CAS also requires `lease_owner = expected_owner`. `None`
    /// is for `finalize_multipart_version` only (same transaction as its won finalize CAS, which is
    /// fenced only by `status = 'pending'`; requiring the owner would strand the session after a
    /// takeover) and also accepts `in_progress`. `aborted`/`completed` are never matched.
    pub async fn finish_complete<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        expected_owner: Option<&str>,
        result_json: &str,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let mut condition = sea_orm::Condition::all().add(UploadColumn::UploadId.eq(upload_id));
        condition = match expected_owner {
            Some(owner) => condition
                .add(UploadColumn::State.eq("completing"))
                .add(UploadColumn::LeaseOwner.eq(owner)),
            None => condition.add(
                sea_orm::Condition::any()
                    .add(UploadColumn::State.eq("completing"))
                    .add(UploadColumn::State.eq("in_progress")),
            ),
        };
        let res = clear_lease(
            UploadEntity::update_many()
                .col_expr(UploadColumn::State, Expr::value("completed"))
                .col_expr(UploadColumn::MimeValidated, Expr::value(true))
                .col_expr(UploadColumn::CompleteResult, Expr::value(result_json)),
        )
        .filter(condition)
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(conn)
        .await
        .map_err(DomainError::from)?;
        Ok(res.rows_affected > 0)
    }

    /// Force-set a session's `expires_at`. **Test-support only; do not call in
    /// production.**
    ///
    /// `#[doc(hidden)]` rather than a feature or `#[cfg(test)]` because it is used by
    /// the external integration-test crate and a feature would break plain `cargo test`.
    #[doc(hidden)]
    pub async fn set_expires_at<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        expires_at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        use sea_orm::sea_query::Expr;
        UploadEntity::update_many()
            .col_expr(UploadColumn::ExpiresAt, Expr::value(expires_at))
            .filter(UploadColumn::UploadId.eq(upload_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Insert or replace a multipart upload part. If `part_number` already exists,
    /// replace it (idempotent re-upload of a part).
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_part<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        part_number: i32,
        backend_etag: &str,
        part_hash: Vec<u8>,
        size: i64,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        PartEntity::delete_many()
            .filter(
                sea_orm::Condition::all()
                    .add(PartColumn::UploadId.eq(upload_id))
                    .add(PartColumn::PartNumber.eq(part_number)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(DomainError::from)?;

        let am = PartActiveModel {
            upload_id: Set(upload_id),
            part_number: Set(part_number),
            backend_etag: Set(backend_etag.to_owned()),
            part_hash: Set(part_hash),
            size: Set(size),
            uploaded_at: Set(now),
        };
        secure_insert::<PartEntity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// List all parts for an upload, ordered by `part_number` ascending.
    pub async fn list_parts<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
    ) -> Result<Vec<MultipartPart>, DomainError> {
        let rows = PartEntity::find()
            .filter(PartColumn::UploadId.eq(upload_id))
            .order_by_asc(PartColumn::PartNumber)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(DomainError::from)?;
        rows.into_iter().map(part_from_model).collect()
    }

    pub async fn list_expired<C: DBRunner>(
        &self,
        conn: &C,
        now: OffsetDateTime,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        let rows = UploadEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::ExpiresAt.lt(now))
                    .add(claimable(now)),
            )
            .order_by_asc(UploadColumn::ExpiresAt)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(DomainError::from)?;
        rows.into_iter().map(session_from_model).collect()
    }

    /// Whether `file_id` has an active (`in_progress` or `completing`) multipart session.
    ///
    /// Guards orphan-file reconciliation: a pending version keyed only on age can look
    /// abandoned while a not-yet-reaped session uses it, and deleting the `files` row would
    /// cascade the session away. Existence uses `LIMIT 1` + `one()`, never `COUNT(*)`.
    pub async fn has_active_for_file<C: DBRunner>(
        &self,
        conn: &C,
        file_id: Uuid,
    ) -> Result<bool, DomainError> {
        let row = UploadEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::FileId.eq(file_id))
                    .add(
                        sea_orm::Condition::any()
                            .add(UploadColumn::State.eq("in_progress"))
                            .add(UploadColumn::State.eq("completing")),
                    ),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .limit(1)
            .one(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(row.is_some())
    }
}

/// Clear the completion lease columns on an UPDATE.
fn clear_lease(update: sea_orm::UpdateMany<UploadEntity>) -> sea_orm::UpdateMany<UploadEntity> {
    use sea_orm::sea_query::Expr;
    update
        .col_expr(
            UploadColumn::LeaseUntil,
            Expr::value(Option::<OffsetDateTime>::None),
        )
        .col_expr(
            UploadColumn::LeaseOwner,
            Expr::value(Option::<String>::None),
        )
}

/// Sessions `complete` may claim and the sweep may reap: `in_progress`, or `completing` with an
/// expired lease.
fn claimable(now: OffsetDateTime) -> sea_orm::Condition {
    sea_orm::Condition::any()
        .add(UploadColumn::State.eq("in_progress"))
        .add(
            sea_orm::Condition::all()
                .add(UploadColumn::State.eq("completing"))
                .add(UploadColumn::LeaseUntil.lt(now)),
        )
}

fn session_from_model(m: UploadModel) -> Result<MultipartUploadSession, DomainError> {
    // A persisted state we cannot parse is a data-contract violation, not an
    // `in_progress` session — surface it rather than manufacturing a default
    // that would let callers operate on a bogus session.
    let state = MultipartUploadState::parse(&m.state).ok_or_else(|| {
        DomainError::database(format!(
            "invalid multipart upload state in DB for {}: {}",
            m.upload_id, m.state
        ))
    })?;
    let missing = |column: &str| {
        DomainError::database(format!(
            "multipart upload {} has no {column} in DB",
            m.upload_id
        ))
    };
    let backend_id = m.backend_id.clone().ok_or_else(|| missing("backend_id"))?;
    let backend_path = m
        .backend_path
        .clone()
        .ok_or_else(|| missing("backend_path"))?;
    let declared_size = u64::try_from(m.declared_size).unwrap_or(0);
    let part_size = u64::try_from(m.part_size).unwrap_or(0);
    Ok(MultipartUploadSession {
        upload_id: m.upload_id,
        file_id: m.file_id,
        version_id: m.version_id,
        backend_upload_handle: m.backend_upload_handle,
        state,
        declared_mime: m.declared_mime,
        mime_validated: m.mime_validated,
        declared_size,
        part_size,
        auto_bind: m.auto_bind,
        lease_until: m.lease_until,
        complete_result: m.complete_result,
        backend_id,
        backend_path,
        created_at: m.created_at,
        expires_at: m.expires_at,
    })
}

fn part_from_model(m: PartModel) -> Result<MultipartPart, DomainError> {
    // Part numbers are `> 0` by DB CHECK; a value that does not fit `u32` is
    // corruption, not part `0`.
    let part_number = u32::try_from(m.part_number).map_err(|_| {
        DomainError::database(format!(
            "invalid part_number in DB for upload {}: {}",
            m.upload_id, m.part_number
        ))
    })?;
    Ok(MultipartPart {
        upload_id: m.upload_id,
        part_number,
        backend_etag: m.backend_etag,
        part_hash: m.part_hash,
        size: m.size,
        uploaded_at: m.uploaded_at,
    })
}
