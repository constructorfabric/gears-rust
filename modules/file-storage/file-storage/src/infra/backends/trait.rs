//! Internal `StorageBackend` adapter trait.
//!
//! The seam between the FileStorage core and a concrete byte store. Lives
//! inside the implementation crate because it speaks types the SDK does
//! not know (e.g. `BackendObjectKey`).

use std::sync::Arc;

use async_trait::async_trait;

use file_storage_sdk::{Backend, BackendId, FileByteStream, FileMeta, PresignedDownload, UrlParams};

use crate::domain::error::DomainError;

/// Opaque per-backend object key, minted by the FileStorage core. For
/// `s3-compatible` backends it is derived deterministically from the file's
/// `file_id` (per ADR-0002 / DESIGN §3.x).
pub type BackendObjectKey = String;

/// Derive the deterministic S3 object key for a given `file_id`.
///
/// Per DESIGN §3 example: `f/{id_hex}` (no per-row randomness — the column
/// itself is dropped from the schema).
#[must_use]
pub fn derive_s3_key(file_id: uuid::Uuid) -> String {
    format!("f/{}", file_id.simple())
}

/// Shared descriptor for a backend, augmented with non-SDK fields the
/// router needs (max signed-URL TTL).
#[derive(Debug, Clone)]
pub struct BackendDescriptor {
    pub sdk: Backend,
    pub max_signed_url_ttl_seconds_value: u64,
    /// Tenant access list for visibility filtering. Empty = visible to
    /// every tenant.
    pub tenant_access: Vec<uuid::Uuid>,
}

impl BackendDescriptor {
    pub fn id(&self) -> BackendId {
        self.sdk.id
    }
    pub fn max_signed_url_ttl_seconds(&self) -> u64 {
        self.max_signed_url_ttl_seconds_value
    }
    pub fn max_file_size_bytes(&self) -> Option<u64> {
        self.sdk.max_file_size_bytes
    }
    pub fn capabilities(&self) -> &[file_storage_sdk::BackendCapability] {
        &self.sdk.capabilities
    }
    pub fn is_visible_to(&self, tenant_id: uuid::Uuid) -> bool {
        self.tenant_access.is_empty() || self.tenant_access.contains(&tenant_id)
    }
}

/// Result of a backend `open_read` — the byte stream plus the backend-side
/// content fingerprint (S3 ETag header) needed for self-healing.
pub struct BackendReadResult {
    pub bytes: FileByteStream,
    /// Backend-reported content hash. For S3 this is the unquoted ETag
    /// from the GET response.
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub struct PresignedGetItem {
    pub key: BackendObjectKey,
    pub params: UrlParams,
    pub mime_type_hint: Option<String>,
    pub display_name_hint: Option<String>,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct PresignedGetOutcome {
    pub key: BackendObjectKey,
    pub result: Result<PresignedDownload, DomainError>,
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn descriptor(&self) -> &BackendDescriptor;

    /// Stream the object's bytes. Returns `BackendReadResult` so the
    /// caller can run self-healing reconciliation against the backend's
    /// fingerprint before returning to the consumer.
    async fn open_read(
        &self,
        key: &BackendObjectKey,
    ) -> Result<BackendReadResult, DomainError>;

    /// Best-effort delete — used by the orphan-delete worker.
    async fn delete_object(&self, key: &BackendObjectKey) -> Result<(), DomainError>;

    /// Issue a presigned PUT URL.
    async fn issue_presigned_put(
        &self,
        key: &BackendObjectKey,
        meta: &FileMeta,
        params: &UrlParams,
        expected_etag: &str,
        ttl_seconds: u64,
    ) -> Result<String, DomainError>;

    /// Batched presigned-GET URL issuance.
    async fn issue_presigned_gets(
        &self,
        items: Vec<PresignedGetItem>,
    ) -> Result<Vec<PresignedGetOutcome>, DomainError>;

    /// HEAD against the backend.
    async fn head_object(&self, key: &BackendObjectKey) -> Result<HeadResult, DomainError>;
}

#[derive(Debug, Clone)]
pub struct HeadResult {
    pub content_hash: String,
    pub size_bytes: u64,
}

/// Type-erased backend instance shared by the registry.
pub type SharedBackend = Arc<dyn StorageBackend>;
