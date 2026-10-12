//! Version-level queries and mutating operations, including the bind and ownership-transfer
//! transactions.

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, FileVersion, VersionStatus};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::infra::content::hash_mode::HashMode;
use crate::infra::storage::store::{Store, pending_version};

/// "No limit" for callers that must see a file's complete version set (cascade delete,
/// backend migration, usage accounting, sweeps); a page cap would under-delete blobs or
/// miscount usage. Kept within `i64::MAX` because `LIMIT` is signed 64-bit on `SQLite`
/// and `Postgres`.
const UNBOUNDED_VERSIONS: u64 = i64::MAX as u64;

impl Store {
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
        let conn = self.db.conn().map_err(DomainError::from)?;
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
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .versions
            .get(&conn, &AccessScope::allow_all(), file_id, version_id)
            .await
    }

    /// List **all** versions of a file, newest first (unbounded; see
    /// `UNBOUNDED_VERSIONS`).
    pub async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .versions
            .list_by_file(
                &conn,
                &AccessScope::allow_all(),
                file_id,
                UNBOUNDED_VERSIONS,
                0,
            )
            .await
    }

    /// List a page of a file's versions, newest first (backs `GET /files/{id}/versions`);
    /// `limit`/`offset` are expected to be clamped by the caller.
    pub async fn list_versions_page(
        &self,
        file_id: Uuid,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .versions
            .list_by_file(&conn, &AccessScope::allow_all(), file_id, limit, offset)
            .await
    }

    /// MIME type of the file's current version; `Ok(None)` only when no content is
    /// bound (DB errors propagate).
    pub async fn current_version_mime(&self, file: &File) -> Result<Option<String>, DomainError> {
        let Some(content_id) = file.content_id else {
            return Ok(None);
        };
        Ok(self
            .get_version(file.file_id, content_id)
            .await?
            .map(|v| v.mime_type))
    }

    /// Record a version's size + hash, mark it `available` and write an audit row in
    /// one transaction. Returns `true` if the version row existed and was updated.
    ///
    /// `mime_type` is the sniffed content type to persist; `None` keeps the declared
    /// one (multipart complete does not validate MIME). `hash_mode`/`part_count` are
    /// set here; for `multipart-composite-sha256`, `manifest` is inserted in the same
    /// transaction.
    #[allow(clippy::too_many_arguments)]
    pub async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        hash_mode: HashMode,
        part_count: Option<i32>,
        manifest: Option<String>,
        mime_type: Option<String>,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let hash_mode_str = hash_mode.as_str();
        let now = OffsetDateTime::now_utc();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    let updated = versions
                        .finalize(
                            tx,
                            &scope,
                            file_id,
                            version_id,
                            size,
                            hash_value,
                            hash_mode_str,
                            part_count,
                            mime_type,
                        )
                        .await?;
                    if updated {
                        // Manifest and version row commit atomically.
                        if let Some(manifest) = manifest {
                            versions
                                .insert_manifest(tx, &scope, version_id, &manifest, now)
                                .await?;
                        }
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(updated)
                })
            })
            .await
    }

    /// Fetch the manifest text of a version, if any (`multipart-composite-sha256` only).
    pub async fn get_version_manifest(
        &self,
        version_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .versions
            .get_manifest(&conn, &AccessScope::allow_all(), version_id)
            .await
    }

    /// Delete a version row and record an audit row in the same transaction.
    ///
    /// Returns `false` if it does not exist or is current. The current check is
    /// re-read inside the transaction and `VersionRepo::delete` also guards
    /// `is_current = false`, so a concurrent `bind` cannot leave `files.content_id`
    /// dangling (the delete then removes 0 rows).
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
                    let scope = AccessScope::allow_all();
                    // Re-read in-transaction: `is_current` mirrors `files.content_id`
                    // (both flip together in `bind_atomic`).
                    let Some(existing) = versions.get(tx, &scope, file_id, version_id).await?
                    else {
                        return Ok::<bool, DomainError>(false);
                    };
                    if existing.is_current {
                        return Ok(false);
                    }
                    let rows_affected = versions.delete(tx, &scope, file_id, version_id).await?;
                    if rows_affected == 0 {
                        // Raced: a concurrent bind made it current after the read above.
                        return Ok(false);
                    }
                    audit_repo.insert(tx, &audit).await?;
                    Ok(true)
                })
            })
            .await
    }

    /// Delete a version row iff it is still `pending`, with an audit row in the same
    /// transaction. Used by the sweep instead of `delete_version` so a version that a
    /// racing `complete_multipart_upload` made `available` is never deleted.
    pub async fn delete_pending_version(
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
                        .delete_if_status(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            version_id,
                            VersionStatus::Pending,
                        )
                        .await?;
                    if removed {
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    /// Swap the content pointer and promote `version_id` as current in one transaction
    /// (the bind CAS), writing an audit row on success.
    ///
    /// `scope` must be the authorized scope for the CAS; the `is_current` flip uses
    /// `allow_all()` since versions have no tenant column and the file was already
    /// checked. Returns `false` on a concurrent CAS conflict.
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
                    // Honours the unique-current index.
                    versions
                        .clear_current(tx, &AccessScope::allow_all(), file_id)
                        .await?;
                    versions
                        .set_current(tx, &AccessScope::allow_all(), file_id, version_id)
                        .await?;
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<bool, DomainError>(true)
                })
            })
            .await
    }

    /// Like `bind_atomic`, additionally enqueuing an optional file-event in the same
    /// transaction.
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

    /// Update a version's `backend_id`/`backend_path`, CAS-gated on the expected values,
    /// and write a `BackendMigrate` audit row in the same transaction. `false` means the
    /// version is gone or another migration moved the pointer; the caller must re-fetch.
    #[allow(clippy::too_many_arguments)]
    pub async fn rebind_version_backend(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        expected_backend_id: &str,
        expected_backend_path: &str,
        new_backend_id: &str,
        new_backend_path: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let expected_backend_id = expected_backend_id.to_owned();
        let expected_backend_path = expected_backend_path.to_owned();
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
                            &expected_backend_id,
                            &expected_backend_path,
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

    /// Update `owner_kind`/`owner_id`, enqueue an optional event and record an audit
    /// row in one transaction. Returns `true` if the file row was found and updated.
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
