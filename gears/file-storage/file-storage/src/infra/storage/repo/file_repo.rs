//! Repository for the `files` table (logical file identity + content pointer).

use sea_orm::ExprTrait;
use sea_orm::sea_query::{Expr, LockType, Query};
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
use crate::infra::storage::entity::file::{ActiveModel, Column, Entity, Model};
use crate::infra::storage::entity::file_version::{
    Column as VersionColumn, Entity as VersionEntity,
};
use crate::infra::storage::mapper::file_from_model;

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
        found.map(file_from_model).transpose()
    }

    /// Lock a `files` row with `SELECT ... FOR UPDATE`, tenant-scoped.
    ///
    /// Call only with a transactional runner (`&SecureTx`), as the very
    /// **first** statement of the transaction -- see
    /// `docs/toolkit_unified_system/11_database_patterns.md`'s "Row locks"
    /// section for the full rationale (parent-before-children ordering,
    /// no external I/O while held). A concurrent `INSERT` into a child
    /// table with a `REFERENCES files (file_id)` foreign key (`file_versions`,
    /// `multipart_uploads`) takes `FOR KEY SHARE` on this row for its FK
    /// check, which conflicts with `FOR UPDATE` -- so once this call
    /// returns, no such insert can be in flight against `file_id` until this
    /// transaction commits or rolls back, and any that raced in earlier is
    /// already visible to a fresh read taken after this call.
    ///
    /// Returns the raw entity model (not the SDK [`File`]) since callers
    /// that need the lock are check-then-act call sites deciding from
    /// `content_id`/other raw columns, not API responses.
    ///
    /// On `SQLite`, `.lock(..)` renders nothing (`sea-query`'s
    /// `prepare_select_lock` is a no-op there) -- correctness on that
    /// backend instead comes from `SQLite`'s single-writer model, which
    /// serializes every write regardless of what any `SELECT` asked for.
    pub async fn lock_for_update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Option<Model>, DomainError> {
        Entity::find()
            .filter(Column::FileId.eq(file_id))
            .lock(LockType::Update)
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_err)
    }

    /// Bind parameters reserved out of the backend's `max_bind_params_for`
    /// budget for everything in `list_by_ids`'s `WHERE` clause besides the
    /// `file_id IN (...)` list itself: whatever `SecureEntityExt::scope_with`
    /// adds for the caller's `AccessScope`. Mirrors
    /// `MetadataRepo::LIST_FOR_FILES_RESERVED_PARAMS`'s reasoning.
    const LIST_BY_IDS_RESERVED_PARAMS: usize = 16;

    /// Batched counterpart of [`Self::get`]: fetch every file in `ids` that
    /// exists (and is visible under `scope`) in a handful of queries instead
    /// of one `get`/`require_file` round trip per id. A `file_id` with no
    /// matching (visible) row simply has no entry in the returned `Vec` --
    /// callers that need to distinguish "absent" from "found" compare
    /// against the id list they passed in.
    ///
    /// `ids` is chunked to `max_bind_params_for` minus
    /// [`Self::LIST_BY_IDS_RESERVED_PARAMS`] before building each `IN (...)`
    /// list, one `SELECT` per chunk -- same reasoning as
    /// `MetadataRepo::list_for_files`'s chunking (t25): an unbounded caller-
    /// supplied id list must not reach the driver's own bind-parameter
    /// ceiling in one statement.
    pub async fn list_by_ids<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<File>, DomainError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let chunk_size = toolkit_db::secure::max_bind_params_for(conn)
            .saturating_sub(Self::LIST_BY_IDS_RESERVED_PARAMS)
            .max(1);
        let mut files = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(chunk_size) {
            let rows = Entity::find()
                .filter(Column::FileId.is_in(chunk.iter().copied()))
                .secure()
                .scope_with(scope)
                .all(conn)
                .await
                .map_err(db_err)?;
            for row in rows {
                files.push(file_from_model(row)?);
            }
        }
        Ok(files)
    }

    /// List files for a mandatory owner filter, newest first, offset-paginated.
    ///
    /// Ordered `(created_at, file_id)` descending, not `created_at` alone:
    /// `created_at` is not unique (several files created in the same
    /// millisecond-resolution instant sort equal on it), and an `OFFSET`
    /// page boundary drawn through a run of equal `created_at` values has no
    /// defined relative order across two separate queries -- a row can be
    /// skipped or repeated across pages. `file_id` is the primary key, so
    /// adding it as a tie-breaker makes the order -- and therefore the page
    /// boundary -- fully deterministic.
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
            .order_by_desc(Column::FileId)
            .limit(limit)
            .offset(offset)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        rows.into_iter().map(file_from_model).collect()
    }

    /// Optimistic compare-and-swap of the content pointer (the bind operation).
    ///
    /// Sets `content_id := new_content` only if the current `content_id` equals
    /// `expected` (or both are NULL for the first bind). Returns `true` on a
    /// successful swap, `false` on an `If-Match` conflict.
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
    /// Bump `meta_version` (and `last_modified_at`) under an optional
    /// optimistic-concurrency guard. When `expected_meta_version` is `Some(v)`
    /// the bump only lands if the current revision is exactly `v`; when it is
    /// `None` the bump is unconditional.
    ///
    /// Returns the **committed** post-bump `meta_version` (`Some`), or `None`
    /// if the guard matched no row (`If-Match-Metadata` conflict). The value is
    /// read back from the row in the same transaction rather than derived as
    /// `expected + 1`, because for an unconditional bump the pre-state is not
    /// known to the caller and a concurrent bump can make `snapshot + 1` wrong.
    pub async fn touch_meta<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
        expected_meta_version: Option<i64>,
        now: OffsetDateTime,
    ) -> Result<Option<i64>, DomainError> {
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
        if res.rows_affected == 0 {
            return Ok(None);
        }

        let row = Entity::find()
            .filter(Column::FileId.eq(file_id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_err)?
            .ok_or_else(|| DomainError::database("file row missing after meta_version bump"))?;
        Ok(Some(row.meta_version))
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

    /// Delete a file row iff it is still a true orphan -- `content_id IS
    /// NULL` **and** it has zero `file_versions` rows -- evaluated as a
    /// single conditional `DELETE`, not as separate pre-checks.
    ///
    /// # Why this exists instead of `SELECT`-then-`DELETE`
    ///
    /// A `SELECT`-then-`DELETE` re-verification of the orphan condition, as
    /// two plain `SELECT`s followed by an unconditional `Self::delete`, does
    /// not hold under `READ COMMITTED`: each plain `SELECT` is its own
    /// statement with its own snapshot, ordinary reads take no locks, and
    /// `crate::infra::storage::store::Store::insert_pending_version` runs
    /// autocommit on a separate connection. Nothing prevents a version from
    /// being inserted and committed in the gap between the two `SELECT`s, or
    /// between the second `SELECT` and the `DELETE` -- and once that DELETE
    /// runs, the FK (`file_versions.file_id -> files.file_id ON DELETE
    /// CASCADE`) silently removes the just-inserted version along with the
    /// file. This is invisible on `SQLite`, whose single-writer model
    /// serializes the interleaving away.
    ///
    /// Folding the whole guard into the `DELETE`'s own `WHERE` (`content_id
    /// IS NULL` plus a `NOT EXISTS` subquery over `file_versions`) narrows
    /// that window from "between two statements" to "inside one statement's
    /// execution", and removes the `content_id` half of the race outright.
    ///
    /// On its own, evaluated in isolation, this single conditional `DELETE`
    /// does **not** make the version half airtight on `PostgreSQL`. Under
    /// `READ COMMITTED` the `NOT EXISTS` subquery is evaluated against the
    /// snapshot taken when this statement began. A concurrent
    /// `insert_pending_version` that commits after that snapshot is
    /// invisible to the subquery; the FK it takes on the parent row (`FOR
    /// KEY SHARE`) does make this `DELETE` wait for it, but once the
    /// inserter commits the parent tuple is only *locked*, not updated, so
    /// `PostgreSQL` resumes without an `EvalPlanQual` re-check and the stale
    /// `NOT EXISTS` verdict stands. The `ON DELETE CASCADE` would then
    /// remove the freshly inserted version.
    ///
    /// This is **not** a gap left open in production: every caller of this
    /// method (`Store::delete_orphan_file_with_event`) takes a `SELECT ...
    /// FOR UPDATE` lock on the same `files` row first (`Self::
    /// lock_for_update`, contradicting an older version of this comment that
    /// claimed the secure ORM exposed no row-lock API -- it does, see
    /// `docs/toolkit_unified_system/11_database_patterns.md`'s "Row locks"
    /// section) and re-verifies "no versions"/"no active multipart session"
    /// fresh, inside that same lock, before ever calling this method. A
    /// racing `insert_pending_version` therefore either commits before that
    /// lock (and is then visible to those fresh re-checks, correctly
    /// aborting the reclaim before it ever reaches this `DELETE`) or blocks
    /// until the reclaiming transaction ends. This method's own `NOT EXISTS`
    /// guard is kept anyway as a second, redundant line of defense -- a
    /// future caller that reaches this method without holding that lock
    /// first would still be exposed to the single-statement snapshot race
    /// described above, which is why this doc comment still spells it out.
    ///
    /// Returns the number of rows removed (0 or 1, keyed on `file_id`). `0`
    /// means the file is already gone, has content bound, or has at least
    /// one version row -- the caller cannot and does not need to distinguish
    /// these from the count alone (mirrors
    /// [`crate::infra::storage::repo::VersionRepo::delete`]'s "returns a
    /// count, not a reason" shape).
    pub async fn delete_if_orphan<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<u64, DomainError> {
        // `EXISTS (SELECT 1 FROM file_versions WHERE file_id = ?)`, negated
        // below -- same subquery shape as `VersionRepo::list_pending_older_than`'s
        // `not_in_subquery` and `toolkit_db`'s own `scope_via_exists` helper.
        let mut has_any_version = Query::select();
        has_any_version
            .expr(Expr::value(1))
            .from(VersionEntity)
            .and_where(VersionColumn::FileId.eq(file_id));
        let no_versions_exist = Condition::all().add(Expr::exists(has_any_version)).not();

        let res = Entity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::FileId.eq(file_id))
                    .add(Column::ContentId.is_null())
                    .add(no_versions_exist),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected)
    }

    /// List `files` rows that never received **any** version at all --
    /// `content_id IS NULL` **and** zero `file_versions` rows -- created
    /// before `created_before`, ordered by `(created_at, file_id)`
    /// ascending, up to `limit` rows. Feeds the cleanup sweep's dedicated
    /// versionless-orphan-file phase
    /// ([`crate::domain::cleanup::CleanupEngine::sweep_versionless_files`]).
    ///
    /// Same correlated-subquery shape as [`Self::delete_if_orphan`]'s
    /// `NOT EXISTS` guard, generalized from a single literal `file_id` to a
    /// column correlated against this query's own `files.file_id` (via
    /// `Expr::col((Entity, Column::FileId))`), since this scans every
    /// tenant's `files` table rather than re-verifying one already-known row.
    ///
    /// This is a plain, uncommitted `SELECT` -- not a guard inside a
    /// `DELETE` -- so it only *finds candidates*. The actual delete for each
    /// one still goes through `delete_if_orphan`'s transactionally-guarded
    /// statement (via `Store::delete_orphan_file_with_event`), which
    /// re-verifies the same zero-versions/`NULL`-`content_id` condition
    /// fresh inside its own transaction; a version or content bound in the
    /// gap between this list and that delete therefore cannot cause data
    /// loss, only a safely-declined delete attempt (same reasoning as
    /// `CleanupEngine::orphan_candidate_file`'s doc comment).
    pub async fn list_versionless_orphan_files<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        created_before: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        let mut has_any_version = Query::select();
        has_any_version
            .expr(Expr::value(1))
            .from(VersionEntity)
            .and_where(Expr::col(VersionColumn::FileId).equals((Entity, Column::FileId)));
        let no_versions_exist = Condition::all().add(Expr::exists(has_any_version)).not();

        let rows = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::ContentId.is_null())
                    .add(Column::CreatedAt.lt(created_before))
                    .add(no_versions_exist),
            )
            .order_by_asc(Column::CreatedAt)
            .order_by_asc(Column::FileId)
            .limit(limit)
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        rows.into_iter().map(file_from_model).collect()
    }

    /// List files across all tenants for the retention sweep engine,
    /// **keyset-paginated by `file_id`** to bound sweep memory on large
    /// deployments. Returns up to `limit` files ordered by `file_id`, starting
    /// strictly after `after` (`None` = from the beginning); the caller loops,
    /// advancing `after` to the last returned `file_id`, until it gets a short
    /// page. Keyset (not offset) paging is used so that deleting expired files
    /// mid-sweep does not shift the window and skip rows.
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
        rows.into_iter().map(file_from_model).collect()
    }

    /// Update `owner_kind` and `owner_id` for a file row, and bump
    /// `last_modified_at`. Returns `true` if a row was found and updated.
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
