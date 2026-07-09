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
                        .update_state(tx, upload_id, "in_progress", "completed")
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

    /// Mark a multipart upload session as `aborted` and record the audit row
    /// in the same transaction.
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
                        .update_state(tx, upload_id, "in_progress", "aborted")
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
}
