//! Repository for the `file_versions` table (immutable content versions).

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{FileVersion, VersionStatus};

use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::file_version::{ActiveModel, Column, Entity};

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
            status: Set(v.status.as_str().to_owned()),
            is_current: Set(v.is_current),
            backend_id: Set(v.backend_id.clone()),
            backend_path: Set(v.backend_path.clone()),
            created_at: Set(v.created_at),
        };
        secure_insert::<Entity>(am, scope, conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Fetch a single version by `(file_id, version_id)`.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        // Look up within the file's versions (a small set) and match the
        // version id in Rust. A direct `version_id = ?` predicate on the
        // composite-PK column proved unreliable across the secure layer.
        let all = self.list_by_file(conn, scope, file_id).await?;
        Ok(all.into_iter().find(|v| v.version_id == version_id))
    }

    /// List all versions of a file, newest first.
    pub async fn list_by_file<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let rows = Entity::find()
            .filter(Column::FileId.eq(file_id))
            .order_by_desc(Column::CreatedAt)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
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
                    .add(Column::VersionId.eq(version_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Record the streamed content's size and hash and mark the version
    /// `available` (the sidecar calls this after durably writing the bytes).
    pub async fn finalize<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<bool, DomainError> {
        // Scope the update to the full `(file_id, version_id)` key so a
        // version_id that belongs to a different file cannot be finalized here.
        let res = Entity::update_many()
            .col_expr(Column::Size, Expr::value(size))
            .col_expr(Column::HashValue, Expr::value(hash_value))
            .col_expr(
                Column::Status,
                Expr::value(file_storage_sdk::VersionStatus::Available.as_str()),
            )
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected == 1)
    }

    /// Clear the `is_current` flag on all versions of a file (used before
    /// promoting a new current version, to honour the unique-current index).
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
            .map_err(db_err)?;
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
            .map_err(db_err)?;
        Ok(())
    }

    /// Delete a single version. Returns `true` if a row was removed.
    pub async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<bool, DomainError> {
        let res = Entity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// List all `pending` version rows whose `created_at` is older than
    /// `older_than`. Used by the orphan-reconciliation sweep.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    pub async fn list_pending_older_than<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        older_than: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let rows = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::Status.eq(VersionStatus::Pending.as_str()))
                    .add(Column::CreatedAt.lt(older_than)),
            )
            .order_by_asc(Column::CreatedAt)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// List all non-current version rows whose `created_at` is older than
    /// `older_than`. Used by the retention-policy sweep for superseded versions.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_non_current_older_than<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        older_than: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let rows = Entity::find()
            .filter(
                Condition::all()
                    // Restrict to finalized/available versions so unfinished
                    // `pending` uploads are left to the orphan-reconciliation
                    // sweep (`list_pending_older_than`) and never deleted here
                    // as retention-superseded.
                    .add(Column::Status.eq(VersionStatus::Available.as_str()))
                    .add(Column::IsCurrent.eq(false))
                    .add(Column::CreatedAt.lt(older_than)),
            )
            .order_by_asc(Column::CreatedAt)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Transactionally update `backend_id` and `backend_path` for a version row.
    /// Used by backend migration.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
    pub async fn rebind_backend<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        version_id: Uuid,
        new_backend_id: &str,
        new_backend_path: &str,
    ) -> Result<bool, DomainError> {
        let res = Entity::update_many()
            .col_expr(Column::BackendId, Expr::value(new_backend_id))
            .col_expr(Column::BackendPath, Expr::value(new_backend_path))
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::VersionId.eq(version_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }
}
