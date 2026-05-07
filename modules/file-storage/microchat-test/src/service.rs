//! Public API of the test-only microchat module.
//!
//! Each method validates locally, calls into `cf-file-storage-sdk`'s
//! `FileStorageClient`, and mirrors the result into the microchat's
//! own `chat_attachments` row. Errors that surface through
//! `FileStorageClient` are wrapped in `MicrochatError::FileStorage`;
//! local rule violations (mime, filename, quota) are surfaced
//! directly.

use std::sync::Arc;

use file_storage_sdk::{
    ByteRange, CapabilityTag, Etag, FileId, FileInfo, FileMeta, FileReadHandle,
    FileStorageClient, FileStorageError, OwnerRef, PresignDownloadItem, PresignedDownload,
    UploadedPart, UrlParams,
};
use modkit_db::secure::Db;
use modkit_security::SecurityContext;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::MicrochatError;
use crate::repo::{Attachment, AttachmentStatus, MicrochatRepo};
use crate::validators::{MIME_ALLOWLIST, validate_filename, validate_mime};

/// Quotas + allowlists that bound a `Microchat` instance. Production
/// would load these from configuration; tests build them inline.
#[derive(Clone, Debug)]
pub struct MicrochatLimits {
    pub max_files_per_user: u32,
    pub allowed_mimes: Vec<&'static str>,
    pub max_filename_len: usize,
}

impl Default for MicrochatLimits {
    fn default() -> Self {
        Self {
            max_files_per_user: 5,
            allowed_mimes: MIME_ALLOWLIST.to_vec(),
            max_filename_len: 255,
        }
    }
}

/// Handle returned by [`Microchat::attach`]. Mirrors the FS-side
/// `PresignedUploadHandle` while exposing only what microchat callers
/// actually need to drive the upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachHandle {
    pub file_id: FileId,
    pub upload_id: String,
    pub part_urls: Vec<String>,
    pub expires_at: OffsetDateTime,
}

pub struct Microchat {
    fs: Arc<dyn FileStorageClient>,
    db: Db,
    repo: Arc<MicrochatRepo>,
    limits: MicrochatLimits,
}

impl Microchat {
    pub fn new(
        fs: Arc<dyn FileStorageClient>,
        db: Db,
        limits: MicrochatLimits,
    ) -> Self {
        Self {
            fs,
            db,
            repo: Arc::new(MicrochatRepo::new()),
            limits,
        }
    }

    /// Test-only constructor for the validator-only suite (§6.1, tests
    /// 1–12). The returned `Microchat` carries a stub
    /// `FileStorageClient` that panics if called, so it can validate
    /// MIME / filename / quota purely against an in-memory SQLite
    /// without spinning up s3s-fs.
    pub fn new_for_validators_only(db: Db, limits: MicrochatLimits) -> Self {
        Self {
            fs: Arc::new(PanicOnUseClient),
            db,
            repo: Arc::new(MicrochatRepo::new()),
            limits,
        }
    }

