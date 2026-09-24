//! Version-level queries and mutating operations.
//!
//! Covers: insert_pending_version, get_version, list_versions,
//! current_version_mime, finalize_version, delete_version,
//! rebind_version_backend, bind_atomic (+ events variant),
//! transfer_ownership_atomic.

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, FileVersion, VersionStatus};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::{BindState, StoredCompleteResult};
use crate::domain::ports::{
    AutoBindOnFinalize, DeleteVersionOutcome, FinalizeMultipartOutcome, FinalizeVersionOutcome,
    MultipartFinishSnapshot,
};
use crate::infra::content::hash_mode::HashMode;
use crate::infra::storage::db::{db_err, transaction_with_bounded_retry};
use crate::infra::storage::store::{Store, pending_version};

/// Sentinel "no limit" passed to [`crate::infra::storage::repo::VersionRepo::list_by_file`]
/// by callers that must see a file's **complete** version set (cascade
/// delete, backend migration, ownership-transfer usage accounting, and the
/// retention/orphan-reconciliation sweeps — see [`Store::list_versions`]).
/// Capping any of those at a page size would silently under-delete backend
/// blobs or under/over-count usage bytes, so they stay unbounded; only the
/// REST-facing [`Store::list_versions_page`] is capped.
///
/// Kept within `i64::MAX` (rather than `u64::MAX`) so it binds safely as a
/// SQL `LIMIT` literal on every backend — `LIMIT`/`OFFSET` are signed 64-bit
/// on SQLite and Postgres, and a `u64::MAX` literal overflows that.
///
/// `pub(super)`: also used by `store::files::delete_file_collecting_versions`,
/// which needs the same "give me every version, no page cap" read inside its
/// own transaction.
pub(super) const UNBOUNDED_VERSIONS: u64 = i64::MAX as u64;

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

    /// List **all** versions of a file, newest first — internal/unbounded,
    /// for callers that need the complete version set (see
    /// [`UNBOUNDED_VERSIONS`]). The paginated, REST-facing counterpart is
    /// [`Self::list_versions_page`].
    pub async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
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

    /// List a page of a file's versions, newest first — backs
    /// `GET /files/{id}/versions`. `limit`/`offset` are expected to
    /// already be clamped by the caller (see
    /// `FileService::list_versions`/`ServiceConfig::max_page_size`).
    pub async fn list_versions_page(
        &self,
        file_id: Uuid,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .list_by_file(&conn, &AccessScope::allow_all(), file_id, limit, offset)
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
    /// `mime_type` is the validated/sniffed content type to persist in place
    /// of the client's original declaration (see `mime::validate` at the
    /// finalize call sites); pass `None` to leave the declared type untouched
    /// (the multipart-complete path does not perform MIME validation).
    ///
    /// `hash_mode`/`part_count` (ADR-0006) are set here at finalize time. For
    /// a `multipart-composite-sha256` completion, `manifest` carries the
    /// canonical offset-manifest text (§3): its `version_hash_manifest` row is
    /// inserted in the **same transaction** as the version-row update, so a
    /// completed multipart version and its verification manifest are committed
    /// atomically. `whole-sha256` completions pass `part_count = None` /
    /// `manifest = None` and write no manifest row.
    ///
    /// An audit row is written in the same transaction.
    ///
    /// `auto_bind`: when `Some`, the finalized version is additionally bound
    /// as the file's current content — the same CAS +
    /// current-flag promotion as [`Self::bind_atomic_with_event`], executed
    /// **inside this same transaction**. A lost CAS is reported via
    /// [`FinalizeVersionOutcome::bound`], never an error: the version stays
    /// `available` and can be rebound manually without a re-upload.
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
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let hash_mode_str = hash_mode.as_str();
        let now = OffsetDateTime::now_utc();
        let db = self.db.db();
        //
        // Retryable (see `db::transaction_with_bounded_retry`): the auto-bind
        // branch below takes `file_versions` then `files` (`versions.finalize`
        // first, `files.bind_content_cas` after); `Store::delete_file[_with_event]`
        // take `files` then cascade into `file_versions` -- the reverse order.
        // Two transactions racing in opposite lock orders is a textbook
        // PostgreSQL deadlock (40P01), and without a retry the loser used to
        // surface as a plain 500. Every value the closure needs is cloned
        // per attempt (see that function's doc comment) rather than moved,
        // so a retried attempt starts from the same inputs as the first.
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let versions = versions.clone();
            let audit_repo = audit_repo.clone();
            let events_repo = events_repo.clone();
            let hash_value = hash_value.clone();
            let manifest = manifest.clone();
            let mime_type = mime_type.clone();
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
                let mut bound = false;
                if updated {
                    // Persist the manifest row transactionally with the
                    // version update for multipart-composite completions.
                    if let Some(manifest) = manifest {
                        versions
                            .insert_manifest(tx, &scope, version_id, &manifest, now)
                            .await?;
                    }
                    audit_repo.insert(tx, &audit).await?;

                    // Bind in the same transaction (mirrors
                    // `bind_atomic_with_event`'s steps 1:1).
                    if let Some(ab) = auto_bind {
                        let swapped = files
                            .bind_content_cas(
                                tx,
                                &scope,
                                file_id,
                                ab.expected_content_id,
                                version_id,
                                now,
                            )
                            .await?;
                        if swapped {
                            versions.clear_current(tx, &scope, file_id).await?;
                            // Same guard as `bind_atomic`/`bind_atomic_with_event`:
                            // a concurrent `delete_version` can remove this
                            // exact version between the CAS above and this
                            // promotion (see `VersionRepo::set_current`'s doc
                            // comment). Abort rather than commit a
                            // `files.content_id` left pointing at a deleted row.
                            let promoted = versions
                                .set_current(tx, &scope, file_id, version_id)
                                .await?;
                            if promoted == 0 {
                                return Err(DomainError::conflict(
                                    "target version no longer exists -- it was deleted concurrently",
                                ));
                            }
                            audit_repo.insert(tx, &ab.audit).await?;
                            if let Some(ev) = ab.event {
                                events_repo.enqueue(tx, &ev).await?;
                            }
                        }
                        bound = swapped;
                    }
                }
                Ok::<FinalizeVersionOutcome, DomainError>(FinalizeVersionOutcome {
                    updated,
                    bound,
                })
            })
        })
        .await
    }

    /// Multipart-completion counterpart of [`Self::finalize_version`]: the
    /// same finalize + auto-bind-CAS transaction, but also transitions the
    /// session `completing → completed` and persists the `complete_result`
    /// snapshot in that SAME transaction (see
    /// [`crate::domain::ports::MultipartStore::finalize_multipart_version`]'s
    /// doc for why this closes the crash gap the old two-transaction
    /// sequence left open).
    ///
    /// `bind_state`/`etag`/`current_etag` cannot be supplied by the caller up
    /// front -- they depend on whether the auto-bind CAS (run inside this
    /// same transaction, moments earlier) won, and, on a lost CAS, on a
    /// fresh read of the file's pointer that must itself happen inside this
    /// transaction to observe exactly what this transaction committed,
    /// never a later racing rebind. So they are derived here, mirroring
    /// `MultipartService::bind_state_for`'s model exactly, instead of being
    /// passed in via [`MultipartFinishSnapshot`].
    pub async fn finalize_multipart_version(
        &self,
        file_id: Uuid,
        manifest: Option<String>,
        mime_type: Option<String>,
        finalize_audit: AuditEntry,
        auto_bind: Option<AutoBindOnFinalize>,
        finish: MultipartFinishSnapshot,
    ) -> Result<FinalizeMultipartOutcome, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let multipart = self.repos.multipart.clone();
        let hash_mode_str = finish.hash_mode.as_str();
        let auto_bind_requested = auto_bind.is_some();
        let now = OffsetDateTime::now_utc();
        let db = self.db.db();
        // Retryable for the same cross-transaction lock-order reason
        // `finalize_version` documents (this is the same transaction body,
        // with the terminal session CAS appended).
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let versions = versions.clone();
            let audit_repo = audit_repo.clone();
            let events_repo = events_repo.clone();
            let multipart = multipart.clone();
            let manifest = manifest.clone();
            let mime_type = mime_type.clone();
            let finalize_audit = finalize_audit.clone();
            let auto_bind = auto_bind.clone();
            let finish = finish.clone();
            Box::pin(async move {
                let scope = AccessScope::allow_all();
                let updated = versions
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
                    // Nothing to finish -- the caller's own `!updated` branch
                    // (lost finalize CAS) handles this exactly as before this
                    // method existed (`converge_or_error_after_lost_finalize_cas`).
                    return Ok::<FinalizeMultipartOutcome, DomainError>(FinalizeMultipartOutcome {
                        updated: false,
                        bound: false,
                        session_completed: false,
                        current_etag: None,
                    });
                }

                if let Some(manifest) = manifest {
                    versions
                        .insert_manifest(tx, &scope, finish.version_id, &manifest, now)
                        .await?;
                }
                audit_repo.insert(tx, &finalize_audit).await?;

                let bound = if let Some(ab) = auto_bind {
                    let swapped = files
                        .bind_content_cas(
                            tx,
                            &scope,
                            file_id,
                            ab.expected_content_id,
                            finish.version_id,
                            now,
                        )
                        .await?;
                    if swapped {
                        versions.clear_current(tx, &scope, file_id).await?;
                        // Same guard as `finalize_version`'s own auto-bind
                        // branch: abort rather than commit a dangling
                        // `files.content_id` if the version was deleted
                        // concurrently.
                        let promoted = versions
                            .set_current(tx, &scope, file_id, finish.version_id)
                            .await?;
                        if promoted == 0 {
                            return Err(DomainError::conflict(
                                "target version no longer exists -- it was deleted concurrently",
                            ));
                        }
                        audit_repo.insert(tx, &ab.audit).await?;
                        if let Some(ev) = ab.event {
                            events_repo.enqueue(tx, &ev).await?;
                        }
                    }
                    swapped
                } else {
                    false
                };

                // Same bind-state model as `MultipartService::bind_state_for`,
                // computed here (inside this transaction) instead of by the
                // caller after it returns -- see this method's doc comment.
                let (bind_state, result_etag, current_etag) = if bound {
                    (
                        BindState::Bound,
                        Some(etag::content_etag(file_id, finish.version_id)),
                        None,
                    )
                } else if auto_bind_requested {
                    let fresh = files.get(tx, &scope, file_id).await?.ok_or_else(|| {
                        DomainError::database(
                            "file row missing during multipart finalize's fresh-etag read",
                        )
                    })?;
                    (BindState::Conflict, None, etag::etag_for(&fresh))
                } else {
                    (BindState::Manual, None, None)
                };

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
                let result_json = serde_json::to_string(&stored)
                    .map_err(|_| DomainError::database("failed to serialize complete result"))?;

                // `None`: this call is made in the SAME transaction as --
                // immediately after -- this same call's own just-WON
                // finalize CAS above, which is itself deliberately
                // owner-blind (fenced only by `status = 'pending'`). See
                // `MultipartRepo::finish_complete`'s doc for why that makes
                // an owner check here both unnecessary (the finalize CAS
                // already proves unique, legitimate authorship) and unsafe
                // (it would re-strand the exact race
                // `f2_stale_completer_converges_instead_of_stranding_after_
                // owner_fencing_fix` exists to prevent).
                let session_completed = multipart
                    .finish_complete(tx, finish.upload_id, None, &result_json)
                    .await?;
                if session_completed {
                    audit_repo.insert(tx, &finish.session_audit).await?;
                }

                // `current_etag` is handed back to the caller too (not just
                // persisted in `stored`/`result_json`) so it can build the
                // SAME live response it returns to the client from this
                // transaction's own in-flight decision, instead of a second,
                // post-commit read that could race a legitimate concurrent
                // rebind -- see `FinalizeMultipartOutcome::current_etag`'s
                // doc comment.
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

    /// Fetch the `version_hash_manifest` text for a version, if one exists
    /// (`multipart-composite-sha256` versions only). Backs mode-aware
    /// re-verification in `migrate_backend`.
    pub async fn get_version_manifest(
        &self,
        version_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .get_manifest(&conn, &AccessScope::allow_all(), version_id)
            .await
    }

    /// Batched counterpart of [`Self::get_version_manifest`]: fetch the
    /// manifest text for a page of versions in one `IN (...)` query, keyed by
    /// `version_id`. `GET /files/{id}/versions` uses this instead of calling
    /// `get_version_manifest` once per version (an N+1 query pattern that
    /// would scale with the page size). Versions without a manifest row
    /// (`whole-sha256`) are simply absent from the map.
    pub async fn get_version_manifests(
        &self,
        version_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .get_manifests(&conn, &AccessScope::allow_all(), version_ids)
            .await
    }

    /// Delete a single version row and record an audit row in the same
    /// transaction.
    ///
    /// Returns `true` if the row was removed, `false` if it does not exist or
    /// is the file's current version -- a version cannot be deleted
    /// while it is current, whether that was already true when the caller
    /// checked or became true concurrently between the caller's check and
    /// this call).
    ///
    /// The "is this the current version?" check is re-read **inside** this
    /// transaction (`versions.get`) rather than trusted from a pre-transaction
    /// snapshot, and [`crate::infra::storage::repo::VersionRepo::delete`]'s own
    /// predicate additionally guards `is_current = false` at the DB level —
    /// so even a concurrent `bind` that commits between the read below and the
    /// delete statement cannot leave `files.content_id` dangling: the delete
    /// simply removes 0 rows and this returns `false`.
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
                    // Transactional re-read: `is_current` mirrors
                    // `files.content_id` (both flip together in `bind_atomic`'s
                    // transaction), so this is equivalent to re-checking
                    // `content_id == version_id` without a second query against
                    // `files`.
                    let Some(existing) = versions.get(tx, &scope, file_id, version_id).await?
                    else {
                        return Ok::<bool, DomainError>(false);
                    };
                    if existing.is_current {
                        return Ok(false);
                    }
                    let rows_affected = versions.delete(tx, &scope, file_id, version_id).await?;
                    if rows_affected == 0 {
                        // Raced: a concurrent bind promoted this version to
                        // current between the read above and the delete
                        // statement — the DB-level guard in `VersionRepo::delete`
                        // caught it.
                        return Ok(false);
                    }
                    audit_repo.insert(tx, &audit).await?;
                    Ok(true)
                })
            })
            .await
    }

    /// Delete a single version row iff it is still `pending`, recording an
    /// audit row in the same transaction. Returns `true` if a row was removed.
    ///
    /// Status-guarded CAS -- used by the cleanup sweep instead
    /// of the unconditional [`Self::delete_version`] when reclaiming an
    /// expired multipart session's pending version row, so a version that a
    /// racing `complete_multipart_upload` has already flipped to `available`
    /// is never deleted.
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

    /// Delete `version_id`, or -- if it is the file's only version -- delete
    /// the whole file, deciding which **inside one transaction**.
    ///
    /// # Why the version count is re-read here, not trusted from the caller
    ///
    /// The caller used to decide "last version -> delete the whole file" from
    /// a `list_versions` snapshot taken before opening any transaction, then
    /// either delegate to a whole-file delete or to [`Self::delete_version`]
    /// based on that stale count. A version inserted by a concurrent
    /// `presign_version`/`initiate_multipart_upload` on this exact `file_id`,
    /// any time between that read and the eventual `DELETE`, was invisible to
    /// the snapshot: the whole-file branch would still fire, cascade-removing
    /// the new version along with the file even though the caller only ever
    /// asked to delete one specific, different version. Re-listing the
    /// file's versions immediately before whichever delete the count
    /// implies, instead of trusting the caller's entire pre-transaction call
    /// chain, used to only narrow that gap to the width of this
    /// transaction -- a concurrent insert could still land between the list
    /// and the delete statement inside this same transaction, on
    /// `PostgreSQL`'s `READ COMMITTED`. Locking the `files` row first (see
    /// the "Row lock" section below) closes that remaining gap too: the
    /// version list is now read only after the lock is held, so nothing can
    /// commit into it unseen between the list and the delete.
    ///
    /// Returns [`DeleteVersionOutcome::FileRemoved`] when `version_id` turns
    /// out (inside this transaction) to be the file's only version --
    /// mirroring `FileService::delete_file_inner`'s existing whole-file
    /// audit/event shape, via `file_audit`/`file_event` -- or
    /// [`DeleteVersionOutcome::VersionRemoved`] via `version_audit` when
    /// other versions remain. [`DeleteVersionOutcome::IsCurrent`] covers both
    /// "was already current when read" and "a concurrent bind promoted it to
    /// current between this read and the delete statement" (the latter
    /// caught by [`crate::infra::storage::repo::VersionRepo::delete`]'s own
    /// `is_current = false` guard, same as plain [`Self::delete_version`]).
    ///
    /// # Row lock closes the whole-file-delete-vs-insert race
    ///
    /// The transaction's first statement locks the `files` row
    /// (`FileRepo::lock_for_update`) before the version list is read, for
    /// the same reason as [`Self::delete_file_collecting_versions`]: a
    /// concurrent `insert_pending_version` on this `file_id` either commits
    /// before this lock (and is then visible to the fresh `list_by_file`
    /// read that follows, so the "is this the only version?" decision below
    /// sees it and takes the `VersionRemoved` branch instead of
    /// `FileRemoved`) or blocks until this transaction ends and then fails
    /// its own FK check once the whole-file branch actually removes the row
    /// (mapped to `FileNotFound`). This closes the gap even on the
    /// single-version-remaining branch, which previously never touched
    /// `files` at all -- see `docs/toolkit_unified_system/11_database_patterns.md`'s
    /// "Row locks" section.
    #[allow(clippy::too_many_arguments)]
    pub async fn delete_version_or_whole_file(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        version_audit: AuditEntry,
        file_audit: AuditEntry,
        file_event: Option<FileEvent>,
    ) -> Result<DeleteVersionOutcome, DomainError> {
        let files = self.repos.files.clone();
        let versions = self.repos.versions.clone();
        let audit_repo = self.repos.audit.clone();
        let events_repo = self.repos.events_outbox.clone();
        let db = self.db.db();
        // Retryable: the `FileRemoved` branch takes `files` then cascades
        // into `file_versions`, the same lock order (and the same
        // cross-transaction deadlock exposure against `finalize_version`'s
        // auto-bind branch) as `delete_file_collecting_versions` -- see that
        // method's comment and `db::transaction_with_bounded_retry`'s doc
        // comment for the retry contract.
        transaction_with_bounded_retry(&db, move |tx| {
            let files = files.clone();
            let versions = versions.clone();
            let audit_repo = audit_repo.clone();
            let events_repo = events_repo.clone();
            let version_audit = version_audit.clone();
            let file_audit = file_audit.clone();
            let file_event = file_event.clone();
            Box::pin(async move {
                let scope = AccessScope::allow_all();
                // First statement: lock the parent row -- see this method's
                // doc comment. `None` means the file is already gone.
                if files.lock_for_update(tx, &scope, file_id).await?.is_none() {
                    return Ok(DeleteVersionOutcome::NotFound);
                }

                // Fresh, in-transaction snapshot -- see this method's doc
                // comment for the race this closes.
                let all = versions
                    .list_by_file(tx, &scope, file_id, UNBOUNDED_VERSIONS, 0)
                    .await?;
                let Some(target) = all.iter().find(|v| v.version_id == version_id).cloned() else {
                    return Ok(DeleteVersionOutcome::NotFound);
                };

                if all.len() == 1 {
                    // `target` is the file's only version (confirmed by the
                    // `find` above) -- delete the whole file in this same
                    // transaction/snapshot.
                    let removed = files.delete(tx, &scope, file_id).await?;
                    if !removed {
                        // Already gone -- a concurrent delete/expiry won
                        // this race between the list above and this
                        // statement.
                        return Ok(DeleteVersionOutcome::NotFound);
                    }
                    audit_repo.insert(tx, &file_audit).await?;
                    if let Some(ev) = file_event {
                        events_repo.enqueue(tx, &ev).await?;
                    }
                    return Ok(DeleteVersionOutcome::FileRemoved(target));
                }

                if target.is_current {
                    return Ok(DeleteVersionOutcome::IsCurrent);
                }
                let rows_affected = versions.delete(tx, &scope, file_id, version_id).await?;
                if rows_affected == 0 {
                    // Raced, in the tiny window between the list above and
                    // this delete statement (still inside one transaction):
                    // either a concurrent bind promoted this version to
                    // current (`VersionRepo::delete`'s own `is_current =
                    // false` guard caught it) or a concurrent delete already
                    // removed it outright. Re-check inside this same
                    // transaction to tell them apart, exactly as the old
                    // two-call version of this decision used to
                    // re-fetch post-transaction -- except this re-check
                    // cannot itself be raced any further, since it runs
                    // inside the same still-open transaction.
                    return Ok(match versions.get(tx, &scope, file_id, version_id).await? {
                        Some(_) => DeleteVersionOutcome::IsCurrent,
                        None => DeleteVersionOutcome::NotFound,
                    });
                }
                audit_repo.insert(tx, &version_audit).await?;
                Ok(DeleteVersionOutcome::VersionRemoved(target))
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
    /// conflict (caller maps to PreconditionFailed; REST maps that canonical
    /// error to HTTP 400).
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
        let db = self.db.db();
        // Retryable: this transaction locks `files` then `file_versions`
        // (`bind_content_cas` first, `clear_current`/`set_current` after),
        // the opposite order from `finalize_version`'s auto-bind branch
        // (`file_versions` then `files`) -- the same cross-transaction
        // deadlock risk `finalize_version` documents, seen from the other
        // side. See `db::transaction_with_bounded_retry` for the retry
        // contract and why every captured value is cloned per attempt.
        transaction_with_bounded_retry(&db, move |tx| {
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
                // Promote the new version as current (unique-current index honoured).
                versions
                    .clear_current(tx, &AccessScope::allow_all(), file_id)
                    .await?;
                // `set_current` returning 0 rows means `version_id` was
                // deleted by a concurrent `delete_version` between the CAS
                // above and this promotion (see `VersionRepo::set_current`'s
                // doc comment for the exact race) -- abort the transaction
                // rather than commit a `files.content_id` that points at a
                // version row which no longer exists.
                let promoted = versions
                    .set_current(tx, &AccessScope::allow_all(), file_id, version_id)
                    .await?;
                if promoted == 0 {
                    return Err(DomainError::conflict(
                        "target version no longer exists -- it was deleted concurrently",
                    ));
                }
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
        let db = self.db.db();
        // Retryable: same `files` -> `file_versions` lock order as
        // `bind_atomic`, and so the same deadlock exposure against
        // `finalize_version`'s reversed order -- see `bind_atomic`'s comment
        // and `db::transaction_with_bounded_retry`'s doc comment for the
        // retry contract and per-attempt cloning.
        transaction_with_bounded_retry(&db, move |tx| {
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
                    .clear_current(tx, &AccessScope::allow_all(), file_id)
                    .await?;
                // See `bind_atomic` and `VersionRepo::set_current`'s doc
                // comment: 0 rows means a concurrent `delete_version` won
                // the race for this version between the CAS and here, and
                // must abort the transaction rather than commit a dangling
                // `files.content_id`.
                let promoted = versions
                    .set_current(tx, &AccessScope::allow_all(), file_id, version_id)
                    .await?;
                if promoted == 0 {
                    return Err(DomainError::conflict(
                        "target version no longer exists -- it was deleted concurrently",
                    ));
                }
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
    /// CAS-gated on `expected_backend_id`/`expected_backend_path`, and write a
    /// `BackendMigrate` audit row in the same transaction.
    ///
    /// Returns `true` if the version row matched the expected pointer and was
    /// updated. `false` means either the version is gone or a concurrent
    /// migration already moved the pointer away from the expected value —
    /// the caller must re-fetch to tell these apart.
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

    // ── ownership transfer ──────────────────────────────────────────────────

    /// Update `owner_kind` + `owner_id` for a file, enqueue an optional event
    /// row, and record an audit row — all in one transaction.
    ///
    /// Returns `true` if the file row was found and updated.
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
