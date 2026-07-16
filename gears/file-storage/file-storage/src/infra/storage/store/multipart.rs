//! Multipart upload session intent methods (P2-M3).

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::audit::AuditEntry;
use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession};
use crate::infra::storage::db::db_err;
use crate::infra::storage::store::Store;

impl Store {
    // ── multipart uploads (P2-M3) ─────────────────────────────────────────────

    /// Create a multipart upload session row.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
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
                expires_at,
                now,
            )
            .await
    }

    /// Fetch a multipart upload session by `upload_id`.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    pub async fn get_multipart_upload(
        &self,
        upload_id: Uuid,
    ) -> Result<Option<MultipartUploadSession>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.multipart.get(&conn, upload_id).await
    }

    /// Insert or replace a multipart upload part.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
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
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .multipart
            .upsert_part(
                &conn,
                upload_id,
                part_number,
                backend_etag,
                part_hash,
                size,
                now,
            )
            .await
    }

    /// Whether `file_id` currently has at least one `in_progress` multipart
    /// upload session (regardless of `expires_at`).
    ///
    /// P2 2.8 orphan-file-reconciliation guard -- see
    /// `MultipartRepo::has_in_progress_for_file`.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
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
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
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
    /// Also flips `mime_validated` to `true` in the same UPDATE (P2
    /// remediation item 1.10): by the time `MultipartService::complete_multipart_upload`
    /// calls this, it has already sniffed the assembled object's leading
    /// bytes and validated them against `session.declared_mime` (bailing out
    /// with `DomainError::mime_mismatch` before ever reaching this call on a
    /// mismatch) — so reaching this point means the content is validated.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn complete_multipart_upload(
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
                    let updated = multipart
                        .update_state(tx, upload_id, "in_progress", "completed", Some(true))
                        .await?;
                    if updated {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    /// Mark a multipart upload session as `aborted`, delete its
    /// `multipart_upload_parts` rows, and record the audit row — all in the
    /// same transaction.
    ///
    /// Part-row deletion (P2 remediation, `docs/features/multipart-coordinator.md`
    /// `inst-abort-delete-parts`) lives here rather than at each call site so
    /// both abort paths that share this single CAS -- the user-driven
    /// `MultipartService::abort_multipart_upload` and the cleanup sweep's
    /// `CleanupEngine::abort_expired_multipart_session` -- get it for free.
    /// Folded into the same transaction as the state flip so a crash between
    /// the two can never leave the session `aborted` with its part rows still
    /// dangling.
    ///
    /// @cpt-cf-file-storage-fr-multipart-upload
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
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
                    let updated = multipart
                        .update_state(tx, upload_id, "in_progress", "aborted", None)
                        .await?;
                    if updated {
                        // @cpt-begin:cpt-cf-file-storage-flow-multipart-abort:p1:inst-abort-delete-parts
                        multipart.delete_parts_for_upload(tx, upload_id).await?;
                        // @cpt-end:cpt-cf-file-storage-flow-multipart-abort:p1:inst-abort-delete-parts
                        // @cpt-cf-file-storage-nfr-audit-completeness
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
