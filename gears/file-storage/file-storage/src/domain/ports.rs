//! Domain-owned capability ports.
//!
//! Each trait names only the `Store` methods one consumer needs; the concrete `Store`
//! implements them all, so consumers never import `crate::infra::storage::Store`.

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

/// Persistence port for the cleanup engine.
#[async_trait]
pub trait CleanupStore: Send + Sync {
    /// List pending versions older than `older_than`, excluding any backing a live
    /// `in_progress` multipart session (`expires_at > now`). Versions of expired
    /// sessions are not excluded; they become reclaimable once the session is aborted.
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

    /// Whether `file_id` has any `in_progress` multipart session (regardless of
    /// `expires_at`). Guards the orphan-file delete: deleting the file would
    /// `ON DELETE CASCADE` a still-live session.
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
        declared_mime: &str,
        declared_size: u64,
        part_size: u64,
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
    ) -> Result<bool, DomainError>;

    /// Mark a multipart session as `completed` + audit in one transaction.
    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
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
