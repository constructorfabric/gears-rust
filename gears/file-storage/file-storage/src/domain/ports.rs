//! Domain-owned capability ports (ISP/DIP).
//!
//! Each trait names only the `Store` methods a specific consumer requires.
//! Consumers depend on `Arc<dyn XxxStore>` (or a generic bound); the concrete
//! `Store` type satisfies all of them via `impl` blocks in `infra/storage/store.rs`.
//!
//! Defining the traits here (in the domain layer) is the DIP move: the domain
//! owns the port; infrastructure (`Store`) implements it. Neither the cleanup
//! engine nor the multipart service imports `crate::infra::storage::Store`
//! directly — they name only this module.
//!
//! `async-trait` is used to match the crate's existing `Authorizer` convention.

use std::collections::HashMap;

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession};
use crate::domain::policy::{
    PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope, StoredPolicy, StoredRetentionRule,
};

/// Instruction to bind a just-finalized version as the file's current content
/// in the **same transaction** as the finalize itself (upload-flow redesign:
/// single-part `bind=auto` finalize, and multipart `complete` on an
/// `auto_bind` session).
///
/// `expected_content_id` is the optimistic-CAS precondition: the exact
/// current pointer the bind may replace (`None` = the file must still have no
/// content, i.e. `content_id IS NULL` — the first-content case). Semantically
/// identical to the CAS a manual `POST /files/{id}/bind` performs under
/// `If-Match` (PRD §5.10) — the precondition is resolved by the caller from
/// the same validated state, only the transport differs.
#[derive(Debug, Clone)]
pub struct AutoBindOnFinalize {
    /// CAS precondition: the exact `files.content_id` the swap may replace
    /// (`None` = must be `NULL`).
    pub expected_content_id: Option<Uuid>,
    /// Audit row for the bind step (in addition to the finalize audit row).
    pub audit: AuditEntry,
    /// Optional `file.content_updated` event, enqueued only when the CAS wins.
    pub event: Option<FileEvent>,
}

/// Result of [`MultipartStore::finalize_version`] / `Store::finalize_version`.
#[derive(Debug, Clone, Copy)]
pub struct FinalizeVersionOutcome {
    /// The version row existed, was `pending`, and is now `available`.
    pub updated: bool,
    /// The auto-bind CAS was requested and won (always `false` when no
    /// [`AutoBindOnFinalize`] was passed, or when `updated` is `false`).
    pub bound: bool,
}

/// Everything [`MultipartStore::finalize_multipart_version`] already knows
/// about the completion before the bind outcome is decided inside its
/// transaction -- every `StoredCompleteResult` field except
/// `bind_state`/`etag`/`current_etag`, which depend on `bound` (and, on a
/// lost auto-bind CAS, a same-transaction fresh read of the file's current
/// pointer) and so can only be resolved once the finalize step itself has
/// run.
#[derive(Debug, Clone)]
pub struct MultipartFinishSnapshot {
    pub upload_id: Uuid,
    pub version_id: Uuid,
    pub size: i64,
    /// Raw (not hex-encoded) content hash -- mirrors
    /// `CompletedMultipartUpload::content_hash`.
    pub content_hash: Vec<u8>,
    pub hash_mode: crate::infra::content::hash_mode::HashMode,
    /// `None` for the degenerate one-part plan (`whole-sha256` versions carry
    /// no `part_count` column) -- the persisted `StoredCompleteResult`'s own
    /// `part_count` still reports the true count (1) in that case; see its
    /// construction in `Store::finalize_multipart_version`.
    pub part_count: Option<i32>,
    /// The `MultipartComplete` audit row, written in the same transaction
    /// as the finalize iff the terminal `completing -> completed` CAS wins
    /// (see [`FinalizeMultipartOutcome::session_completed`]).
    pub session_audit: AuditEntry,
}

