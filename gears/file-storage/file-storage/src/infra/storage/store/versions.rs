//! Version-level queries and mutating operations, including the bind and ownership-transfer
//! transactions.

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, TxConfig};
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, FileVersion, VersionStatus};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::domain::error_convert::db_err;
use crate::domain::etag;
use crate::domain::multipart::{BindDecision, MultipartUploadState, StoredCompleteResult};
use crate::domain::ports::{
    AutoBindOnFinalize, FinalizeMultipartOutcome, FinalizeVersionOutcome, MultipartFinishSnapshot,
};
use crate::infra::content::hash_mode::HashMode;
use crate::infra::storage::repo::Repos;
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

    /// List a page of a file's versions, newest first, keyset-paginated in either direction
    /// (backs `GET /files/{id}/versions`). `limit` is already clamped by the caller; `cursor`
    /// is validated against `versions_binding`, so a cursor from a different file is rejected
    /// with `400`.
    ///
    /// Fetches `limit + 1` rows to detect a further page (no `COUNT`); `finish_page` trims,
    /// restores canonical order and builds both cursors.
    pub async fn list_versions_page(
        &self,
        file_id: Uuid,
        limit: u64,
        cursor: Option<toolkit_odata::CursorV1>,
    ) -> Result<toolkit_odata::Page<FileVersion>, DomainError> {
        use crate::domain::pagination;

        let binding = pagination::versions_binding(file_id);
        let after = cursor
            .map(|cursor| {
                pagination::seek(cursor, pagination::VERSIONS_ID_FIELD, binding.as_deref())
            })
            .transpose()?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        let rows = self
            .repos
            .versions
            .list_by_file_page(
                &conn,
                &AccessScope::allow_all(),
                file_id,
                limit.saturating_add(1),
                after,
            )
            .await?;

        Ok(pagination::finish_page(
            rows,
            limit,
            after,
            pagination::VERSIONS_ID_FIELD,
            binding.as_deref(),
            |v| (v.created_at, v.version_id),
        )?)
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
    /// one transaction; `updated` reports whether the version row existed and was updated.
    ///
    /// `mime_type` is the sniffed content type to persist; `None` keeps the declared
    /// one (multipart complete does not validate MIME). `hash_mode`/`part_count` are
    /// set here; for `multipart-composite-sha256`, `manifest` is inserted in the same
    /// transaction.
    ///
    /// `auto_bind`: also binds the version as current content in this transaction (same CAS and
    /// promotion as `bind_atomic_with_event`); a lost CAS is reported via `bound`, not an error.
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
        auto_bind: Option<AutoBindOnFinalize>,
    ) -> Result<FinalizeVersionOutcome, DomainError> {
        let repos = self.repos.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let hash_mode_str = hash_mode.as_str();
        let now = OffsetDateTime::now_utc();
        // Retryable: auto-bind locks `file_versions` then `files`, a file delete the opposite; can deadlock.
        self.db
            .db()
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let repos = repos.clone();
                let versions = versions.clone();
                let audit_repo = audit_repo.clone();
                let hash_value = hash_value.clone();
                let mime_type = mime_type.clone();
                let manifest = manifest.clone();
                let audit = audit.clone();
                let auto_bind = auto_bind.clone();
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
                    let bound = if updated {
                        // Manifest and version row commit atomically.
                        if let Some(manifest) = manifest {
                            versions
                                .insert_manifest(tx, &scope, version_id, &manifest, now)
                                .await?;
                        }
                        audit_repo.insert(tx, &audit).await?;

                        auto_bind_in_tx(&repos, tx, file_id, version_id, auto_bind, now).await?
                    } else {
                        false
                    };
                    Ok::<FinalizeVersionOutcome, DomainError>(FinalizeVersionOutcome {
                        updated,
                        bound,
                    })
                })
            })
            .await
    }

    /// Multipart counterpart of `finalize_version`: finalize + auto-bind CAS + session
    /// `completing -> completed` + snapshot, all in ONE transaction.
    ///
    /// The session row is locked first and its `state` re-checked: the finalize CAS is fenced only
    /// by `status = 'pending'`, so a stale completer whose session cleanup already aborted could
    /// otherwise finalize and bind. A vanished row or `aborted` fails with `DomainError::conflict`;
    /// `completed` and `in_progress` are tolerated (the version CAS reports `updated: false` / the
    /// owner-`None` finish arm closes the session).
    pub async fn finalize_multipart_version(
        &self,
        file_id: Uuid,
        manifest: Option<String>,
        mime_type: Option<String>,
        finalize_audit: AuditEntry,
        auto_bind: Option<AutoBindOnFinalize>,
        finish: MultipartFinishSnapshot,
    ) -> Result<FinalizeMultipartOutcome, DomainError> {
        let repos = self.repos.clone();
        let hash_mode_str = finish.hash_mode.as_str();
        let auto_bind_requested = auto_bind.is_some();
        let now = OffsetDateTime::now_utc();
        // Retryable: lock order `multipart_uploads` -> `file_versions` -> `files`; only the latter half can deadlock.
        self.db
            .db()
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let repos = repos.clone();
                let manifest = manifest.clone();
                let mime_type = mime_type.clone();
                let finalize_audit = finalize_audit.clone();
                let auto_bind = auto_bind.clone();
                let finish = finish.clone();
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    let session_state = repos
                        .multipart
                        .lock_session_state(tx, finish.upload_id)
                        .await?;
                    let reclaimed_by_cleanup = match session_state.as_deref() {
                        Some(state) => state == MultipartUploadState::Aborted.as_str(),
                        None => true,
                    };
                    if reclaimed_by_cleanup {
                        return Err(DomainError::conflict(
                            crate::domain::error::MULTIPART_SESSION_RECLAIMED_BY_CLEANUP_MESSAGE,
                        ));
                    }
                    let updated = repos
                        .versions
                        .finalize(
                            tx,
                            &scope,
                            file_id,
                            finish.version_id,
                            finish.size,
                            finish.content_hash.clone(),
                            hash_mode_str,
                            finish.part_count,
                            mime_type,
                        )
                        .await?;
                    if !updated {
                        return Ok::<FinalizeMultipartOutcome, DomainError>(
                            FinalizeMultipartOutcome {
                                updated: false,
                                bound: false,
                                session_completed: false,
                                current_etag: None,
                            },
                        );
                    }

                    if let Some(manifest) = manifest {
                        repos
                            .versions
                            .insert_manifest(tx, &scope, finish.version_id, &manifest, now)
                            .await?;
                    }
                    repos.audit.insert(tx, &finalize_audit).await?;

                    let bound =
                        auto_bind_in_tx(&repos, tx, file_id, finish.version_id, auto_bind, now)
                            .await?;

                    let decision = if bound {
                        BindDecision::Bound
                    } else if auto_bind_requested {
                        let fresh =
                            repos.files.get(tx, &scope, file_id).await?.ok_or_else(|| {
                                DomainError::database(
                                    "file row missing during multipart finalize's fresh-etag read",
                                )
                            })?;
                        BindDecision::Conflict {
                            current_etag: etag::etag_for(&fresh),
                        }
                    } else {
                        BindDecision::Manual
                    };
                    let (bind_state, result_etag, current_etag) =
                        decision.outcome(file_id, finish.version_id);

                    let stored = StoredCompleteResult {
                        version_id: finish.version_id,
                        size: finish.size,
                        content_hash: hex::encode(&finish.content_hash),
                        hash_mode: hash_mode_str.to_owned(),
                        part_count: finish.part_count.unwrap_or(1),
                        bind_state: bind_state.as_str().to_owned(),
                        etag: result_etag,
                        current_etag: current_etag.clone(),
                    };
                    let result_json = serde_json::to_string(&stored).map_err(|_| {
                        DomainError::database("failed to serialize complete result")
                    })?;

                    // Owner `None`: the won finalize CAS proves authorship, and an owner check would re-strand a
                    // stale completer; `in_progress` is accepted too (see `MultipartRepo::finish_complete`).
                    let session_completed = repos
                        .multipart
                        .finish_complete(tx, finish.upload_id, None, &result_json)
                        .await?;
                    if session_completed {
                        repos.audit.insert(tx, &finish.session_audit).await?;
                    }

                    Ok(FinalizeMultipartOutcome {
                        updated,
                        bound,
                        session_completed,
                        current_etag,
                    })
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

    pub async fn get_version_manifests(
        &self,
        version_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repos
            .versions
            .get_manifests(&conn, &AccessScope::allow_all(), version_ids)
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
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let versions = versions.clone();
                let audit_repo = audit_repo.clone();
                let audit = audit.clone();
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
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let versions = versions.clone();
                let audit_repo = audit_repo.clone();
                let audit = audit.clone();
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
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let files = files.clone();
                let versions = versions.clone();
                let audit_repo = audit_repo.clone();
                let bind_scope = bind_scope.clone();
                let audit = audit.clone();
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
                        .promote_current(tx, &AccessScope::allow_all(), file_id, version_id)
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
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let files = files.clone();
                let versions = versions.clone();
                let audit_repo = audit_repo.clone();
                let events_repo = events_repo.clone();
                let bind_scope = bind_scope.clone();
                let audit = audit.clone();
                let event = event.clone();
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
                        .promote_current(tx, &AccessScope::allow_all(), file_id, version_id)
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
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let versions = versions.clone();
                let audit_repo = audit_repo.clone();
                let expected_backend_id = expected_backend_id.clone();
                let expected_backend_path = expected_backend_path.clone();
                let new_backend_id = new_backend_id.clone();
                let new_backend_path = new_backend_path.clone();
                let audit = audit.clone();
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
            .transaction_with_retry(TxConfig::default(), db_err, move |tx| {
                let files = files.clone();
                let audit_repo = audit_repo.clone();
                let events_repo = events_repo.clone();
                let transfer_scope = transfer_scope.clone();
                let new_owner_kind = new_owner_kind.clone();
                let audit = audit.clone();
                let event = event.clone();
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

/// Auto-bind tail of a finalize transaction: the first-bind CAS and, if it wins, promotion, the
/// `bound_on_finalize` marker, audit and event. `None` or a lost CAS yields `false` (not an error).
async fn auto_bind_in_tx<C: DBRunner>(
    repos: &Repos,
    tx: &C,
    file_id: Uuid,
    version_id: Uuid,
    auto_bind: Option<AutoBindOnFinalize>,
    now: OffsetDateTime,
) -> Result<bool, DomainError> {
    let Some(ab) = auto_bind else {
        return Ok(false);
    };
    let scope = AccessScope::allow_all();
    let swapped = repos
        .files
        .bind_content_cas(tx, &scope, file_id, ab.expected_content_id, version_id, now)
        .await?;
    if swapped {
        repos
            .versions
            .promote_current(tx, &scope, file_id, version_id)
            .await?;
        repos
            .versions
            .mark_bound_on_finalize(tx, &scope, file_id, version_id)
            .await?;
        repos.audit.insert(tx, &ab.audit).await?;
        if let Some(ev) = ab.event {
            repos.events_outbox.enqueue(tx, &ev).await?;
        }
    }
    Ok(swapped)
}
