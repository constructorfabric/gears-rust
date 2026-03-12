use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use bytes::Bytes;
use mini_chat_sdk::models::{Attachment, AttachmentKind};

use crate::domain::repos::attachment_repo::AttachmentWithProvider;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;

use uuid::Uuid;

use crate::config::MiniChatConfig;
use crate::domain::error::DomainError;
use crate::domain::repos::{
    AttachmentRepository, VectorStoreRepository, attachment_repo::NewAttachmentEntity,
};

use super::actions;
use super::resources;

/// Result of validating `attachment_ids` + `rag_attachment_ids` for a send-message request.
#[derive(Debug, Clone)]
pub struct ValidatedSendAttachments {
    /// Attachments from `attachment_ids` — these get persisted to `message_attachments`.
    pub message_attachments: Vec<AttachmentWithProvider>,
    /// Whether the chat has any ready documents (for `AllChat` retrieval scope fallback).
    pub chat_has_ready_documents: bool,
}

/// Supported content types for file uploads.
const SUPPORTED_DOCUMENT_TYPES: &[&str] = &[
    "application/pdf",
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/json",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
];

const SUPPORTED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp"];

/// Normalize a MIME content type: lowercase, strip parameters after `;`, trim.
fn normalize_content_type(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_supported_content_type(ct: &str) -> bool {
    SUPPORTED_DOCUMENT_TYPES.contains(&ct) || SUPPORTED_IMAGE_TYPES.contains(&ct)
}

/// Verify that the file's leading bytes (magic bytes) are consistent with the
/// declared `content_type`.  Only types with well-known signatures are checked;
/// text-based and Office Open XML formats are allowed through without sniffing
/// because they lack a single reliable magic sequence.
fn verify_magic_bytes(content_type: &str, data: &[u8]) -> bool {
    match content_type {
        // PNG: starts with 8-byte signature
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        // JPEG: starts with SOI marker
        "image/jpeg" => data.starts_with(&[0xFF, 0xD8, 0xFF]),
        // WebP: RIFF....WEBP
        "image/webp" => data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP",
        // PDF: starts with %PDF
        "application/pdf" => data.starts_with(b"%PDF"),
        // OOXML (docx/xlsx/pptx): ZIP archives starting with PK
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            data.starts_with(b"PK")
        }
        // Text-based formats — no reliable magic bytes, allow through
        "text/plain" | "text/markdown" | "text/csv" | "application/json" => true,
        // Unknown type — reject (should not reach here if is_supported_content_type is checked first)
        _ => false,
    }
}

/// Service handling file attachment operations.
///
/// Validation, permission checks, and DB persistence happen here.
/// Background processing (provider upload, thumbnails, summaries) is delegated
/// to the outbox-driven attachment handler for crash-safe processing.
#[domain_model]
pub struct AttachmentService<A: AttachmentRepository + 'static, V: VectorStoreRepository + 'static>
{
    db: Arc<super::DbProvider>,
    attachment_repo: Arc<A>,
    #[allow(dead_code)]
    vector_store_repo: Arc<V>,
    enforcer: PolicyEnforcer,
    config: Arc<MiniChatConfig>,
    attachment_outbox: Arc<dyn crate::domain::repos::AttachmentOutboxEnqueuer>,
}

