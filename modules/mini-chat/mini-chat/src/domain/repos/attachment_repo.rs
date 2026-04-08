use std::collections::HashMap;

use async_trait::async_trait;
use modkit_db::secure::DBRunner;
use modkit_macros::domain_model;
use modkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::llm::AttachmentRef;
use crate::infra::db::entity::attachment::Model as AttachmentModel;

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
}

/// Parameters for CAS transition `pending|uploaded → failed`.
#[domain_model]
pub struct SetFailedParams {
    pub id: Uuid,
    pub error_code: String,
    /// Expected source status (`"pending"` or `"uploaded"`).
    pub from_status: String,
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
}
