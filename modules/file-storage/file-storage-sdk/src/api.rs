//! `FileStorageClient` trait — the consumer-facing `FileStorage` SDK.
//!
//! Mirrors `rust-traits.md` exactly. P1 surface is **in-process SDK only**
//! (no REST surface in P1; REST is P2 per `cpt-cf-file-storage-fr-rest-api`).
//! 11 methods total.

use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::errors::FileStorageError;
use crate::models::{
    Backend, BackendId, ByteRange, CapabilityTag, Etag, FileByteStream, FileId, FileInfo,
    FileList, FileMeta, FileMetaUpdate, FileReadHandle, ListFilesQuery, OwnerRef,
    PresignDownloadItem, PresignDownloadOutcome, PresignedUploadHandle, UploadedPart, UrlParams,
    VersionId,
};

/// Public API trait for the `FileStorage` module.
///
/// Registered in `ClientHub` by the `file-storage` module.
#[async_trait]
pub trait FileStorageClient: Send + Sync {
    // ── Backends ────────────────────────────────────────────────────────────

    /// List backends visible to the caller's tenant.
    async fn list_backends(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Backend>, FileStorageError>;

    // ── Upload coordination (multipart) ─────────────────────────────────────

    /// Open a presigned multipart-upload session.
    ///
    /// - `file_id = None` ⇒ initial upload: server mints a fresh `FileId`,
    ///   INSERTs a `PendingUpload` row, runs `CreateMultipartUpload` on the
    ///   backend.
    /// - `file_id = Some(id)` ⇒ variant-B re-upload: same `file_id`, same
    ///   backend object key. The caller MUST NOT supply `meta` mutations on
    ///   this variant — the server pins the row's CURRENT metadata into the
    ///   part URLs (see `cpt-cf-file-storage-constraint-meta-via-put-meta-only`).
    /// - `backend_id = None` ⇒ falls back to the tenant's `default_private`
    ///   backend.
    /// - `capability` selects the upload protocol (P1 ships only
    ///   `upload.s3.multipart.sigv4.v1`).
    /// - `part_count` is the number of parts the caller plans to upload
    ///   (1..=10000 per the S3 hard cap). Single-byte uploads use
    ///   `part_count = 1` (last-part rule).
    ///
    /// Returns `PresignedUploadHandle { file_id, upload_id, part_urls,
    /// expires_at }`. **`upload_id` is NOT persisted** by FileStorage — the
    /// caller MUST keep it for the companion `complete_upload` /
    /// `abort_upload`.
    async fn create_presigned_upload(
        &self,
        ctx: &SecurityContext,
        file_id: Option<FileId>,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        meta: FileMeta,
        capability: &CapabilityTag,
        part_count: u32,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, FileStorageError>;

    /// Commit a multipart upload via 3-phase commit.
    ///
    /// Phase 1 (DB): `PendingUpload → Completing` via conditional UPDATE.
    /// Phase 2 (S3): `CompleteMultipartUpload(upload_id, parts)` — captures
    /// the finalized `(etag, version_id)` from the backend response.
    /// Phase 3 (DB): `Completing → Uploaded` writing the new
    /// `(etag, version_id, size_bytes)` and clearing `upload_expires_at`.
    ///
    /// Returns the post-commit `FileInfo`.
    async fn complete_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
        parts: Vec<UploadedPart>,
    ) -> Result<FileInfo, FileStorageError>;

    /// Abort one multipart upload session. Issues `AbortMultipartUpload` on
    /// the backend and (for initial uploads only) hard-DELETEs the
    /// `PendingUpload` row. Idempotent.
    async fn abort_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
    ) -> Result<(), FileStorageError>;

    // ── File lookups ────────────────────────────────────────────────────────

    /// Read the authoritative `FileInfo` from the DB. `etag` and
    /// `version_id` are optional CAS pins; mismatch → `EtagMismatch`.
    async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError>;

    /// Atomic DB+S3 metadata sync via `CopyObject` self-copy with
    /// `MetadataDirective: REPLACE` (2-phase commit `Uploaded →
    /// MetaUpdating → Uploaded`). `etag` and `version_id` are optional CAS
    /// pins; together they make the strong-CAS path ABA-safe.
    async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError>;

    /// 2-phase hard delete (`Uploaded → Deleting → row hard-deleted`).
    /// Both pins optional.
    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<(), FileStorageError>;

    /// Owner-scoped listing with cursor pagination (P1 — owner only).
    async fn list_files(
        &self,
        ctx: &SecurityContext,
        query: ListFilesQuery,
    ) -> Result<FileList, FileStorageError>;

    // ── Streaming I/O (in-process SDK only — no REST in P1) ────────────────

    /// Open a streaming reader over the file content.
    ///
    /// `etag` is a CAS pin. `version_id` is a historical selector when S3
    /// versioning is enabled (`GetObject?versionId=v`); on backends without
    /// versioning `Some(v)` is a null-safe mismatch and surfaces
    /// `EtagMismatch`. `range` is an optional partial-read selector that
    /// maps to HTTP `Range: bytes=...` on the backend `GetObject`; constitutive
    /// S3 feature, no capability tag required. When `range = Some(_)`,
    /// `FileReadHandle.bytes` carries only the requested diapason and
    /// `FileReadHandle.range` mirrors the backend's `Content-Range`.
    /// `FileReadHandle.info` always reflects the FULL object metadata.
    ///
    /// In-band recovery: if the row is in `Completing` or `MetaUpdating`,
    /// the SDK runs HEAD-and-sync against the backend to roll the row to
    /// its terminal state before serving the read.
    async fn read_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
        range: Option<ByteRange>,
    ) -> Result<FileReadHandle, FileStorageError>;

    /// Single-call streaming upload for in-process consumers. Drives the
    /// same multipart lifecycle as the presigned path internally —
    /// `create_presigned_upload` semantically, then per-chunk `UploadPart`,
    /// then `complete_upload` — without the presign round-trip. There is
    /// **no single-shot `PutObject` path** in any phase.
    ///
    /// `file_id = None` ⇒ initial upload; `file_id = Some(id)` ⇒ variant-B
    /// re-upload. Both `etag` and `version_id` are optional CAS pins on the
    /// re-upload variant.
    async fn put_file(
        &self,
        ctx: &SecurityContext,
        file_id: Option<FileId>,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        meta: FileMeta,
        bytes: FileByteStream,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError>;

    // ── Presigned download URLs (batch-first) ───────────────────────────────

    /// Issue a batch of presigned download URLs (one per item). Per item:
    /// `etag` (when present) is a fail-fast CAS pin; `version_id` (when
    /// present and `capability` is a `*.versioned.*` variant) selects a
    /// historical generation. `capability` selects the signing/audience
    /// policy (`download.s3.sigv4.v1`, `download.s3.public.v1`, plus their
    /// `versioned` variants).
    async fn presign_urls(
        &self,
        ctx: &SecurityContext,
        items: Vec<PresignDownloadItem>,
    ) -> Result<Vec<PresignDownloadOutcome>, FileStorageError>;
}
