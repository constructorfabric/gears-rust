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
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion};

use crate::domain::audit::{AuditEntry, FileEvent};
use crate::domain::error::DomainError;
use crate::domain::multipart::{MultipartPart, MultipartUploadSession};
use crate::domain::policy::{PolicyScope, StoredPolicy, StoredRetentionRule};

// ── CleanupStore ──────────────────────────────────────────────────────────────

/// Narrow persistence port for the cleanup engine.
///
/// Contains only the `Store` methods that `CleanupEngine` invokes.
/// `Store` implements this trait in `infra/storage/store.rs`.
#[async_trait]
pub trait CleanupStore: Send + Sync {
    /// List pending version rows older than `older_than`.
    async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError>;

    /// Delete a version row + audit in one transaction. Returns `true` if removed.
    async fn delete_version(
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

    /// Delete a file row, optionally enqueue a file-event, and audit — all in
    /// one transaction. Returns `true` if a row was removed.
    async fn delete_file_with_event(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError>;
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

    /// Create a multipart upload session row.
    #[allow(clippy::too_many_arguments)]
    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        declared_mime: &str,
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
    async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
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
