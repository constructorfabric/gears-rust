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
use crate::domain::ports::DeletedFile;
use crate::infra::storage::db::{db_err, transaction_with_bounded_retry};
use crate::infra::storage::store::versions::UNBOUNDED_VERSIONS;
use crate::infra::storage::store::{IdempotencyInsert, Store, pending_version};

/// Overwrite `detail`/`payload`'s top-level `"version_count"` key with the
/// freshly-counted version list length, if that key is present -- see
/// [`Store::delete_file_collecting_versions`]'s doc comment for why the
/// caller cannot know the true count before this transaction runs. A no-op
/// for a JSON value that carries no such key (e.g. the retention-sweep's
/// `RetentionDelete` audit detail, which never included one).
fn patch_version_count(value: &mut serde_json::Value, count: usize) {
    if let serde_json::Value::Object(map) = value
        && map.contains_key("version_count")
    {
        map.insert("version_count".to_owned(), serde_json::json!(count));
    }
}

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

    /// Batched counterpart of [`Self::get_file`]/[`Self::require_file`]:
    /// fetch every file in `ids` that exists (and is visible under `scope`)
    /// in one query (chunked against the bind-parameter budget, see
    /// `FileRepo::list_by_ids`) instead of one round trip per id.
    pub async fn list_files_by_ids(
        &self,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<File>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.files.list_by_ids(&conn, scope, ids).await
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

    /// Delete a file row, collecting the version rows for backend-blob
    /// cleanup **inside the same transaction** as the delete, optionally
    /// enqueue a file-event, and write an audit row -- all atomically.
    ///
    /// # Why the versions are listed HERE, not by the caller beforehand
    ///
    /// The caller used to call `Store::list_versions` before ever opening a
    /// transaction, build an audit/event payload from that list's length,
    /// then delete unconditionally. A version inserted by a
    /// concurrent `presign_version`/`initiate_multipart_upload` on this exact
    /// `file_id`, any time between that early read and the delete
    /// transaction's commit, was cascade-removed along with the file (`ON
    /// DELETE CASCADE`) but never appeared in the pre-transaction list -- its
    /// backend blob was never queued for the caller's best-effort cleanup,
    /// and once the DB rows are gone, the cleanup engine (which only ever
    /// looks at rows still in the database) can never find it either: a
    /// permanent storage leak, not a temporary orphan with a backstop.
    /// Re-reading the version list immediately before the `DELETE`, instead
    /// of trusting a snapshot from arbitrarily earlier, used to only narrow
    /// that gap to the width of this transaction -- see the "Row lock"
    /// section below for why locking the parent row first closes it
    /// completely instead: any insert that commits before this statement
    /// runs is included in the returned list (and therefore in the caller's
    /// cleanup/audit/usage accounting), and one that hasn't yet cannot land
    /// unseen before the list is read.
    ///
    /// `audit.detail`/`event.payload`'s top-level `"version_count"` key, if
    /// present, is overwritten with the freshly-counted length before either
    /// row is persisted -- the caller cannot know the true count until this
    /// transaction runs, so it must not bake a stale one into the JSON it
    /// hands in.
    ///
    /// # Row lock closes the delete-vs-insert race
    ///
    /// The transaction's first statement locks the `files` row
    /// (`FileRepo::lock_for_update`, `SELECT ... FOR UPDATE`) before the
    /// version list is even read. A concurrent `presign_version`/
    /// `initiate_multipart_upload` on this exact `file_id`
    /// (`insert_pending_version`) takes `FOR KEY SHARE` on this same row for
    /// its FK check, which conflicts with `FOR UPDATE` -- so that insert
    /// either commits strictly before this lock is granted (and is then
    /// necessarily visible to the fresh `list_by_file` read a few lines
    /// below, since that read happens after the lock) or blocks until this
    /// transaction ends and then fails its own FK check against the
    /// now-deleted row (mapped to `FileNotFound` -- see
    /// `VersionRepo::insert`). Either way there is no window left in which a
    /// version can be inserted, committed, and cascade-removed without ever
    /// appearing in `collected`. See
    /// `docs/toolkit_unified_system/11_database_patterns.md`'s "Row locks"
    /// section for the general pattern this follows.
    pub async fn delete_file_collecting_versions(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<DeletedFile, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let retention_rules = self.repos.retention_rules.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let del_scope = scope.clone();
        let db = self.db.db();
        // Retryable for the same reason as `delete_file` (see its comment):
        // `files` then cascaded `file_versions`, the opposite order from
        // `finalize_version`'s auto-bind branch.
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let versions = versions.clone();
            let retention_rules = retention_rules.clone();
            let audit_repo = audit_repo.clone();
            let events_repo = events_repo.clone();
            let del_scope = del_scope.clone();
            let mut audit = audit.clone();
            let mut event = event.clone();
            Box::pin(async move {
                let scope_all = AccessScope::allow_all();
                // First statement: lock the parent row -- see this method's
                // doc comment. `None` means the file is already gone (a
                // concurrent delete/expiry won outright); nothing left to
                // collect or remove.
                if files
                    .lock_for_update(tx, &del_scope, file_id)
                    .await?
                    .is_none()
                {
                    return Ok::<DeletedFile, DomainError>(DeletedFile {
                        removed: false,
                        versions: Vec::new(),
                    });
                }

                // Fresh, in-transaction snapshot -- see this method's doc
                // comment for the race this closes.
                let collected = versions
                    .list_by_file(tx, &scope_all, file_id, UNBOUNDED_VERSIONS, 0)
                    .await?;
                patch_version_count(&mut audit.detail, collected.len());
                if let Some(ev) = event.as_mut() {
                    patch_version_count(&mut ev.payload, collected.len());
                }

                let removed = files.delete(tx, &del_scope, file_id).await?;
                if removed {
                    // `retention_rules` has no FK from `scope_target_id` to
                    // `files.file_id` -- remove any `File`-scope rule still
                    // targeting this file in the SAME transaction, so it can
                    // never outlive its target (see
                    // `RetentionRuleRepo::delete_file_scope_rules`'s doc
                    // comment).
                    retention_rules
                        .delete_file_scope_rules(tx, &scope_all, file_id)
                        .await?;
                    audit_repo.insert(tx, &audit).await?;
                    if let Some(ev) = event {
                        events_repo.enqueue(tx, &ev).await?;
                    }
                }
                Ok::<DeletedFile, DomainError>(DeletedFile {
                    removed,
                    versions: if removed { collected } else { Vec::new() },
                })
            })
        })
        .await
    }

    /// Delete the parent `files` row left behind by an abandoned
    /// pending-version orphan.
    ///
    /// Unlike [`Self::delete_file_collecting_versions`] (unconditional -- used
    /// by the retention-expiry sweep, which has already decided the file must
    /// go regardless of its version count), this method re-verifies the
    /// orphan condition -- `content_id IS NULL`, zero version rows, and no
    /// active (`in_progress`/`completing`) multipart session -- fresh inside
    /// this transaction before removing the row.
    ///
    /// # Row lock closes the reclaim-vs-insert race
    ///
    /// The transaction's first statement locks the `files` row
    /// (`FileRepo::lock_for_update`), exactly as
    /// [`Self::delete_file_collecting_versions`] does, before any of the
    /// three orphan checks run. This used to be guarded by
    /// [`crate::infra::storage::repo::FileRepo::delete_if_orphan`] alone -- a
    /// single conditional `DELETE` whose own `NOT EXISTS` subquery is
    /// evaluated against the snapshot at the start of that statement. Under
    /// `READ COMMITTED`, a concurrent `insert_pending_version` that commits
    /// *after* that snapshot but *before* the `DELETE` actually runs is
    /// invisible to the subquery; the FK it takes on the parent row (`FOR
    /// KEY SHARE`) made the `DELETE` wait for it, but once the inserter
    /// committed, PostgreSQL resumed without an `EvalPlanQual` re-check and
    /// the stale `NOT EXISTS` verdict stood -- `ON DELETE CASCADE` then
    /// removed the freshly-inserted version along with the file, and the
    /// insert's own caller was never told. Locking the row first replaces
    /// that single embedded check with separate, ordinary reads taken
    /// *after* the lock is held: a racing insert either committed before the
    /// lock (and is therefore visible to these reads, correctly aborting the
    /// reclaim) or blocks until this transaction ends and then fails its own
    /// FK check (mapped to `FileNotFound`). Either way the version half of
    /// the race is closed the same way the delete-vs-insert race above is.
    ///
    /// `delete_if_orphan`'s own `content_id IS NULL` + `NOT EXISTS` guard is
    /// kept as the actual `DELETE` statement -- a second, redundant line of
    /// defense now that the three checks above have already made the
    /// decision, not the sole guard.
    ///
    /// Returns `true` if the file row was removed; `false` if a check did
    /// not pass (a version now exists, content is bound, or a multipart
    /// session is active) or the row was already gone (e.g. a concurrent
    /// sweep).
    pub async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let multipart = self.repos.multipart.clone();
        let retention_rules = self.repos.retention_rules.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let db = self.db.db();
        // Retryable: `lock_for_update` now takes `files` as the very first
        // statement, the same exposure as `delete_file`/
        // `delete_file_collecting_versions` against `finalize_version`'s
        // reversed lock order.
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let versions = versions.clone();
            let multipart = multipart.clone();
            let retention_rules = retention_rules.clone();
            let audit_repo = audit_repo.clone();
            let events_repo = events_repo.clone();
            let audit = audit.clone();
            let event = event.clone();
            Box::pin(async move {
                let scope = AccessScope::allow_all();
                // First statement: lock the parent row -- see this method's
                // doc comment.
                let Some(locked) = files.lock_for_update(tx, &scope, file_id).await? else {
                    return Ok::<bool, DomainError>(false); // already gone
                };
                if locked.content_id.is_some() {
                    return Ok(false); // content bound concurrently -- not an orphan
                }
                // `LIMIT 1`: existence, not a count -- same reasoning as
                // `MultipartRepo::has_active_for_file`'s own doc comment.
                let has_version = !versions
                    .list_by_file(tx, &scope, file_id, 1, 0)
                    .await?
                    .is_empty();
                if has_version {
                    return Ok(false);
                }
                if multipart.has_active_for_file(tx, file_id).await? {
                    return Ok(false);
                }

                let removed = files.delete_if_orphan(tx, &scope, file_id).await? > 0;
                if removed {
                    // See `delete_file_collecting_versions`'s matching call
                    // for why this has no FK to lean on instead.
                    retention_rules
                        .delete_file_scope_rules(tx, &scope, file_id)
                        .await?;
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
