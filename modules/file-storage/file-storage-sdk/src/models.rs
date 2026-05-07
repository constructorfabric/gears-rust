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

/// Canonical, opaque file handle (per ADR-0002). External URLs and
/// cross-module references all key off `FileId`.
pub type FileId = Uuid;

/// Stable identity of a backend instance, assigned once in the static TOML
/// roster.
pub type BackendId = Uuid;

/// Raw S3 ETag (sans surrounding quotes) for the file's current bytes.
/// **Content fingerprint only** — does not track metadata changes
/// (`cpt-cf-file-storage-constraint-etag-content-only`). On multipart-uploaded
/// files the format is `<md5-of-md5s>-<part-count>`; on single-PUT files it is
/// the MD5. Used for conditional updates (HTTP `If-Match`-style optimistic
/// concurrency) on routes that opt in.
pub type Etag = String;

/// Raw S3 VersionId (opaque, up to 1024 bytes) for the file's current
/// generation. `Some` only when S3 returned a `x-amz-version-id` header on
/// the relevant write (per ADR-0005). FileStorage treats the value as opaque
/// — no parsing, sorting, or monotonicity assumptions.
///
/// Two roles on SDK methods:
/// - **Historical selector** on `read_file` and `presign_urls` (download):
///   the bytes returned correspond to that exact S3 generation
///   (`GetObject?versionId=<vid>`).
/// - **CAS pin** on `get_file_info`, `put_file_info`, `delete_file`,
///   `put_file`, `create_presigned_upload` (variant-B re-upload): the call
///   asserts the row's current `version_id` matches before committing.
///   Mismatch surfaces as `EtagMismatch`.
pub type VersionId = String;

/// Versioned capability identifier — a flat string of the form
/// `<operation>.<protocol>.<algorithm>.<variant>?.v<n>`. Each tag describes
/// one (operation, protocol, signing-strategy, optional modifier, version)
/// tuple that a backend can serve.
///
/// **Grammar.** Regex
/// `^[a-z][a-z0-9_]+(\.[a-z][a-z0-9_]+){2,4}\.v\d+$`. The leading segment is
/// the SDK operation (`upload` / `download`); the next is the protocol
/// family (`s3`, `gcs`, `azure`, …); the next is the signing/auth flavor
/// (`sigv4`, `sigv4a`, `sas_user`, `multipart`, `public` for anonymous
/// bare-HTTPS); the optional fourth descriptor is a variant modifier
/// (currently only `versioned`); the trailing segment is the contract
/// version (`v1`, `v2`, …).
///
/// **P1 whitelist (5 tags).** Validated at module init against
/// `KNOWN_CAPABILITIES`; unknown tag → fail-fast init.
/// - `upload.s3.multipart.sigv4.v1`
/// - `download.s3.sigv4.v1`
/// - `download.s3.sigv4.versioned.v1`
/// - `download.s3.public.v1`
/// - `download.s3.public.versioned.v1`
pub type CapabilityTag = String;

/// Compile-time whitelist of capability tags known to the SDK. The roster
/// loader validates every declared tag against this list at module init.
/// Adding a new signing strategy or wire protocol is a new entry here plus a
/// new branch in the adapter's `match` — no enum migration, no breaking
/// schema change.
// @cpt-begin:cpt-cf-file-storage-fr-backend-capabilities:p1:inst-known-capabilities-whitelist
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "upload.s3.multipart.sigv4.v1",
    "download.s3.sigv4.v1",
    "download.s3.sigv4.versioned.v1",
    "download.s3.public.v1",
    "download.s3.public.versioned.v1",
];
// @cpt-end:cpt-cf-file-storage-fr-backend-capabilities:p1:inst-known-capabilities-whitelist

// ── Backend descriptors ─────────────────────────────────────────────────────

/// One backend declared in the FileStorage roster. Versioning support is
/// expressed through the `*.versioned.*` capability tags rather than a
/// separate flag; FileStorage does NOT probe `GetBucketVersioning` at boot
/// (`cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    /// Stable backend identity, assigned once in the static TOML roster.
    pub id: BackendId,
    /// `true` when this backend is the tenant's default for new private
    /// files (presigned downloads only).
    pub default_private: bool,
    /// `true` when this backend is the tenant's default for new public-read
    /// files (paired with `download.s3.public.v1` capability).
    pub default_public: bool,
    /// Versioned capability tags declared by the backend. Validated against
    /// `KNOWN_CAPABILITIES` at module init.
    pub capabilities: Vec<CapabilityTag>,
    /// Per-backend hard ceiling on object size in bytes. `None` falls back
    /// to the S3 single-object maximum of 5 TiB.
    pub max_file_size_bytes: Option<u64>,
    /// Per-backend hard ceiling on aggregate user-metadata size in bytes.
    /// `None` falls back to the S3 user-metadata budget of 2 KiB.
    pub max_metadata_bytes: Option<u64>,
    /// Per-backend hard ceiling on presigned-URL TTL in seconds. `None`
    /// falls back to the AWS SigV4 maximum of 7 days (604_800).
    pub max_presign_ttl_seconds: Option<u64>,
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

