//! Repository for `multipart_uploads` and `multipart_upload_parts`.
//!
//! No tenant isolation at the entity level (no `tenant_id` column) —
//! all queries use `AccessScope::allow_all()`. The tenant boundary is
//! enforced through the parent `files` row before a session is created.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession, MultipartUploadState};
use crate::infra::storage::db::db_err;
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
        declared_mime: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let am = UploadActiveModel {
            upload_id: Set(upload_id),
            file_id: Set(file_id),
            version_id: Set(version_id),
            backend_upload_handle: Set(backend_upload_handle.to_owned()),
            state: Set("in_progress".to_owned()),
            declared_mime: Set(declared_mime.to_owned()),
            mime_validated: Set(false),
            created_at: Set(now),
            expires_at: Set(expires_at),
        };
        // No tenant scope on this table — allow_all() is correct here.
        secure_insert::<UploadEntity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(db_err)?;
        Ok(())
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
            .map_err(db_err)?;
        found.map(session_from_model).transpose()
    }

    /// Compare-and-set the `state` of a multipart upload session: transition to
    /// `new_state` only if the row is currently in `expected_state`. Returns
    /// `true` if a row matched and was updated, `false` on a stale transition
    /// (e.g. a `complete`/`abort` race where another writer already moved it).
    pub async fn update_state<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        expected_state: &str,
        new_state: &str,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = UploadEntity::update_many()
            .col_expr(UploadColumn::State, Expr::value(new_state))
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::UploadId.eq(upload_id))
                    .add(UploadColumn::State.eq(expected_state)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
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
        // Delete existing row with the same PK first (insert-or-replace semantics).
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
            .map_err(db_err)?;

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
            .map_err(db_err)?;
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
            .map_err(db_err)?;
        rows.into_iter().map(part_from_model).collect()
    }

    /// List all `in_progress` upload sessions whose `expires_at` is before `now`.
    /// Used by the orphan-reconciliation sweep to clean up stale sessions.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    pub async fn list_expired<C: DBRunner>(
        &self,
        conn: &C,
        now: OffsetDateTime,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        let rows = UploadEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::State.eq("in_progress"))
                    .add(UploadColumn::ExpiresAt.lt(now)),
            )
            .order_by_asc(UploadColumn::ExpiresAt)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(db_err)?;
        rows.into_iter().map(session_from_model).collect()
    }
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
    Ok(MultipartUploadSession {
        upload_id: m.upload_id,
        file_id: m.file_id,
        version_id: m.version_id,
        backend_upload_handle: m.backend_upload_handle,
        state,
        declared_mime: m.declared_mime,
        mime_validated: m.mime_validated,
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
