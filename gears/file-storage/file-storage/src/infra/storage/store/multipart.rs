//! Multipart upload session intent methods.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::audit::AuditEntry;
use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession};
use crate::infra::storage::db::db_err;
use crate::infra::storage::store::Store;

impl Store {
    // ── multipart uploads ─────────────────────────────────────────────────────

    /// Create a multipart upload session row.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_multipart_upload(
        &self,
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
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .create(
                &conn,
                upload_id,
                file_id,
                version_id,
                backend_upload_handle,
                declared_mime,
                declared_size,
                part_size,
                auto_bind,
                expires_at,
                now,
            )
            .await
    }

    /// Fetch a multipart upload session by `upload_id`.
    pub async fn get_multipart_upload(
        &self,
        upload_id: Uuid,
    ) -> Result<Option<MultipartUploadSession>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.multipart.get(&conn, upload_id).await
    }

    /// Insert or replace a multipart upload part, guarded and written in a
    /// single transaction: the guard (re-checking `state == 'in_progress'`,
    /// taking a row lock that serializes against any concurrent
    /// session-row CAS) and the write happen inside one transaction -- see
    /// `MultipartRepo::upsert_part` for exactly how.
    ///
    /// Without that, a bare (non-transactional) state check followed by a
    /// separate, unguarded DELETE-then-INSERT leaves two independent race
    /// windows open:
    ///
    /// 1. `complete_multipart_upload` snapshotting `list_multipart_parts` in
    ///    the instant between the DELETE and INSERT would see the part as
    ///    missing and fail the upload with a spurious "parts missing" `409`,
    ///    even though the part had in fact been (re-)reported.
    /// 2. A `report_part` that races a concurrent `abort_multipart_upload`
    ///    (whose CAS + `delete_parts_for_upload` run together in one
    ///    transaction) could have its own state check pass, then have its
    ///    unguarded INSERT land AFTER the abort's `delete_parts_for_upload`
    ///    already ran -- leaving a permanently orphaned part row, since a
    ///    `multipart_uploads` row is never deleted, only transitioned (so
    ///    nothing ever revisits it to clean the row up).
    ///
    /// # Error contract
    ///
    /// When the guard finds the session no longer `in_progress`, this
    /// returns `Err(DomainError::multipart_upload_not_in_progress(..))`
    /// directly -- the same error variant `MultipartService::report_part`'s
    /// own (fast-path) state check returns for that case, so callers that
    /// propagate this method's `Err` via `?` get the correct `409` either
    /// way.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_multipart_part(
        &self,
        upload_id: Uuid,
        part_number: i32,
        backend_etag: &str,
        part_hash: Vec<u8>,
        size: i64,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let multipart = self.repos.multipart.clone();
        let backend_etag = backend_etag.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let written = multipart
                        .upsert_part(
                            tx,
                            upload_id,
                            part_number,
                            &backend_etag,
                            part_hash,
                            size,
                            now,
                        )
                        .await?;
                    if written {
                        return Ok(());
                    }
                    // Guard lost: the session is no longer `in_progress`.
                    // Fetch its current state, best-effort, purely to put an
                    // accurate value on the error -- a lookup failure here
                    // must not hide the fact that the part was NOT written,
                    // so it falls back to a placeholder rather than
                    // propagating and masking the real (guard) failure.
                    let state = multipart
                        .get(tx, upload_id)
                        .await
                        .ok()
                        .flatten()
                        .map_or("gone", |session| session.state.as_str());
                    Err(DomainError::multipart_upload_not_in_progress(
                        upload_id, state,
                    ))
                })
            })
            .await
    }

    /// Whether `file_id` currently has at least one `in_progress` multipart
    /// upload session (regardless of `expires_at`).
    ///
    /// Orphan-file-reconciliation guard -- see
    /// `MultipartRepo::has_in_progress_for_file`.
    pub async fn has_in_progress_multipart_for_file(
        &self,
        file_id: Uuid,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .has_in_progress_for_file(&conn, file_id)
            .await
    }

    /// Force-set a session's `expires_at`. **Test-support only; do not call
    /// in production** -- see `MultipartRepo::set_expires_at` for why this
    /// exists and why it is `#[doc(hidden)]` rather than gated behind a
    /// Cargo feature (it is called from the external integration-test crate
    /// `tests/cleanup_test.rs`).
    #[doc(hidden)]
    pub async fn set_multipart_expires_at_for_test(
        &self,
        upload_id: Uuid,
        expires_at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .set_expires_at(&conn, upload_id, expires_at)
            .await
    }

    /// List all parts for a multipart upload.
    pub async fn list_multipart_parts(
        &self,
        upload_id: Uuid,
    ) -> Result<Vec<MultipartPart>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.multipart.list_parts(&conn, upload_id).await
    }

    /// Mark a multipart upload session as `completed` and record the audit row
    /// in the same transaction.
    ///
    /// Also flips `mime_validated` to `true` in the same UPDATE: by the time
    /// `MultipartService::complete_multipart_upload` calls this, it has
    /// already sniffed the assembled object's leading
    /// bytes and validated them against `session.declared_mime` (bailing out
    /// with `DomainError::mime_mismatch` before ever reaching this call on a
    /// mismatch) — so reaching this point means the content is validated.
    pub async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        result_json: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let multipart = self.repos.multipart.clone();
        let audit_repo = self.repos.audit.clone();
        let result_json = result_json.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    // The terminal transition comes from `completing` (the
                    // completion lease), persisting the response snapshot
                    // for idempotent re-completes.
                    let updated = multipart
                        .finish_complete(tx, upload_id, &result_json)
                        .await?;
                    if updated {
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    /// Acquire (or take over an expired) completion lease — see
    /// `MultipartRepo::acquire_complete_lease`.
    pub async fn acquire_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .acquire_complete_lease(&conn, upload_id, owner, lease_until, now)
            .await
    }

    /// Release a held completion lease after a failed assembly — see
    /// `MultipartRepo::release_complete_lease`.
    pub async fn release_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .release_complete_lease(&conn, upload_id, owner)
            .await
    }

    /// Mark a multipart upload session as `aborted`, delete its
    /// `multipart_upload_parts` rows, and record the audit row — all in the
    /// same transaction.
    ///
    /// Part-row deletion (`docs/features/multipart-coordinator.md`'s
    /// `inst-abort-delete-parts`) lives here rather than at each call site so
    /// both abort paths that share this single CAS -- the user-driven
    /// `MultipartService::abort_multipart_upload` and the cleanup sweep's
    /// `CleanupEngine::abort_expired_multipart_session` -- get it for free.
    /// Folded into the same transaction as the state flip so a crash between
    /// the two can never leave the session `aborted` with its part rows still
    /// dangling.
    pub async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let multipart = self.repos.multipart.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let mut updated = multipart
                        .update_state(tx, upload_id, "in_progress", "aborted", None)
                        .await?;
                    if !updated {
                        // A `completing` session whose lease has expired
                        // (completer died mid-assembly) is
                        // also abortable — by the cleanup sweep or an
                        // explicit client abort. A LIVE lease is never
                        // aborted out from under its completer (the CAS
                        // below requires `lease_until < now`).
                        updated = multipart
                            .abort_expired_completing(tx, upload_id, OffsetDateTime::now_utc())
                            .await?;
                    }
                    if updated {
                        multipart.delete_parts_for_upload(tx, upload_id).await?;
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    /// Delete all `multipart_upload_parts` rows for `upload_id`. Returns the
    /// number of rows removed.
    ///
    /// Exposed as a standalone `Store` method (in addition to being folded
    /// into [`Self::abort_multipart_upload`]'s own transaction) so tests and
    /// any future caller outside the abort CAS can assert on / drive part-row
    /// cleanup directly.
    pub async fn delete_parts_for_upload(&self, upload_id: Uuid) -> Result<u64, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .delete_parts_for_upload(&conn, upload_id)
            .await
    }
}