/// User-supplied key/value pairs attached to a file. Mirrored as
/// `x-amz-meta-<k>=<v>` on the S3 object. Aggregate size is capped per
/// `Backend.max_metadata_bytes`.
pub type CustomMetadata = BTreeMap<String, String>;

/// Caller-provided file metadata at upload time.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// Display name (original upload name); pinned into `Content-Disposition`.
    pub name: String,
    /// Declared MIME type; pinned into `Content-Type`.
    pub mime_type: String,
    /// Mandatory GTS file type (`gts.cf.fstorage.file.type.v1~…`).
    /// Immutable after creation. Stored DB-only — NOT mirrored to S3.
    pub gts_file_type: String,
    /// Application-defined `x-amz-meta-<k>=<v>` mirror.
    pub custom_metadata: CustomMetadata,
}

/// Body for `put_file_info` — every field is optional. `Some(v)` replaces;
/// `None` keeps. **`gts_file_type` is structurally immutable** and is NOT
/// declared here.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMetaUpdate {
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub custom_metadata: Option<CustomMetadata>,
}

/// Authoritative view of a file that FileStorage hands back to callers.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Opaque file handle (UUID). Immutable for the file's lifetime.
    pub file_id: FileId,
    pub backend_id: BackendId,
    /// Logical path inside the backend's tenant scope. Immutable —
    /// FileStorage has no rename/move surface.
    pub file_path: String,
    pub owner: OwnerRef,
    pub meta: FileMeta,
    pub status: FileStatus,
    /// Raw S3 ETag for the current bytes (sans surrounding quotes).
    pub etag: Etag,
    /// Raw S3 VersionId for the current generation; `Some` when S3 returned
    /// `x-amz-version-id` on the latest write, `None` otherwise.
    pub version_id: Option<VersionId>,
    pub size_bytes: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// `Some` while `status == PendingUpload` — the presigned URL TTL
    /// boundary. Cleared on `Uploaded`.
    pub upload_expires_at: Option<OffsetDateTime>,
}

/// Lifecycle states for a file row in the FileStorage database (P1).
///
/// Stable terminal: `Uploaded`. Transient (phases of multi-phase commits):
/// `PendingUpload`, `Completing`, `MetaUpdating`, `Deleting`.
// @cpt-begin:cpt-cf-file-storage-state-files-schema-and-repo-row:p1:inst-file-status-enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// Initial: row inserted by `create_presigned_upload`. Bytes have not
    /// been finalized at the backend yet.
    PendingUpload,
    /// Phase 1 of `complete_upload`: `pending_upload → completing`. Phase 2
    /// will invoke `CompleteMultipartUpload` on the backend.
    Completing,
    /// Stable durable state — byte content is finalized at the backend, DB
    /// metadata mirrors it.
    Uploaded,
    /// Phase 1 of `put_file_info`: `uploaded → meta_updating`. Phase 2 will
    /// run a `CopyObject` self-copy with `MetadataDirective: REPLACE`.
    MetaUpdating,
    /// Phase 1 of `delete_file`: `uploaded → deleting`. Phase 2 will issue
    /// the backend `DeleteObject`; Phase 3 hard-deletes the row.
    Deleting,
}
// @cpt-end:cpt-cf-file-storage-state-files-schema-and-repo-row:p1:inst-file-status-enum

// ── Presigned URLs ──────────────────────────────────────────────────────────

/// Knobs applied to a presigned URL.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParams {
    /// TTL in seconds. Capped by `Backend.max_presign_ttl_seconds`.
    pub expires_in_seconds: u64,
    /// Override `Content-Disposition` on response (download direction).
    pub content_disposition: Option<String>,
    /// Override `Content-Type` on response (download direction).
    pub content_type_override: Option<String>,
    /// Optional client IP allowlist enforced by the backend (S3 bucket
    /// policy condition). Empty = no restriction.
    pub allowed_client_cidrs: Vec<String>,
}

impl Default for UrlParams {
    fn default() -> Self {
        Self {
            expires_in_seconds: 600,
            content_disposition: None,
            content_type_override: None,
            allowed_client_cidrs: Vec::new(),
        }
    }
}

