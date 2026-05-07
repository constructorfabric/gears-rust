//! Internal `StorageBackend` adapter trait.
//!
//! The seam between the FileStorage core and a concrete byte store. Lives
//! inside the implementation crate because it speaks types the SDK does
//! not know (e.g. `BackendObjectKey`).

use std::sync::Arc;

use async_trait::async_trait;

use file_storage_sdk::{
    Backend, BackendId, ByteRange, CapabilityTag, FileByteStream, FileMeta, ResolvedByteRange,
    UploadedPart,
};

use crate::domain::error::DomainError;

/// Opaque per-backend object key, minted by the FileStorage core. For
/// `s3-compatible` backends it is derived deterministically from the file's
/// `file_id` (per ADR-0002 / DESIGN §3.x).
pub type BackendObjectKey = String;

// @cpt-begin:cpt-cf-file-storage-principle-file-id-address:p1:inst-derive-s3-key
// @cpt-begin:cpt-cf-file-storage-adr-opaque-file-ids:p1:inst-derive-s3-key
/// Derive the deterministic S3 object key for a given `file_id`.
#[must_use]
pub fn derive_s3_key(file_id: uuid::Uuid) -> String {
    format!("f/{}", file_id.simple())
}
// @cpt-end:cpt-cf-file-storage-principle-file-id-address:p1:inst-derive-s3-key
// @cpt-end:cpt-cf-file-storage-adr-opaque-file-ids:p1:inst-derive-s3-key

/// Shared descriptor for a backend, augmented with non-SDK fields the
/// router needs.
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
    pub fn capabilities(&self) -> &[CapabilityTag] {
        &self.sdk.capabilities
    }
    pub fn declares(&self, cap: &str) -> bool {
        self.sdk.capabilities.iter().any(|c| c == cap)
    }
    pub fn is_visible_to(&self, tenant_id: uuid::Uuid) -> bool {
        self.tenant_access.is_empty() || self.tenant_access.contains(&tenant_id)
    }
}

/// Backend-side metadata captured from S3 response headers.
#[derive(Debug, Clone)]
pub struct BackendObjectMetadata {
    /// Raw ETag (sans surrounding quotes).
    pub etag: String,
    /// Backend-side `x-amz-version-id`. `Some` when bucket has S3 versioning.
    pub version_id: Option<String>,
    pub size_bytes: u64,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    /// `x-amz-meta-*` user-metadata mirror.
    pub user_metadata: std::collections::BTreeMap<String, String>,
}

/// Result of a backend `open_read` — the byte stream plus the metadata
/// captured from the response headers, plus the resolved range when a
/// partial read was requested.
pub struct BackendReadResult {
    pub bytes: FileByteStream,
    pub metadata: BackendObjectMetadata,
    /// `Some` iff caller passed `range = Some(_)`. Mirrors the
    /// `Content-Range` response header.
    pub range: Option<ResolvedByteRange>,
}

/// Output of a successful `complete_multipart_upload`.
#[derive(Debug, Clone)]
pub struct MultipartCompleteResult {
    pub etag: String,
    pub version_id: Option<String>,
    pub size_bytes: u64,
}

/// Output of a successful `copy_object_self_replace_meta`.
#[derive(Debug, Clone)]
pub struct CopyObjectResult {
    pub etag: String,
    pub version_id: Option<String>,
}

/// Per-item input to `issue_presigned_gets`.
#[derive(Debug, Clone)]
pub struct PresignedGetItem {
    pub key: BackendObjectKey,
    pub capability: CapabilityTag,
    pub params: file_storage_sdk::UrlParams,
    pub mime_type_hint: Option<String>,
    pub display_name_hint: Option<String>,
    pub expires_in_seconds: u64,
    /// Honoured only when the chosen capability is a `*.versioned.*` variant.
    pub version_id: Option<String>,
}

