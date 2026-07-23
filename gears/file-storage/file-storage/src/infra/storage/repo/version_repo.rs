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
use crate::infra::storage::db::db_err;
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
            .map_err(db_err)?;
        Ok(())
    }

    /// Fetch a single version by `(file_id, version_id)`.
    ///
    /// P2 2.2: this used to delegate to [`Self::list_by_file`] and `.find()`
    /// the target in Rust, with a comment claiming a direct two-column
    /// predicate "proved unreliable across the secure layer". Re-investigated
    /// for this change: `mark_available`/`finalize`/`clear_current`/
    /// `set_current`/`delete`/`delete_if_status`/`rebind_backend` below all
    /// use this exact `Condition::all()` two-`.add()` shape successfully on
    /// `update_many()`/`delete_many()`, and `SecureSelect::filter()` (see
    /// `toolkit_db::secure::select`) supports the same composition on
    /// `find()`. A direct-predicate `.one()` query was verified against
    /// `version_repo_get_returns_correct_row_among_many` (versions seeded
    /// across two files, sharing a UUID prefix pattern) with no cross-file
    /// bleed, so the scan-and-filter workaround was not a real limitation —
    /// the original comment's claim does not reproduce. Kept as a direct
    /// query, closing the per-file amplification-DoS surface on the
    /// `get`/`finalize`/`bind`/`download_url` hot path.
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
            .map_err(db_err)?;
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
                    .add(Column::VersionId.eq(version_id))
                    .add(Column::Status.eq(VersionStatus::Pending.as_str())),
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
    ///
    /// `hash_mode`/`part_count` (ADR-0006) are set here, at **finalize** time,
    /// not at pending-insert time — a pending row is created before it is
    /// known whether the upload will complete single-part (`whole-sha256`,
    /// `part_count = None`) or multipart (`multipart-composite-sha256`,
    /// `part_count = Some(n)`). `hash_algorithm` is never touched (it is
    /// always `'SHA-256'` for both modes).
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
        // Scope the update to the full `(file_id, version_id)` key so a
        // version_id that belongs to a different file cannot be finalized here.
        let mut update = Entity::update_many()
            .col_expr(Column::Size, Expr::value(size))
            .col_expr(Column::HashValue, Expr::value(hash_value))
            .col_expr(Column::HashMode, Expr::value(hash_mode))
            .col_expr(Column::PartCount, Expr::value(part_count))
            .col_expr(
                Column::Status,
                Expr::value(file_storage_sdk::VersionStatus::Available.as_str()),
            );
        // `mime_type` is only rewritten when the caller has a validated/sniffed
        // type to persist (single-part finalize); the multipart-complete path
        // passes `None` and leaves the declared type untouched.
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
            .map_err(db_err)?;
        Ok(res.rows_affected == 1)
    }

    /// Insert the `version_hash_manifest` row for a `multipart-composite-sha256`
    /// version (ADR-0006 §4/§5). Called in the **same transaction** as
    /// [`Self::finalize`] so the manifest and the version row's
    /// `(hash_mode, part_count, hash_value = root)` are committed atomically.
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
            .map_err(db_err)?;
        Ok(())
    }

    /// Fetch the `version_hash_manifest` row's manifest text for a version, if
    /// one exists (`multipart-composite-sha256` versions only). Used by
    /// `migrate_backend` and any mode-aware re-verification path.
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
            .map_err(db_err)?;
        Ok(found.map(|m| m.manifest))
    }

    /// Batched counterpart of [`Self::get_manifest`]: fetch the manifest text
    /// for many versions in a single `IN (...)` query, keyed by `version_id`.
    /// Used by `GET /files/{id}/versions` so that rendering a page of `N`
    /// versions' manifests costs one query instead of `N` (mirrors
    /// `MetadataRepo::list_for_files`'s N+1 avoidance). A version with no
    /// manifest row (`whole-sha256`) simply has no entry in the returned map.
    pub async fn get_manifests<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        version_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>, DomainError> {
        if version_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows = ManifestEntity::find()
            .filter(ManifestColumn::VersionId.is_in(version_ids.iter().copied()))
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;
        Ok(rows.into_iter().map(|m| (m.version_id, m.manifest)).collect())
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

    /// Delete a single version. Returns the number of rows removed (0 or 1
    /// for this `(file_id, version_id)`-keyed predicate).
    ///
    /// P2 2.7: the predicate is guarded with `is_current = false` so a delete
    /// can never remove the version a file's `content_id` currently points
    /// at, even if the caller's own "is this current?" check ran against a
    /// stale snapshot (a concurrent `bind` promoted this exact version to
    /// current in between). The guard is evaluated by the DB in the same
    /// statement as the delete, so there is no window between "check" and
    /// "delete" for a race to land in. Returning the raw row count (rather
    /// than a bool) lets [`crate::infra::storage::store::Store::delete_version`]
    /// tell "deleted" apart from "not found / guarded because current" without
    /// re-deriving that distinction from a second predicate.
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
            .map_err(db_err)?;
        Ok(res.rows_affected)
    }

    /// Delete a single version row iff its current `status` matches `expected`.
    /// Returns `true` if a row was removed, `false` if the row is missing or
    /// its status no longer matches (a concurrent writer already moved it on).
    ///
    /// Status-guarded delete CAS -- same `Condition::all()` pattern as
    /// [`Self::finalize`]'s pending-only guard (P2 0.4). Used by the cleanup
    /// sweep (P2 0.3 step 5) so a pending version that a racing
    /// `complete_multipart_upload` has already flipped to `available` can
    /// never be deleted out from under it.
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
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// List all `pending` version rows whose `created_at` is older than
    /// `older_than`, **excluding** any version that is still the backing
    /// version of a live `in_progress` multipart session (`expires_at >
    /// now`). Used by the orphan-reconciliation sweep.
    ///
    /// A long-running multipart upload (big file, generous URL TTL) keeps its
    /// backing version `pending` for the whole session, which can outlive
    /// `orphan_grace_secs`; without this guard the sweep would delete the
    /// version out from under the in-progress upload. A session whose
    /// `expires_at` has *already* passed is deliberately NOT excluded here --
    /// it is aborted by the next sweep step (`sweep_expired_multipart`), and
    /// its version becomes reclaimable on a later sweep once the session row
    /// itself transitions out of `in_progress`.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-dod:cpt-cf-file-storage-dod-cleanup-live-multipart-guard:p1
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
            .map_err(db_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Transactionally update `backend_id` and `backend_path` for a version row,
    /// CAS-gated on the version's *current* `backend_id`/`backend_path`.
    /// Used by backend migration.
    ///
    /// `0` rows affected now means either "version gone" (the row's
    /// `(file_id, version_id)` no longer exists — today's meaning) **or**
    /// "the backend pointer changed concurrently" (a different migration won
    /// the race and already moved the row past `expected_backend_id`/
    /// `expected_backend_path`) — the caller must re-fetch to distinguish
    /// these.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
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
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }
}
