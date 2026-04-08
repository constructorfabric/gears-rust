use std::collections::HashMap;
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::config::RagConfig;
use crate::domain::error::DomainError;
use crate::domain::mime_validation::{AttachmentKind, AttachmentPurpose};
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::ports::metric_labels::{kind as kind_label, upload_result};
use crate::domain::ports::{
    AddFileToVectorStoreParams, FileStorageProvider, UploadFileParams, VectorStoreProvider,
};
use crate::domain::repos::{
    AttachmentRepository, ChatRepository, InsertVectorStoreParams, ModelResolver, OutboxEnqueuer,
    VectorStoreRepository,
};
use crate::infra::db::entity::attachment::Model as AttachmentModel;
use crate::infra::llm::provider_resolver::ProviderResolver;

use super::DbProvider;

// ── RAII guard for attachments_pending gauge ─────────────────────────────

/// Ensures `decrement_attachments_pending` is always called, even on early
/// returns or `?`-propagation. Call `defuse()` on the happy path to perform
/// an explicit decrement and disarm the Drop guard.
#[domain_model]
struct PendingGuard {
    metrics: Arc<dyn MiniChatMetricsPort>,
    armed: bool,
}

impl PendingGuard {
    fn new(metrics: &Arc<dyn MiniChatMetricsPort>) -> Self {
        metrics.increment_attachments_pending();
        Self {
            metrics: Arc::clone(metrics),
            armed: true,
        }
    }

    /// Explicit decrement + disarm (happy path).
    fn defuse(mut self) {
        self.armed = false;
        self.metrics.decrement_attachments_pending();
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            self.metrics.decrement_attachments_pending();
        }
    }
}

// ── Upload limits ────────────────────────────────────────────────────────

/// Effective per-file size limits resolved from config + CCM per-model.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct UploadLimits {
    pub max_file_bytes: u64,
    pub max_image_bytes: u64,
}

/// Code interpreter availability for uploads.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeInterpreterStatus {
    /// Model supports CI and kill switch is off.
    Allowed,
    /// Model does not support CI or kill switch is on.
    Denied,
    /// Model resolution failed transiently — cannot determine CI support.
    Unknown,
}

/// Pre-resolved context returned by `get_upload_context` so that
/// `upload_file` can skip the duplicate authz + model resolution.
#[domain_model]
pub struct UploadContext {
    pub scope: AccessScope,
    pub provider_id: String,
    pub storage_backend: String,
    pub limits: UploadLimits,
    /// Whether `text/csv` uploads should be remapped to `text/plain`.
    pub allow_csv_upload: bool,
    /// Whether the resolved model supports `code_interpreter` and the kill
    /// switch is not active. Pre-resolved to avoid duplicate model lookups.
    /// `Unknown` when model resolution failed transiently.
    pub code_interpreter_status: CodeInterpreterStatus,
}

// ── Error helpers for transaction boundary crossing ─────────────────────
// Follows the mutation_to_db_err / unwrap_mutation_err pattern from turn_service.rs.

#[allow(de0309_must_have_domain_model)]
#[derive(Debug, thiserror::Error)]
enum AttachmentMutationError {
    #[error("document limit exceeded: {message}")]
    DocumentLimitExceeded { message: String },
    #[error("storage limit exceeded: {message}")]
    StorageLimitExceeded { message: String },
    #[error("chat not found: {chat_id}")]
    ChatNotFound { chat_id: Uuid },
}

fn mutation_to_db_err(e: AttachmentMutationError) -> modkit_db::DbError {
    modkit_db::DbError::Other(anyhow::Error::new(e))
}

fn unwrap_mutation_err(e: modkit_db::DbError) -> DomainError {
    match e {
        modkit_db::DbError::Other(anyhow_err) => {
            match anyhow_err.downcast::<AttachmentMutationError>() {
                Ok(me) => match me {
                    AttachmentMutationError::DocumentLimitExceeded { message } => {
                        DomainError::DocumentLimitExceeded { message }
                    }
                    AttachmentMutationError::StorageLimitExceeded { message } => {
                        DomainError::StorageLimitExceeded { message }
                    }
                    AttachmentMutationError::ChatNotFound { chat_id } => {
                        DomainError::chat_not_found(chat_id)
                    }
                },
                Err(other) => DomainError::database(other.to_string()),
            }
        }
        other => DomainError::database(other.to_string()),
    }
}

