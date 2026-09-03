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
use crate::infra::storage::db::{db_err, transaction_with_bounded_retry};
use crate::infra::storage::store::{IdempotencyInsert, Store, pending_version};

/// De-duplicate a new file's initial `custom_metadata` entries (last
/// occurrence in the request wins) before batching them into one
/// `MetadataRepo::insert_many` call. Nothing upstream (`NewFile::
/// custom_metadata: Vec<CustomMetadataEntry>`) guarantees a client can't
/// list the same key twice in one create request, and a single multi-row
/// INSERT with the same `(file_id, key)` twice would violate the primary key
/// and fail the whole create -- so duplicates must be resolved to "last one
/// wins" before batching.
fn dedup_initial_metadata(entries: &[(String, String)]) -> Vec<(String, String)> {
    let mut deduped: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for (k, v) in entries {
        deduped.insert(k.as_str(), v.as_str());
    }
    deduped
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

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
    pub async fn delete_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let del_scope = scope.clone();
        let db = self.db.db();
        // Retryable: `files.delete` cascades (FK `ON DELETE CASCADE`) into
        // `file_versions`, i.e. this transaction locks `files` then
        // `file_versions` -- the opposite order from `finalize_version`'s
        // auto-bind branch (`file_versions` then `files`). See
        // `db::transaction_with_bounded_retry` for the retry contract.
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let audit_repo = audit_repo.clone();
            let del_scope = del_scope.clone();
            let audit = audit.clone();
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

    // ── create ───────────────────────────────────────────────────────────────

    /// Insert a new file row + a pending version row + any initial custom-
    /// metadata entries in ONE transaction, so a failure partway through cannot
    /// leave a visible file with no version (or partial metadata) behind.
    ///
    /// An audit row is written in the same transaction.
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
                    let deduped_metadata = dedup_initial_metadata(&metadata_entries);
                    metadata
                        .insert_many(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            &deduped_metadata,
                            now,
                        )
                        .await?;
                    audit_repo.insert(tx, &audit).await?;
                    Ok::<(), DomainError>(())
                })
            })
            .await
    }

    // ── file-events variants ─────────────────────────────────────────────────

    /// Delete a file row (FK cascade removes versions + custom metadata),
    /// optionally enqueue a file-event, and write an audit row — all in a
    /// single transaction.
    ///
    /// Returns `true` if a row was removed.
    ///
    /// This is the events-aware variant of [`delete_file`]; the original method
    /// is preserved for callers that do not need event enqueuing.
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
        let db = self.db.db();
        // Retryable for the same reason as `delete_file` (see its comment):
        // `files` then cascaded `file_versions`, opposite `finalize_version`'s
        // auto-bind order.
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let audit_repo = audit_repo.clone();
            let events_repo = events_repo.clone();
            let del_scope = del_scope.clone();
            let audit = audit.clone();
            let event = event.clone();
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
    /// pending-version orphan.
    ///
    /// Unlike [`Self::delete_file_with_event`] (unconditional -- used by the
    /// retention-expiry sweep, which has already decided the file must go
    /// regardless of its version count), this method re-verifies the orphan
    /// condition (`content_id IS NULL` and zero version rows) as part of
    /// **the same conditional `DELETE` statement** that removes the row --
    /// see [`crate::infra::storage::repo::FileRepo::delete_if_orphan`]'s doc
    /// comment for the full reasoning.
    ///
    /// This used to re-read `files`/`versions` with two plain `SELECT`s
    /// inside the transaction before an unconditional delete, on the theory
    /// that a version inserted between the caller's pre-check and this call
    /// was "guaranteed to be seen" -- that was **incorrect** under `READ
    /// COMMITTED` (ordinary `SELECT`s take no locks and each is its own
    /// snapshot, so a concurrently-inserted, autocommitted pending version
    /// could be missed and then cascade-deleted along with the file; SQLite
    /// does not reproduce this, which is why it went unnoticed). Delegating
    /// the whole guard to `delete_if_orphan`'s single statement narrows that
    /// window to the span of one statement and removes the `content_id` half
    /// of it entirely -- but it does not eliminate the version half on
    /// PostgreSQL, for the MVCC reason spelled out in that method's own doc
    /// comment. Do not read this call as race-free; read it as "no longer
    /// racy between two application-level reads".
    ///
    /// Returns `true` if the file row was removed; `false` if the guard did
    /// not match (a version now exists or content is bound) or the row was
    /// already gone (e.g. a concurrent sweep).
    pub async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let db = self.db.db();
        // Retryable: `delete_if_orphan` still touches `files` first (its
        // guard re-reads `content_id`/version count before deleting), the
        // same exposure as `delete_file`/`delete_file_with_event` against
        // `finalize_version`'s reversed lock order.
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let audit_repo = audit_repo.clone();
            let events_repo = events_repo.clone();
            let audit = audit.clone();
            let event = event.clone();
            Box::pin(async move {
                let scope = AccessScope::allow_all();
                let removed = files.delete_if_orphan(tx, &scope, file_id).await? > 0;
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
                    let deduped_metadata = dedup_initial_metadata(&metadata_entries);
                    metadata
                        .insert_many(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            &deduped_metadata,
                            now,
                        )
                        .await?;
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
    /// WITHOUT pre-registering any version. Used by the merged `POST /files`
    /// create+plan path, where the multipart
    /// initiate that follows registers its own pending version — the
    /// pre-registered single-part version of
    /// [`Self::create_file_with_pending_version_and_event`] would only become
    /// an orphan here.
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
                    let deduped_metadata = dedup_initial_metadata(&metadata_entries);
                    metadata
                        .insert_many(
                            tx,
                            &AccessScope::allow_all(),
                            file_id,
                            &deduped_metadata,
                            now,
                        )
                        .await?;
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
    pub async fn list_file_events(
        &self,
        file_id: Uuid,
    ) -> Result<Vec<crate::infra::storage::repo::FileEventRow>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.events_outbox.list_for_file(&conn, file_id).await
    }
}
