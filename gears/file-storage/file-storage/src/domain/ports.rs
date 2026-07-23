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

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
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
    async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError>;

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

    /// List `in_progress` multipart sessions whose `expires_at` is before `now`.
    async fn list_expired_multipart_uploads(
        &self,
        now: OffsetDateTime,
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

    /// List all versions of a file, newest first.
    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError>;

    /// Fetch a file by id (unscoped -- the sweep runs across all tenants).
    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError>;

    /// Whether `file_id` currently has at least one `in_progress` multipart
    /// upload session (regardless of `expires_at`). Guards the P2 2.8
    /// orphan-file delete against racing a not-yet-expired multipart session
    /// whose pending version was just reclaimed by `sweep_abandoned_pending`
    /// (keyed only on version age, not multipart session state) -- without
    /// this check, deleting the file would `ON DELETE CASCADE` the still
    /// live session out from under the upload.
    async fn has_in_progress_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError>;

    /// Delete a file row, optionally enqueue a file-event, and audit — all in
    /// one transaction. Returns `true` if a row was removed.
    async fn delete_file_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError>;

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

    /// Bulk-delete all `idempotency_keys` rows whose `expires_at` is at or
    /// before `now`. Returns the number of rows removed.
    async fn delete_expired_idempotency_keys(
        &self,
        now: OffsetDateTime,
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
    #[allow(clippy::too_many_arguments)]
    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
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
    async fn get_version_manifest(
        &self,
        version_id: Uuid,
    ) -> Result<Option<String>, DomainError>;

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

    /// Terminal transition `completing → completed` + persist the response
    /// snapshot (`result_json`) + audit, in one (fast) transaction.
    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
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

// ── DataPlanePort ─────────────────────────────────────────────────────────────

/// Narrow control-plane port for the data-plane service.
///
/// `DataPlaneService` only needs four control-plane operations:
/// access to the backend registry (for construction), pre-flight auth,
/// version look-up, and post-upload finalization. Exposing a focused
/// trait here (ISP/DIP) lets `data_plane.rs` avoid a direct dependency
/// on the full `FileService` type, keeping its fan-in off `service.rs`
/// and reducing `service.rs`'s HK `fan_in`.
///
/// `FileService` implements this trait in `domain/service.rs`.
#[async_trait]
pub trait DataPlanePort: Send + Sync {
    /// The backend registry shared between the control and data planes.
    /// Used by `DataPlaneService::new` to clone the registry without
    /// needing a direct reference to `FileService`.
    fn backends(&self) -> &crate::infra::backend::BackendRegistry;

    /// Authorize a write operation for the given file before bytes are
    /// persisted. Called as a pre-flight check before the blob is written
    /// to the backend so a rejected request never touches storage.
    async fn authorize_write(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
    ) -> Result<(), DomainError>;

    /// Fetch a single version by `(file_id, version_id)`.
    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError>;

    /// Record an uploaded version's size + hash and mark it available.
    /// Re-checks authorization and policy as defense-in-depth.
    async fn finalize_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
    ) -> Result<(), DomainError>;
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
