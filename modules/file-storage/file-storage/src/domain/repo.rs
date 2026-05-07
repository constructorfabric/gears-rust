//! `FilesRepo` trait — the persistence boundary for FileStorage (P1).

use async_trait::async_trait;
use file_storage_sdk::{FileInfo, FileMetaUpdate, FileStatus};
use modkit_db::secure::DBRunner;
use time::OffsetDateTime;
use uuid::Uuid;

use super::error::DomainError;

/// Outcome of a conditional UPDATE / DELETE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOutcome {
    /// Row updated. Caller wins.
    Applied,
    /// 0 rows matched. Caller lost the race or row moved.
    NoMatch,
}

#[derive(Debug, Clone)]
pub struct InsertPendingArgs {
    pub file_id: Uuid,
    pub tenant_id: Uuid,
    pub backend_id: Uuid,
    pub file_path: String,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    pub mime_type: String,
    pub etag_pinned: String,
    pub upload_expires_at: Option<OffsetDateTime>,
    pub custom_metadata_json: String,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct ListFilesArgs {
    pub tenant_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct ListFilesPage {
    pub items: Vec<FileInfo>,
    pub next_cursor: Option<String>,
}

/// Outcome of a status / etag transition.
#[derive(Debug, Clone)]
pub enum ChangeStatusOutcome {
    Applied(FileInfo),
    NoMatch,
}

#[async_trait]
pub trait FilesRepo: Send + Sync {
    async fn insert_pending<C: DBRunner>(
        &self,
        runner: &C,
        args: InsertPendingArgs,
    ) -> Result<FileInfo, DomainError>;

    async fn get_by_id<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
    ) -> Result<Option<FileInfo>, DomainError>;

    async fn get_by_id_system<C: DBRunner>(
        &self,
        runner: &C,
        file_id: Uuid,
    ) -> Result<Option<FileInfo>, DomainError>;

    /// Phase 1 of `complete_upload`: `pending_upload → completing` via
    /// conditional UPDATE on `(file_id, status='pending_upload')`.
    async fn begin_complete_upload<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<MutationOutcome, DomainError>;

    /// Phase 3 of `complete_upload`: `completing → uploaded` writing the
    /// finalized `(etag, version_id, size_bytes)` and clearing
    /// `upload_expires_at`.
    async fn finish_complete_upload<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        new_etag: &str,
        new_version_id: Option<&str>,
        new_size_bytes: u64,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError>;

    /// Phase 1 of `put_file_info`: `uploaded → meta_updating` via
    /// conditional UPDATE that captures the row's current etag/version_id.
    async fn begin_meta_update<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: Option<&str>,
        old_version_id: Option<&str>,
        update: &FileMetaUpdate,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError>;

    /// Phase 3 of `put_file_info`: `meta_updating → uploaded` writing the
    /// new `(etag, version_id)` (whatever S3 returned from the
    /// `CopyObject` self-copy).
    async fn finish_meta_update<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        new_etag: &str,
        new_version_id: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError>;

    /// Phase 1 of `delete_file`: `uploaded → deleting`.
    async fn begin_delete<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: Option<&str>,
        old_version_id: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<MutationOutcome, DomainError>;

    /// Phase 3 of `delete_file`: hard DELETE of a `deleting` row.
    async fn finish_delete<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
    ) -> Result<MutationOutcome, DomainError>;

    /// Hard DELETE used by `abort_upload` (initial-upload variant).
    async fn delete_pending_upload<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
    ) -> Result<MutationOutcome, DomainError>;

    /// Roll a row's status and (etag, version_id) atomically — used by
    /// in-band recovery to flip transient states (`Completing` /
    /// `MetaUpdating`) back to `Uploaded` after a successful HEAD-and-sync.
    async fn rollforward_to_uploaded_system<C: DBRunner>(
        &self,
        runner: &C,
        file_id: Uuid,
        from_status: FileStatus,
        new_etag: &str,
        new_version_id: Option<&str>,
        new_size_bytes: u64,
        now: OffsetDateTime,
    ) -> Result<MutationOutcome, DomainError>;

    async fn list_paginated<C: DBRunner>(
        &self,
        runner: &C,
        args: ListFilesArgs,
    ) -> Result<ListFilesPage, DomainError>;
}
