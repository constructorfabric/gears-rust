//! `FileStorageClient` trait — the consumer-facing `FileStorage` SDK.
//!
//! Mirrors `rust-traits.md` exactly. P1 surface includes both `read_file`
//! and `put_file` as in-process SDK methods (no REST surface in P1).

use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::errors::FileStorageError;
use crate::models::{
    Backend, BackendId, Etag, FileByteStream, FileId, FileInfo, FileList, FileMeta, FileMetaUpdate,
    FileReadHandle, FileStatus, ListFilesQuery, OwnerRef, PresignDownloadItem,
    PresignDownloadOutcome, PresignedUploadHandle, UrlParams,
};

/// Public API trait for the `FileStorage` module.
///
/// Registered in `ClientHub` by the `file-storage` module.
#[async_trait]
pub trait FileStorageClient: Send + Sync {
    // ── Backends ────────────────────────────────────────────────────────────

    /// `GET /api/file-storage/v1/storages` — list backends visible to the
    /// caller's tenant.
    async fn list_backends(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Backend>, FileStorageError>;

    // ── Upload coordination ─────────────────────────────────────────────────

    /// Step 2 of the lifecycle. Validates input, registers a row with
    /// `status = PendingUpload`, and returns a presigned PUT URL.
    ///
    /// `backend_id` is optional: when `None`, `FileStorage` falls back to
    /// the caller's tenant `default_private` backend.
    async fn create_presigned_url(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        file_path: &str,
        meta: FileMeta,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, FileStorageError>;

    /// Step 4 of the lifecycle. Acknowledges that the end-client has
    /// finished writing to the backend.
    async fn change_status(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        target: FileStatus,
        old_etag: Etag,
        new_etag: Etag,
    ) -> Result<FileInfo, FileStorageError>;

    // ── File lookups ────────────────────────────────────────────────────────

    async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<FileInfo, FileStorageError>;

    async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Etag,
    ) -> Result<FileInfo, FileStorageError>;

    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Etag,
    ) -> Result<(), FileStorageError>;

    async fn list_files(
        &self,
        ctx: &SecurityContext,
        query: ListFilesQuery,
    ) -> Result<FileList, FileStorageError>;

    // ── Streaming I/O (in-process SDK only — no REST in P1) ─────────────────

    /// Open a streaming reader over the file content. Self-healing
    /// reconciliation runs on the read path per ADR-0004.
    async fn read_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<FileReadHandle, FileStorageError>;

    /// Single-call streaming upload that internally drives the full
    /// `create_presigned_url` → backend PUT → `change_status` lifecycle.
    /// In-process SDK only — no REST surface in P1.
    async fn put_file(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        file_path: &str,
        meta: FileMeta,
        bytes: FileByteStream,
        etag: Option<&Etag>,
    ) -> Result<FileInfo, FileStorageError>;

    // ── Presigned download URLs (batch-first) ───────────────────────────────

    async fn presign_urls(
        &self,
        ctx: &SecurityContext,
        items: Vec<PresignDownloadItem>,
    ) -> Result<Vec<PresignDownloadOutcome>, FileStorageError>;
}
