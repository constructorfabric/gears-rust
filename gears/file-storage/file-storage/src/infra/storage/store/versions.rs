//! Version-level queries and mutating operations.
//!
//! Covers: insert_pending_version, get_version, list_versions,
//! current_version_mime, finalize_version, delete_version,
//! rebind_version_backend, bind_atomic (+ events variant),
//! transfer_ownership_atomic.

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, FileVersion};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::store::{Store, pending_version};

impl Store {
    // ── version management ───────────────────────────────────────────────────

    /// Insert a pending version row (for `presign_version`).
    pub async fn insert_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        mime_type: &str,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        let pending = pending_version(
            file_id,
            version_id,
            mime_type,
            backend_id,
            backend_path,
            now,
        );
        self.repos
            .versions
            .insert(&conn, &AccessScope::allow_all(), &pending)
            .await
    }

    /// Fetch a single version by `(file_id, version_id)`.
    pub async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .get(&conn, &AccessScope::allow_all(), file_id, version_id)
            .await
    }

    /// List all versions of a file, newest first.
    pub async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .list_by_file(&conn, &AccessScope::allow_all(), file_id)
            .await
    }

    /// Return the MIME type of the file's current (bound) version, if any.
    /// `Ok(None)` means there is genuinely no bound content; a DB/connection
    /// failure is propagated as `Err` (never silently treated as "no mime").
    pub async fn current_version_mime(&self, file: &File) -> Result<Option<String>, DomainError> {
        let Some(content_id) = file.content_id else {
            return Ok(None);
        };
        Ok(self
            .get_version(file.file_id, content_id)
            .await?
            .map(|v| v.mime_type))
    }

    /// Record a version's size + hash and mark it `available`.
    /// Returns `true` if the version row existed and was updated.
    ///
    /// An audit row is written in the same transaction.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = versions
                        .finalize(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            version_id,
                            size,
                            hash_value,
                        )
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

    /// Delete a single version row and record an audit row in the same
    /// transaction.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let removed = versions
                        .delete(tx, &AccessScope::allow_all(), file_id, version_id)
                        .await?;
                    if removed {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    // ── atomic multi-step operations ─────────────────────────────────────────

    /// Swap the content pointer + promote `version_id` as current, in a single
    /// transaction (the bind CAS — DESIGN §3.7). An audit row is written in the
    /// same transaction on a successful swap.
    ///
    /// The `scope` used for the CAS update must be the authorized scope
    /// (returned by the authorizer); the `is_current` flip uses
    /// `allow_all()` because the version row has no tenant column and the
    /// parent file was already checked.
    ///
    /// Returns `true` on a successful swap, `false` on a concurrent CAS
    /// conflict (caller maps to 412 PreconditionFailed).
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn bind_atomic(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        expected_content_id: Option<Uuid>,
        version_id: Uuid,
        now: OffsetDateTime,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let bind_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let swapped = files
                        .bind_content_cas(
                            tx,
                            &bind_scope,
                            file_id,
                            expected_content_id,
                            version_id,
                            now,
                        )
                        .await?;
                    if !swapped {
                        return Ok(false);
                    }
                    // Promote the new version as current (unique-current index honoured).
                    versions
                        .clear_current(tx, &AccessScope::allow_all(), file_id)
                        .await?;
                    versions
                        .set_current(tx, &AccessScope::allow_all(), file_id, version_id)
                        .await?;
                    // @cpt-cf-file-storage-nfr-audit-completeness
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<bool, DomainError>(true)
                })
            })
            .await
    }

    /// Swap the content pointer + promote `version_id` as current, optionally
    /// enqueue a file-event — all in a single transaction.
    ///
    /// This is the events-aware variant of [`bind_atomic`]; the original is
    /// preserved for callers that do not need event enqueuing.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-nfr-audit-completeness
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_atomic_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        expected_content_id: Option<Uuid>,
        version_id: Uuid,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let bind_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let swapped = files
                        .bind_content_cas(
                            tx,
                            &bind_scope,
                            file_id,
                            expected_content_id,
                            version_id,
                            now,
                        )
                        .await?;
                    if !swapped {
                        return Ok(false);
                    }
                    versions
                        .clear_current(tx, &AccessScope::allow_all(), file_id)
                        .await?;
                    versions
                        .set_current(tx, &AccessScope::allow_all(), file_id, version_id)
                        .await?;
                    audit_repo.insert(tx, &audit).await?;
                    if let Some(ev) = event {
                        events_repo.enqueue(tx, &ev).await?;
                    }
                    Ok::<bool, DomainError>(true)
                })
            })
            .await
    }

    /// Transactionally update `backend_id` and `backend_path` for a version row,
    /// and write a `BackendMigrate` audit row in the same transaction.
    ///
    /// Returns `true` if the version row was found and updated.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
    pub async fn rebind_version_backend(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        new_backend_id: &str,
        new_backend_path: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let new_backend_id = new_backend_id.to_owned();
        let new_backend_path = new_backend_path.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = versions
                        .rebind_backend(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            version_id,
                            &new_backend_id,
                            &new_backend_path,
                        )
                        .await?;
                    if updated {
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    // ── ownership transfer (P2-M5) ────────────────────────────────────────────

    /// Update `owner_kind` + `owner_id` for a file, enqueue an optional event
    /// row, and record an audit row — all in one transaction.
    ///
    /// Returns `true` if the file row was found and updated.
    ///
    /// @cpt-cf-file-storage-fr-ownership-transfer
    /// @cpt-cf-file-storage-fr-file-events
    #[allow(clippy::too_many_arguments)]
    pub async fn transfer_ownership_atomic(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        new_owner_kind: &str,
        new_owner_id: Uuid,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let transfer_scope = scope.clone();
        let new_owner_kind = new_owner_kind.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let updated = files
                        .update_owner(
                            tx,
                            &transfer_scope,
                            file_id,
                            &new_owner_kind,
                            new_owner_id,
                            now,
                        )
                        .await?;
                    if updated {
                        audit_repo.insert(tx, &audit).await?;
                        if let Some(ev) = event {
                            events_repo.enqueue(tx, &ev).await?;
                        }
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }
}
