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

    /// Delete the parent `files` row left behind by an abandoned
    /// pending-version orphan (P2 2.8), re-verifying **inside this
    /// transaction** that the file still has zero remaining versions and a
    /// `NULL` `content_id` before deleting it.
    ///
    /// Unlike [`Self::delete_file_with_event`] (unconditional -- used by the
    /// retention-expiry sweep, which has already decided the file must go
    /// regardless of its version count), this method re-reads `files`/
    /// `versions` fresh inside the same transaction that performs the
    /// delete, so a version inserted or bound between the caller's
    /// pre-check (`list_versions` + `get_file`) and this call is guaranteed
    /// to be seen and aborts the deletion -- the DELETE simply matches zero
    /// intent and the file (with its new version) is left untouched.
    ///
    /// Returns `true` if the file row was removed; `false` if the guard
    /// failed (a version now exists or content is bound) or the row was
    /// already gone (e.g. a concurrent sweep).
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-cf-file-storage-fr-file-events
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
                    // Re-check both halves of the orphan guard fresh, inside
                    // this transaction, rather than trusting the caller's
                    // pre-transaction snapshot.
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
                    // a committed create always has a replay record. Only a
                    // *lapsed* row for the same key is deleted first inside the
                    // repo (an expired row's PK would otherwise collide with a
                    // legitimate new insert); a live-key PK conflict from a
                    // concurrent duplicate create is NOT tolerated — every
                    // failure, including that conflict, is propagated by
                    // `IdempotencyRepo::insert` and rolls this whole creation
                    // back, so the racing caller retries and replays the
                    // winner's record via `get` instead of ending up with two
                    // files (see `IdempotencyRepo::insert`'s doc comment).
                    if let Some(idem) = idempotency {
                        idempotency_repo.insert(tx, &idem, file_id, now).await?;
                    }
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    /// Create the file row (+ initial custom metadata, audit, optional event)
    /// WITHOUT pre-registering any version (upload-flow redesign). Used by
    /// the merged `POST /files` create+plan path, where the multipart
    /// initiate that follows registers its own pending version — the
    /// pre-registered single-part version of
    /// [`Self::create_file_with_pending_version_and_event`] would only become
    /// an orphan here.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-fr-file-events
    pub async fn create_file_with_event(
        &self,
        new: &NewFile,
        file_id: Uuid,
        tenant_id: Uuid,
        now: OffsetDateTime,
        audit: AuditEntry,
        event: Option<FileEvent>,
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
        let metadata_entries: Vec<(String, String)> = new
            .custom_metadata
            .iter()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect();

        let files = self.repos.files.clone();
        let metadata = self.repos.metadata.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    files.create(tx, &AccessScope::allow_all(), &file).await?;
                    for (key, value) in &metadata_entries {
                        metadata
                            .upsert(tx, &AccessScope::allow_all(), file_id, key, value, now)
                            .await?;
                    }
                    audit_repo.insert(tx, &audit).await?;
                    if let Some(ev) = event {
                        events_repo.enqueue(tx, &ev).await?;
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