/// Result of `create_presigned_upload`. Every P1 upload uses S3 multipart
/// (single-part for small files), so the handle always carries a
/// backend-supplied `upload_id` and a list of part URLs.
///
/// **`upload_id` is opaque to FileStorage** and is **NOT persisted** in the
/// DB — the caller MUST keep it for the subsequent `complete_upload` /
/// `abort_upload` call.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUploadHandle {
    pub file_id: FileId,
    /// Backend-supplied multipart session id. Round-trips through the caller.
    pub upload_id: String,
    /// One presigned PUT URL per part. The caller PUTs `parts[i]` to
    /// `part_urls[i]` and collects the per-part `ETag` from the response.
    pub part_urls: Vec<String>,
    /// Wall-clock deadline after which the part URLs no longer work.
    pub expires_at: OffsetDateTime,
}

/// One part the caller uploaded successfully — the `(part_number, etag)`
/// pair S3 needs in `CompleteMultipartUpload`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedPart {
    /// Part number (1..=10000).
    pub part_number: u32,
    /// Per-part ETag from the `UploadPart` response.
    pub etag: Etag,
}

/// Input item to `presign_urls` — one element of the batch.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignDownloadItem {
    pub file_id: FileId,
    /// Selects the signed-download policy (e.g. `download.s3.sigv4.v1` for
    /// time-limited private GET, `download.s3.public.v1` for bare-HTTPS,
    /// `*.versioned.*` variants accept `version_id`). Mismatch with the
    /// resolved backend's declared `capabilities` produces a per-item
    /// `CapabilityUnavailable` outcome.
    pub capability: CapabilityTag,
    pub params: UrlParams,
    /// Optional fail-fast etag pin. When supplied the server verifies the
    /// row's current `etag` before signing.
    pub etag: Option<Etag>,
    /// Optional historical generation; only honoured when `capability` is a
    /// `*.versioned.*` variant.
    pub version_id: Option<VersionId>,
}

/// Per-item outcome of `presign_urls` — either a signed URL or a failure.
#[derive(Debug, Clone)]
pub struct PresignDownloadOutcome {
    pub file_id: FileId,
    pub result: Result<PresignedDownload, FileStorageError>,
}

/// Output of a successful presign-download request.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedDownload {
    pub url: String,
    /// Wall-clock deadline. For `download.s3.public.*` URLs (`is_public ==
    /// true`) this is a far-future sentinel.
    pub expires_at: OffsetDateTime,
    /// `true` for bare-HTTPS URLs from public-read backends.
    pub is_public: bool,
}

// ── Range reads ─────────────────────────────────────────────────────────────

/// Byte-range selector for partial reads on `read_file`. Maps 1:1 onto the
/// HTTP `Range: bytes=...` header (RFC 7233), which every S3-class backend
/// honours on `GetObject` (constitutive S3 feature — does not require a
/// capability tag).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `Range: bytes=START-END` — both bounds inclusive. Validated at the
    /// SDK boundary: `start > end` is `BadRequest`.
    Inclusive { start: u64, end: u64 },
    /// `Range: bytes=START-` — from offset to end of object.
    From(u64),
    /// `Range: bytes=-N` — last N bytes (suffix range). `N == 0` is
    /// `BadRequest`.
    Suffix(u64),
}

/// Resolved byte range that the backend actually served, captured from the
/// `Content-Range: bytes START-END/TOTAL` response header. Returned on
/// `FileReadHandle.range` whenever a range was requested.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedByteRange {
    /// First byte of the served range (inclusive).
    pub start: u64,
    /// Last byte of the served range (inclusive).
    pub end_inclusive: u64,
    /// Full object size in bytes (the `TOTAL` part of `Content-Range`).
    pub total: u64,
}

// ── Streaming bodies ────────────────────────────────────────────────────────

/// Outbound body for `read_file` and inbound body for `put_file`.
pub type FileByteStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, FileStorageError>> + Send + 'static>>;

/// Reader-side handle returned by `read_file`.
///
/// When a partial read was requested, `bytes` carries only the requested
/// diapason and `range` is populated with the resolved bounds and total
/// object size; `info` always reflects the FULL object metadata.
pub struct FileReadHandle {
    pub info: FileInfo,
    pub bytes: FileByteStream,
    /// `Some` iff the caller passed `range = Some(_)` to `read_file`.
    /// Mirrors the HTTP `Content-Range` header from the backend.
    pub range: Option<ResolvedByteRange>,
}

impl std::fmt::Debug for FileReadHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileReadHandle")
            .field("info", &self.info)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

// ── Listing / filtering ─────────────────────────────────────────────────────

/// Optional filters for `list_files`. P1 exposes only owner-scoped listing
/// with cursor pagination. When `owner_id` is `None`, the implementation
/// defaults to the caller's `subject_id`. Sort order is fixed to
/// `created_at DESC, id ASC`. Other filters (`mime_type`, `gts_file_type`,
/// date range, `backend_id`) are P2 deltas — see DESIGN §4.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListFilesQuery {
    pub owner_id: Option<Uuid>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileList {
    pub items: Vec<FileInfo>,
    pub next_cursor: Option<String>,
}