/// Result of [`MultipartStore::finalize_multipart_version`].
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct FinalizeMultipartOutcome {
    /// The version row existed, was `pending`, and is now `available`.
    pub updated: bool,
    /// The auto-bind CAS was requested and won (always `false` when no
    /// [`AutoBindOnFinalize`] was passed, or when `updated` is `false`).
    pub bound: bool,
    /// The session's terminal `completing -> completed` CAS won in this SAME
    /// transaction, persisting the `complete_result` snapshot alongside the
    /// finalize -- always `false` when `updated` is `false` (nothing to
    /// finish). Also `false` in the rare case where this call's own
    /// completion lease was raced away between the finalize and the
    /// terminal CAS (mirrors the old, separately-transacted
    /// `finish_session`'s `finished == false` branch): the caller must fall
    /// back to that same re-check-and-converge handling.
    pub session_completed: bool,
    /// The file's CURRENT content `ETag`, read INSIDE this same transaction,
    /// when an auto-bind was requested but its CAS lost (`Some` only in that
    /// case). The caller must use this value (rather than a second,
    /// post-transaction read) to build the `BindState::Conflict` response it
    /// returns to the client -- otherwise a rebind landing in the gap
    /// between this transaction's commit and that second read would make the
    /// live response disagree with the `complete_result` snapshot this same
    /// transaction just persisted (the exact divergence this method exists
    /// to close).
    pub current_etag: Option<String>,
}

/// Result of `Store::delete_file_collecting_versions` /
/// [`CleanupStore::delete_file_with_event_collecting_versions`].
///
/// The version list is collected **inside the same transaction** as the
/// delete itself (see that method's doc comment) so a version inserted
/// concurrently, anywhere between an old pre-transaction snapshot and this
/// transaction's commit, cannot be cascade-removed without ever being seen
/// by the caller's backend-blob cleanup.
#[derive(Debug, Clone)]
pub struct DeletedFile {
    /// Whether the `files` row (and its cascaded `file_versions` rows) was
    /// actually removed. `false` means the file was already gone (a
    /// concurrent delete/expiry won the race) -- `versions` is empty in
    /// that case, and no audit/event was written.
    pub removed: bool,
    /// Every version row that existed for this file at the moment of
    /// deletion, for the caller's best-effort backend-blob cleanup and usage
    /// accounting.
    pub versions: Vec<FileVersion>,
}

/// Result of `Store::delete_version_or_whole_file`.
///
/// The version-count decision ("is `version_id` the file's only version?")
/// is made **inside the transaction** that performs whichever delete it
/// implies, instead of from a pre-transaction snapshot -- see that method's
/// doc comment for the race this closes.
#[derive(Debug, Clone)]
pub enum DeleteVersionOutcome {
    /// `version_id` does not exist for this file (or the file itself is
    /// already gone).
    NotFound,
    /// `version_id` is the file's current content; the caller must bind
    /// another version before it can be deleted.
    IsCurrent,
    /// Just `version_id` was removed; the file and its other versions are
    /// untouched.
    VersionRemoved(FileVersion),
    /// `version_id` was the file's only version, so the whole file (and
    /// this, its one version) was removed too -- mirrors
    /// `FileService::delete_file_inner`'s unconditional whole-file delete.
    FileRemoved(FileVersion),
}

// ── CleanupStore ──────────────────────────────────────────────────────────────

/// Narrow persistence port for the cleanup engine.
///
/// Contains only the `Store` methods that `CleanupEngine` invokes.
/// `Store` implements this trait in `infra/storage/store.rs`.
#[async_trait]
pub trait CleanupStore: Send + Sync {
    /// List pending version rows older than `older_than`, excluding any
    /// version still backing a live `in_progress` multipart session
    /// (`expires_at > now`) -- such a version is never selected, regardless
    /// of age, so the sweep cannot reclaim it out from under an in-progress
    /// upload. A session whose `expires_at` has already passed is not
    /// excluded: it is aborted by the next sweep step
    /// (`sweep_expired_multipart`) and its version becomes reclaimable on a
    /// later sweep.
    ///
    /// Ordered `(created_at, version_id)` ascending, up to `limit` rows --
    /// one batch per sweep pass, same as
    /// [`Self::list_versionless_orphan_files`]. No cursor is threaded through:
    /// every row this query returns is either deleted or moved off `pending`
    /// by the caller, so it drops out of the next pass's result set on its
    /// own: a plain re-run of the same query, not a resumed scan, picks up
    /// whatever this pass's batch cap left behind.
    async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
        now: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<FileVersion>, DomainError>;