/// Per-item outcome of `issue_presigned_gets`.
#[derive(Debug, Clone)]
pub struct PresignedGetOutcome {
    pub key: BackendObjectKey,
    pub result: Result<file_storage_sdk::PresignedDownload, DomainError>,
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn descriptor(&self) -> &BackendDescriptor;

    // ── Multipart upload (the only upload path in P1) ───────────────────────

    /// Open an S3 multipart-upload session. Returns the backend-supplied
    /// opaque `upload_id`. The session expires per the bucket lifecycle
    /// configuration; FileStorage does NOT persist the `upload_id`.
    async fn create_multipart_upload(
        &self,
        key: &BackendObjectKey,
        meta: &FileMeta,
    ) -> Result<String, DomainError>;

    /// Issue presigned PUT URLs for `part_count` parts of a multipart
    /// session. Returns one URL per part in ascending part_number order
    /// (1..=part_count).
    async fn presign_upload_parts(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
        part_count: u32,
        ttl_seconds: u64,
    ) -> Result<Vec<String>, DomainError>;

    /// Finalize a multipart upload. Captures the final `(etag, version_id,
    /// size_bytes)` from the backend response.
    async fn complete_multipart_upload(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
        parts: &[UploadedPart],
    ) -> Result<MultipartCompleteResult, DomainError>;

    /// Best-effort abort of a multipart session. Idempotent.
    async fn abort_multipart_upload(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
    ) -> Result<(), DomainError>;

    // ── Read / metadata ─────────────────────────────────────────────────────

    /// Stream the object's bytes, optionally constrained to a byte range.
    /// `range = Some(_)` adds an HTTP `Range: bytes=...` header.
    async fn open_read(
        &self,
        key: &BackendObjectKey,
        range: Option<ByteRange>,
    ) -> Result<BackendReadResult, DomainError>;

    /// HEAD against the backend. Used by transient-state recovery and the
    /// strong-CAS path of `put_file_info`.
    async fn head_object(
        &self,
        key: &BackendObjectKey,
    ) -> Result<BackendObjectMetadata, DomainError>;

    /// `CopyObject` self-copy with `MetadataDirective: REPLACE`. Optional
    /// `if_match_etag` adds the `x-amz-copy-source-if-match` precondition
    /// for strong-CAS metadata updates.
    async fn copy_object_self_replace_meta(
        &self,
        key: &BackendObjectKey,
        meta: &FileMeta,
        if_match_etag: Option<&str>,
    ) -> Result<CopyObjectResult, DomainError>;

    /// `DeleteObject` on the backend. Idempotent (returns `Ok(())` on 404).
    async fn delete_object(&self, key: &BackendObjectKey) -> Result<(), DomainError>;

    // ── Presigned download URLs ─────────────────────────────────────────────

    /// Batched presigned-GET URL issuance.
    async fn issue_presigned_gets(
        &self,
        items: Vec<PresignedGetItem>,
    ) -> Result<Vec<PresignedGetOutcome>, DomainError>;
}

/// Type-erased backend instance shared by the registry.
pub type SharedBackend = Arc<dyn StorageBackend>;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn derive_s3_key_uses_simple_hex_prefix() {
        let id = Uuid::nil();
        assert_eq!(derive_s3_key(id), "f/00000000000000000000000000000000");
    }

    #[test]
    fn derive_s3_key_is_deterministic() {
        let id = Uuid::parse_str("11112222-3333-4444-5555-666677778888").unwrap();
        let a = derive_s3_key(id);
        let b = derive_s3_key(id);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_s3_key_differs_per_file_id() {
        let a = derive_s3_key(Uuid::new_v4());
        let b = derive_s3_key(Uuid::new_v4());
        assert_ne!(a, b);
    }

    #[test]
    fn derive_s3_key_no_dashes() {
        // simple format = no hyphens, lowercase hex only.
        let id = Uuid::new_v4();
        let key = derive_s3_key(id);
        assert!(key.starts_with("f/"));
        assert!(!key.contains('-'), "key should use simple format: {key}");
        assert_eq!(key.len(), 2 + 32);
    }
}
