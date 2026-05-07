//! `FilesRepo` trait — the persistence boundary for FileStorage.
//!
//! Owns every read/write against `file_storage.files` so the service layer
//! never touches SeaORM directly. The contract is shaped around the
//! optimistic-concurrency primitive — every mutation is a single
//! etag-conditional UPDATE that returns the row count (0 → caller lost the
//! race; 1 → caller won) — plus the partial unique index that uniqueness-
//! enforces last-write-wins on `(tenant_id, backend_id, file_path)` for
//! `status = 'uploaded'` rows.

use async_trait::async_trait;
use file_storage_sdk::{FileInfo, FileMetaUpdate, FileStatus};
use modkit_db::secure::DBRunner;
use time::OffsetDateTime;
use uuid::Uuid;

use super::error::DomainError;

/// Outcome of an etag-conditional UPDATE/DELETE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOutcome {
    /// Row updated. Caller wins.
    Applied,
    /// 0 rows matched. Caller lost the race or row moved.
    NoMatch,
}

/// Snapshot the repo returns when it inserts a new pending-upload row.
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
    /// Filter by owner principal. `None` = caller-default scope (caller
    /// resolves this before invoking the repo).
    pub owner_id: Option<Uuid>,
    pub backend_id: Option<Uuid>,
    pub mime_type: Option<String>,
    pub gts_file_type: Option<String>,
    pub created_after: Option<OffsetDateTime>,
    pub created_before: Option<OffsetDateTime>,
    pub cursor: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct ListFilesPage {
    pub items: Vec<FileInfo>,
    pub next_cursor: Option<String>,
}

/// Tag for the kind of late-arrival branch chosen by `change_status`.
#[derive(Debug, Clone)]
pub enum ChangeStatusOutcome {
    /// Standard etag-conditional UPDATE matched 1 row — caller wins.
    Applied(FileInfo),
    /// 0 rows matched the standard UPDATE.
    NoMatch,
}

#[derive(Debug, Clone)]
pub struct DeleteOutcome {
    pub outcome: MutationOutcome,
    /// `(backend_id, file_id)` of the deleted row, when the delete was
    /// applied. Used by the orphan-delete worker to derive the S3 key.
    pub backend_id: Option<Uuid>,
}

/// Internal-only projection over the row's persistence-layer fields.
#[derive(Debug, Clone)]
pub struct PersistenceFields {
    pub backend_id: Uuid,
    pub meta_revision: i64,
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

    async fn get_persistence_fields<C: DBRunner>(
        &self,
        runner: &C,
        file_id: Uuid,
    ) -> Result<Option<PersistenceFields>, DomainError>;

    async fn update_metadata_etag_conditional<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: &str,
        update: &FileMetaUpdate,
        new_content_hash: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError>;

    async fn change_status_with_supersession<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: &str,
        target: FileStatus,
        new_content_hash: &str,
        now: OffsetDateTime,
    ) -> Result<ChangeStatusOutcome, DomainError>;

    async fn delete_etag_conditional<C: DBRunner>(
        &self,
        runner: &C,
        tenant_id: Uuid,
        file_id: Uuid,
        old_etag: &str,
    ) -> Result<DeleteOutcome, DomainError>;

    async fn repair_etag_system_context<C: DBRunner>(
        &self,
        runner: &C,
        file_id: Uuid,
        old_etag: &str,
        new_etag: &str,
    ) -> Result<MutationOutcome, DomainError>;

    async fn list_paginated<C: DBRunner>(
        &self,
        runner: &C,
        args: ListFilesArgs,
    ) -> Result<ListFilesPage, DomainError>;
}
