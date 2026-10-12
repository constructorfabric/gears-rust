//! File-level queries and mutating operations on the `files` table.

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, NewFile, OwnerFilter};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::infra::storage::store::{IdempotencyInsert, Store, pending_version};

impl Store {
    /// Fetch a file by `(scope, file_id)`. Returns `None` when absent.
    pub async fn get_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Option<File>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos.files.get(&conn, scope, file_id).await
    }

    /// Like `get_file` but errors with `FileNotFound` when absent.
    pub async fn require_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<File, DomainError> {
        self.get_file(scope, file_id)
            .await?
            .ok_or_else(|| DomainError::file_not_found(file_id))
    }

    /// List files for an owner filter, newest first (limit/offset).
    pub async fn list_files(
        &self,
        scope: &AccessScope,
        owner: OwnerFilter,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<File>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .files
            .list(&conn, scope, owner, limit, offset)
            .await
    }

    /// Delete a file row (FK cascade removes versions and custom metadata) and write
    /// an audit row in one transaction. Returns `true` if a row was removed.
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
                        audit_repo.insert(tx, &audit).await?;
                    }
                    Ok::<bool, DomainError>(removed)
                })
            })
            .await
    }

    /// Insert a file row, a pending version and initial custom metadata plus an audit
    /// row in ONE transaction, so a partial failure leaves no file without a version.
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
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    /// Delete a file row (FK cascade removes versions and custom metadata), optionally
    /// enqueue a file-event, and write an audit row, in one transaction. Returns `true`
    /// if a row was removed.
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

    /// Delete the `files` row left by an abandoned pending-version orphan, re-checking
    /// **inside this transaction** that it has no versions and a `NULL` `content_id`.
    ///
    /// Unlike `delete_file_with_event` (unconditional, used by retention expiry), a
    /// version inserted or bound after the caller's pre-check aborts the deletion.
    /// Returns `false` if the guard failed or the row was already gone.
    pub async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    // Re-check the orphan guard inside the transaction, not on the
                    // caller's earlier snapshot.
                    let Some(file) = files.get(tx, &scope, file_id).await? else {
                        return Ok::<bool, DomainError>(false);
                    };
                    if file.content_id.is_some() {
                        return Ok(false);
                    }
                    let remaining = versions.list_by_file(tx, &scope, file_id, 1, 0).await?;
                    if !remaining.is_empty() {
                        return Ok(false);
                    }
                    let removed = files.delete(tx, &scope, file_id).await?;
                    if removed {
                        audit_repo.insert(tx, &audit).await?;
                        if let Some(ev) = event {
                            events_repo.enqueue(tx, &ev).await?;
                        }
                    }
                    Ok(removed)
                })
            })
            .await
    }

    /// Create a file + pending version + initial metadata + optional event and
    /// idempotency record in one transaction.
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
                    // Same transaction, so a committed create always has a replay record;
                    // a live-key conflict (concurrent duplicate) rolls the creation back.
                    if let Some(idem) = idempotency {
                        idempotency_repo.insert(tx, &idem, file_id, now).await?;
                    }
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    /// List file-event rows for a file ordered by occurrence time (tests only).
    pub async fn list_file_events(
        &self,
        file_id: Uuid,
    ) -> Result<Vec<crate::infra::storage::repo::FileEventRow>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos.events_outbox.list_for_file(&conn, file_id).await
    }
}
