//! Repository for the `files_custom_metadata` table (user key/value pairs).

use std::collections::HashMap;

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

    /// Batched counterpart of `list`: fetch custom-metadata entries for many
    /// files in a single `IN (...)` query, grouped by `file_id`. Used by
    /// `GET /files` listing so that rendering a page of `N` files' metadata
    /// costs one query instead of `N` (see `Store::list_metadata_for_files`).
    /// A `file_id` with no custom metadata simply has no entry in the
    /// returned map (never an empty `Vec` — callers should treat "absent" and
    /// "empty" the same way, e.g. via `.get(&id).cloned().unwrap_or_default()`).
    pub async fn list_for_files<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        if file_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = Entity::find()
            .filter(Column::FileId.is_in(file_ids.iter().copied()))
            .order_by_asc(Column::Key)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        let mut grouped: HashMap<Uuid, Vec<CustomMetadataEntry>> = HashMap::new();
        for row in rows {
            grouped
                .entry(row.file_id)
                .or_default()
                .push(CustomMetadataEntry {
                    key: row.key,
                    value: row.value,
                });
        }
        Ok(grouped)
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
