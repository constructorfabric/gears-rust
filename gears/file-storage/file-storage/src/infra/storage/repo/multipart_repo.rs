//! Repository for `multipart_uploads` and `multipart_upload_parts`.
//!
//! No tenant isolation at the entity level (no `tenant_id` column) —
//! all queries use `AccessScope::allow_all()`. The tenant boundary is
//! enforced through the parent `files` row before a session is created.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict, SecureUpdateExt,
    secure_insert,
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
        auto_bind: bool,
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
            auto_bind: Set(auto_bind),
            lease_until: Set(None),
            lease_owner: Set(None),
            complete_result: Set(None),
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
    /// -- used by the `in_progress` → `completed` transition to flip
    /// `mime_validated` to `true` alongside the state
    /// change, since `complete_multipart_upload` only reaches this call after
    /// the assembled object's content has already been sniffed and validated
    /// against the declared MIME type. The `in_progress` → `aborted`
    /// transition passes `None` — an aborted upload's content was never
    /// validated.
    ///
    /// Also doubles as a row-locking primitive: [`Self::upsert_part`] calls
    /// this with `expected_state == new_state == "in_progress"` purely to
    /// take Postgres's implicit row lock on a matching `UPDATE` -- see that
    /// method's doc comment for why a real (if value-preserving) `UPDATE` is
    /// required there instead of a plain `SELECT`.
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

    /// Acquire (or take over) the completion lease: one conditional UPDATE
    /// moving the session to `completing` from either
    /// `in_progress` (fresh acquire) or an **expired** `completing`
    /// (takeover after a crashed completer). Never blocks and never holds a
    /// transaction across I/O; `false` = someone else holds a live lease, the
    /// session is terminal, or the session itself has expired (see below).
    ///
    /// Fenced by `expires_at > now`: without this, the CAS filter only looks
    /// at `state`/`lease_until`, so a session
    /// whose `expires_at` has already passed but which the abandoned-session
    /// sweep has not yet reaped can still win a fresh lease here. The
    /// call site's own `session.expires_at <= now` guard
    /// (`MultipartService::complete_multipart_upload`) is checked against an
    /// **earlier-loaded snapshot** and is therefore only a fast, non-authoritative
    /// rejection -- it cannot see a session that expired in the gap between
    /// that snapshot read and this CAS. Folding the same condition into the
    /// CAS's `WHERE` clause makes the database row itself, not a stale
    /// in-memory copy, the source of truth: the lease can never be acquired
    /// for a session that is expired *at the instant the UPDATE runs*.
    pub async fn acquire_complete_lease<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = UploadEntity::update_many()
            .col_expr(UploadColumn::State, Expr::value("completing"))
            .col_expr(UploadColumn::LeaseUntil, Expr::value(lease_until))
            .col_expr(UploadColumn::LeaseOwner, Expr::value(owner))
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::UploadId.eq(upload_id))
                    .add(UploadColumn::ExpiresAt.gt(now))
                    .add(
                        sea_orm::Condition::any()
                            .add(UploadColumn::State.eq("in_progress"))
                            .add(
                                sea_orm::Condition::all()
                                    .add(UploadColumn::State.eq("completing"))
                                    .add(UploadColumn::LeaseUntil.lt(now)),
                            ),
                    ),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// Release a held completion lease back to `in_progress` (assembly
    /// failed with a real error — the next `complete` retries immediately
    /// instead of waiting out `lease_until`). Scoped to `owner` so a
    /// takeover's lease is never clobbered by the crashed original.
    pub async fn release_complete_lease<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        owner: &str,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = UploadEntity::update_many()
            .col_expr(UploadColumn::State, Expr::value("in_progress"))
            .col_expr(
                UploadColumn::LeaseUntil,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .col_expr(
                UploadColumn::LeaseOwner,
                Expr::value(Option::<String>::None),
            )
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::UploadId.eq(upload_id))
                    .add(UploadColumn::State.eq("completing"))
                    .add(UploadColumn::LeaseOwner.eq(owner)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// Abort a `completing` session whose lease has EXPIRED (completer died
    /// mid-assembly): CAS `state = 'completing' AND lease_until < now` →
    /// `aborted`. A live lease never matches, so an in-flight completer is
    /// never aborted out from under itself.
    pub async fn abort_expired_completing<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = UploadEntity::update_many()
            .col_expr(UploadColumn::State, Expr::value("aborted"))
            .col_expr(
                UploadColumn::LeaseUntil,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .col_expr(
                UploadColumn::LeaseOwner,
                Expr::value(Option::<String>::None),
            )
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::UploadId.eq(upload_id))
                    .add(UploadColumn::State.eq("completing"))
                    .add(UploadColumn::LeaseUntil.lt(now)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }

    /// Terminal transition `completing → completed`, persisting the response
    /// snapshot (`complete_result` JSON) and clearing the lease.
    pub async fn finish_complete<C: DBRunner>(
        &self,
        conn: &C,
        upload_id: Uuid,
        result_json: &str,
    ) -> Result<bool, DomainError> {
        use sea_orm::sea_query::Expr;
        let res = UploadEntity::update_many()
            .col_expr(UploadColumn::State, Expr::value("completed"))
            .col_expr(UploadColumn::MimeValidated, Expr::value(true))
            .col_expr(UploadColumn::CompleteResult, Expr::value(result_json))
            .col_expr(
                UploadColumn::LeaseUntil,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .col_expr(
                UploadColumn::LeaseOwner,
                Expr::value(Option::<String>::None),
            )
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::UploadId.eq(upload_id))
                    .add(UploadColumn::State.eq("completing")),
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
    /// concurrency: `sweep_after_complete_wins_does_not_delete_bound_version`
    /// in `cleanup_test.rs` backdates a session's `expires_at` *after* a
    /// successful `complete_multipart_upload`, which a defense-in-depth check
    /// would otherwise reject if the session were built with a past
    /// `expires_at` from the start.
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

    /// Insert or update one multipart-upload part row, guarded by a
    /// same-transaction check that the parent session is still
    /// `in_progress`. Must be called with `conn` bound to the SAME
    /// transaction as the caller's other work -- see
    /// [`crate::infra::storage::store::Store::upsert_multipart_part`], the
    /// only caller, for the two concrete corruption modes this closes
    /// (a part accepted after `complete_multipart_upload` already snapshotted
    /// the part list, and a part inserted after `abort_multipart_upload`'s
    /// `delete_parts_for_upload` already ran -- becoming a permanent orphan,
    /// since a `multipart_uploads` row is never deleted, only transitioned).
    ///
    /// # Why the guard is a dummy self-CAS, not a plain re-read
    ///
    /// The guard performs an `in_progress -> in_progress` self-transition
    /// through [`Self::update_state`] rather than a plain `SELECT`. Postgres
    /// takes a row-level lock on every row an `UPDATE` matches for the
    /// duration of the transaction, even when the written value equals the
    /// existing one -- so this single call both (a) checks, right now, that
    /// the session is still `in_progress`, and (b) forces any concurrent CAS
    /// against the SAME session row (`abort_multipart_upload`'s
    /// `in_progress -> aborted`, `acquire_complete_lease`'s
    /// `in_progress -> completing`) to block until this transaction commits
    /// or rolls back, then re-evaluate its own `WHERE state = 'in_progress'`
    /// against the now-current row.
    ///
    /// A plain unlocked `SELECT` re-read would NOT close the race this
    /// exists to fix, even inside a transaction: under the default `READ
    /// COMMITTED` isolation, two transactions can each read `in_progress`
    /// before either commits, and then commit in either order -- so the part
    /// row this method inserts could still land in the table AFTER a
    /// concurrent abort's `delete_parts_for_upload` already ran and
    /// committed, reintroducing the exact orphaned-row bug this change
    /// exists to close. Only serializing on the shared row via a real
    /// (dummy) `UPDATE` prevents that interleaving.
    ///
    /// # Returns
    ///
    /// `true` if the part row was written. `false` if the guard lost --
    /// the session is not currently `in_progress` (including "does not
    /// exist", though in practice a session row is never deleted, only
    /// transitioned, so this only ever means "not `in_progress`"); the part
    /// row is then left untouched.
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
    ) -> Result<bool, DomainError> {
        let locked = self
            .update_state(conn, upload_id, "in_progress", "in_progress", None)
            .await?;
        if !locked {
            return Ok(false);
        }

        // Single-statement `INSERT ... ON CONFLICT (upload_id, part_number)
        // DO UPDATE`, replacing the previous DELETE-then-INSERT pair. Those
        // two statements were not transactional with each other (this method
        // used to run on a bare connection -- see the caller's doc comment),
        // so a `complete_multipart_upload` racing between them could
        // snapshot the part list in the instant the row was absent. A single
        // upsert statement has no such instant: the row is either the old
        // version or the new one, never neither. `SecureOnConflict` is used
        // (rather than raw `sea_orm::sea_query::OnConflict`) purely for its
        // tenant-immutability check -- moot here since this entity has no
        // tenant column (`#[secure(no_tenant, ...)]` on the entity), but it
        // is the project's standard on-conflict entry point regardless.
        let on_conflict =
            SecureOnConflict::<PartEntity>::columns([PartColumn::UploadId, PartColumn::PartNumber])
                .update_columns([
                    PartColumn::BackendEtag,
                    PartColumn::PartHash,
                    PartColumn::Size,
                    PartColumn::UploadedAt,
                ])
                .map_err(db_err)?;

        let am = PartActiveModel {
            upload_id: Set(upload_id),
            part_number: Set(part_number),
            backend_etag: Set(backend_etag.to_owned()),
            part_hash: Set(part_hash),
            size: Set(size),
            uploaded_at: Set(now),
        };
        PartEntity::insert(am)
            .secure()
            .scope_unchecked(&AccessScope::allow_all())
            .map_err(db_err)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(true)
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
    pub async fn list_expired<C: DBRunner>(
        &self,
        conn: &C,
        now: OffsetDateTime,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        let rows = UploadEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::ExpiresAt.lt(now))
                    .add(
                        sea_orm::Condition::any()
                            .add(UploadColumn::State.eq("in_progress"))
                            // A `completing` session whose completer died
                            // AND whose session lifetime
                            // has passed is also abandoned — but only once
                            // its lease has expired too, so a live completer
                            // racing `expires_at` is never reaped mid-flight.
                            .add(
                                sea_orm::Condition::all()
                                    .add(UploadColumn::State.eq("completing"))
                                    .add(UploadColumn::LeaseUntil.lt(now)),
                            ),
                    ),
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
    /// Used by the orphan-file-reconciliation guard: a file's pending
    /// version can look "abandoned" to [`Self::list_expired`]'s sibling sweep
    /// step (`sweep_abandoned_pending`, keyed only on the version's age) even
    /// while it is the live target of a *not-yet-expired* multipart session --
    /// deleting the parent `files` row in that window would `ON DELETE
    /// CASCADE` the still-`in_progress` session out from under the upload.
    ///
    /// Existence via `LIMIT 1` + `one()` rather than `COUNT(*)`: this project
    /// forbids `COUNT` queries for existence checks (a full/partial scan just
    /// to throw the number away) -- `LIMIT 1` lets the planner stop at the
    /// first matching row instead of counting every `in_progress` session for
    /// the file.
    pub async fn has_in_progress_for_file<C: DBRunner>(
        &self,
        conn: &C,
        file_id: Uuid,
    ) -> Result<bool, DomainError> {
        let row = UploadEntity::find()
            .filter(
                sea_orm::Condition::all()
                    .add(UploadColumn::FileId.eq(file_id))
                    .add(UploadColumn::State.eq("in_progress")),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .limit(1)
            .one(conn)
            .await
            .map_err(db_err)?;
        Ok(row.is_some())
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
        auto_bind: m.auto_bind,
        lease_until: m.lease_until,
        complete_result: m.complete_result,
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
