use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::llm::AttachmentRef;
use crate::infra::db::entity::attachment::{Model as AttachmentModel, SecondaryUploadStatus};

/// Parameters for inserting a new attachment row in `pending` status.
#[domain_model]
pub struct InsertAttachmentParams {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub uploaded_by_user_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub storage_backend: String,
    pub attachment_kind: String,
    pub for_file_search: bool,
    pub for_code_interpreter: bool,
}

/// Parameters for CAS transition `pending → uploaded`.
///
/// `size_bytes` is the exact byte count observed during streaming upload,
/// set here because the size is unknown at INSERT time (streaming).
#[domain_model]
pub struct SetUploadedParams {
    pub id: Uuid,
    pub provider_file_id: String,
    pub size_bytes: i64,
}

/// Parameters for CAS transition `uploaded → ready`.
#[domain_model]
pub struct SetReadyParams {
    pub id: Uuid,
    /// Server-generated thumbnail bytes (WebP). `None` for documents or
    /// when thumbnail generation failed/was skipped.
    pub img_thumbnail: Option<Vec<u8>>,
    pub img_thumbnail_width: Option<i32>,
    pub img_thumbnail_height: Option<i32>,
}

/// Parameters for CAS transition `pending|uploaded → failed`.
#[domain_model]
pub struct SetFailedParams {
    pub id: Uuid,
    pub error_code: String,
    /// Expected source status (`"pending"` or `"uploaded"`).
    pub from_status: String,
}

/// Parameters for recording the outcome of a per-attachment secondary upload
/// (currently: Anthropic Files API; see `anthropic-provider-support.md` §8.1).
/// Status transitions: `not_attempted` → `pending` → `uploaded` | `failed`.
#[domain_model]
pub struct SetSecondaryUploadParams {
    pub id: Uuid,
    /// Provider-side file id when the upload succeeded; `None` on `pending`
    /// or `failed`.
    pub secondary_file_id: Option<String>,
    /// New status — must be `Pending`, `Uploaded`, or `Failed`. The repo
    /// rejects `NotAttempted` (set only at INSERT time).
    pub secondary_status: SecondaryUploadStatus,
    /// Provider-kind string (e.g. `"anthropic"`). Must be `Some` for
    /// `Pending` and `Uploaded`; may be `None` only when transitioning to
    /// `Failed` from a state where the provider was never recorded.
    pub secondary_provider_kind: Option<String>,
}

/// Repository trait for attachment persistence operations.
#[async_trait]
#[allow(dead_code, clippy::too_many_arguments)]
pub trait AttachmentRepository: Send + Sync {
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: InsertAttachmentParams,
    ) -> Result<AttachmentModel, DomainError>;
    async fn cas_set_uploaded<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetUploadedParams,
    ) -> Result<u64, DomainError>;
    /// Refresh `updated_at` of an `uploaded`, non-deleted row that no cleanup
    /// owns. The background indexing wait calls it on each poll so the upload
    /// reaper does not take a row that is still being indexed. Returns rows
    /// affected (0: the row is gone, no longer `uploaded`, or its chat was
    /// deleted).
    async fn touch_uploaded<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError>;
    /// CAS `uploaded` → `failed` with `error_code`, and mark the row for the
    /// attachment cleanup (`cleanup_status = pending`), so the provider file
    /// is deleted by the outbox cleanup with its retries. Only a row no
    /// cleanup owns yet is changed. Returns rows affected.
    async fn cas_fail_uploaded_for_cleanup<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
        error_code: &str,
    ) -> Result<u64, DomainError>;
    async fn cas_set_ready<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetReadyParams,
    ) -> Result<u64, DomainError>;
    async fn cas_set_failed<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetFailedParams,
    ) -> Result<u64, DomainError>;
    /// Update the secondary-upload state on an existing attachment.
    ///
    /// Independent of the primary `status` lifecycle — the row may be in any
    /// primary status when this is called. Returns the number of rows updated
    /// (0 if the attachment was not found or was soft-deleted).
    async fn set_secondary_upload<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        params: SetSecondaryUploadParams,
    ) -> Result<u64, DomainError>;
    /// Build a `provider_file_id → secondary_file_id` lookup map for a chat,
    /// restricted to attachments where `secondary_status = 'uploaded'` and
    /// `secondary_provider_kind = provider_kind`. Used by adapters that need
    /// to substitute provider-specific ids into outbound content blocks
    /// (e.g. Anthropic Messages API image/document blocks).
    async fn build_secondary_file_id_map<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
        provider_kind: &str,
    ) -> Result<HashMap<String, String>, DomainError>;
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<AttachmentModel>, DomainError>;
    async fn get_batch<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        ids: &[Uuid],
    ) -> Result<Vec<AttachmentModel>, DomainError>;
    async fn soft_delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<u64, DomainError>;
    async fn count_ready_documents<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError>;
    async fn count_documents<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError>;
    async fn sum_size_bytes<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<i64, DomainError>;
    async fn build_provider_file_id_map<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<HashMap<String, AttachmentRef>, DomainError>;
    /// Returns provider file IDs for all ready `code_interpreter` attachments in a chat.
    async fn get_code_interpreter_file_ids<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<Vec<String>, DomainError>;

    // ── Cleanup methods (no AccessScope — used by background workers) ───

    /// Load all attachments for a chat that still need provider cleanup.
    ///
    /// Filters: `chat_id AND cleanup_status = 'pending'`.
    async fn find_pending_cleanup_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
    ) -> Result<Vec<AttachmentModel>, DomainError>;

    /// Mark a single attachment's cleanup as done.
    ///
    /// CAS guard: only transitions from `pending`. Returns rows affected
    /// (0 if already terminal — idempotent).
    async fn mark_cleanup_done<C: DBRunner>(
        &self,
        runner: &C,
        attachment_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Record a retryable cleanup failure (atomic read-modify-write).
    ///
    /// Atomically increments `cleanup_attempts`, sets `last_cleanup_error` and
    /// `cleanup_updated_at`. If `cleanup_attempts` reaches `max_attempts`, transitions
    /// to `failed` instead of staying `pending`.
    async fn record_cleanup_attempt<C: DBRunner>(
        &self,
        runner: &C,
        attachment_id: Uuid,
        error: &str,
        max_attempts: u32,
    ) -> Result<crate::domain::repos::CleanupOutcome, DomainError>;

    /// Bulk-set `cleanup_status = 'pending'` for all active attachments of a chat.
    ///
    /// Filters: `chat_id AND cleanup_status IS NULL AND deleted_at IS NULL`.
    /// Used inside the chat-deletion transaction before the chat itself is soft-deleted.
    /// Returns count of rows updated.
    async fn mark_attachments_pending_for_chat<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
    ) -> Result<u64, DomainError>;

    /// Count attachments in terminal `failed` cleanup state for a chat.
    ///
    /// Used to emit a metric when vector store is deleted with failed attachments.
    async fn count_failed_cleanup_by_chat<C: DBRunner>(
        &self,
        runner: &C,
        chat_id: Uuid,
    ) -> Result<u64, DomainError>;
}
