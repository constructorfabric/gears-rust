//! File-level queries and mutating operations on the `files` table.
//!
//! Covers: get / require / list / delete (plain + with event) / create
//! (plain + with event + idempotency).

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, NewFile, OwnerFilter};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::store::{IdempotencyInsert, Store, pending_version};

impl Store {
    // ── file queries ─────────────────────────────────────────────────────────

    /// Fetch a file by `(scope, file_id)`. Returns `None` when absent.
    pub async fn get_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Option<File>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.files.get(&conn, scope, file_id).await
    }

    /// Like [`get_file`] but errors with `FileNotFound` when absent.
    pub async fn require_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<File, DomainError> {
        self.get_file(scope, file_id)
            .await?
            .ok_or_else(|| DomainError::file_not_found(file_id))
    }

    /// List files for an owner filter, newest-first, offset-paginated.
    pub async fn list_files(
        &self,
        scope: &AccessScope,
        owner: OwnerFilter,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<File>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .files
            .list(&conn, scope, owner, limit, offset)
            .await
    }

    /// Delete a file row (FK cascade removes versions + custom metadata) and
    /// write an audit row — both in a single transaction.
    ///
    /// Returns `true` if a row was removed.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn delete_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let del_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let removed = files.delete(tx, &del_scope, file_id).await?;
                    if removed {
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    // ── create ───────────────────────────────────────────────────────────────

    /// Insert a new file row + a pending version row + any initial custom-
    /// metadata entries in ONE transaction, so a failure partway through cannot
    /// leave a visible file with no version (or partial metadata) behind.
    ///
    /// An audit row is written in the same transaction.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_with_pending_version(
        &self,
        new: &NewFile,
        file_id: Uuid,
        version_id: Uuid,
        tenant_id: Uuid,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
        audit: AuditEntry,
    ) -> Result<(), DomainError> {
        let file = File {
            file_id,
            tenant_id,
            owner_kind: new.owner_kind,
            owner_id: new.owner_id,
            name: new.name.clone(),
            gts_file_type: new.gts_file_type.clone(),
            content_id: None,
            meta_version: 0,
            created_at: now,
            last_modified_at: now,
        };
        let pending = pending_version(
            file_id,
            version_id,
            &new.mime_type,
            backend_id,
            backend_path,
            now,
        );
        // Own the initial metadata entries so the transaction closure can move them.
        let metadata_entries: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();

        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let metadata = self.repos.metadata.clone();
        let audit_repo = self.repos.audit.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    files.create(tx, &AccessScope::allow_all(), &file).await?;
                    versions
                        .insert(tx, &AccessScope::allow_all(), &pending)
                        .await?;
                    for (key, value) in &metadata_entries {
                        metadata
                            .upsert(tx, &AccessScope::allow_all(), file_id, key, value, now)
                            .await?;
                    }
                    // @cpt-cf-file-storage-nfr-audit-completeness
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    // ── file-events variants (P2-M5) ─────────────────────────────────────────

    /// Delete a file row (FK cascade removes versions + custom metadata),
    /// optionally enqueue a file-event, and write an audit row — all in a
    /// single transaction.
    ///
    /// Returns `true` if a row was removed.
    ///
    /// This is the events-aware variant of [`delete_file`]; the original method
    /// is preserved for callers that do not need event enqueuing.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-nfr-audit-completeness
    pub async fn delete_file_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let del_scope = scope.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let removed = files.delete(tx, &del_scope, file_id).await?;
                    if removed {
                        audit_repo.insert(tx, &audit).await?;
                        if let Some(ev) = event {
                            events_repo.enqueue(tx, &ev).await?;
                        }
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    /// Create a new file + pending version + initial metadata + optional event,
    /// all in one transaction.
    ///
    /// This is the events-aware variant of [`create_file_with_pending_version`];
    /// the original is preserved for callers that do not need event enqueuing.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    /// @cpt-cf-file-storage-nfr-audit-completeness
    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_with_pending_version_and_event(
        &self,
        new: &NewFile,
        file_id: Uuid,
        version_id: Uuid,
        tenant_id: Uuid,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
        idempotency: Option<IdempotencyInsert>,
    ) -> Result<(), DomainError> {
        let file = File {
            file_id,
            tenant_id,
            owner_kind: new.owner_kind,
            owner_id: new.owner_id,
            name: new.name.clone(),
            gts_file_type: new.gts_file_type.clone(),
            content_id: None,
            meta_version: 0,
            created_at: now,
            last_modified_at: now,
        };
        let pending = pending_version(
            file_id,
            version_id,
            &new.mime_type,
            backend_id,
            backend_path,
            now,
        );
        let metadata_entries: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();

        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let metadata = self.repos.metadata.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let idempotency_repo = self.repos.idempotency_keys.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    files.create(tx, &AccessScope::allow_all(), &file).await?;
                    versions
                        .insert(tx, &AccessScope::allow_all(), &pending)
                        .await?;
                    for (key, value) in &metadata_entries {
                        metadata
                            .upsert(tx, &AccessScope::allow_all(), file_id, key, value, now)
                            .await?;
                    }
                    audit_repo.insert(tx, &audit).await?;
                    if let Some(ev) = event {
                        events_repo.enqueue(tx, &ev).await?;
                    }
                    // Persist the idempotency record in the same transaction, so
                    // a committed create always has a replay record. A PK
                    // conflict (concurrent duplicate) is tolerated inside the
                    // repo; any real DB error rolls the whole creation back.
                    if let Some(idem) = idempotency {
                        idempotency_repo
                            .insert(
                                tx,
                                idem.tenant_id,
                                &idem.owner_kind,
                                idem.owner_id,
                                &idem.key,
                                file_id,
                                idem.response_status,
                                &idem.response_body,
                                &idem.response_etag,
                                idem.expires_at,
                                now,
                            )
                            .await?;
                    }
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    /// List file-event rows for a specific file ordered by occurrence time.
    ///
    /// Intended for testing; not exposed on the REST API.
    ///
    /// @cpt-cf-file-storage-fr-file-events
    pub async fn list_file_events(
        &self,
        file_id: Uuid,
    ) -> Result<Vec<crate::infra::storage::repo::FileEventRow>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.events_outbox.list_for_file(&conn, file_id).await
    }
}
