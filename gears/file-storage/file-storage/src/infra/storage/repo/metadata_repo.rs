//! Repository for the `files_custom_metadata` table (user key/value pairs).

use std::collections::HashMap;

use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, max_bind_params_for, secure_insert,
    secure_insert_many,
};
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

    /// Bind parameters reserved out of the backend's [`max_bind_params_for`]
    /// budget for everything in `delete_keys`'s `WHERE` clause besides the
    /// `key IN (...)` list itself: the `file_id` equality predicate, plus
    /// whatever [`SecureDeleteExt::scope_with`] adds for the caller's
    /// [`AccessScope`] (tenant/owner columns and the like). Those predicates
    /// are few and fixed in count, but the exact number depends on the scope
    /// implementation, so this reserves headroom rather than counting them
    /// precisely -- a chunk that ends up a few keys smaller than strictly
    /// necessary is harmless, while one that overshoots the driver's real
    /// limit is a 500.
    const DELETE_KEYS_RESERVED_PARAMS: usize = 16;

    /// Batched counterpart of [`Self::delete_key`]: delete every row in
    /// `keys` for `file_id` via `DELETE ... WHERE key IN (...)`, instead of
    /// one `DELETE` per key. A no-op (zero statements) for an empty `keys`
    /// slice. Returns the total number of rows removed.
    ///
    /// `keys` is chunked to [`max_bind_params_for`] minus
    /// [`Self::DELETE_KEYS_RESERVED_PARAMS`] before building the `IN (...)`
    /// list, and one `DELETE` is issued per chunk. Without chunking, the
    /// number of bind parameters here equals the number of key/value pairs in
    /// a single client PATCH request, and `metadata_limits.max_pairs` is an
    /// `Option<u32>` that defaults to `None` ("no limit") -- so a large
    /// enough request body (the gateway allows up to 16 MiB) reaches the
    /// driver's own bind-parameter ceiling (65535 on `PostgreSQL`, 32766 on
    /// `SQLite`) and the statement fails outright rather than degrading
    /// gracefully. Chunking trades that hard failure for a handful of extra
    /// round trips on the (rare) oversized request, while leaving the common
    /// case -- a handful of keys -- at exactly one statement.
    ///
    /// This is the delete half of the metadata patch; the insert half
    /// ([`Self::insert_many`], backed by [`secure_insert_many`]) already
    /// chunks itself internally by row count under the same
    /// [`max_bind_params_for`] budget, for the same reason.
    ///
    /// Splitting one logical delete into several statements is safe here
    /// specifically because [`Store::patch_metadata_atomic`] (the only
    /// caller) always runs this inside its own transaction: all chunks
    /// commit or roll back together, so a caller can't observe a partial
    /// delete. A hypothetical caller outside a transaction would not get
    /// that guarantee and should wrap itself in one before relying on this
    /// method for more than a handful of keys.
    pub async fn delete_keys<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        keys: &[String],
    ) -> Result<u64, DomainError> {
        if keys.is_empty() {
            return Ok(0);
        }
        let chunk_size = max_bind_params_for(conn)
            .saturating_sub(Self::DELETE_KEYS_RESERVED_PARAMS)
            .max(1);
        let mut total_rows_affected: u64 = 0;
        for chunk in keys.chunks(chunk_size) {
            let res = Entity::delete_many()
                .filter(
                    Condition::all()
                        .add(Column::FileId.eq(file_id))
                        .add(Column::Key.is_in(chunk.iter().cloned())),
                )
                .secure()
                .scope_with(scope)
                .exec(conn)
                .await
                .map_err(db_err)?;
            total_rows_affected += res.rows_affected;
        }
        Ok(total_rows_affected)
    }

    /// Batched counterpart of a loop of [`Self::upsert`]'s insert half:
    /// insert every `(key, value)` entry in one multi-row `INSERT`, instead
    /// of one `INSERT` per entry. Callers must have already
    /// removed any pre-existing rows for these keys (e.g. via
    /// [`Self::delete_keys`]) -- this method only inserts, it does not
    /// upsert. A no-op (zero statements) for an empty `entries` slice.
    pub async fn insert_many<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        entries: &[(String, String)],
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        if entries.is_empty() {
            return Ok(());
        }
        let models: Vec<ActiveModel> = entries
            .iter()
            .map(|(key, value)| ActiveModel {
                file_id: Set(file_id),
                key: Set(key.clone()),
                value: Set(value.clone()),
                set_at: Set(now),
            })
            .collect();
        secure_insert_many::<Entity>(models, scope, conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }
}