/// Service handling file attachment operations.
#[domain_model]
pub struct AttachmentService<
    CR: ChatRepository,
    AR: AttachmentRepository + 'static,
    VSR: VectorStoreRepository + 'static,
> {
    db: Arc<DbProvider>,
    attachment_repo: Arc<AR>,
    chat_repo: Arc<CR>,
    vector_store_repo: Arc<VSR>,
    outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
    enforcer: PolicyEnforcer,
    file_storage: Arc<dyn FileStorageProvider>,
    vector_store: Arc<dyn VectorStoreProvider>,
    provider_resolver: Arc<ProviderResolver>,
    model_resolver: Arc<dyn ModelResolver>,
    rag_config: RagConfig,
    metrics: Arc<dyn MiniChatMetricsPort>,
}

impl<
    CR: ChatRepository + 'static,
    AR: AttachmentRepository + 'static,
    VSR: VectorStoreRepository + 'static,
> AttachmentService<CR, AR, VSR>
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        db: Arc<DbProvider>,
        attachment_repo: Arc<AR>,
        chat_repo: Arc<CR>,
        vector_store_repo: Arc<VSR>,
        outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
        enforcer: PolicyEnforcer,
        file_storage: Arc<dyn FileStorageProvider>,
        vector_store: Arc<dyn VectorStoreProvider>,
        provider_resolver: Arc<ProviderResolver>,
        model_resolver: Arc<dyn ModelResolver>,
        rag_config: RagConfig,
        metrics: Arc<dyn MiniChatMetricsPort>,
    ) -> Self {
        Self {
            db,
            attachment_repo,
            chat_repo,
            vector_store_repo,
            outbox_enqueuer,
            enforcer,
            file_storage,
            vector_store,
            provider_resolver,
            model_resolver,
            rag_config,
            metrics,
        }
    }

    /// Resolve the effective upload size limits for a chat.
    ///
    /// Performs authz + chat ownership + model resolution, then computes
    /// `min(ConfigMap, CCM per-model)` for each kind. Returns an
    /// `UploadContext` that `upload_file` can reuse (no double-authz).
    ///
    /// Falls back to ConfigMap-only limits if model resolution fails
    /// (e.g., CCM snapshot unavailable).
    pub(crate) async fn get_upload_context(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
    ) -> Result<UploadContext, DomainError> {
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &super::resources::CHAT,
                super::actions::UPLOAD_ATTACHMENT,
                Some(chat_id),
            )
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;
        let chat_scope = scope.ensure_owner(ctx.subject_id());
        let chat = self
            .chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await?
            .ok_or_else(|| DomainError::not_found("Chat", chat_id))?;

        // ConfigMap ceiling (always available).
        let config_file_bytes = u64::from(self.rag_config.uploaded_file_max_size_kb) * 1024;
        let config_image_bytes = u64::from(self.rag_config.uploaded_image_max_size_kb) * 1024;

        // CCM per-model limit (best-effort — fall back to ConfigMap on failure).
        let (provider_id, storage_backend, ccm_bytes, model_supports_ci) =
            self.resolve_model_limits(ctx, chat_id, chat.model).await;

        let code_interpreter_status = self
            .resolve_ci_status(ctx, chat_id, model_supports_ci)
            .await;

        let limits = UploadLimits {
            max_file_bytes: ccm_bytes.map_or(config_file_bytes, |ccm| config_file_bytes.min(ccm)),
            max_image_bytes: ccm_bytes
                .map_or(config_image_bytes, |ccm| config_image_bytes.min(ccm)),
        };

        Ok(UploadContext {
            scope,
            provider_id,
            storage_backend,
            limits,
            allow_csv_upload: self.rag_config.allow_csv_upload,
            code_interpreter_status,
        })
    }

    /// Resolve provider, storage backend, per-model byte limit, and CI support
    /// from the model catalog. Falls back to ConfigMap-only on transient failure.
    async fn resolve_model_limits(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        model: String,
    ) -> (String, String, Option<u64>, Option<bool>) {
        match self
            .model_resolver
            .resolve_model(ctx.subject_id(), Some(model))
            .await
        {
            Ok(resolved) => {
                let backend = self
                    .provider_resolver
                    .resolve_storage_backend(&resolved.provider_id);
                let ccm = u64::from(resolved.max_file_size_mb) * 1_048_576;
                let ci = Some(resolved.tool_support.code_interpreter);
                (resolved.provider_id, backend, Some(ccm), ci)
            }
            Err(e) => {
                tracing::warn!(
                    chat_id = %chat_id,
                    error = %e,
                    "model resolution failed for upload limits; using ConfigMap only"
                );
                let fallback_provider = "openai".to_owned();
                let backend = self
                    .provider_resolver
                    .resolve_storage_backend(&fallback_provider);
                (fallback_provider, backend, None, None)
            }
        }
    }

    /// Determine code interpreter status from model capability and kill switch.
    /// Fail-closed: kill switch lookup failure → `Denied`.
    async fn resolve_ci_status(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        model_supports_ci: Option<bool>,
    ) -> CodeInterpreterStatus {
        let disable = match self
            .model_resolver
            .get_kill_switches(ctx.subject_id())
            .await
        {
            Ok(ks) => ks.disable_code_interpreter,
            Err(e) => {
                tracing::warn!(
                    chat_id = %chat_id,
                    error = %e,
                    "kill-switch lookup failed; disabling code interpreter uploads"
                );
                return CodeInterpreterStatus::Denied;
            }
        };

        match model_supports_ci {
            None => CodeInterpreterStatus::Unknown,
            Some(true) if !disable => CodeInterpreterStatus::Allowed,
            _ => CodeInterpreterStatus::Denied,
        }
    }

    /// Get attachment metadata by ID.
    ///
    /// Returns all rows including soft-deleted — handler checks `deleted_at` → 404.
    pub(crate) async fn get_attachment(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<AttachmentModel, DomainError> {
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &super::resources::CHAT,
                super::actions::READ_ATTACHMENT,
                Some(chat_id),
            )
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // Verify user owns the chat (ensure_owner for defence-in-depth).
        let chat_scope = scope.ensure_owner(ctx.subject_id());
        self.chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await?
            .ok_or_else(|| DomainError::not_found("Chat", chat_id))?;

        // Attachment entity is no_owner — use tenant-only scope.
        let att_scope = scope.tenant_only();
        let row = self
            .attachment_repo
            .get(&conn, &att_scope, attachment_id)
            .await?
            .ok_or_else(|| DomainError::not_found("Attachment", attachment_id))?;

        // Chat-scoped access: attachment must belong to the requested chat
        if row.chat_id != chat_id {
            return Err(DomainError::not_found("Attachment", attachment_id));
        }

        // Handler-level 404 for soft-deleted
        if row.deleted_at.is_some() {
            return Err(DomainError::not_found("Attachment", attachment_id));
        }

        Ok(row)
    }

    /// Soft-delete an attachment.
    ///
    /// Ordering: load → 404 → ownership check → 403 → idempotent (204 if already deleted)
    /// → `message_attachments` guard → TX(soft-delete + outbox) → 204.
    pub(crate) async fn delete_attachment(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), DomainError> {
        let scope = self
            .enforcer
            .access_scope(
                ctx,
                &super::resources::CHAT,
                super::actions::DELETE_ATTACHMENT,
                Some(chat_id),
            )
            .await?;

        let conn = self.db.conn().map_err(DomainError::from)?;

        // Verify user owns the chat (ensure_owner for defence-in-depth).
        let chat_scope = scope.ensure_owner(ctx.subject_id());
        self.chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await?
            .ok_or_else(|| DomainError::not_found("Chat", chat_id))?;

        // Load row (including soft-deleted); attachment is no_owner → tenant scope.
        let att_scope = scope.tenant_only();
        let row = self
            .attachment_repo
            .get(&conn, &att_scope, attachment_id)
            .await?
            .ok_or_else(|| DomainError::not_found("Attachment", attachment_id))?;

        // Chat-scoped access: attachment must belong to the requested chat
        if row.chat_id != chat_id {
            return Err(DomainError::not_found("Attachment", attachment_id));
        }

        // Ownership check (explicit since entity uses no_owner)
        if row.uploaded_by_user_id != ctx.subject_id() {
            return Err(DomainError::Forbidden);
        }

        // Idempotent: already deleted → 204
        if row.deleted_at.is_some() {
            return Ok(());
        }

        // TX(soft-delete + outbox enqueue) — atomic per DESIGN.md Phase 1.
        let event = crate::domain::repos::AttachmentCleanupEvent {
            event_type: "attachment_deleted".to_owned(),
            tenant_id: row.tenant_id,
            chat_id: row.chat_id,
            attachment_id: row.id,
            provider_file_id: row.provider_file_id.clone(),
            vector_store_id: None, // populated when vector store cleanup is needed
            storage_backend: row.storage_backend.clone(),
            attachment_kind: row.attachment_kind.to_string(),
            deleted_at: time::OffsetDateTime::now_utc(),
        };

        let attachment_repo = Arc::clone(&self.attachment_repo);
        let outbox_enqueuer = Arc::clone(&self.outbox_enqueuer);
        let scope_tx = scope.clone();

        let affected = self
            .db
            .transaction(move |tx| {
                Box::pin(async move {
                    // CAS-guarded soft-delete: WHERE deleted_at IS NULL AND NOT EXISTS(message_attachments)
                    let affected = attachment_repo
                        .soft_delete(tx, &scope_tx, attachment_id)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    if affected > 0 {
                        // Enqueue cleanup event in the same TX
                        outbox_enqueuer
                            .enqueue_attachment_cleanup(tx, event)
                            .await
                            .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;
                    }

                    Ok(affected)
                })
            })
            .await
            .map_err(|e: modkit_db::DbError| DomainError::database(e.to_string()))?;

        if affected == 0 {
            // Ambiguity: rows_affected=0 could mean concurrent delete OR message reference.
            // Re-check to distinguish (outside TX — read-only).
            let reloaded = self
                .attachment_repo
                .get(&conn, &scope, attachment_id)
                .await?;
            match reloaded {
                Some(r) if r.deleted_at.is_some() => {
                    // Concurrent delete — idempotent 204
                    return Ok(());
                }
                Some(_) => {
                    // deleted_at IS NULL but soft_delete failed → message reference exists
                    return Err(DomainError::conflict(
                        "attachment_locked",
                        "Attachment is referenced by one or more messages and cannot be deleted",
                    ));
                }
                None => {
                    // Row vanished entirely (shouldn't happen with soft-deletes)
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    /// Get or create a vector store for the given chat.
    ///
    /// Protocol (must NOT hold DB connection during OAGW call):
    /// 1. INSERT row with `vector_store_id = NULL` → COMMIT
    /// 2. Call OAGW to create vector store (1–3s HTTP call, outside TX)
    /// 3. CAS UPDATE `SET vector_store_id = :id WHERE vector_store_id IS NULL`
    ///
    /// Loser path (unique violation on INSERT): poll `find_by_chat` with
    /// exponential backoff until `vector_store_id` is populated.
    /// Timeout after 5 polls → 503.
    pub(crate) async fn get_or_create_vector_store(
        &self,
        ctx: SecurityContext,
        scope: &AccessScope,
        tenant_id: Uuid,
        chat_id: Uuid,
        provider_id: &str,
    ) -> Result<String, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;

        // Fast path: vector store already exists and is populated.
        let expected_backend = self.provider_resolver.resolve_storage_backend(provider_id);
        if let Some(row) = self
            .vector_store_repo
            .find_by_chat(&conn, scope, chat_id)
            .await?
        {
            // Provider consistency: reject if existing VS was created for a
            // different provider than the current upload's resolved provider.
            if row.provider != expected_backend {
                return Err(DomainError::conflict(
                    "provider_mismatch",
                    format!(
                        "vector store provider mismatch: existing='{}', current='{expected_backend}'",
                        row.provider
                    ),
                ));
            }
            if let Some(vs_id) = row.vector_store_id {
                return Ok(vs_id);
            }
            // Row exists but vector_store_id is NULL → creation in progress.
            // Fall through to loser polling path.
            return self.poll_vector_store(scope, chat_id).await;
        }

        // Try to become the winner: insert a placeholder row.
        let row_id = Uuid::now_v7();

        match self
            .vector_store_repo
            .insert(
                &conn,
                scope,
                InsertVectorStoreParams {
                    id: row_id,
                    tenant_id,
                    chat_id,
                    provider: expected_backend,
                },
            )
            .await
        {
            Ok(_) => {
                // Winner path: we inserted the placeholder.
                // Create vector store via provider trait.
                let vs_id = match self
                    .vector_store
                    .create_vector_store(ctx, provider_id)
                    .await
                {
                    Ok(id) => id,
                    Err(e) => {
                        // Best-effort cleanup: remove stuck-NULL placeholder row
                        let cleanup_conn = self.db.conn().ok();
                        if let Some(cc) = cleanup_conn {
                            drop(self.vector_store_repo.delete(&cc, scope, row_id).await);
                        }
                        return Err(DomainError::from(e));
                    }
                };

                // CAS: set vector_store_id on our row.
                let conn2 = self.db.conn().map_err(DomainError::from)?;
                let affected = self
                    .vector_store_repo
                    .cas_set_vector_store_id(&conn2, scope, row_id, &vs_id)
                    .await?;

                if affected == 0 {
                    // Should not happen — we inserted the row and no one else
                    // can CAS it. Log and return the ID anyway.
                    tracing::warn!(
                        row_id = %row_id,
                        "CAS set vector_store_id returned 0 (unexpected)"
                    );
                }

                Ok(vs_id)
            }
            Err(DomainError::Conflict { .. }) => {
                // Loser path: another upload already inserted the row.
                self.poll_vector_store(scope, chat_id).await
            }
            Err(e) => {
                self.handle_vector_store_insert_race(&conn, scope, chat_id, e)
                    .await
            }
        }
    }

    /// Defensive fallback for vector-store insert failures that may be
    /// unrecognised unique-constraint violations (race with a concurrent upload).
    /// If a row now exists, treat as a loser path; otherwise propagate the error.
    async fn handle_vector_store_insert_race(
        &self,
        conn: &modkit_db::DbConn<'_>,
        scope: &AccessScope,
        chat_id: Uuid,
        original_err: DomainError,
    ) -> Result<String, DomainError> {
        match self
            .vector_store_repo
            .find_by_chat(conn, scope, chat_id)
            .await
        {
            Ok(Some(row)) if row.vector_store_id.is_some() => {
                tracing::warn!(
                    chat_id = %chat_id,
                    error = %original_err,
                    "vector store insert failed but row exists (concurrent insert); using existing"
                );
                #[allow(clippy::unwrap_used)]
                Ok(row.vector_store_id.unwrap())
            }
            Ok(Some(_)) => {
                tracing::warn!(
                    chat_id = %chat_id,
                    error = %original_err,
                    "vector store insert failed but placeholder exists (concurrent insert); polling"
                );
                self.poll_vector_store(scope, chat_id).await
            }
            _ => Err(original_err),
        }
    }

    /// Poll `find_by_chat` with exponential backoff until `vector_store_id`
    /// is populated. Timeout after 5 polls → 503 with `Retry-After: 3`.
    ///
    /// If the row vanishes (winner rolled back), returns an error so the
    /// caller can retry the full get-or-create flow.
    async fn poll_vector_store(
        &self,
        scope: &AccessScope,
        chat_id: Uuid,
    ) -> Result<String, DomainError> {
        const BACKOFF_MS: &[u64] = &[100, 200, 400, 800, 1600];

        for delay_ms in BACKOFF_MS {
            tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;

            let conn = self.db.conn().map_err(DomainError::from)?;
            match self
                .vector_store_repo
                .find_by_chat(&conn, scope, chat_id)
                .await?
            {
                Some(row) if row.vector_store_id.is_some() => {
                    #[allow(clippy::unwrap_used)]
                    return Ok(row.vector_store_id.unwrap());
                }
                Some(_) => {
                    // Still NULL — winner hasn't finished yet. Keep polling.
                }
                None => {
                    // Row vanished — winner rolled back. Return error so the
                    // caller can retry the full get-or-create flow.
                    return Err(DomainError::ProviderError {
                        code: "vector_store_race".to_owned(),
                        sanitized_message:
                            "Vector store row vanished during creation; please retry".to_owned(),
                    });
                }
            }
        }

        Err(DomainError::ProviderError {
            code: "vector_store_timeout".to_owned(),
            sanitized_message: "Timed out waiting for vector store creation".to_owned(),
        })
    }

    /// Upload a file attachment to a chat.
    ///
    /// Flow: use pre-resolved `UploadContext` (from `get_upload_context`) ->
    ///   TX(lock chat, check limits, insert pending) -> COMMIT ->
    ///   upload stream to provider via OAGW -> CAS `set_uploaded` (with exact size) ->
    ///   branch on kind:
    ///   - Document: vector store get-or-create + add file with attributes + CAS `set_ready`
    ///   - Image: CAS `set_ready` directly
    ///
    /// MIME validation and per-file size enforcement are handled by the
    /// handler before calling this method. The handler passes the
    /// pre-validated MIME, attachment kind, and a size-limited `FileStream`.
    ///
    /// Returns the created attachment row.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        clippy::cast_precision_loss
    )]
    pub(crate) async fn upload_file(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        upload_ctx: UploadContext,
        filename: String,
        validated_mime: &str,
        attachment_kind: AttachmentKind,
        file_stream: crate::domain::ports::FileStream,
        size_hint: Option<u64>,
    ) -> Result<AttachmentModel, DomainError> {
        use crate::domain::mime_validation::structured_filename;
        use crate::domain::repos::InsertAttachmentParams;

        let tenant_id = ctx.subject_tenant_id();
        let user_id = ctx.subject_id();
        let is_document = attachment_kind == AttachmentKind::Document;

        let scope = upload_ctx.scope;
        let provider_id = upload_ctx.provider_id;
        let storage_backend = upload_ctx.storage_backend;

        // Resolve purposes from the pre-validated MIME type.
        let purposes = crate::domain::mime_validation::resolve_purposes(validated_mime);

        #[allow(clippy::cast_possible_wrap)]
        let hint_bytes = size_hint.map_or(0i64, |h| h as i64);

        let attachment_id = Uuid::now_v7();

        // Code interpreter gating (pre-resolved in UploadContext).
        // When CI is blocked, remove it from purposes rather than rejecting
        // outright — the attachment may still serve other purposes (e.g.
        // FileSearch). Only reject if no purposes remain after filtering.
        // When CI status is Unknown (transient resolution failure), return 503
        // so the client can retry rather than hard-rejecting the upload.
        let purposes = if purposes.contains(&AttachmentPurpose::CodeInterpreter)
            && upload_ctx.code_interpreter_status != CodeInterpreterStatus::Allowed
        {
            if upload_ctx.code_interpreter_status == CodeInterpreterStatus::Unknown {
                return Err(DomainError::service_unavailable(
                    "Unable to determine code interpreter support; please retry",
                ));
            }
            let filtered: Vec<_> = purposes
                .into_iter()
                .filter(|p| *p != AttachmentPurpose::CodeInterpreter)
                .collect();
            if filtered.is_empty() {
                return Err(DomainError::validation(
                    "Code interpreter is currently unavailable",
                ));
            }
            tracing::debug!(
                %filename,
                "CodeInterpreter purpose stripped; continuing with remaining purposes"
            );
            filtered
        } else {
            purposes
        };

        let chat_scope = scope.ensure_owner(ctx.subject_id());

        let attachment_repo = Arc::clone(&self.attachment_repo);
        let chat_repo = Arc::clone(&self.chat_repo);
        let rag_config = self.rag_config.clone();
        let chat_scope_tx = chat_scope.clone();
        let scope_tx = scope.clone();
        let kind_str = attachment_kind.to_string();
        let insert_params = InsertAttachmentParams {
            id: attachment_id,
            tenant_id,
            chat_id,
            uploaded_by_user_id: user_id,
            filename: filename.clone(),
            content_type: validated_mime.to_owned(),
            size_bytes: hint_bytes,
            storage_backend: storage_backend.clone(),
            attachment_kind: kind_str,
            for_file_search: purposes.contains(&AttachmentPurpose::FileSearch),
            for_code_interpreter: purposes.contains(&AttachmentPurpose::CodeInterpreter),
        };

        let _row = self
            .db
            .transaction(|tx| {
                Box::pin(async move {
                    // Lock chat row to serialize concurrent uploads
                    let _chat = chat_repo
                        .get_for_update(tx, &chat_scope_tx, chat_id)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?
                        .ok_or_else(|| {
                            mutation_to_db_err(AttachmentMutationError::ChatNotFound { chat_id })
                        })?;

                    // Check limits
                    if is_document {
                        let doc_count = attachment_repo
                            .count_documents(tx, &scope_tx, chat_id)
                            .await
                            .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;
                        if doc_count >= i64::from(rag_config.max_documents_per_chat) {
                            return Err(mutation_to_db_err(
                                AttachmentMutationError::DocumentLimitExceeded {
                                    message: format!(
                                        "Chat already has {doc_count} documents (limit: {})",
                                        rag_config.max_documents_per_chat
                                    ),
                                },
                            ));
                        }
                    }

                    // Aggregate size check (best-effort with size_hint).
                    if hint_bytes > 0 {
                        let current_bytes = attachment_repo
                            .sum_size_bytes(tx, &scope_tx, chat_id)
                            .await
                            .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;
                        let max_bytes =
                            i64::from(rag_config.max_total_upload_mb_per_chat) * 1_048_576;
                        if current_bytes + hint_bytes > max_bytes {
                            return Err(mutation_to_db_err(
                                AttachmentMutationError::StorageLimitExceeded {
                                    message: format!("Upload would exceed {max_bytes} byte limit"),
                                },
                            ));
                        }
                    }

                    // Insert pending row (size_bytes = hint or 0; exact set in set_uploaded)
                    let row = attachment_repo
                        .insert(tx, &scope_tx, insert_params)
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    Ok(row)
                })
            })
            .await
            .map_err(unwrap_mutation_err)?;

        // Metrics: attachment is now pending (in-flight to provider).
        // PendingGuard ensures decrement on every exit path (Drop-based).
        let kind_metric = if is_document {
            kind_label::DOCUMENT
        } else {
            kind_label::IMAGE
        };
        let pending_guard = PendingGuard::new(&self.metrics);

        // 3. Upload stream to provider (outside TX — avoids holding pool)
        let structured_name = structured_filename(chat_id, attachment_id, validated_mime);

        let (provider_file_id, bytes_uploaded) = match self
            .file_storage
            .upload_file(
                ctx.clone(),
                &provider_id,
                UploadFileParams {
                    filename: structured_name,
                    content_type: validated_mime.to_owned(),
                    file_stream,
                    purpose: "assistants".to_owned(),
                },
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                // Size-limit error from the streaming adapter → FileTooLarge (413).
                if let crate::domain::ports::FileStorageError::Rejected {
                    ref code,
                    ref message,
                } = e
                    && code == "file_too_large"
                {
                    self.try_set_failed(&scope, attachment_id, "pending", "file_too_large")
                        .await;
                    self.metrics
                        .record_attachment_upload(kind_metric, upload_result::FILE_TOO_LARGE);
                    return Err(DomainError::FileTooLarge {
                        message: message.clone(),
                    });
                }
                // P1-13: upload failure → CAS set_failed from pending
                self.try_set_failed(&scope, attachment_id, "pending", "upload_failed")
                    .await;
                self.metrics
                    .record_attachment_upload(kind_metric, upload_result::PROVIDER_ERROR);
                return Err(DomainError::from(e));
            }
        };

        // 4. CAS: pending → uploaded (with exact size from provider)
        {
            use crate::domain::repos::SetUploadedParams;
            #[allow(clippy::cast_possible_wrap)]
            let exact_i64 = bytes_uploaded as i64;
            let conn = self.db.conn().map_err(DomainError::from)?;
            let affected = self
                .attachment_repo
                .cas_set_uploaded(
                    &conn,
                    &scope,
                    SetUploadedParams {
                        id: attachment_id,
                        provider_file_id: provider_file_id.clone(),
                        size_bytes: exact_i64,
                    },
                )
                .await?;
            if affected == 0 {
                // P1-14: Concurrent soft-delete — best-effort cleanup provider file
                tracing::warn!(attachment_id = %attachment_id, "CAS set_uploaded returned 0 (concurrent delete?)");
                self.spawn_delete_file(ctx.clone(), &provider_id, &provider_file_id);
                return Err(DomainError::not_found("Attachment", attachment_id));
            }
        }

        // 5. Execute purpose-specific paths (each fires independently).
        // - FileSearch + document → vector store indexing
        // - CodeInterpreter → (no extra step during upload; file is used at stream time)
        // - Images → (no extra step; sent inline)
        // When an attachment has multiple purposes, all matching paths execute.
        if is_document && purposes.contains(&AttachmentPurpose::FileSearch) {
            // Get or create vector store
            let vs_id = match self
                .get_or_create_vector_store(ctx.clone(), &scope, tenant_id, chat_id, &provider_id)
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    // Cleanup: attachment stuck in `uploaded` → set failed + delete provider file
                    self.try_set_failed(&scope, attachment_id, "uploaded", "vector_store_failed")
                        .await;
                    self.spawn_delete_file(ctx.clone(), &provider_id, &provider_file_id);
                    self.metrics
                        .record_attachment_upload(kind_metric, upload_result::PROVIDER_ERROR);
                    return Err(e);
                }
            };

            // Add file to vector store with attachment_id attribute
            if let Err(e) = self
                .vector_store
                .add_file_to_vector_store(
                    ctx.clone(),
                    &provider_id,
                    AddFileToVectorStoreParams {
                        vector_store_id: vs_id,
                        provider_file_id: provider_file_id.clone(),
                        attributes: HashMap::from([(
                            "attachment_id".to_owned(),
                            attachment_id.to_string(),
                        )]),
                    },
                )
                .await
            {
                // P1-13: indexing failure → CAS set_failed from uploaded,
                // best-effort delete provider file
                self.try_set_failed(&scope, attachment_id, "uploaded", "indexing_failed")
                    .await;
                self.spawn_delete_file(ctx.clone(), &provider_id, &provider_file_id);
                self.metrics
                    .record_attachment_upload(kind_metric, upload_result::PROVIDER_ERROR);
                return Err(DomainError::from(e));
            }
        }

        // 6. CAS: uploaded → ready
        {
            use crate::domain::repos::SetReadyParams;
            let conn = self.db.conn().map_err(DomainError::from)?;
            let affected = self
                .attachment_repo
                .cas_set_ready(&conn, &scope, SetReadyParams { id: attachment_id })
                .await?;
            if affected == 0 {
                // P1-14: Concurrent soft-delete — best-effort cleanup
                tracing::warn!(attachment_id = %attachment_id, "CAS set_ready returned 0 (concurrent delete?)");
                self.spawn_delete_file(ctx.clone(), &provider_id, &provider_file_id);
                return Err(DomainError::not_found("Attachment", attachment_id));
            }
        }

        // Metrics: upload succeeded — defuse guard (explicit decrement + disarm)
        self.metrics
            .record_attachment_upload(kind_metric, upload_result::OK);
        #[allow(clippy::cast_precision_loss)]
        self.metrics
            .record_attachment_upload_bytes(kind_metric, bytes_uploaded as f64);
        pending_guard.defuse();

        // Reload final state
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.attachment_repo
            .get(&conn, &scope, attachment_id)
            .await?
            .ok_or_else(|| DomainError::not_found("Attachment", attachment_id))
    }

    /// Best-effort CAS `set_failed` — log on failure, never propagate.
    async fn try_set_failed(
        &self,
        scope: &AccessScope,
        attachment_id: Uuid,
        from_status: &str,
        error_code: &str,
    ) {
        use crate::domain::repos::SetFailedParams;
        let Ok(conn) = self.db.conn().map_err(DomainError::from) else {
            tracing::error!(attachment_id = %attachment_id, "failed to acquire connection for set_failed");
            return;
        };
        if let Err(e) = self
            .attachment_repo
            .cas_set_failed(
                &conn,
                scope,
                SetFailedParams {
                    id: attachment_id,
                    error_code: error_code.to_owned(),
                    from_status: from_status.to_owned(),
                },
            )
            .await
        {
            tracing::error!(attachment_id = %attachment_id, error = %e, "failed to set attachment to failed state");
        }
    }

    /// Fire-and-forget delete of a provider file via the storage trait.
    fn spawn_delete_file(&self, ctx: SecurityContext, provider_id: &str, provider_file_id: &str) {
        let storage = Arc::clone(&self.file_storage);
        let pid = provider_id.to_owned();
        let fid = provider_file_id.to_owned();
        tokio::spawn(async move {
            if let Err(e) = storage.delete_file(ctx, &pid, &fid).await {
                tracing::warn!(provider_file_id = %fid, error = %e, "fire-and-forget file delete failed");
            }
        });
    }
}

#[cfg(test)]
#[path = "attachment_service_test.rs"]
mod tests;
