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
use crate::domain::ports::{AutoBindOnFinalize, FinalizeVersionOutcome};
use crate::infra::content::hash_mode::HashMode;
use crate::infra::storage::db::db_err;
use crate::infra::storage::store::{Store, pending_version};

/// Sentinel "no limit" passed to [`crate::infra::storage::repo::VersionRepo::list_by_file`]
/// by callers that must see a file's **complete** version set (cascade
/// delete, backend migration, ownership-transfer usage accounting, and the
/// retention/orphan-reconciliation sweeps — see [`Store::list_versions`]).
/// Capping any of those at a page size would silently under-delete backend
/// blobs or under/over-count usage bytes, so they stay unbounded; only the
/// REST-facing [`Store::list_versions_page`] is capped (P2 2.2).
///
/// Kept within `i64::MAX` (rather than `u64::MAX`) so it binds safely as a
/// SQL `LIMIT` literal on every backend — `LIMIT`/`OFFSET` are signed 64-bit
/// on SQLite and Postgres, and a `u64::MAX` literal overflows that.
const UNBOUNDED_VERSIONS: u64 = i64::MAX as u64;

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
    /// [`Self::list_versions_page`] (P2 2.2).
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
    /// `GET /files/{id}/versions` (P2 2.2). `limit`/`offset` are expected to
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
    /// `auto_bind` (upload-flow redesign): when `Some`, the finalized version
    /// is additionally bound as the file's current content — the same CAS +
    /// current-flag promotion as [`Self::bind_atomic_with_event`], executed
    /// **inside this same transaction**. A lost CAS is reported via
    /// [`FinalizeVersionOutcome::bound`], never an error: the version stays
    /// `available` and can be rebound manually without a re-upload.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
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
        // @cpt-begin:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-commit-or-rollback
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
                    let mut bound = false;
                    if updated {
                        // Persist the manifest row transactionally with the
                        // version update for multipart-composite completions.
                        if let Some(manifest) = manifest {
                            versions
                                .insert_manifest(tx, &scope, version_id, &manifest, now)
                                .await?;
                        }
                        // @cpt-cf-file-storage-nfr-audit-completeness
                        // @cpt-begin:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-insert-same-tx
                        audit_repo.insert(tx, &audit).await?;
                        // @cpt-end:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-insert-same-tx

                        // Upload-flow redesign: bind in the same transaction
                        // (mirrors `bind_atomic_with_event`'s steps 1:1).
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
                                versions.set_current(tx, &scope, file_id, version_id).await?;
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
        // @cpt-end:cpt-cf-file-storage-flow-audit-trail-record-write:p1:inst-audit-commit-or-rollback
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
    /// is the file's current version (P2 2.7 — a version cannot be deleted
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
                    // @cpt-cf-file-storage-nfr-audit-completeness
                    audit_repo.insert(tx, &audit).await?;
                    Ok(true)
                })
            })
            .await
    }

    /// Delete a single version row iff it is still `pending`, recording an
    /// audit row in the same transaction. Returns `true` if a row was removed.
    ///
    /// Status-guarded CAS (P2 0.3 step 5) -- used by the cleanup sweep instead
    /// of the unconditional [`Self::delete_version`] when reclaiming an
    /// expired multipart session's pending version row, so a version that a
    /// racing `complete_multipart_upload` has already flipped to `available`
    /// is never deleted.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
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
    /// conflict (caller maps to PreconditionFailed; REST maps that canonical
    /// error to HTTP 400).
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
    /// CAS-gated on `expected_backend_id`/`expected_backend_path`, and write a
    /// `BackendMigrate` audit row in the same transaction.
    ///
    /// Returns `true` if the version row matched the expected pointer and was
    /// updated. `false` means either the version is gone or a concurrent
    /// migration already moved the pointer away from the expected value —
    /// the caller must re-fetch to tell these apart.
    ///
    /// @cpt-cf-file-storage-fr-backend-migration
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
