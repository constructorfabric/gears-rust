//! Repository for the `file_versions` table (immutable content versions).

use sea_orm::sea_query::{Expr, Query};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{FileVersion, VersionStatus};

use crate::domain::error::DomainError;
use crate::infra::storage::entity::file_version::{ActiveModel, Column, Entity};
use crate::infra::storage::entity::multipart_upload::{
    Column as MultipartUploadColumn, Entity as MultipartUploadEntity,
};
use crate::infra::storage::entity::version_hash_manifest::{
    ActiveModel as ManifestActiveModel, Column as ManifestColumn, Entity as ManifestEntity,
};

/// Repository over the `file_versions` table.
#[derive(Clone, Default)]
pub struct VersionRepo;

impl VersionRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Pre-register a version row (typically `status = pending`).
    pub async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        v: &FileVersion,
    ) -> Result<(), DomainError> {
        let am = ActiveModel {
            file_id: Set(v.file_id),
            version_id: Set(v.version_id),
            mime_type: Set(v.mime_type.clone()),
            size: Set(v.size),
            hash_algorithm: Set(v.hash_algorithm.clone()),
            hash_value: Set(v.hash_value.clone()),
            hash_mode: Set(v.hash_mode.clone()),
            part_count: Set(v.part_count),
            status: Set(v.status.as_str().to_owned()),
            is_current: Set(v.is_current),
            backend_id: Set(v.backend_id.clone()),
            backend_path: Set(v.backend_path.clone()),
            created_at: Set(v.created_at),
        };
        secure_insert::<Entity>(am, scope, conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Fetch a single version by `(file_id, version_id)` with a direct predicate.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        let found = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id)),
            )
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(found.map(Into::into))
    }

    /// List a page of a file's versions, newest first.
    pub async fn list_by_file<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let rows = Entity::find()
            .filter(Column::FileId.eq(file_id))
            .order_by_desc(Column::CreatedAt)
            .limit(limit)
            .offset(offset)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Mark a version `available` (after its bytes are durably written).
    pub async fn mark_available<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<(), DomainError> {
        Entity::update_many()
            .col_expr(
                Column::Status,
                Expr::value(file_storage_sdk::VersionStatus::Available.as_str()),
            )
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id))
                    .add(Column::Status.eq(VersionStatus::Pending.as_str())),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Record the content's size and hash and mark a `pending` version `available`.
    ///
    /// `hash_mode`/`part_count` are set here, not at insert time, since a pending row
    /// is created before it is known whether the upload is single-part or multipart.
    #[allow(clippy::too_many_arguments)]
    pub async fn finalize<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        hash_mode: &str,
        part_count: Option<i32>,
        mime_type: Option<String>,
    ) -> Result<bool, DomainError> {
        let mut update = Entity::update_many()
            .col_expr(Column::Size, Expr::value(size))
            .col_expr(Column::HashValue, Expr::value(hash_value))
            .col_expr(Column::HashMode, Expr::value(hash_mode))
            .col_expr(Column::PartCount, Expr::value(part_count))
            .col_expr(
                Column::Status,
                Expr::value(file_storage_sdk::VersionStatus::Available.as_str()),
            );
        // `mime_type` is rewritten only when given (single-part finalize); multipart
        // complete passes `None` and keeps the declared type.
        if let Some(mime_type) = mime_type {
            update = update.col_expr(Column::MimeType, Expr::value(mime_type));
        }
        let res = update
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id))
                    .add(Column::Status.eq(VersionStatus::Pending.as_str())),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(res.rows_affected == 1)
    }

    /// Insert the `version_hash_manifest` row for a `multipart-composite-sha256`
    /// version; call in the same transaction as `finalize`.
    pub async fn insert_manifest<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        version_id: Uuid,
        manifest: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let am = ManifestActiveModel {
            version_id: Set(version_id),
            manifest: Set(manifest.to_owned()),
            created_at: Set(now),
        };
        secure_insert::<ManifestEntity>(am, scope, conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Fetch the manifest text of a version, if any (`multipart-composite-sha256` only).
    pub async fn get_manifest<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        version_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        let found = ManifestEntity::find()
            .filter(ManifestColumn::VersionId.eq(version_id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(found.map(|m| m.manifest))
    }

    /// Clear `is_current` on all versions of a file (before promoting a new current one).
    pub async fn clear_current<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<(), DomainError> {
        Entity::update_many()
            .col_expr(Column::IsCurrent, Expr::value(false))
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::IsCurrent.eq(true)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Promote one version to `is_current = true`.
    pub async fn set_current<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<(), DomainError> {
        Entity::update_many()
            .col_expr(Column::IsCurrent, Expr::value(true))
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// Delete a single version; returns the number of rows removed (0 or 1).
    ///
    /// Guarded with `is_current = false` in the same statement, so a concurrent `bind`
    /// that promoted this version cannot be raced into deleting the current content.
    pub async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<u64, DomainError> {
        let res = Entity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id))
                    .add(Column::IsCurrent.eq(false)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(res.rows_affected)
    }

    /// Delete a version row iff its `status` matches `expected`; `false` if missing or
    /// already moved on. Lets the sweep avoid deleting a pending version that a racing
    /// `complete_multipart_upload` just made `available`.
    pub async fn delete_if_status<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
        expected: VersionStatus,
    ) -> Result<bool, DomainError> {
        let res = Entity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id))
                    .add(Column::Status.eq(expected.as_str())),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(res.rows_affected > 0)
    }

    /// List `pending` versions created before `older_than`, excluding versions backing
    /// a live `in_progress` session (`expires_at > now`).
    ///
    /// A long multipart upload keeps its version `pending` for the whole session; an
    /// already-expired session is not excluded and is aborted by the sweep first.
    pub async fn list_pending_older_than<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        older_than: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let rows = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::Status.eq(VersionStatus::Pending.as_str()))
                    .add(Column::CreatedAt.lt(older_than))
                    .add(
                        Column::VersionId.not_in_subquery(
                            Query::select()
                                .column(MultipartUploadColumn::VersionId)
                                .from(MultipartUploadEntity)
                                .and_where(MultipartUploadColumn::State.eq("in_progress"))
                                .and_where(MultipartUploadColumn::ExpiresAt.gt(now))
                                .to_owned(),
                        ),
                    ),
            )
            .order_by_asc(Column::CreatedAt)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Update `backend_id`/`backend_path`, CAS-gated on the current values (backend
    /// migration). `false` means the version is gone or another migration moved it
    /// first; the caller must re-fetch to tell which.
    #[allow(clippy::too_many_arguments)]
    pub async fn rebind_backend<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
        expected_backend_id: &str,
        expected_backend_path: &str,
        new_backend_id: &str,
        new_backend_path: &str,
    ) -> Result<bool, DomainError> {
        let res = Entity::update_many()
            .col_expr(Column::BackendId, Expr::value(new_backend_id))
            .col_expr(Column::BackendPath, Expr::value(new_backend_path))
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id))
                    .add(Column::BackendId.eq(expected_backend_id))
                    .add(Column::BackendPath.eq(expected_backend_path)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(res.rows_affected > 0)
    }
}