impl<A: AttachmentRepository + 'static, V: VectorStoreRepository + 'static>
    AttachmentService<A, V>
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        db: Arc<super::DbProvider>,
        attachment_repo: Arc<A>,
        vector_store_repo: Arc<V>,
        enforcer: PolicyEnforcer,
        config: Arc<MiniChatConfig>,
        attachment_outbox: Arc<dyn crate::domain::repos::AttachmentOutboxEnqueuer>,
    ) -> Self {
        Self {
            db,
            attachment_repo,
            vector_store_repo,
            enforcer,
            config,
            attachment_outbox,
        }
    }

    /// Maximum upload size in bytes (from config).
    pub(crate) fn max_upload_size_bytes(&self) -> usize {
        self.config.attachments.max_upload_size_bytes
    }

    /// Upload an attachment to a chat.
    ///
    /// Returns 201 with status=pending immediately; background task processes the file.
    pub async fn upload_attachment(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        filename: String,
        content_type: String,
        size_bytes: i64,
        data: Bytes,
    ) -> Result<Attachment, DomainError> {
        // 1. AuthZ check
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &resources::CHAT,
                actions::UPLOAD_ATTACHMENT,
                Some(chat_id),
            )
            .await?;

        // 2. Normalize content_type (lowercase, strip MIME parameters)
        let content_type = normalize_content_type(&content_type);

        // 3. Validate file size (cheap check first)
        if size_bytes < 0 {
            return Err(DomainError::Validation {
                message: "size_bytes must be non-negative".into(),
            });
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        if (size_bytes as usize) > self.config.attachments.max_upload_size_bytes {
            return Err(DomainError::FileTooLarge {
                max_bytes: self.config.attachments.max_upload_size_bytes,
            });
        }

        // 4. Validate content_type
        if !is_supported_content_type(&content_type) {
            return Err(DomainError::UnsupportedFileType { content_type });
        }

        // 5. Verify magic bytes match the declared content type
        if !verify_magic_bytes(&content_type, &data) {
            return Err(DomainError::Validation {
                message: format!(
                    "file content does not match declared content type '{content_type}'"
                ),
            });
        }

        // 6. Enforce daily per-user upload quota (DESIGN §Error Catalogue: quota_exceeded / uploads).
        let max_daily = self.config.attachments.max_uploads_per_user_per_day;
        if max_daily > 0 {
            let conn = self.db.conn()?;
            let daily_count = self
                .attachment_repo
                .count_uploads_by_user_today(&conn, &scope, ctx.subject_id())
                .await?;
            #[allow(clippy::cast_possible_truncation)]
            if (daily_count as usize) >= max_daily {
                return Err(DomainError::UploadQuotaExceeded);
            }
        }

        // 7. Determine attachment kind
        let kind = AttachmentKind::from_content_type(&content_type);

        // 8. Verify chat exists, enforce per-chat limits, and INSERT — all in one
        //    transaction to prevent TOCTOU races on document count / byte limits.
        let attachment_id = Uuid::now_v7();
        let tenant_id = ctx.subject_tenant_id();
        let user_id = ctx.subject_id();
        let attachment_repo = Arc::clone(&self.attachment_repo);
        let config = Arc::clone(&self.config);

        let attachment = self
            .db
            .transaction(|tx| {
                let scope = scope.clone();
                let filename = filename.clone();
                let content_type = content_type.clone();
                Box::pin(async move {
                    let map = |e: DomainError| modkit_db::DbError::Other(anyhow::Error::new(e));

                    // 8a. Verify chat exists (prevents orphaned attachment rows)
                    {
                        use modkit_db::secure::{ScopeError, SecureEntityExt};
                        use sea_orm::EntityTrait;
                        let exists = crate::infra::db::entity::chat::Entity::find_by_id(chat_id)
                            .secure()
                            .scope_with(&scope)
                            .one(tx)
                            .await
                            .map_err(|e| match e {
                                ScopeError::Denied(msg) => DomainError::Forbidden {
                                    message: msg.to_owned(),
                                },
                                other => DomainError::Internal {
                                    message: format!("scope error: {other}"),
                                },
                            })
                            .map_err(map)?;
                        if exists.is_none() {
                            return Err(map(DomainError::ChatNotFound { id: chat_id }));
                        }
                    }

                    // 8b. If document: enforce per-chat RAG limits.
                    // NOTE: per-chat byte cap applies to documents only — images are
                    // not counted because they are not indexed in the vector store and
                    // are individually bounded by `max_upload_size_bytes`.
                    if kind == AttachmentKind::Document {
                        let doc_count = attachment_repo
                            .count_documents_by_chat(tx, &scope, chat_id)
                            .await
                            .map_err(map)?;
                        #[allow(clippy::cast_possible_truncation)]
                        if (doc_count as usize) >= config.attachments.max_documents_per_chat {
                            return Err(map(DomainError::DocumentLimitExceeded {
                                max: config.attachments.max_documents_per_chat,
                            }));
                        }

                        let total_bytes = attachment_repo
                            .total_document_bytes_by_chat(tx, &scope, chat_id)
                            .await
                            .map_err(map)?;
                        let max_bytes =
                            (config.attachments.max_total_upload_mb_per_chat as u64) * 1_048_576;
                        #[allow(clippy::cast_sign_loss)]
                        if total_bytes + (size_bytes as u64) > max_bytes {
                            return Err(map(DomainError::UploadSizeLimitExceeded {
                                max_mb: config.attachments.max_total_upload_mb_per_chat,
                            }));
                        }
                    }

                    // 8c. INSERT attachment metadata with status: Pending
                    let attachment = attachment_repo
                        .insert(
                            tx,
                            &scope,
                            NewAttachmentEntity {
                                id: attachment_id,
                                tenant_id,
                                chat_id,
                                uploaded_by_user_id: user_id,
                                filename,
                                content_type,
                                size_bytes,
                                storage_backend: config.storage_backend.clone(),
                                attachment_kind: kind.as_str().to_owned(),
                                upload_blob: Some(data.to_vec()),
                            },
                        )
                        .await
                        .map_err(map)?;

                    Ok(attachment)
                })
            })
            .await
            .map_err(|e| match e {
                modkit_db::DbError::Other(err) => match err.downcast::<DomainError>() {
                    Ok(domain_err) => domain_err,
                    Err(err) => DomainError::from(modkit_db::DbError::Other(err)),
                },
                other => DomainError::from(other),
            })?;

        // 9. Enqueue background processing via the outbox (crash-safe).
        // The upload_blob was stored in the attachment row during the transaction above.
        // The outbox handler will load it, process the file, and clear upload_blob.
        let processing_event = crate::domain::repos::AttachmentProcessingEvent {
            tenant_id,
            chat_id,
            attachment_id,
            attachment_kind: match kind {
                AttachmentKind::Image => "image".to_owned(),
                AttachmentKind::Document => "document".to_owned(),
            },
            filename,
            content_type,
            size_bytes: attachment.size_bytes,
            uploaded_by_user_id: ctx.subject_id(),
        };

        let conn = self.db.conn()?;
        self.attachment_outbox
            .enqueue_attachment_processing(&conn, processing_event)
            .await?;
        self.attachment_outbox.flush();

        Ok(attachment)
    }

    /// Get attachment metadata by ID.
    pub async fn get_attachment(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Attachment, DomainError> {
        // AuthZ check
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &resources::CHAT,
                actions::READ_ATTACHMENT,
                Some(chat_id),
            )
            .await?;

        let conn = self.db.conn()?;
        let attachment = self
            .attachment_repo
            .find_by_id(&conn, &scope, attachment_id)
            .await?
            .ok_or(DomainError::AttachmentNotFound { id: attachment_id })?;

        // Verify attachment belongs to the requested chat
        if attachment.chat_id != chat_id {
            return Err(DomainError::AttachmentNotFound { id: attachment_id });
        }

        Ok(attachment)
    }

    /// Validate attachments for downstream consumers (e.g., message sending).
    /// Verifies ownership, status=ready, returns kind + `provider_file_id`.
    #[allow(dead_code)]
    pub async fn validate_attachments(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        attachment_ids: &[Uuid],
    ) -> Result<Vec<AttachmentWithProvider>, DomainError> {
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &resources::CHAT,
                actions::READ_ATTACHMENT,
                Some(chat_id),
            )
            .await?;

        // Single connection for both chat-existence check and attachment lookup
        // to avoid TOCTOU gap (chat could be deleted between two separate calls).
        let conn = self.db.conn()?;
        {
            use modkit_db::secure::{ScopeError, SecureEntityExt};
            use sea_orm::EntityTrait;
            let exists = crate::infra::db::entity::chat::Entity::find_by_id(chat_id)
                .secure()
                .scope_with(&scope)
                .one(&conn)
                .await
                .map_err(|e| match e {
                    ScopeError::Denied(msg) => DomainError::Forbidden {
                        message: msg.to_owned(),
                    },
                    other => DomainError::Internal {
                        message: format!("scope error: {other}"),
                    },
                })?;
            if exists.is_none() {
                return Err(DomainError::ChatNotFound { id: chat_id });
            }
        }

        let attachments = self
            .attachment_repo
            .find_ready_by_ids(&conn, &scope, chat_id, attachment_ids)
            .await?;

        // Verify all requested IDs were found and ready
        if attachments.len() != attachment_ids.len() {
            let found_ids: std::collections::HashSet<Uuid> =
                attachments.iter().map(|a| a.attachment.id).collect();
            for id in attachment_ids {
                if !found_ids.contains(id) {
                    return Err(DomainError::AttachmentNotReady { id: *id });
                }
            }
        }

        Ok(attachments)
    }

    /// Validate `attachment_ids` and `rag_attachment_ids` for a send-message request.
    ///
    /// Enforces all RAG.md rules:
    /// - No duplicates within either array
    /// - No overlap between the two arrays
    /// - `rag_attachment_ids` must contain only documents (no images)
    /// - All IDs must belong to the same chat, tenant, and be `status=ready`
    ///
    /// Returns the validated attachments (from `attachment_ids` only — `rag_attachment_ids`
    /// are validated but not returned since they are not persisted to `message_attachments`).
    pub async fn validate_send_message_attachments(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        attachment_ids: &[Uuid],
        rag_attachment_ids: &[Uuid],
    ) -> Result<ValidatedSendAttachments, DomainError> {
        // 1. Check for duplicates within each array
        {
            let mut seen = std::collections::HashSet::new();
            for id in attachment_ids {
                if !seen.insert(id) {
                    return Err(DomainError::DuplicateAttachmentId { id: *id });
                }
            }
        }
        {
            let mut seen = std::collections::HashSet::new();
            for id in rag_attachment_ids {
                if !seen.insert(id) {
                    return Err(DomainError::DuplicateAttachmentId { id: *id });
                }
            }
        }

        // 2. Check for overlap between the two arrays
        if !attachment_ids.is_empty() && !rag_attachment_ids.is_empty() {
            let att_set: std::collections::HashSet<&Uuid> = attachment_ids.iter().collect();
            for id in rag_attachment_ids {
                if att_set.contains(id) {
                    return Err(DomainError::AttachmentIdOverlap { id: *id });
                }
            }
        }

        // 3. Validate all IDs exist, belong to chat, and are ready
        let all_ids: Vec<Uuid> = attachment_ids
            .iter()
            .chain(rag_attachment_ids.iter())
            .copied()
            .collect();

        let all_attachments = if all_ids.is_empty() {
            Vec::new()
        } else {
            self.validate_attachments(ctx, chat_id, &all_ids).await?
        };

        // 4. Check that rag_attachment_ids contains only documents
        let att_map: std::collections::HashMap<Uuid, &AttachmentWithProvider> = all_attachments
            .iter()
            .map(|a| (a.attachment.id, a))
            .collect();
        for id in rag_attachment_ids {
            if let Some(a) = att_map.get(id)
                && a.attachment.kind == AttachmentKind::Image
            {
                return Err(DomainError::ImageInRagScope { id: *id });
            }
        }

        // 5. Split: attachments for message_attachments vs rag-only
        let message_attachments: Vec<AttachmentWithProvider> = attachment_ids
            .iter()
            .filter_map(|id| att_map.get(id).copied().cloned())
            .collect();

        // Check if chat has any ready documents (for AllChat fallback)
        let conn = self.db.conn()?;
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &resources::CHAT,
                actions::READ_ATTACHMENT,
                Some(chat_id),
            )
            .await?;
        let chat_ready_docs = self
            .attachment_repo
            .find_ready_documents_by_chat(&conn, &scope, chat_id)
            .await?;

        Ok(ValidatedSendAttachments {
            message_attachments,
            chat_has_ready_documents: !chat_ready_docs.is_empty(),
        })
    }

    /// Get the provider `vector_store_id` for a chat (if one exists).
    /// Returns `None` if the chat has no vector store.
    pub async fn get_chat_vector_store_id(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
    ) -> Result<Option<String>, DomainError> {
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &resources::CHAT,
                actions::READ_ATTACHMENT,
                Some(chat_id),
            )
            .await?;
        let conn = self.db.conn()?;
        let tenant_id = ctx.subject_tenant_id();
        let store = self
            .vector_store_repo
            .find_by_chat(&conn, &scope, tenant_id, chat_id)
            .await?;
        Ok(store.and_then(|s| s.vector_store_id))
    }

    /// Mark all chat attachments for cleanup (called by chat deletion flow).
    #[allow(dead_code)]
    pub async fn mark_chat_attachments_for_cleanup(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
    ) -> Result<u64, DomainError> {
        let scope = self
            .enforcer
            .access_scope(ctx, &resources::CHAT, actions::DELETE, Some(chat_id))
            .await?;

        let conn = self.db.conn()?;
        self.attachment_repo
            .mark_for_cleanup(&conn, &scope, chat_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use authz_resolver_sdk::{
        AuthZResolverClient, AuthZResolverError, PolicyEnforcer,
        constraints::{Constraint, InPredicate, Predicate},
        models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
    };
    use bytes::Bytes;
    use mini_chat_sdk::models::{AttachmentKind, AttachmentStatus};
    use modkit_db::migration_runner::run_migrations_for_testing;
    use modkit_db::secure::SecureInsertExt;
    use modkit_db::{ConnectOpts, DBProvider, Db, connect_db};
    use modkit_security::{AccessScope, SecurityContext, pep_properties};
    use oagw_sdk::{Body, ServiceGatewayClientV1};
    use sea_orm::ActiveValue::Set;
    use sea_orm::EntityTrait;
    use sea_orm_migration::MigratorTrait;
    use time::OffsetDateTime;
    use uuid::Uuid;

    use modkit_macros::domain_model;

    use super::AttachmentService;
    use crate::config::{AttachmentConfig, MiniChatConfig};
    use crate::infra::db::migrations::Migrator;
    use crate::infra::db::repo::attachment_repo::AttachmentRepository;
    use crate::infra::db::repo::vector_store_repo::VectorStoreRepository;
    use crate::infra::llm::provider_resolver::ProviderResolver;
    use crate::infra::oagw::files_client::OagwFilesClient;
    use crate::infra::oagw::vector_store_client::OagwVectorStoreClient;
    /// No-op outbox enqueuer for tests — events are accepted but not processed.
    struct NoopAttachmentOutboxEnqueuer;

    #[async_trait]
    impl crate::domain::repos::attachment_outbox::AttachmentOutboxEnqueuer
        for NoopAttachmentOutboxEnqueuer
    {
        async fn enqueue_attachment_processing(
            &self,
            _runner: &(dyn modkit_db::secure::DBRunner + Sync),
            _event: crate::domain::repos::attachment_outbox::AttachmentProcessingEvent,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }
        fn flush(&self) {}
    }

    type DbProvider = DBProvider<modkit_db::DbError>;
    type TestService = AttachmentService<AttachmentRepository, VectorStoreRepository>;

    mod test_chat_entity {
        use modkit_db_macros::Scopable;
        use modkit_macros::domain_model;
        #[allow(clippy::wildcard_imports)]
        use sea_orm::entity::prelude::*;
        use time::OffsetDateTime;
        use uuid::Uuid;

        #[domain_model]
        #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
        #[sea_orm(table_name = "chats")]
        #[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
        #[allow(clippy::struct_field_names)]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub tenant_id: Uuid,
            pub user_id: Uuid,
            #[sea_orm(column_type = "String(StringLen::N(64))")]
            pub model: String,
            pub title: Option<String>,
            pub is_temporary: bool,
            pub created_at: OffsetDateTime,
            pub updated_at: OffsetDateTime,
            pub deleted_at: Option<OffsetDateTime>,
        }

        #[domain_model]
        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}
        impl ActiveModelBehavior for ActiveModel {}
    }

    async fn inmem_db() -> Db {
        let opts = ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = connect_db("sqlite::memory:", opts).await.expect("connect");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations");
        db
    }

    #[domain_model]
    struct AllowingAuthZResolver;

    #[async_trait]
    impl AuthZResolverClient for AllowingAuthZResolver {
        async fn evaluate(
            &self,
            request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            let root_id = request
                .context
                .tenant_context
                .as_ref()
                .and_then(|tc| tc.root_id)
                .or_else(|| {
                    request
                        .subject
                        .properties
                        .get("tenant_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                })
                .ok_or_else(|| {
                    AuthZResolverError::Internal("tenant context required".to_owned())
                })?;
            let predicates = vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [root_id],
            ))];
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint { predicates }],
                    ..Default::default()
                },
            })
        }
    }

    #[domain_model]
    struct DenyingAuthZResolver;

    #[async_trait]
    impl AuthZResolverClient for DenyingAuthZResolver {
        async fn evaluate(
            &self,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            })
        }
    }

    #[domain_model]
    #[allow(clippy::struct_excessive_bools)]
    struct MockGateway {
        fail_uploads: bool,
        fail_vector_store_create: bool,
        fail_vector_store_add: bool,
        vs_create_count: AtomicUsize,
    }

    impl MockGateway {
        fn new() -> Self {
            Self {
                fail_uploads: false,
                fail_vector_store_create: false,
                fail_vector_store_add: false,
                vs_create_count: AtomicUsize::new(0),
            }
        }
        fn failing_uploads() -> Self {
            Self {
                fail_uploads: true,
                ..Self::new()
            }
        }
        fn failing_vector_store_add() -> Self {
            Self {
                fail_vector_store_add: true,
                ..Self::new()
            }
        }
    }

    #[async_trait]
    impl ServiceGatewayClientV1 for MockGateway {
        async fn proxy_request(
            &self,
            _ctx: SecurityContext,
            req: http::Request<Body>,
        ) -> Result<http::Response<Body>, oagw_sdk::error::ServiceGatewayError> {
            let uri = req.uri().to_string();
            let method = req.method().as_str();
            if uri.contains("/v1/files") && method == "POST" {
                if self.fail_uploads {
                    return Ok(http::Response::builder()
                        .status(500)
                        .body(Body::Bytes(Bytes::from(r#"{"error":"server error"}"#)))
                        .unwrap());
                }
                let body = serde_json::json!({"id": format!("file-{}", Uuid::new_v4())});
                return Ok(http::Response::builder()
                    .status(200)
                    .body(Body::Bytes(Bytes::from(serde_json::to_vec(&body).unwrap())))
                    .unwrap());
            }
            if uri.contains("/v1/files/") && method == "DELETE" {
                return Ok(http::Response::builder()
                    .status(200)
                    .body(Body::Empty)
                    .unwrap());
            }
            if uri.contains("/v1/vector_stores") && !uri.contains("/files") && method == "POST" {
                self.vs_create_count.fetch_add(1, Ordering::SeqCst);
                if self.fail_vector_store_create {
                    return Ok(http::Response::builder()
                        .status(500)
                        .body(Body::Bytes(Bytes::from(r#"{"error":"vs create failed"}"#)))
                        .unwrap());
                }
                let body = serde_json::json!({"id": format!("vs-{}", Uuid::new_v4())});
                return Ok(http::Response::builder()
                    .status(200)
                    .body(Body::Bytes(Bytes::from(serde_json::to_vec(&body).unwrap())))
                    .unwrap());
            }
            if uri.contains("/v1/vector_stores/") && uri.contains("/files") && method == "POST" {
                if self.fail_vector_store_add {
                    return Ok(http::Response::builder()
                        .status(500)
                        .body(Body::Bytes(Bytes::from(r#"{"error":"vs add failed"}"#)))
                        .unwrap());
                }
                return Ok(http::Response::builder()
                    .status(200)
                    .body(Body::Bytes(Bytes::from(r#"{"id":"vsf-1"}"#)))
                    .unwrap());
            }
            if uri.contains("/v1/vector_stores/") && method == "DELETE" {
                return Ok(http::Response::builder()
                    .status(200)
                    .body(Body::Empty)
                    .unwrap());
            }
            Ok(http::Response::builder()
                .status(404)
                .body(Body::Empty)
                .unwrap())
        }
        async fn create_upstream(
            &self,
            _ctx: SecurityContext,
            _req: oagw_sdk::CreateUpstreamRequest,
        ) -> Result<oagw_sdk::Upstream, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _ctx: SecurityContext,
            _id: Uuid,
        ) -> Result<oagw_sdk::Upstream, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _ctx: SecurityContext,
            _query: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Upstream>, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _ctx: SecurityContext,
            _id: Uuid,
            _req: oagw_sdk::UpdateUpstreamRequest,
        ) -> Result<oagw_sdk::Upstream, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_upstream(
            &self,
            _ctx: SecurityContext,
            _id: Uuid,
        ) -> Result<(), oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn create_route(
            &self,
            _ctx: SecurityContext,
            _req: oagw_sdk::CreateRouteRequest,
        ) -> Result<oagw_sdk::Route, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn get_route(
            &self,
            _ctx: SecurityContext,
            _id: Uuid,
        ) -> Result<oagw_sdk::Route, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _ctx: SecurityContext,
            _upstream_id: Uuid,
            _query: &oagw_sdk::ListQuery,
        ) -> Result<Vec<oagw_sdk::Route>, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _ctx: SecurityContext,
            _id: Uuid,
            _req: oagw_sdk::UpdateRouteRequest,
        ) -> Result<oagw_sdk::Route, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn delete_route(
            &self,
            _ctx: SecurityContext,
            _id: Uuid,
        ) -> Result<(), oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn resolve_upstream(
            &self,
            _ctx: SecurityContext,
            _alias: &str,
        ) -> Result<oagw_sdk::Upstream, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
        async fn resolve_route(
            &self,
            _ctx: SecurityContext,
            _upstream_id: Uuid,
            _method: &str,
            _path: &str,
        ) -> Result<oagw_sdk::Route, oagw_sdk::error::ServiceGatewayError> {
            unimplemented!()
        }
    }

    fn test_ctx(tenant_id: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant_id)
            .build()
            .unwrap()
    }

    fn test_config() -> MiniChatConfig {
        MiniChatConfig {
            attachments: AttachmentConfig {
                max_upload_size_bytes: 1_000_000,
                max_documents_per_chat: 3,
                max_total_upload_mb_per_chat: 5,
                ..AttachmentConfig::default()
            },
            ..MiniChatConfig::default()
        }
    }

    async fn insert_chat(db: &DbProvider, tenant_id: Uuid, chat_id: Uuid) {
        use modkit_security::AccessScope;
        let now = OffsetDateTime::now_utc();
        let scope = AccessScope::for_tenant(tenant_id);
        let model = test_chat_entity::ActiveModel {
            id: Set(chat_id),
            tenant_id: Set(tenant_id),
            user_id: Set(Uuid::new_v4()),
            model: Set("gpt-4o".to_owned()),
            title: Set(None),
            is_temporary: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
        };
        test_chat_entity::Entity::insert(model.clone())
            .secure()
            .scope_with_model(&scope, &model)
            .unwrap()
            .exec(&db.conn().unwrap())
            .await
            .unwrap();
    }

    #[allow(clippy::needless_pass_by_value)]
    fn build_service(
        db: Db,
        gw: Arc<dyn ServiceGatewayClientV1>,
        authz: Arc<dyn AuthZResolverClient>,
        config: MiniChatConfig,
    ) -> (Arc<DbProvider>, TestService) {
        let db = Arc::new(DbProvider::new(db));
        let attachment_repo = Arc::new(AttachmentRepository);
        let vector_store_repo = Arc::new(VectorStoreRepository);
        let enforcer = PolicyEnforcer::new(authz);
        let oagw_files = Arc::new(OagwFilesClient::new(Arc::clone(&gw), "openai".to_owned()));
        let oagw_vector_stores = Arc::new(OagwVectorStoreClient::new(
            Arc::clone(&gw),
            "openai".to_owned(),
        ));
        let provider_resolver = Arc::new(ProviderResolver::new(&gw, config.providers.clone()));
        let config = Arc::new(config);

        // Create a no-op outbox enqueuer for tests (events are enqueued but not processed).
        let attachment_outbox: Arc<dyn crate::domain::repos::AttachmentOutboxEnqueuer> =
            Arc::new(NoopAttachmentOutboxEnqueuer);

        let service = AttachmentService::new(
            Arc::clone(&db),
            attachment_repo,
            vector_store_repo,
            enforcer,
            config,
            attachment_outbox,
        );
        (db, service)
    }

    fn default_service(db: Db) -> (Arc<DbProvider>, TestService) {
        let gw: Arc<dyn ServiceGatewayClientV1> = Arc::new(MockGateway::new());
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowingAuthZResolver);
        build_service(db, gw, authz, test_config())
    }

    async fn yield_background() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    /// Build test bytes with valid magic bytes for the given content type.
    fn test_bytes(content_type: &str, size: usize) -> Bytes {
        let header: &[u8] = match content_type {
            "application/pdf" => b"%PDF-1.4 ",
            "image/png" => b"\x89PNG\r\n\x1a\n",
            "image/jpeg" => &[0xFF, 0xD8, 0xFF, 0xE0],
            "image/webp" => b"RIFF\x00\x00\x00\x00WEBP",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
                b"PK\x03\x04"
            }
            // text types don't need magic bytes
            _ => b"",
        };
        let mut buf = Vec::with_capacity(size.max(header.len()));
        buf.extend_from_slice(header);
        if buf.len() < size {
            buf.resize(size, 0);
        }
        Bytes::from(buf)
    }

    #[tokio::test]
    async fn upload_document_returns_pending() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let attachment = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "report.pdf".to_owned(),
                "application/pdf".to_owned(),
                1024,
                test_bytes("application/pdf", 1024),
            )
            .await
            .unwrap();
        assert_eq!(attachment.status, AttachmentStatus::Pending);
        assert_eq!(attachment.kind, AttachmentKind::Document);
        assert_eq!(attachment.filename, "report.pdf");
    }

    #[tokio::test]
    async fn upload_image_returns_pending() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let attachment = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "photo.png".to_owned(),
                "image/png".to_owned(),
                5000,
                test_bytes("image/png", 5000),
            )
            .await
            .unwrap();
        assert_eq!(attachment.status, AttachmentStatus::Pending);
        assert_eq!(attachment.kind, AttachmentKind::Image);
    }

    #[tokio::test]
    async fn upload_unsupported_content_type_rejected() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "video.mp4".to_owned(),
                "video/mp4".to_owned(),
                1024,
                Bytes::from(vec![0u8; 1024]),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::UnsupportedFileType { .. })
        ));
    }

    #[tokio::test]
    async fn upload_file_too_large_rejected() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "huge.pdf".to_owned(),
                "application/pdf".to_owned(),
                2_000_000,
                test_bytes("application/pdf", 100),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::FileTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn upload_document_limit_exceeded() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for i in 0..3 {
            svc.upload_attachment(
                &ctx,
                chat_id,
                format!("doc{i}.pdf"),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();
        }
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "doc4.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::DocumentLimitExceeded { max: 3 })
        ));
    }

    #[tokio::test]
    async fn upload_daily_quota_exceeded() {
        let config = MiniChatConfig {
            attachments: AttachmentConfig {
                max_uploads_per_user_per_day: 3,
                ..AttachmentConfig::default()
            },
            ..MiniChatConfig::default()
        };
        let gw: Arc<dyn ServiceGatewayClientV1> = Arc::new(MockGateway::new());
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowingAuthZResolver);
        let (db, svc) = build_service(inmem_db().await, gw, authz, config);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for i in 0..3 {
            svc.upload_attachment(
                &ctx,
                chat_id,
                format!("file{i}.pdf"),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();
        }
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "file4.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::UploadQuotaExceeded)
        ));
    }

    #[tokio::test]
    async fn upload_total_mb_exceeded() {
        let config = MiniChatConfig {
            attachments: AttachmentConfig {
                max_upload_size_bytes: 5_000_000,
                max_documents_per_chat: 10,
                max_total_upload_mb_per_chat: 5,
                ..AttachmentConfig::default()
            },
            ..MiniChatConfig::default()
        };
        let gw: Arc<dyn ServiceGatewayClientV1> = Arc::new(MockGateway::new());
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowingAuthZResolver);
        let (db, svc) = build_service(inmem_db().await, gw, authz, config);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        svc.upload_attachment(
            &ctx,
            chat_id,
            "big.pdf".to_owned(),
            "application/pdf".to_owned(),
            4 * 1_048_576,
            test_bytes("application/pdf", 100),
        )
        .await
        .unwrap();
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "another.pdf".to_owned(),
                "application/pdf".to_owned(),
                2 * 1_048_576,
                test_bytes("application/pdf", 100),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::UploadSizeLimitExceeded { .. })
        ));
    }

    #[tokio::test]
    async fn image_upload_not_subject_to_document_limits() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for i in 0..3 {
            svc.upload_attachment(
                &ctx,
                chat_id,
                format!("doc{i}.pdf"),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();
        }
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "photo.png".to_owned(),
                "image/png".to_owned(),
                5000,
                test_bytes("image/png", 100),
            )
            .await;
        assert!(result.is_ok(), "image upload should bypass document limits");
    }

    #[tokio::test]
    async fn upload_stores_storage_backend_from_config() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let attachment = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "test.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();
        assert_eq!(attachment.storage_backend, "azure");
    }

    #[tokio::test]
    async fn upload_authz_denied() {
        let gw: Arc<dyn ServiceGatewayClientV1> = Arc::new(MockGateway::new());
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(DenyingAuthZResolver);
        let (db, svc) = build_service(inmem_db().await, gw, authz, test_config());
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "test.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::Forbidden { .. })
        ));
    }

    #[tokio::test]
    async fn get_attachment_returns_found() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let uploaded = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "test.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();
        let found = svc
            .get_attachment(&ctx, chat_id, uploaded.id)
            .await
            .unwrap();
        assert_eq!(found.id, uploaded.id);
        assert_eq!(found.filename, "test.pdf");
    }

    #[tokio::test]
    async fn get_attachment_not_found() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let result = svc.get_attachment(&ctx, chat_id, Uuid::new_v4()).await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::AttachmentNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn get_attachment_wrong_chat_returns_not_found() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_a = Uuid::new_v4();
        let chat_b = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_a).await;
        insert_chat(&db, tenant_id, chat_b).await;
        let uploaded = svc
            .upload_attachment(
                &ctx,
                chat_a,
                "test.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();
        let result = svc.get_attachment(&ctx, chat_b, uploaded.id).await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::AttachmentNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn validate_attachments_empty_list_returns_empty() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let result = svc.validate_attachments(&ctx, chat_id, &[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn validate_attachments_pending_returns_not_ready() {
        use crate::domain::repos::AttachmentRepository as _;
        use crate::domain::repos::attachment_repo::NewAttachmentEntity;

        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        // Insert directly via repo to avoid the background task that would mark it Ready.
        let repo = AttachmentRepository;
        let scope = svc
            .enforcer
            .access_scope(
                &ctx,
                &super::resources::CHAT,
                super::actions::UPLOAD_ATTACHMENT,
                Some(chat_id),
            )
            .await
            .unwrap();
        let attachment = repo
            .insert(
                &db.conn().unwrap(),
                &scope,
                NewAttachmentEntity {
                    id: Uuid::now_v7(),
                    tenant_id,
                    chat_id,
                    uploaded_by_user_id: ctx.subject_id(),
                    filename: "test.pdf".to_owned(),
                    content_type: "application/pdf".to_owned(),
                    size_bytes: 100,
                    storage_backend: "oagw".to_owned(),
                    attachment_kind: "document".to_owned(),
                    upload_blob: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(attachment.status, AttachmentStatus::Pending);

        let result = svc
            .validate_attachments(&ctx, chat_id, &[attachment.id])
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::AttachmentNotReady { .. })
        ));
    }

    #[tokio::test]
    async fn validate_ready_attachments_returns_with_provider_ids() {
        use crate::domain::repos::AttachmentRepository as _;

        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        // Insert attachment in pending state, then manually transition to ready
        // with a provider_file_id (simulating what the outbox handler does).
        let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant_id);
        let att = repo
            .insert(
                &conn,
                &scope,
                crate::domain::repos::attachment_repo::NewAttachmentEntity {
                    id: Uuid::now_v7(),
                    tenant_id,
                    chat_id,
                    uploaded_by_user_id: ctx.subject_id(),
                    filename: "test.pdf".to_owned(),
                    content_type: "application/pdf".to_owned(),
                    size_bytes: 100,
                    storage_backend: "oagw".to_owned(),
                    attachment_kind: "document".to_owned(),
                    upload_blob: None,
                },
            )
            .await
            .unwrap();
        repo.update_status(
            &conn,
            &scope,
            att.id,
            AttachmentStatus::Ready,
            Some("file-test-123".to_owned()),
            None,
        )
        .await
        .unwrap();

        let validated = svc
            .validate_attachments(&ctx, chat_id, &[att.id])
            .await
            .unwrap();
        assert_eq!(validated.len(), 1);
        assert!(!validated[0].provider_file_id.is_empty());
        assert_eq!(validated[0].attachment.id, att.id);
    }

    #[tokio::test]
    async fn mark_for_cleanup_returns_count() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for i in 0..3 {
            svc.upload_attachment(
                &ctx,
                chat_id,
                format!("file{i}.pdf"),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();
        }
        let count = svc
            .mark_chat_attachments_for_cleanup(&ctx, chat_id)
            .await
            .unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn mark_for_cleanup_empty_chat() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let count = svc
            .mark_chat_attachments_for_cleanup(&ctx, chat_id)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn document_upload_stays_pending_with_outbox() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let uploaded = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "report.pdf".to_owned(),
                "application/pdf".to_owned(),
                512,
                test_bytes("application/pdf", 512),
            )
            .await
            .unwrap();
        // With outbox pattern, upload returns pending; processing happens asynchronously.
        assert_eq!(uploaded.status, AttachmentStatus::Pending);
        let found = svc
            .get_attachment(&ctx, chat_id, uploaded.id)
            .await
            .unwrap();
        assert_eq!(found.status, AttachmentStatus::Pending);
    }

    #[tokio::test]
    async fn upload_with_failing_provider_still_enqueues_pending() {
        // With outbox pattern, provider failures happen during outbox processing,
        // not during the upload request. Upload always returns pending.
        let gw: Arc<dyn ServiceGatewayClientV1> = Arc::new(MockGateway::failing_uploads());
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowingAuthZResolver);
        let (db, svc) = build_service(inmem_db().await, gw, authz, test_config());
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let uploaded = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "report.pdf".to_owned(),
                "application/pdf".to_owned(),
                512,
                test_bytes("application/pdf", 512),
            )
            .await
            .unwrap();
        assert_eq!(uploaded.status, AttachmentStatus::Pending);
    }

    #[tokio::test]
    async fn image_upload_stays_pending_with_outbox() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let uploaded = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "photo.png".to_owned(),
                "image/png".to_owned(),
                100,
                test_bytes("image/png", 100),
            )
            .await
            .unwrap();
        assert_eq!(uploaded.status, AttachmentStatus::Pending);
        assert_eq!(uploaded.kind, AttachmentKind::Image);
    }

    #[tokio::test]
    async fn document_upload_stores_upload_blob() {
        // Verify that upload_blob is stored in the attachment row for outbox processing.
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let uploaded = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "report.pdf".to_owned(),
                "application/pdf".to_owned(),
                512,
                test_bytes("application/pdf", 512),
            )
            .await
            .unwrap();
        assert_eq!(uploaded.status, AttachmentStatus::Pending);

        // Verify upload_blob is stored (via load_upload_blob repo method)
        use crate::domain::repos::AttachmentRepository as _;
        let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant_id);
        let blob = repo
            .load_upload_blob(&conn, &scope, uploaded.id)
            .await
            .unwrap();
        assert!(blob.is_some(), "upload_blob should be stored after upload");
        assert_eq!(blob.unwrap().len(), 512);
    }

    #[tokio::test]
    async fn all_supported_document_types_accepted() {
        let config = MiniChatConfig {
            attachments: AttachmentConfig {
                max_documents_per_chat: 50,
                ..test_config().attachments
            },
            ..test_config()
        };
        let gw: Arc<dyn ServiceGatewayClientV1> = Arc::new(MockGateway::new());
        let authz: Arc<dyn AuthZResolverClient> = Arc::new(AllowingAuthZResolver);
        let (db, svc) = build_service(inmem_db().await, gw, authz, config);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for ct in [
            "application/pdf",
            "text/plain",
            "text/markdown",
            "text/csv",
            "application/json",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ] {
            let result = svc
                .upload_attachment(
                    &ctx,
                    chat_id,
                    format!("file.{ct}"),
                    ct.to_owned(),
                    100,
                    test_bytes(ct, 100),
                )
                .await;
            assert!(result.is_ok(), "{ct} should be accepted");
            assert_eq!(result.unwrap().kind, AttachmentKind::Document);
        }
    }

    #[tokio::test]
    async fn all_supported_image_types_accepted() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for ct in ["image/png", "image/jpeg", "image/webp"] {
            let result = svc
                .upload_attachment(
                    &ctx,
                    chat_id,
                    format!("img.{ct}"),
                    ct.to_owned(),
                    100,
                    test_bytes(ct, 100),
                )
                .await;
            assert!(result.is_ok(), "{ct} should be accepted");
            assert_eq!(result.unwrap().kind, AttachmentKind::Image);
        }
    }

    #[tokio::test]
    async fn unsupported_types_rejected() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        for ct in [
            "video/mp4",
            "image/gif",
            "application/octet-stream",
            "audio/mpeg",
        ] {
            let result = svc
                .upload_attachment(
                    &ctx,
                    chat_id,
                    format!("file.{ct}"),
                    ct.to_owned(),
                    100,
                    Bytes::from(vec![0u8; 100]),
                )
                .await;
            assert!(
                matches!(
                    result,
                    Err(crate::domain::error::DomainError::UnsupportedFileType { .. })
                ),
                "{ct} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn upload_pdf_with_wrong_magic_bytes_rejected() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        // Send PNG magic bytes but declare content type as PDF
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "fake.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("image/png", 100),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::Validation { .. })
        ));
    }

    #[tokio::test]
    async fn upload_image_with_wrong_magic_bytes_rejected() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        // Send PDF magic bytes but declare content type as PNG
        let result = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "fake.png".to_owned(),
                "image/png".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await;
        assert!(matches!(
            result,
            Err(crate::domain::error::DomainError::Validation { .. })
        ));
    }

    #[test]
    fn verify_magic_bytes_correctness() {
        use super::verify_magic_bytes;
        // Positive cases
        assert!(verify_magic_bytes(
            "image/png",
            b"\x89PNG\r\n\x1a\n rest of file"
        ));
        assert!(verify_magic_bytes(
            "image/jpeg",
            &[0xFF, 0xD8, 0xFF, 0xE0, 0x00]
        ));
        assert!(verify_magic_bytes(
            "image/webp",
            b"RIFF\x00\x00\x00\x00WEBP"
        ));
        assert!(verify_magic_bytes("application/pdf", b"%PDF-1.7 content"));
        assert!(verify_magic_bytes(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            b"PK\x03\x04data"
        ));
        assert!(verify_magic_bytes("text/plain", b"any content"));
        assert!(verify_magic_bytes("application/json", b"{}"));
        // Negative cases
        assert!(!verify_magic_bytes("image/png", b"not a png"));
        assert!(!verify_magic_bytes("image/jpeg", b"\x00\x00\x00"));
        assert!(!verify_magic_bytes(
            "image/webp",
            b"RIFF\x00\x00\x00\x00NOPE"
        ));
        assert!(!verify_magic_bytes("application/pdf", b"<html>"));
        assert!(!verify_magic_bytes(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            b"not zip"
        ));
        // Empty data
        assert!(!verify_magic_bytes("image/png", b""));
        assert!(!verify_magic_bytes("application/pdf", b""));
    }

    #[test]
    fn normalize_content_type_strips_params_and_lowercases() {
        use super::normalize_content_type;
        assert_eq!(normalize_content_type("image/png"), "image/png");
        assert_eq!(
            normalize_content_type("image/png; charset=binary"),
            "image/png"
        );
        assert_eq!(normalize_content_type("Image/PNG"), "image/png");
        assert_eq!(
            normalize_content_type("  Application/PDF ; foo=bar "),
            "application/pdf"
        );
        assert_eq!(normalize_content_type(""), "");
    }

    #[test]
    fn is_supported_after_normalization() {
        use super::{is_supported_content_type, normalize_content_type};
        // These would fail without normalization
        assert!(is_supported_content_type(&normalize_content_type(
            "image/png; charset=binary"
        )));
        assert!(is_supported_content_type(&normalize_content_type(
            "Image/JPEG"
        )));
        assert!(is_supported_content_type(&normalize_content_type(
            "APPLICATION/PDF"
        )));
        // Still rejects unsupported types
        assert!(!is_supported_content_type(&normalize_content_type(
            "video/mp4"
        )));
    }

    // ── validate_send_message_attachments tests ─────────────────────────

    /// Helper: insert a ready attachment with provider_file_id directly via repo.
    async fn insert_ready_attachment(
        db: &DbProvider,
        tenant_id: Uuid,
        chat_id: Uuid,
        user_id: Uuid,
        kind: &str,
    ) -> mini_chat_sdk::models::Attachment {
        use crate::domain::repos::AttachmentRepository as _;
        let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
        let conn = db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant_id);
        let att = repo
            .insert(
                &conn,
                &scope,
                crate::domain::repos::attachment_repo::NewAttachmentEntity {
                    id: Uuid::now_v7(),
                    tenant_id,
                    chat_id,
                    uploaded_by_user_id: user_id,
                    filename: format!("file.{kind}"),
                    content_type: if kind == "image" {
                        "image/png".to_owned()
                    } else {
                        "application/pdf".to_owned()
                    },
                    size_bytes: 100,
                    storage_backend: "oagw".to_owned(),
                    attachment_kind: kind.to_owned(),
                    upload_blob: None,
                },
            )
            .await
            .unwrap();
        repo.update_status(
            &conn,
            &scope,
            att.id,
            AttachmentStatus::Ready,
            Some(format!("file-{}", att.id)),
            None,
        )
        .await
        .unwrap();
        repo.find_by_id(&conn, &scope, att.id)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn validate_send_msg_duplicate_in_attachment_ids() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let att =
            insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "document").await;

        let err = svc
            .validate_send_message_attachments(&ctx, chat_id, &[att.id, att.id], &[])
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::domain::error::DomainError::DuplicateAttachmentId { id } if id == att.id),
            "expected DuplicateAttachmentId, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_send_msg_duplicate_in_rag_ids() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let doc =
            insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "document").await;

        let err = svc
            .validate_send_message_attachments(&ctx, chat_id, &[], &[doc.id, doc.id])
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::domain::error::DomainError::DuplicateAttachmentId { id } if id == doc.id),
            "expected DuplicateAttachmentId, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_send_msg_overlap_between_arrays() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let doc =
            insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "document").await;

        let err = svc
            .validate_send_message_attachments(&ctx, chat_id, &[doc.id], &[doc.id])
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::domain::error::DomainError::AttachmentIdOverlap { id } if id == doc.id),
            "expected AttachmentIdOverlap, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_send_msg_image_in_rag_rejected() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let img = insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "image").await;

        let err = svc
            .validate_send_message_attachments(&ctx, chat_id, &[], &[img.id])
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::domain::error::DomainError::ImageInRagScope { id } if id == img.id),
            "expected ImageInRagScope, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_send_msg_not_ready_rejected() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        // Insert a pending (not ready) attachment
        let uploaded = svc
            .upload_attachment(
                &ctx,
                chat_id,
                "test.pdf".to_owned(),
                "application/pdf".to_owned(),
                100,
                test_bytes("application/pdf", 100),
            )
            .await
            .unwrap();

        let err = svc
            .validate_send_message_attachments(&ctx, chat_id, &[uploaded.id], &[])
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                crate::domain::error::DomainError::AttachmentNotReady { .. }
            ),
            "expected AttachmentNotReady, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn validate_send_msg_happy_path_docs_only() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let doc1 =
            insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "document").await;
        let doc2 =
            insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "document").await;

        let result = svc
            .validate_send_message_attachments(&ctx, chat_id, &[doc1.id, doc2.id], &[])
            .await
            .unwrap();
        assert_eq!(result.message_attachments.len(), 2);
        assert!(result.chat_has_ready_documents);
    }

    #[tokio::test]
    async fn validate_send_msg_happy_path_with_rag() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;
        let img = insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "image").await;
        let doc =
            insert_ready_attachment(&db, tenant_id, chat_id, ctx.subject_id(), "document").await;

        let result = svc
            .validate_send_message_attachments(&ctx, chat_id, &[img.id], &[doc.id])
            .await
            .unwrap();
        // message_attachments only contains the image (from attachment_ids)
        assert_eq!(result.message_attachments.len(), 1);
        assert_eq!(result.message_attachments[0].attachment.id, img.id);
        assert!(result.chat_has_ready_documents);
    }

    #[tokio::test]
    async fn validate_send_msg_empty_arrays() {
        let (db, svc) = default_service(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let ctx = test_ctx(tenant_id);
        insert_chat(&db, tenant_id, chat_id).await;

        let result = svc
            .validate_send_message_attachments(&ctx, chat_id, &[], &[])
            .await
            .unwrap();
        assert!(result.message_attachments.is_empty());
        assert!(!result.chat_has_ready_documents);
    }
}