    pub fn limits(&self) -> &MicrochatLimits {
        &self.limits
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn repo(&self) -> &MicrochatRepo {
        &self.repo
    }

    pub fn fs(&self) -> &Arc<dyn FileStorageClient> {
        &self.fs
    }

    /// Begin a new attachment: validate, reserve a quota slot, ask the
    /// FileStorage service for a multipart-presigned upload, and
    /// persist a `pending` row in `chat_attachments`. The caller
    /// drives the actual byte upload via the returned `part_urls`.
    pub async fn attach(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        owner_id: Uuid,
        meta: FileMeta,
        part_count: u32,
    ) -> Result<AttachHandle, MicrochatError> {
        validate_mime(&meta.mime_type, &self.limits)?;
        validate_filename(&meta.name, &self.limits)?;

        let conn = self.db_conn()?;
        self.repo
            .enforce_quota(&conn, owner_id, self.limits.max_files_per_user)
            .await?;

        let cap: CapabilityTag = "upload.s3.multipart.sigv4.v1".to_string();
        let owner = OwnerRef {
            tenant_id: ctx.subject_tenant_id(),
            owner_id,
        };
        let handle = self
            .fs
            .create_presigned_upload(
                ctx,
                None,
                None,
                owner,
                meta.clone(),
                &cap,
                part_count,
                UrlParams::default(),
            )
            .await?;

        // The pending row needs to land before we hand the URLs to
        // the caller — otherwise a concurrent `complete` racing the
        // PUTs would find no microchat row to mark active.
        self.repo
            .insert_pending(
                &conn,
                handle.file_id,
                chat_id,
                owner_id,
                &meta.name,
                &meta.mime_type,
                OffsetDateTime::now_utc(),
            )
            .await?;

        Ok(AttachHandle {
            file_id: handle.file_id,
            upload_id: handle.upload_id,
            part_urls: handle.part_urls,
            expires_at: handle.expires_at,
        })
    }

    /// Finalize a previously started attach: complete the upload at
    /// the FileStorage service, then promote the `chat_attachments`
    /// row from `pending` to `active` with the final ETag and size.
    pub async fn complete(
        &self,
        ctx: &SecurityContext,
        _chat_id: Uuid,
        file_id: FileId,
        upload_id: &str,
        parts: Vec<UploadedPart>,
    ) -> Result<Attachment, MicrochatError> {
        let info: FileInfo = self
            .fs
            .complete_upload(ctx, file_id, upload_id, parts)
            .await?;

        let conn = self.db_conn()?;
        self.repo
            .mark_active(&conn, file_id, &info.etag, info.size_bytes)
            .await?;
        self.repo
            .find(&conn, file_id)
            .await?
            .ok_or(MicrochatError::NotFound)
    }

    /// Cancel an in-flight upload: ask the FileStorage service to
    /// abort multipart staging, then drop the pending row entirely
    /// (the row never reached `active`, so deletion is the right
    /// terminal state — keeping a tombstone would deadlock the user's
    /// quota counter).
    pub async fn abort(
        &self,
        ctx: &SecurityContext,
        _chat_id: Uuid,
        file_id: FileId,
        upload_id: &str,
    ) -> Result<(), MicrochatError> {
        self.fs.abort_upload(ctx, file_id, upload_id).await?;
        let conn = self.db_conn()?;
        self.repo.delete_row(&conn, file_id).await?;
        Ok(())
    }

    /// All non-deleted attachments on the given chat, ordered by
    /// insertion.
    pub async fn list(
        &self,
        _ctx: &SecurityContext,
        chat_id: Uuid,
    ) -> Result<Vec<Attachment>, MicrochatError> {
        let conn = self.db_conn()?;
        let mut rows = self.repo.list_by_chat(&conn, chat_id).await?;
        rows.retain(|a| a.status != AttachmentStatus::Deleted);
        Ok(rows)
    }

    /// Read bytes for an attachment. Forwards directly to
    /// `FileStorageClient::read_file`; an optional `range` selects a
    /// byte interval. Refuses to read a row that was deleted in this
    /// microchat (status `deleted`) — the FS-side row may still
    /// exist briefly during the staged delete but the chat-level
    /// view is already gone.
    pub async fn read(
        &self,
        ctx: &SecurityContext,
        _chat_id: Uuid,
        file_id: FileId,
        range: Option<ByteRange>,
    ) -> Result<FileReadHandle, MicrochatError> {
        let conn = self.db_conn()?;
        let row = self
            .repo
            .find(&conn, file_id)
            .await?
            .ok_or(MicrochatError::NotFound)?;
        if row.status == AttachmentStatus::Deleted {
            return Err(MicrochatError::NotFound);
        }
        Ok(self.fs.read_file(ctx, file_id, None, None, range).await?)
    }

    /// Delete an attachment. Forwards to `FileStorageClient::delete_file`
    /// and, on success, marks the local row as `deleted`. The FS-side
    /// `EtagMismatch` etc. surfaces unchanged through
    /// `MicrochatError::FileStorage` so callers can pin the etag for
    /// safety.
    pub async fn delete(
        &self,
        ctx: &SecurityContext,
        _chat_id: Uuid,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<(), MicrochatError> {
        self.fs.delete_file(ctx, file_id, etag, None).await?;
        let conn = self.db_conn()?;
        self.repo.mark_deleted(&conn, file_id).await?;
        Ok(())
    }

    /// Sign a download URL for one attachment. Capability mismatch,
    /// row-not-found, etc. propagate from the FileStorage service.
    pub async fn presign_download(
        &self,
        ctx: &SecurityContext,
        _chat_id: Uuid,
        file_id: FileId,
        capability: &CapabilityTag,
    ) -> Result<PresignedDownload, MicrochatError> {
        let item = PresignDownloadItem {
            file_id,
            capability: capability.clone(),
            params: UrlParams::default(),
            etag: None,
            version_id: None,
        };
        let outcomes = self.fs.presign_urls(ctx, vec![item]).await?;
        let outcome = outcomes
            .into_iter()
            .next()
            .ok_or_else(|| MicrochatError::FileStorage(FileStorageError::Internal))?;
        outcome.result.map_err(MicrochatError::FileStorage)
    }

    fn db_conn(&self) -> Result<modkit_db::secure::DbConn<'_>, MicrochatError> {
        self.db
            .conn()
            .map_err(|e| MicrochatError::Database(e.to_string()))
    }
}

// ── Panic-on-use FS client for the validator-only constructor ───────────

struct PanicOnUseClient;

#[async_trait::async_trait]
impl FileStorageClient for PanicOnUseClient {
    async fn list_backends(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<file_storage_sdk::Backend>, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn create_presigned_upload(
        &self,
        _ctx: &SecurityContext,
        _file_id: Option<FileId>,
        _backend_id: Option<file_storage_sdk::BackendId>,
        _owner: file_storage_sdk::OwnerRef,
        _meta: FileMeta,
        _capability: &CapabilityTag,
        _part_count: u32,
        _params: file_storage_sdk::UrlParams,
    ) -> Result<file_storage_sdk::PresignedUploadHandle, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn complete_upload(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _upload_id: &str,
        _parts: Vec<UploadedPart>,
    ) -> Result<file_storage_sdk::FileInfo, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn abort_upload(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _upload_id: &str,
    ) -> Result<(), FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn get_file_info(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _etag: Option<&Etag>,
        _version_id: Option<&file_storage_sdk::VersionId>,
    ) -> Result<file_storage_sdk::FileInfo, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn put_file_info(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _update: file_storage_sdk::FileMetaUpdate,
        _etag: Option<&Etag>,
        _version_id: Option<&file_storage_sdk::VersionId>,
    ) -> Result<file_storage_sdk::FileInfo, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn delete_file(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _etag: Option<&Etag>,
        _version_id: Option<&file_storage_sdk::VersionId>,
    ) -> Result<(), FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn list_files(
        &self,
        _ctx: &SecurityContext,
        _query: file_storage_sdk::ListFilesQuery,
    ) -> Result<file_storage_sdk::FileList, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn read_file(
        &self,
        _ctx: &SecurityContext,
        _file_id: FileId,
        _etag: Option<&Etag>,
        _version_id: Option<&file_storage_sdk::VersionId>,
        _range: Option<ByteRange>,
    ) -> Result<FileReadHandle, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn put_file(
        &self,
        _ctx: &SecurityContext,
        _file_id: Option<FileId>,
        _backend_id: Option<file_storage_sdk::BackendId>,
        _owner: file_storage_sdk::OwnerRef,
        _meta: FileMeta,
        _bytes: file_storage_sdk::FileByteStream,
        _etag: Option<&Etag>,
        _version_id: Option<&file_storage_sdk::VersionId>,
    ) -> Result<file_storage_sdk::FileInfo, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }

    async fn presign_urls(
        &self,
        _ctx: &SecurityContext,
        _items: Vec<file_storage_sdk::PresignDownloadItem>,
    ) -> Result<Vec<file_storage_sdk::PresignDownloadOutcome>, FileStorageError> {
        panic!("PanicOnUseClient: validator-only Microchat must not touch FileStorage")
    }
}
