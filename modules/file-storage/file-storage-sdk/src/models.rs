//! Public SDK models for `FileStorage`.
//!
//! Aligned with [`rust-traits.md`](../../docs/rust-traits.md) and the
//! `OpenAPI` schema at [`openapi.yaml`](../../docs/openapi.yaml).
//!
//! Domain types use `#[domain_model]` to enforce DDD boundaries (no
//! infrastructure types in the public surface).

use std::collections::BTreeMap;
use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;
use modkit_macros::domain_model;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::errors::FileStorageError;

// ── Identifiers ─────────────────────────────────────────────────────────────

/// Canonical, opaque file handle (per ADR-0002).
pub type FileId = Uuid;

/// Stable identity of a backend instance, assigned once in the static TOML
/// roster.
pub type BackendId = Uuid;

/// Strong content fingerprint produced by `FileStorage`.
///
/// Hex-encoded SHA-256 of `content_hash || ":" || meta_revision`. Used for
/// conditional updates and optimistic concurrency.
pub type Etag = String;

// ── Backend descriptors ─────────────────────────────────────────────────────

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    /// Stable backend identity, assigned once in the static TOML roster.
    pub id: BackendId,
    pub kind: BackendKind,
    pub default_public: bool,
    pub default_private: bool,
    pub transport: BackendTransport,
    pub capabilities: Vec<BackendCapability>,
    pub max_file_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// The only kind produced and consumed by P1 code paths.
    S3Compatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendTransport {
    /// Bytes flow client ↔ backend directly via presigned URLs. The only
    /// transport produced by P1 backends.
    Redirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCapability {
    /// Backend can sign time-limited URLs for client-direct PUT/GET.
    PresignedUrls,
    /// Backend can issue a public, signature-free download URL.
    PublicReadUrls,
    /// Backend honours `If-Match: "<etag>"` and `If-None-Match: "*"` on
    /// `PutObject` (RFC 9110 / 412 Precondition Failed).
    PresignedConditionalPut,
}

// ── Owner ───────────────────────────────────────────────────────────────────

/// Owner principal of a file. `owner_id` is the principal's UUID — a user or
/// an app — FileStorage does not distinguish; the kind is tracked in the
/// identity / authz subsystem.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRef {
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
}

// ── File metadata ───────────────────────────────────────────────────────────

pub type CustomMetadata = BTreeMap<String, String>;

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub name: String,
    pub mime_type: String,
    pub gts_file_type: String,
    pub size_bytes: Option<u64>,
    pub custom_metadata: CustomMetadata,
}

#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMetaUpdate {
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub custom_metadata: Option<CustomMetadata>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub file_id: FileId,
    pub backend_id: BackendId,
    pub file_path: String,
    pub owner: OwnerRef,
    pub meta: FileMeta,
    pub status: FileStatus,
    pub etag: Etag,
    pub size_bytes: u64,
    pub created_at: OffsetDateTime,
    pub modified_at: OffsetDateTime,
    pub upload_expires_at: Option<OffsetDateTime>,
}

/// Lifecycle states for a file row in the FileStorage database (P1).
///
/// `PendingUpload → Uploaded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    PendingUpload,
    Uploaded,
}

// ── Presigned URLs ──────────────────────────────────────────────────────────

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParams {
    pub expires_in_seconds: u64,
    pub content_disposition: Option<String>,
    pub content_type_override: Option<String>,
    pub allowed_client_cidrs: Vec<String>,
    pub refresh_etag: bool,
}

impl Default for UrlParams {
    fn default() -> Self {
        Self {
            expires_in_seconds: 600,
            content_disposition: None,
            content_type_override: None,
            allowed_client_cidrs: Vec::new(),
            refresh_etag: false,
        }
    }
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUploadHandle {
    pub file_id: FileId,
    pub upload_url: String,
    pub etag_pinned: Etag,
    pub expires_at: OffsetDateTime,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignDownloadItem {
    pub file_id: FileId,
    pub params: UrlParams,
    pub etag: Option<Etag>,
}

#[derive(Debug, Clone)]
pub struct PresignDownloadOutcome {
    pub file_id: FileId,
    pub result: Result<PresignedDownload, FileStorageError>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedDownload {
    pub url: String,
    pub expires_at: OffsetDateTime,
    pub is_public: bool,
}

// ── Streaming bodies ────────────────────────────────────────────────────────
//
// `FileByteStream` is the outbound stream type used by `read_file` and the
// inbound stream type used by `put_file`.

pub type FileByteStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, FileStorageError>> + Send + 'static>>;

pub struct FileReadHandle {
    pub info: FileInfo,
    pub bytes: FileByteStream,
}

impl std::fmt::Debug for FileReadHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileReadHandle")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

// ── Listing / filtering ─────────────────────────────────────────────────────

/// Optional filters for `list_files`. When `owner_id` is `None`, the
/// implementation defaults to the caller's `subject_id`.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListFilesQuery {
    pub owner_id: Option<Uuid>,
    pub backend_id: Option<BackendId>,
    pub mime_type: Option<String>,
    pub gts_file_type: Option<String>,
    pub created_after: Option<OffsetDateTime>,
    pub created_before: Option<OffsetDateTime>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileList {
    pub items: Vec<FileInfo>,
    pub next_cursor: Option<String>,
}
