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
        declared_size: u64,
        part_size: u64,
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
    ///
    /// `mime_validated`, when `Some`, is set in the **same** UPDATE statement
    /// (P2 remediation item 1.10) — used by the `in_progress` → `completed`
    /// transition to flip `mime_validated` to `true` alongside the state
    /// change, since `complete_multipart_upload` only reaches this call after
    /// the assembled object's content has already been sniffed and validated
    /// against the declared MIME type. The `in_progress` → `aborted`
    /// transition passes `None` — an aborted upload's content was never
    /// validated.
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
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// Force-set a session's `expires_at`, unconditionally.
    ///
    /// **Test-support only; do not call in production.** Production code
    /// never mutates `expires_at` after a session is created — calling this
    /// bypasses that invariant. This exists so unit tests can
    /// deterministically simulate "time passing" on an already-created
    /// (possibly already-completed) session without a real sleep or
    /// concurrency, per the unit-testing doctrine (P2 0.3 --
    /// `sweep_after_complete_wins_does_not_delete_bound_version` in
    /// `cleanup_test.rs` backdates a session's `expires_at` *after* a
    /// successful `complete_multipart_upload`, which the P2 0.3 step-3
    /// defense-in-depth check would otherwise reject if the session were
    /// built with a past `expires_at` from the start).
    ///
    /// `#[doc(hidden)]` rather than a `test-support` Cargo feature: this
    /// method is called from the external integration-test crate
    /// `tests/cleanup_test.rs`, so `#[cfg(test)]` alone would not reach it,
    /// and gating it behind a non-default feature would make the standard
    /// `cargo test -p cf-gears-file-storage` command fail to compile that
    /// test (or silently skip it via `required-features`) unless every
    /// caller — including CI — also passed `--features test-support`.
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
            .map_err(db_err)?;
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

    /// Delete all `multipart_upload_parts` rows for `upload_id`. Returns the
    /// number of rows removed.
    ///
    /// Part rows are otherwise unbounded growth: the session row itself is
    /// never deleted (only its `state` column flips to `aborted`/`completed`),
    /// and there is no DB-level cascade from a state flip to its part rows —
    /// see [`crate::infra::storage::entity::multipart_upload_part`], which
    /// defines no `Relation`. Called from the abort flow (both the
    /// user-driven `DELETE .../multipart/{upload_id}` and the
    /// orphan-reconciliation sweep's expired-session cleanup) per
    /// `docs/features/multipart-coordinator.md`'s abort `DoD`
    /// (`inst-abort-delete-parts`). Deliberately **not** called from
    /// `complete_multipart_upload` -- the docs do not list part-row deletion
    /// as part of `complete`'s contract.
    pub async fn delete_parts_for_upload<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
    ) -> Result<u64, DomainError> {
        let res = PartEntity::delete_many()
            .filter(PartColumn::UploadId.eq(upload_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected)
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

    /// Whether `file_id` has at least one `in_progress` multipart upload
    /// session, regardless of its `expires_at`.
    ///
    /// Used by the P2 2.8 orphan-file-reconciliation guard: a file's pending
    /// version can look "abandoned" to [`Self::list_expired`]'s sibling sweep
    /// step (`sweep_abandoned_pending`, keyed only on the version's age) even
    /// while it is the live target of a *not-yet-expired* multipart session --
    /// deleting the parent `files` row in that window would `ON DELETE
    /// CASCADE` the still-`in_progress` session out from under the upload.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    pub async fn has_in_progress_for_file<C: DBRunner>(
        &self,
        conn: &C,
        file_id: Uuid,
    ) -> Result<bool, DomainError> {
        let count = UploadEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::FileId.eq(file_id))
                    .add(UploadColumn::State.eq("in_progress")),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .count(conn)
            .await
            .map_err(db_err)?;
        Ok(count > 0)
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
