//! Multipart upload session intent methods.

use time::OffsetDateTime;
use toolkit_db::secure::TxConfig;
use uuid::Uuid;

use crate::domain::audit::AuditEntry;
use crate::domain::error::DomainError;
use crate::domain::error_convert::db_err;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession};
use crate::infra::storage::store::Store;

impl Store {
    /// Create a multipart upload session row.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        backend_id: &str,
        backend_path: &str,
        declared_mime: &str,
        declared_size: u64,
        part_size: u64,
        auto_bind: bool,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .multipart
            .create(
                &conn,
                upload_id,
                file_id,
                version_id,
                backend_upload_handle,
                backend_id,
                backend_path,
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
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos.multipart.get(&conn, upload_id).await
    }

    /// Insert or replace a multipart upload part.
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
        let conn = self.db.conn().map_err(DomainError::from)?;
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

    /// Whether `file_id` has an active (`in_progress` or `completing`) session (orphan-reconciliation guard).
    pub async fn has_active_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .multipart
            .has_active_for_file(&conn, file_id)
            .await
    }

    /// Force-set a session's `expires_at`. **Test-support only; do not call in
    /// production** (see `MultipartRepo::set_expires_at`).
    #[doc(hidden)]
    pub async fn set_multipart_expires_at_for_test(
        &self,
        upload_id: Uuid,
        expires_at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
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
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos.multipart.list_parts(&conn, upload_id).await
    }

    /// Mark a session `completed` (also setting `mime_validated`: the caller has already
    /// sniffed and validated the assembled object), persist the response snapshot and record
    /// the audit row in the same transaction.
    ///
    /// `lease_owner` must still match (see `MultipartRepo::finish_complete`). Only reached via
    /// `MultipartService::finish_session` (takeover / converge after a lost finalize CAS).
    pub async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        lease_owner: &str,
        result_json: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let multipart = self.repos.multipart.clone();
        let audit_repo = self.repos.audit.clone();
        let lease_owner = lease_owner.to_owned();
        let result_json = result_json.to_owned();
        self.db
            .db()
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let multipart = multipart.clone();
                let audit_repo = audit_repo.clone();
                let audit = audit.clone();
                let lease_owner = lease_owner.clone();
                let result_json = result_json.clone();
                Box::pin(async move {
                    // Terminal transition from `completing`; persists the snapshot for idempotent re-completes.
                    let updated = multipart
                        .finish_complete(tx, upload_id, Some(&lease_owner), &result_json)
                        .await?;
                    if updated {
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    /// Acquire (or take over an expired) lease; see `MultipartRepo::acquire_complete_lease`.
    pub async fn acquire_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .multipart
            .acquire_complete_lease(&conn, upload_id, owner, lease_until, now)
            .await
    }

    /// Release a held lease after a failed assembly; see `MultipartRepo::release_complete_lease`.
    pub async fn release_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .multipart
            .release_complete_lease(&conn, upload_id, owner)
            .await
    }

    /// Mark a session `aborted` and record the audit row in one transaction (user abort and cleanup sweep).
    pub async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let multipart = self.repos.multipart.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let multipart = multipart.clone();
                let audit_repo = audit_repo.clone();
                let audit = audit.clone();
                Box::pin(async move {
                    let mut updated = multipart
                        .update_state(tx, upload_id, "in_progress", "aborted", None)
                        .await?;
                    if !updated {
                        // An expired `completing` lease is abortable too; a live one never is (`lease_until < now`).
                        updated = multipart
                            .abort_expired_completing(tx, upload_id, OffsetDateTime::now_utc())
                            .await?;
                    }
                    if updated {
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }
}
