//! Repository for the `files` table (logical file identity + content pointer).

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, OwnerFilter};

use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::file::{ActiveModel, Column, Entity};

/// Repository over the `files` table.
#[derive(Clone, Default)]
pub struct FileRepo;

impl FileRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Insert a brand-new file row (no content bound yet).
    pub async fn create<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file: &File,
    ) -> Result<(), DomainError> {
        let am = ActiveModel {
            file_id: Set(file.file_id),
            tenant_id: Set(file.tenant_id),
            owner_kind: Set(file.owner_kind.as_str().to_owned()),
            owner_id: Set(file.owner_id),
            name: Set(file.name.clone()),
            gts_file_type: Set(file.gts_file_type.clone()),
            content_id: Set(file.content_id),
            meta_version: Set(file.meta_version),
            created_at: Set(file.created_at),
            last_modified_at: Set(file.last_modified_at),
        };
        secure_insert::<Entity>(am, scope, conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Fetch a file by id, tenant-scoped.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Option<File>, DomainError> {
        let found = Entity::find()
            .filter(Column::FileId.eq(file_id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_err)?;
        Ok(found.map(Into::into))
    }

    /// List files for a mandatory owner filter, newest first, offset-paginated.
    pub async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        owner: OwnerFilter,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<File>, DomainError> {
        let rows = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::OwnerKind.eq(owner.owner_kind.as_str()))
                    .add(Column::OwnerId.eq(owner.owner_id)),
            )
            .order_by_desc(Column::CreatedAt)
            .limit(limit)
            .offset(offset)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Optimistic compare-and-swap of the content pointer (the bind operation).
    ///
    /// Sets `content_id := new_content` only if the current `content_id` equals
    /// `expected` (or both are NULL for the first bind). Returns `true` on a
    /// successful swap, `false` on an `If-Match` conflict (412).
    pub async fn bind_content_cas<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        expected: Option<Uuid>,
        new_content: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let mut predicate = Condition::all().add(Column::FileId.eq(file_id));
        predicate = match expected {
            Some(v) => predicate.add(Column::ContentId.eq(v)),
            None => predicate.add(Column::ContentId.is_null()),
        };

        let res = Entity::update_many()
            .col_expr(Column::ContentId, Expr::value(new_content))
            .col_expr(Column::LastModifiedAt, Expr::value(now))
            .filter(predicate)
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// Bump `meta_version` and `last_modified_at` for a metadata-only write,
    /// optionally guarded by an `If-Match-Metadata` prepredicateition on the current
    /// `meta_version`. Returns `false` if the prepredicateition did not match.
    pub async fn touch_meta<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        expected_meta_version: Option<i64>,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let mut predicate = Condition::all().add(Column::FileId.eq(file_id));
        if let Some(mv) = expected_meta_version {
            predicate = predicate.add(Column::MetaVersion.eq(mv));
        }

        let res = Entity::update_many()
            .col_expr(Column::MetaVersion, Expr::col(Column::MetaVersion).add(1))
            .col_expr(Column::LastModifiedAt, Expr::value(now))
            .filter(predicate)
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// Delete a file (FK cascade removes its versions and custom metadata).
    /// Returns `true` if a row was removed.
    pub async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<bool, DomainError> {
        let res = Entity::delete_many()
            .filter(Column::FileId.eq(file_id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// List files across all tenants for the retention sweep engine,
    /// **keyset-paginated by `file_id`** to bound sweep memory on large
    /// deployments. Returns up to `limit` files ordered by `file_id`, starting
    /// strictly after `after` (`None` = from the beginning); the caller loops,
    /// advancing `after` to the last returned `file_id`, until it gets a short
    /// page. Keyset (not offset) paging is used so that deleting expired files
    /// mid-sweep does not shift the window and skip rows.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_all_for_sweep<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        let mut query = Entity::find();
        if let Some(after_id) = after {
            query = query.filter(Column::FileId.gt(after_id));
        }
        let rows = query
            .order_by_asc(Column::FileId)
            .limit(limit)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Update `owner_kind` and `owner_id` for a file row, and bump
    /// `last_modified_at`. Returns `true` if a row was found and updated.
    ///
    /// @cpt-cf-file-storage-fr-ownership-transfer
    pub async fn update_owner<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        new_owner_kind: &str,
        new_owner_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let res = Entity::update_many()
            .col_expr(Column::OwnerKind, Expr::value(new_owner_kind.to_owned()))
            .col_expr(Column::OwnerId, Expr::value(new_owner_id))
            .col_expr(Column::LastModifiedAt, Expr::value(now))
            .filter(Column::FileId.eq(file_id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }
}
