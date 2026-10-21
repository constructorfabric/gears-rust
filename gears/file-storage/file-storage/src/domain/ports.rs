//! Domain-owned capability ports.
//!
//! Each trait names only the `Store` methods one consumer needs; the concrete `Store`
//! implements them all, so consumers never import `crate::infra::storage::Store`.

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession};
use crate::domain::policy::{
    PolicyBody, PolicyScope, RetentionRuleBody, RetentionScope, StoredPolicy, StoredRetentionRule,
};

/// Bind a just-finalized version as the file's content in the SAME transaction as the finalize
/// (single-part `bind=auto`), under the same optimistic CAS as a manual `bind`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct AutoBindOnFinalize {
    pub expected_content_id: Option<Uuid>,
    pub audit: AuditEntry,
    pub event: Option<FileEvent>,
}

/// Result of `finalize_version`.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct FinalizeVersionOutcome {
    pub updated: bool,
    pub bound: bool,
}

/// Completion facts known before the bind outcome (everything but `bind_state`/`etag`/`current_etag`).
#[domain_model]
#[derive(Debug, Clone)]
pub struct MultipartFinishSnapshot {
    pub upload_id: Uuid,
    pub version_id: Uuid,
    pub size: i64,
    pub content_hash: Vec<u8>,
    pub hash_mode: crate::infra::content::hash_mode::HashMode,
    pub part_count: Option<i32>,
    pub session_audit: AuditEntry,
}

/// Result of [`MultipartStore::finalize_multipart_version`].
#[domain_model]
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct FinalizeMultipartOutcome {
    pub updated: bool,
    pub bound: bool,
    /// The terminal `completing -> completed` CAS won in this transaction; `false` when `updated` is
    /// `false` or the lease was taken over (the caller re-checks and converges).
    pub session_completed: bool,
    /// Current content `ETag` when the auto-bind CAS lost (`Some` only then). Read in this
    /// transaction so it agrees with the persisted snapshot, not a later read.
    pub current_etag: Option<String>,
}

/// Persistence port for the cleanup engine.
#[async_trait]
pub trait CleanupStore: Send + Sync {
    /// List pending versions older than `older_than`, excluding any backing a live
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

    /// Delete a version row iff it is still `pending` + audit, in one transaction.
    /// Returns `true` if removed. Status-guarded so a version flipped to `available`
    /// by a racing `complete_multipart_upload` is never deleted.
    async fn delete_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;

    /// List `in_progress` (or lease-lapsed `completing`) sessions with `expires_at` before `now`.
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

    /// Whether `file_id` has an active (`in_progress` or `completing`) session, regardless of
    /// `expires_at`/`lease_until`. Guards orphan-file delete (the file would `ON DELETE CASCADE` a
    /// live session); only the expired-multipart sweep's CAS may declare a stuck lease abandoned.
    async fn has_active_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError>;

    /// Delete a file row, optionally enqueue a file-event, and audit — all in
    /// one transaction. Returns `true` if a row was removed.
    async fn delete_file_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError>;

    /// Delete an orphan `files` row, re-verifying **in the same transaction** that it
    /// still has zero versions and a `NULL` `content_id`. Returns `true` if removed.
    async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError>;

    /// Bulk-delete `idempotency_keys` rows with `expires_at <= now`; returns the count.
    async fn delete_expired_idempotency_keys(
        &self,
        now: OffsetDateTime,
    ) -> Result<u64, DomainError>;
}

/// Persistence port for the multipart upload service.
#[async_trait]
pub trait MultipartStore: Send + Sync {
    /// Fetch a file by `(scope, file_id)`, or return `FileNotFound`.
    async fn require_file(&self, scope: &AccessScope, file_id: Uuid) -> Result<File, DomainError>;

    /// Fetch the policy for `(policy_scope, scope_owner_id)` in a tenant, if any.
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

    /// Create a multipart upload session row.
    #[allow(clippy::too_many_arguments)]
    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        backend_id: &str,
        backend_path: &str,
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

    /// Record a version's size + hash and mark it `available`.
    ///
    /// `hash_mode`/`part_count`/`manifest` (ADR-0006) are persisted transactionally with
    /// the version row; `manifest` is `Some` only for `multipart-composite-sha256`.
    /// `validated_mime` is the sniffed MIME type stored in place of the declared one.
    ///
    /// `auto_bind`, when `Some`, binds in the same transaction under the same CAS as a manual `bind`;
    /// a lost CAS is reported in `bound`, not an error.
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

    /// Finalize a multipart version AND move its session `completing -> completed` (with snapshot)
    /// in one transaction with the auto-bind CAS, so a crash cannot leave no snapshot.
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

    /// Terminal `completing -> completed` + snapshot (`result_json`) + audit in one transaction,
    /// for the takeover/converge paths. `lease_owner` must still match.
    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        lease_owner: &str,
        result_json: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError>;

    /// Acquire (or take over an expired) completion lease with one conditional UPDATE.
    /// `false` = a live lease exists or the session is terminal.
    async fn acquire_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError>;

    /// Release a held lease back to `in_progress`, scoped to `owner`.
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

/// Persistence port for the policy administration service.
#[async_trait]
pub trait PolicyStore: Send + Sync {
    /// Resolve a `file`-scope rule's `scope_target_id` to a `File`, so per-file
    /// `WRITE` can be re-authorized.
    async fn require_file(&self, scope: &AccessScope, file_id: Uuid) -> Result<File, DomainError>;

    /// Fetch the raw policy for `(policy_scope, scope_owner_id)` in a tenant, if any.
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

    /// List retention rules for a tenant, cursor-paginated in either direction. Items are always in
    /// canonical order (`created_at DESC, rule_id DESC`); `cursor` resumes in the direction it carries.
    ///
    /// # Errors
    /// A cursor error (`domain::pagination`) for a mismatched or malformed
    /// `cursor`, or the underlying store error.
    async fn list_retention_rules_page(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        limit: u64,
        cursor: Option<toolkit_odata::CursorV1>,
    ) -> Result<toolkit_odata::Page<StoredRetentionRule>, DomainError>;

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

    /// Fetch a retention rule by `rule_id`; callers use it to re-authorize by
    /// scope/target (a bare id carries no ownership).
    async fn get_retention_rule(
        &self,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<StoredRetentionRule>, DomainError>;
}

/// Control-plane operations the data-plane service needs, implemented by `FileService`.
#[async_trait]
pub trait DataPlanePort: Send + Sync {
    /// The backend registry shared between the control and data planes.
    fn backends(&self) -> &crate::infra::backend::BackendRegistry;

    /// Pre-flight write authorization, so a rejected request never touches storage.
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

/// Metrics port. `FileStorageMetricsMeter` is the `OTel`-backed implementation;
/// `NoopMetrics` is the default, real wiring is opted into via `.with_metrics(...)`.
pub trait FileStorageMetricsPort: Send + Sync {
    /// Record a control-plane operation outcome, e.g. `("create_file", "ok")`.
    fn record_operation(&self, op: &str, result: &str);

    /// Record a storage-backend operation failure (`backend_id`, `op`).
    fn record_backend_error(&self, backend_id: &str, op: &str);

    /// Record a quota-enforcement denial for `op` (e.g. `"create_file"`,
    /// `"initiate_multipart_upload"`).
    fn record_quota_denied(&self, op: &str);

    /// Record one cleanup sweep's tallies (mirrors `cleanup::SweepResult`).
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
    /// Only wired at the sidecar; control-plane routes get request metrics from the
    /// api-gateway.
    fn record_request(&self, route: &str, method: &str, status: u16, latency_ms: f64);
}