    /// List `files` rows that never received **any** version at all --
    /// `content_id IS NULL` **and** zero `file_versions` rows exist for
    /// them -- created before `created_before`, ordered `(created_at,
    /// file_id)` ascending, up to `limit` rows.
    ///
    /// Feeds the second phase of sweep step 1
    /// ([`crate::domain::cleanup::CleanupEngine::sweep_versionless_files`]).
    /// Unlike [`Self::list_abandoned_pending_versions`] above (keyed on the
    /// age of a `file_versions` row that exists), this method finds `files`
    /// rows that never got a version row in the first place -- e.g. a
    /// process crash between `FileService::create_file_bare`'s commit and
    /// `MultipartService::initiate_multipart_upload`'s
    /// `insert_pending_version`, or a failed
    /// `FileService::compensate_failed_multipart_initiate`. Neither
    /// `list_abandoned_pending_versions` nor `list_expired_multipart_uploads`
    /// can ever select such a row, since both key off a `file_versions` (or
    /// `multipart_uploads`) row that was never created.
    async fn list_versionless_orphan_files(
        &self,
        created_before: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<File>, DomainError>;

    /// Delete a version row + audit in one transaction. Returns `true` if removed.
    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;

    /// Delete a version row iff it is still `pending` + audit, in one
    /// transaction. Returns `true` if removed.
    ///
    /// Status-guarded CAS (P2 0.3 step 5) -- used by the cleanup engine
    /// instead of [`Self::delete_version`] when reclaiming an expired
    /// multipart session's pending version, so a version already flipped to
    /// `available` by a racing `complete_multipart_upload` is never deleted.
    async fn delete_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;

    /// List `in_progress` (or lease-lapsed `completing`) multipart sessions
    /// whose `expires_at` is before `now`, ordered `(expires_at, upload_id)`
    /// ascending, up to `limit` rows -- one batch per sweep pass, same as
    /// [`Self::list_abandoned_pending_versions`]. No cursor is threaded
    /// through here either: the caller CASes each returned session's `state`
    /// away from `in_progress`/`completing`, so it drops out of the next
    /// pass's result set on its own, and a plain re-run of the same query
    /// picks up whatever this pass's batch cap left behind.
    async fn list_expired_multipart_uploads(
        &self,
        now: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<MultipartUploadSession>, DomainError>;

    /// Mark a multipart session as `aborted` + audit in one transaction.
    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;

    /// Fetch a single version by `(file_id, version_id)`.
    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError>;

    /// List all retention rules across all tenants and scopes (sweep engine).
    async fn list_all_retention_rules(&self) -> Result<Vec<StoredRetentionRule>, DomainError>;

    /// List files across all tenants, keyset-paginated by `file_id`.
    async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError>;

    /// List all custom-metadata entries for a file.
    async fn list_metadata(&self, file_id: Uuid) -> Result<Vec<CustomMetadataEntry>, DomainError>;

    /// Batched counterpart of [`Self::list_metadata`]: fetch the custom
    /// metadata of many files in ONE query, keyed by `file_id`. Files with no
    /// metadata are simply absent from the map.
    ///
    /// The retention sweep needs this to avoid a per-file query while walking
    /// the whole `files` table: `sweep_retention_expiry` pages through every
    /// file in the deployment, and any page whose files have a
    /// metadata-criterion rule applied to them would otherwise cost one
    /// `list_metadata` round trip per file, every `sweep_interval_secs`. The
    /// same batching already backs `GET /files` on the read path.
    async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError>;

    /// List all versions of a file, newest first.
    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError>;

    /// Fetch a file by id (unscoped -- the sweep runs across all tenants).
    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError>;

    /// Batched counterpart of [`Self::get_file`] (unscoped, same reason):
    /// fetch every file in `ids` that exists in one query instead of one
    /// round trip per id. Used by the abandoned-pending-version and
    /// expired-multipart-session sweeps to resolve a whole candidate batch's
    /// audit `tenant_id`s up front, rather than one `get_file` per candidate.
    async fn list_files_by_ids(&self, ids: &[Uuid]) -> Result<Vec<File>, DomainError>;

    /// Whether `file_id` currently has at least one *active* (`in_progress`
    /// or `completing`) multipart upload session, regardless of
    /// `expires_at`/`lease_until`. Guards the P2 2.8 orphan-file delete
    /// against racing a not-yet-reaped multipart session whose pending
    /// version was just reclaimed by `sweep_abandoned_pending` (keyed only on
    /// version age, not multipart session state) -- without this check,
    /// deleting the file would `ON DELETE CASCADE` the still-active session
    /// out from under the upload. `completing` blocks unconditionally
    /// (lease status is not consulted): a completer may be assembling the
    /// final object right now, and only `sweep_expired_multipart`'s own CAS
    /// gets to decide a stuck lease is actually abandoned.
    async fn has_active_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError>;

    /// Delete a file row, collecting its version rows for backend-blob
    /// cleanup **inside the same transaction** as the delete, optionally
    /// enqueue a file-event, and audit — all atomically. See
    /// `Store::delete_file_collecting_versions`'s doc comment for why the
    /// version list must be read inside this transaction, not by the caller
    /// beforehand.
    async fn delete_file_with_event_collecting_versions(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<DeletedFile, DomainError>;

    /// Delete the parent `files` row of an abandoned-pending-version orphan
    /// (P2 2.8), re-verifying **inside the same transaction** that the file
    /// still has zero versions and a `NULL` `content_id` before deleting it.
    /// Returns `true` if the row was removed.
    async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError>;

    /// Delete at most `limit` `idempotency_keys` rows whose `expires_at` is
    /// at or before `now`, oldest-expired first -- batched like every other
    /// sweep phase in this trait (`list_abandoned_pending_versions`,
    /// `list_versionless_orphan_files`, `list_expired_multipart_uploads`),
    /// so a stalled sweep's backlog cannot turn the next tick into one
    /// unbounded `DELETE`. Returns the number of rows removed.
    async fn delete_expired_idempotency_keys(
        &self,
        now: OffsetDateTime,
        limit: u64,
    ) -> Result<u64, DomainError>;
}

// ── MultipartStore ────────────────────────────────────────────────────────────

/// Narrow persistence port for the multipart upload service.
///
/// Contains only the `Store` methods that `MultipartService` invokes.
/// `Store` implements this trait in `infra/storage/store.rs`.
#[async_trait]
pub trait MultipartStore: Send + Sync {
    /// Fetch a file by `(scope, file_id)`, or return `FileNotFound`.
    async fn require_file(&self, scope: &AccessScope, file_id: Uuid) -> Result<File, DomainError>;

    /// Fetch the policy for a given `(policy_scope, scope_owner_id)` within a
    /// tenant. Returns `None` when none is configured.
    async fn get_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError>;

    /// Insert a pending version row.
    #[allow(clippy::too_many_arguments)]
    async fn insert_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        mime_type: &str,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Create a multipart upload session row. `auto_bind` records whether
    /// `complete` should bind the finalized version itself (upload-flow
    /// redesign; only the merged `POST /files` create+plan path sets it).
    /// `backend_id`/`backend_path` are the backend and object path the
    /// pending version this session finalizes into was just given — always
    /// `Some` from the real initiate flow; `None` only ever exercised by
    /// tests reconstructing the pre-migration legacy row shape.
    #[allow(clippy::too_many_arguments)]
    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        backend_id: Option<&str>,
        backend_path: Option<&str>,
        declared_mime: &str,
        declared_size: u64,
        part_size: u64,
        auto_bind: bool,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// Fetch a multipart upload session by `upload_id`.
    async fn get_multipart_upload(
        &self,
        upload_id: Uuid,
    ) -> Result<Option<MultipartUploadSession>, DomainError>;

    /// Fetch a single version by `(file_id, version_id)`.
    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError>;

    /// Fetch the stored `version_hash_manifest` text for a version, if one
    /// exists (`multipart-composite-sha256` versions only). Used by the
    /// idempotent re-complete path to rebuild the original response.
    async fn get_version_manifest(&self, version_id: Uuid) -> Result<Option<String>, DomainError>;

    /// Insert or replace a multipart upload part.
    #[allow(clippy::too_many_arguments)]
    async fn upsert_multipart_part(
        &self,
        upload_id: Uuid,
        part_number: i32,
        backend_etag: &str,
        part_hash: Vec<u8>,
        size: i64,
        now: OffsetDateTime,
    ) -> Result<(), DomainError>;

    /// List all parts for a multipart upload.
    async fn list_multipart_parts(
        &self,
        upload_id: Uuid,
    ) -> Result<Vec<MultipartPart>, DomainError>;

    /// Record a version's size + hash and mark it `available`, optionally
    /// binding it as the file's current content in the same transaction.
    ///
    /// `hash_mode`/`part_count`/`manifest` (ADR-0006) let the multipart
    /// completion persist the `multipart-composite-sha256` discriminator, its
    /// part count, and the offset-manifest row transactionally with the
    /// version-row update. `manifest` is `Some` only for
    /// `multipart-composite-sha256` completions.
    ///
    /// `validated_mime` (P2 remediation item 1.10) is the sniffed/canonical
    /// MIME type to persist in place of the client's declared type, mirroring
    /// the single-part `Store::finalize_version`'s `mime_type` parameter —
    /// `complete_multipart_upload` sniffs the assembled object's leading
    /// bytes before calling this, so it is always `Some` on that path.
    ///
    /// `auto_bind` (upload-flow redesign): when `Some`, the finalized version
    /// is additionally bound as the file's current content under the same
    /// optimistic CAS a manual `bind` uses — in the **same transaction** as
    /// the finalize, so a crash can never leave "finalized because of the
    /// bind intent, but not bound" ambiguity. A lost CAS is NOT an error:
    /// `FinalizeVersionOutcome::bound` reports it, the version stays
    /// `available` and manually rebindable without a re-upload.
    #[allow(clippy::too_many_arguments)]
    async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        hash_mode: crate::infra::content::hash_mode::HashMode,
        part_count: Option<i32>,
        manifest: Option<String>,
        validated_mime: Option<String>,
        audit: AuditEntry,
        auto_bind: Option<AutoBindOnFinalize>,
    ) -> Result<FinalizeVersionOutcome, DomainError>;

    /// Finalize a multipart completion's version AND transition its session
    /// `completing → completed` (persisting the `complete_result` snapshot),
    /// in the SAME transaction as the finalize + auto-bind CAS.
    ///
    /// Closes the gap the separate `finalize_version` + `complete_multipart_upload`
    /// pair used to leave open: a crash between the two committed the version
    /// as `available` (bind decided) while the session's snapshot was never
    /// written, so an idempotent re-complete's `replay_completed` fell back to
    /// re-deriving `bind_state` from the file's CURRENT (possibly since
    /// legitimately rebound) content pointer instead of the historical one.
    /// See [`MultipartFinishSnapshot`]/[`FinalizeMultipartOutcome`] for what
    /// crosses the transaction boundary in each direction.
    #[allow(clippy::too_many_arguments)]
    async fn finalize_multipart_version(
        &self,
        file_id: Uuid,
        manifest: Option<String>,
        validated_mime: Option<String>,
        finalize_audit: AuditEntry,
        auto_bind: Option<AutoBindOnFinalize>,
        finish: MultipartFinishSnapshot,
    ) -> Result<FinalizeMultipartOutcome, DomainError>;

    /// Terminal transition `completing → completed` + persist the response
    /// snapshot (`result_json`) + audit, in one (fast) transaction.
    ///
    /// Used only by the takeover/converge recovery paths
    /// (`MultipartService::finish_session`'s remaining callers), which
    /// re-derive the response from already-committed state rather than
    /// building a fresh `complete_result` snapshot inline — the main
    /// first-attempt completion path uses
    /// [`Self::finalize_multipart_version`] instead, which folds this same
    /// transition into the finalize transaction itself.
    ///
    /// `lease_owner`: this call's own completion-lease owner, required to
    /// still match the session's current lease owner (DBS-05 hardening) --
    /// see `MultipartRepo::finish_complete`'s doc comment.
    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        lease_owner: &str,
        result_json: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;

    /// Acquire (or take over an expired) completion lease: one conditional
    /// UPDATE `in_progress|expired-completing → completing(owner, until)`.
    /// `false` = a live lease exists or the session is terminal.
    async fn acquire_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Release a held completion lease back to `in_progress` (assembly
    /// failed) — scoped to `owner`.
    async fn release_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
    ) -> Result<bool, DomainError>;

    /// Mark a multipart session as `aborted` + audit in one transaction.
    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;

    /// Delete a version row + audit in one transaction. Returns `true` if removed.
    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;
}

// ── PolicyStore ───────────────────────────────────────────────────────────────

/// Narrow persistence port for the policy administration service.
///
/// Contains only the `Store` methods that `PolicyService` invokes.
/// `Store` implements this trait in `infra/storage/store.rs`.
#[async_trait]
pub trait PolicyStore: Send + Sync {
    /// Resolve a `file`-scope retention rule's `scope_target_id` to a `File`
    /// (needed to re-authorize per-file `WRITE` before create/delete). Mirrors
    /// the identical method on `MultipartStore` — same underlying
    /// `Store::require_file`/`FileRepo` lookup, exposed through this narrower
    /// port too.
    async fn require_file(&self, scope: &AccessScope, file_id: Uuid) -> Result<File, DomainError>;

    /// Batched counterpart of [`Self::require_file`]: fetch every file in
    /// `ids` that exists (and is visible under `scope`) in one query
    /// (chunked against the backend's bind-parameter budget), instead of one
    /// round trip per id. Used by `PolicyService::list_retention_rules`'s
    /// non-admin path to resolve every distinct `File`-scope rule target on
    /// a page in a single call.
    async fn list_files_by_ids(
        &self,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<File>, DomainError>;

    /// Fetch the raw policy for a given `(policy_scope, scope_owner_id)` within
    /// a tenant. Returns `None` when none is configured.
    async fn get_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError>;

    /// Upsert the policy for a given `(policy_scope, scope_owner_id)`.
    /// Returns the `policy_id`.
    async fn upsert_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &PolicyBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError>;

    /// List all retention rules for a tenant (all scopes).
    async fn list_retention_rules(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError>;

    /// Insert a new retention rule. Returns the assigned `rule_id`.
    async fn insert_retention_rule(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        retention_scope: &RetentionScope,
        scope_target_id: Option<Uuid>,
        body: &RetentionRuleBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError>;

    /// Delete a retention rule by `rule_id`. Returns `true` if a row was removed.
    async fn delete_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Fetch a single retention rule by `rule_id`, if it exists. Used by
    /// `delete_retention_rule` to re-authorize by scope/target before deleting
    /// (a bare `rule_id` carries no ownership information on its own).
    async fn get_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<StoredRetentionRule>, DomainError>;
}

// ── FileStorageMetricsPort ──────────────────────────────────────────────────────

/// Metrics port (P2 1.8 remediation — zero metrics/observability).
///
/// Follows the platform's established `OTel` `Meter`-method-API pattern (mirrors
/// `gears/mini-chat/mini-chat/src/domain/ports.rs`'s `MiniChatMetricsPort` /
/// `infra/metrics.rs`'s `MiniChatMetricsMeter`) rather than the `metrics`-crate
/// macros. `crate::infra::metrics::FileStorageMetricsMeter` is the sole
/// OTel-backed implementation, obtained via `opentelemetry::global::meter_with_scope`
/// once per process — `gear.rs` for the control plane, `bin/sidecar.rs` for the
/// data plane. `crate::infra::metrics::NoopMetrics` is the default so every
/// existing `FileService::new` / `MultipartService::new` call site (used
/// throughout the integration-test suite) keeps compiling unchanged; real
/// wiring is opted into via `.with_metrics(...)`.
pub trait FileStorageMetricsPort: Send + Sync {
    /// Record a control-plane service-entry-point outcome, e.g.
    /// `record_operation("create_file", "ok")` / `("bind", "denied")` /
    /// `("finalize_upload", "error")`.
    fn record_operation(&self, op: &str, result: &str);

    /// Record a storage-backend operation failure (`backend_id`, `op`).
    fn record_backend_error(&self, backend_id: &str, op: &str);

    /// Record a quota-enforcement denial for `op` (e.g. `"create_file"`,
    /// `"initiate_multipart_upload"`).
    fn record_quota_denied(&self, op: &str);

    /// Record one background cleanup sweep's tallies — mirrors
    /// `cleanup::SweepResult`'s counters (`idempotency_keys_deleted` landed
    /// in the P2 1.9 remediation; `abandoned_files_deleted` in P2 2.8).
    fn record_sweep_result(
        &self,
        abandoned_pending_deleted: u64,
        abandoned_files_deleted: u64,
        expired_multipart_aborted: u64,
        retention_expired_deleted: u64,
        idempotency_keys_deleted: u64,
    );

    /// Record bytes received from a client upload (sidecar ingress).
    fn record_ingress_bytes(&self, bytes: f64);

    /// Record bytes served to a client download (sidecar egress).
    fn record_egress_bytes(&self, bytes: f64);

    /// Record one sidecar HTTP request's route/method/status/latency.
    ///
    /// The control plane's own REST routes already get
    /// `http.server.request.duration` from the platform's api-gateway
    /// (`gears/system/api-gateway/src/middleware/http_metrics.rs`, applied to
    /// every proxied gear route) — this port method is only wired at the
    /// sidecar, a standalone process the gateway never proxies.
    fn record_request(&self, route: &str, method: &str, status: u16, latency_ms: f64);
}
