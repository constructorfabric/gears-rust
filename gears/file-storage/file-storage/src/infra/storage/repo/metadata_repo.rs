//! Repository for the `files_custom_metadata` table (user key/value pairs).

use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::CustomMetadataEntry;

use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::custom_metadata::{ActiveModel, Column, Entity};

/// Repository over the `files_custom_metadata` table.
#[derive(Clone, Default)]
pub struct MetadataRepo;

impl MetadataRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// List all custom-metadata entries of a file, ordered by key.
    pub async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Vec<CustomMetadataEntry>, DomainError> {
        let rows = Entity::find()
            .filter(Column::FileId.eq(file_id))
            .order_by_asc(Column::Key)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Upsert one key (delete-then-insert; merge-patch semantics live in the
    /// service). Custom-metadata writes never carry tenant data of their own —
    /// the parent file is already authorized.
    pub async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        key: &str,
        value: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        self.delete_key(conn, scope, file_id, key).await?;
        let am = ActiveModel {
            file_id: Set(file_id),
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            set_at: Set(now),
        };
        secure_insert::<Entity>(am, scope, conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Delete one key. Returns `true` if a row was removed.
    pub async fn delete_key<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        key: &str,
    ) -> Result<bool, DomainError> {
        let res = Entity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::Key.eq(key)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }
}
