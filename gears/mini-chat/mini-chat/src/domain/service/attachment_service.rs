use std::collections::HashMap;
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use bytes::Bytes;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::config::{RagConfig, ThumbnailConfig};
use crate::domain::error::DomainError;
use crate::domain::mime_validation::{AttachmentKind, AttachmentPurpose};
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::ports::metric_labels::{kind as kind_label, upload_result};
use crate::domain::ports::{
    AddFileToVectorStoreParams, FileStorageProvider, UploadFileParams, VectorStoreProvider,
};
use crate::domain::repos::{
    AttachmentRepository, ChatRepository, InsertVectorStoreParams, ModelResolver, OutboxEnqueuer,
    SetSecondaryUploadParams, VectorStoreRepository,
};
use crate::infra::db::entity::attachment::{
    Model as AttachmentModel, SecondaryUploadStatus, secondary_provider_kind,
};
use crate::infra::llm::provider_resolver::ProviderResolver;

use super::DbProvider;

/// Hard ceiling on the parallel Anthropic Files API upload during the primary
/// readiness path. A wedged upstream must not delay the user-visible
/// transition to `ready` — on timeout the secondary upload is recorded as
/// `Failed` and the primary path proceeds normally.
const ANTHROPIC_UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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

/// Information needed to perform the parallel "secondary" upload to
/// Anthropic's Files API alongside the primary Azure/OpenAI upload.
///
/// Populated only when the chat's LLM provider uses the Anthropic Messages
/// adapter (see `anthropic-provider-support.md` §8.0). `upstream_alias` is the
/// OAGW alias under which Anthropic's API was registered at gear init.
#[domain_model]
#[derive(Debug, Clone)]
pub struct AnthropicUploadInfo {
    pub upstream_alias: String,
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
    /// `Some` when the chat's LLM provider is Anthropic — the upload code
    /// will perform a secondary parallel upload to Anthropic's Files API
    /// after the primary Azure/OpenAI upload succeeds. `None` otherwise.
    pub anthropic_upload: Option<AnthropicUploadInfo>,
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

fn mutation_to_db_err(e: AttachmentMutationError) -> toolkit_db::DbError {
    toolkit_db::DbError::Other(anyhow::Error::new(e))
}

fn unwrap_mutation_err(e: toolkit_db::DbError) -> DomainError {
    match e {
        toolkit_db::DbError::Other(anyhow_err) => {
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
    thumbnail_config: ThumbnailConfig,
    metrics: Arc<dyn MiniChatMetricsPort>,
    /// Anthropic Files API client used for the parallel upload when the chat's
    /// LLM provider is Anthropic. `None` when no Anthropic provider is
    /// configured — the parallel upload is skipped silently in that case.
    anthropic_files_client:
        Option<Arc<crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient>>,
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
        thumbnail_config: ThumbnailConfig,
        metrics: Arc<dyn MiniChatMetricsPort>,
        anthropic_files_client: Option<
            Arc<crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient>,
        >,
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
            thumbnail_config,
            metrics,
            anthropic_files_client,
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
        let (provider_id, storage_backend, ccm_bytes, model_supports_ci, anthropic_upload) =
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
            anthropic_upload,
        })
    }

    /// Resolve provider, storage backend, per-model byte limit, and CI support
    /// from the model catalog. Falls back to ConfigMap-only on transient failure.
    async fn resolve_model_limits(
        &self,
        ctx: &SecurityContext,
        chat_id: Uuid,
        model: String,
    ) -> (
        String,
        String,
        Option<u64>,
        Option<bool>,
        Option<AnthropicUploadInfo>,
    ) {
        match self
            .model_resolver
            .resolve_model(ctx.subject_id(), Some(model))
            .await
        {
            Ok(resolved) => {
                // Storage operations (file upload, vector store, cleanup) must
                // route to the LLM provider's `rag_provider` when set — see
                // `anthropic-provider-support.md` §7.5. The LLM provider may
                // have no file/vector-store API of its own (Anthropic), so we
                // resolve to a storage-capable provider here once and pass the
                // resolved id downstream as the canonical provider_id for the
                // attachment row.
                let storage_provider_id = self
                    .provider_resolver
                    .resolve_rag_provider(&resolved.provider_id)
                    .to_owned();
                let backend = self
                    .provider_resolver
                    .resolve_storage_backend(&storage_provider_id);
                let ccm = u64::from(resolved.max_file_size_mb) * 1_048_576;
                let ci = Some(resolved.tool_support.code_interpreter);

                // §8.0: when the chat's LLM provider is Anthropic, the upload
                // path performs a second parallel upload to Anthropic's Files
                // API. Capture the LLM provider's upstream alias here so the
                // upload code doesn't have to redo the resolution.
                let anthropic_upload = if self
                    .provider_resolver
                    .is_anthropic_messages(&resolved.provider_id)
                {
                    let tenant_str = ctx.subject_tenant_id().to_string();
                    self.provider_resolver
                        .upstream_alias_for(&resolved.provider_id, Some(&tenant_str))
                        .map(|alias| AnthropicUploadInfo {
                            upstream_alias: alias.to_owned(),
                        })
                } else {
                    None
                };

                (
                    storage_provider_id,
                    backend,
                    Some(ccm),
                    ci,
                    anthropic_upload,
                )
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
                (fallback_provider, backend, None, None, None)
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

    /// Resolve the coordinates needed to issue a secondary-provider delete
    /// (currently: Anthropic Files API) for a soft-deleted attachment.
    ///
    /// Returns `None` whenever any precondition is missing — the parallel
    /// upload never succeeded, the chat's LLM provider is not Anthropic, or
    /// the OAGW upstream alias can't be resolved. The caller treats `None`
    /// as "no secondary cleanup needed"; the primary cleanup still proceeds.
    ///
    /// DB lookup errors propagate (`?`); model-resolver errors are logged
    /// and swallowed since they shouldn't block the user-visible delete.
    async fn resolve_secondary_cleanup_ref(
        &self,
        ctx: &SecurityContext,
        chat_scope: &AccessScope,
        chat_id: Uuid,
        row: &AttachmentModel,
        conn: &toolkit_db::DbConn<'_>,
    ) -> Result<Option<crate::domain::repos::SecondaryCleanupRef>, DomainError> {
        if !matches!(row.secondary_status, SecondaryUploadStatus::Uploaded) {
            return Ok(None);
        }
        let (Some(file_id), Some(provider_kind)) = (
            row.secondary_file_id.as_ref(),
            row.secondary_provider_kind.as_ref(),
        ) else {
            return Ok(None);
        };

        let chat_row = self
            .chat_repo
            .get(conn, chat_scope, chat_id)
            .await?
            .ok_or_else(|| DomainError::not_found("Chat", chat_id))?;

        let resolved = match self
            .model_resolver
            .resolve_model(ctx.subject_id(), Some(chat_row.model.clone()))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    chat_id = %chat_id,
                    model = %chat_row.model,
                    "delete_attachment: could not resolve model for secondary cleanup alias"
                );
                return Ok(None);
            }
        };

        if !self
            .provider_resolver
            .is_anthropic_messages(&resolved.provider_id)
        {
            return Ok(None);
        }

        let tenant = ctx.subject_tenant_id().to_string();
        Ok(self
            .provider_resolver
            .upstream_alias_for(&resolved.provider_id, Some(&tenant))
            .map(|alias| crate::domain::repos::SecondaryCleanupRef {
                file_id: file_id.clone(),
                provider_kind: provider_kind.clone(),
                upstream_alias: alias.to_owned(),
            }))
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

        // Resolve secondary-upload delete coordinates at enqueue time so the
        // cleanup worker stays free of ProviderResolver. `None` on any
        // non-Anthropic chat, missing attachment fields, or alias-resolution
        // failure — the primary cleanup proceeds regardless.
        let secondary_ref = self
            .resolve_secondary_cleanup_ref(ctx, &chat_scope, chat_id, &row, &conn)
            .await?;

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
            secondary_ref,
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
                        .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    if affected > 0 {
                        // Enqueue cleanup event in the same TX
                        outbox_enqueuer
                            .enqueue_attachment_cleanup(tx, event)
                            .await
                            .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;
                    }

                    Ok(affected)
                })
            })
            .await
            .map_err(|e: toolkit_db::DbError| DomainError::database(e.to_string()))?;

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
        conn: &toolkit_db::DbConn<'_>,
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
        use std::time::Duration;

        use tokio_retry::RetryIf;

        const BACKOFF_MS: [u64; 5] = [100, 200, 400, 800, 1600];

        // Wait once before the first poll (the winning writer needs time to
        // populate `vector_store_id`), then let tokio-retry space the remaining
        // polls with the tail of the doubling schedule — same 5 polls / 3100ms
        // budget as the previous hand-rolled loop.
        tokio::time::sleep(Duration::from_millis(BACKOFF_MS[0])).await;

        let strategy = BACKOFF_MS[1..].iter().map(|ms| Duration::from_millis(*ms));

        let poll = || async {
            let conn = self.db.conn().map_err(DomainError::from)?;
            match self
                .vector_store_repo
                .find_by_chat(&conn, scope, chat_id)
                .await?
            {
                Some(row) if row.vector_store_id.is_some() =>
                {
                    #[allow(clippy::unwrap_used)]
                    Ok(row.vector_store_id.unwrap())
                }
                // Still NULL — winner hasn't finished yet. Signal "keep polling"
                // with the timeout error so that, once retries are exhausted,
                // that is exactly the error the caller receives.
                Some(_) => Err(DomainError::ProviderError {
                    code: "vector_store_timeout".to_owned(),
                    sanitized_message: "Timed out waiting for vector store creation".to_owned(),
                }),
                // Row vanished — winner rolled back. Non-retryable: the caller
                // retries the full get-or-create flow.
                None => Err(DomainError::ProviderError {
                    code: "vector_store_race".to_owned(),
                    sanitized_message: "Vector store row vanished during creation; please retry"
                        .to_owned(),
                }),
            }
        };

        // Keep polling only while the row exists but is still pending.
        let still_pending = |e: &DomainError| matches!(e, DomainError::ProviderError { code, .. } if code.as_str() == "vector_store_timeout");

        RetryIf::start(strategy, poll, still_pending).await
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
                        .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?
                        .ok_or_else(|| {
                            mutation_to_db_err(AttachmentMutationError::ChatNotFound { chat_id })
                        })?;

                    // Check limits
                    if is_document {
                        let doc_count = attachment_repo
                            .count_documents(tx, &scope_tx, chat_id)
                            .await
                            .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;
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
                            .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;
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
                        .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;

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

        // 3. Upload stream to provider (outside TX — avoids holding pool).
        //    For images, buffer the raw bytes while streaming so we can
        //    generate a thumbnail after the upload completes.
        let structured_name = structured_filename(chat_id, attachment_id, validated_mime);

        // Arc<Mutex<Option<Vec>>> bridges two ownership scopes: the stream
        // closure (writes chunks during upload) and the post-upload thumbnail
        // path (reads the buffer).  The stream is consumed sequentially so no
        // real contention occurs; Arc<Mutex> is needed only to satisfy Send + 'static.
        let image_buffer: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(if is_document {
                None
            } else {
                Some(Vec::new())
            }));

        let upload_stream: crate::domain::ports::FileStream = if is_document {
            file_stream
        } else {
            let buf = std::sync::Arc::clone(&image_buffer);
            let max_buf = self.thumbnail_config.max_decode_bytes;
            Box::pin(futures::stream::StreamExt::map(file_stream, move |chunk| {
                if let Ok(ref bytes) = chunk {
                    let mut guard = buf
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(ref mut v) = *guard {
                        // Stop buffering if we exceed decode limit — thumbnail
                        // generation will be skipped, but upload continues.
                        // saturating_add avoids overflow when both values are large.
                        if v.len().saturating_add(bytes.len()) <= max_buf {
                            v.extend_from_slice(bytes);
                        } else {
                            *guard = None;
                        }
                    }
                }
                chunk
            }))
        };

        let (provider_file_id, bytes_uploaded) = match self
            .file_storage
            .upload_file(
                ctx.clone(),
                &provider_id,
                UploadFileParams {
                    filename: structured_name,
                    content_type: validated_mime.to_owned(),
                    file_stream: upload_stream,
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

            // 4b. Post-upload aggregate storage check.
            // Reuses `conn` from step 4 so `sum_size_bytes` sees the just-written
            // row without an extra connection checkout.
            // Runs for all attachment types (not just documents) to match the
            // preflight check which is also type-agnostic.
            let total_bytes = self
                .attachment_repo
                .sum_size_bytes(&conn, &scope, chat_id)
                .await?;
            let max_bytes = i64::from(self.rag_config.max_total_upload_mb_per_chat) * 1_048_576;
            if total_bytes > max_bytes {
                self.try_set_failed(&scope, attachment_id, "uploaded", "storage_limit_exceeded")
                    .await;
                self.spawn_delete_file(ctx.clone(), &provider_id, &provider_file_id);
                self.metrics
                    .record_attachment_upload(kind_metric, upload_result::STORAGE_LIMIT_EXCEEDED);
                return Err(DomainError::StorageLimitExceeded {
                    message: format!(
                        "Upload causes total to exceed {max_bytes} byte limit (current total: {total_bytes})"
                    ),
                });
            }
        }

        // Take the buffered image bytes ONCE and convert via `Bytes::from(Vec<u8>)`,
        // which re-uses the existing allocation (zero-copy move). Subsequent
        // consumers — the parallel Anthropic upload below and the thumbnail
        // generator at step 5b — share the same buffer via `Bytes::clone()`,
        // a cheap `Arc::clone`. Per `anthropic-provider-support.md` §8.0.1
        // this keeps peak memory at ~1x file size.
        //
        // `None` for documents (buffer was never allocated) and for images
        // that exceeded `thumbnail.max_decode_bytes` (buffer cleared mid-stream).
        let buffered_image_bytes: Option<Bytes> = image_buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(Bytes::from);

        // Captured for CAS race-loss cleanup at step 6 — set inside the
        // `'anthropic_upload` block when the secondary upload succeeds, so
        // the cleanup branch at step 6 can issue a best-effort Anthropic
        // delete and avoid leaking the upstream file.
        let mut secondary_cleanup: Option<(String, String)> = None;

        // 4c. Parallel "secondary" upload to Anthropic Files API.
        //
        // Per `anthropic-provider-support.md` §8.0: when the chat's LLM
        // provider is Anthropic, files are also uploaded to Anthropic's
        // Files API so the `load_files` tool and image content blocks can
        // reference them via `secondary_file_id` with
        // `secondary_provider_kind = "anthropic"`. Failure here is non-fatal:
        // Azure remains the primary store and the attachment still
        // transitions to `ready` — but `secondary_status = failed` and the
        // adapter will skip blocks that reference this attachment.
        //
        // Currently only images are supported because we have raw bytes
        // already buffered (for thumbnail generation). Document support
        // would require a re-download from Azure — see follow-up.
        'anthropic_upload: {
            let (Some(info), Some(client)) =
                (&upload_ctx.anthropic_upload, &self.anthropic_files_client)
            else {
                break 'anthropic_upload;
            };
            if is_document {
                break 'anthropic_upload;
            }
            let Some(bytes) = buffered_image_bytes.as_ref() else {
                // Image bytes weren't buffered (size > thumbnail.max_decode_bytes).
                // Anthropic upload would require re-download from Azure — skip for now.
                tracing::warn!(
                    attachment_id = %attachment_id,
                    "image bytes not retained (file too large for thumbnail buffer); \
                     skipping parallel Anthropic upload"
                );
                break 'anthropic_upload;
            };

            // The whole parallel-upload block is best-effort: failures here
            // must NOT abort the primary upload. Acquire connections inside a
            // tight scope so a transient pool failure (or a slow Anthropic
            // upload) doesn't block other paths from a DB conn.
            match self.db.conn() {
                Ok(conn) => {
                    if let Err(e) = self
                        .attachment_repo
                        .set_secondary_upload(
                            &conn,
                            &scope,
                            SetSecondaryUploadParams {
                                id: attachment_id,
                                secondary_file_id: None,
                                secondary_status: SecondaryUploadStatus::Pending,
                                secondary_provider_kind: Some(
                                    secondary_provider_kind::ANTHROPIC.to_owned(),
                                ),
                            },
                        )
                        .await
                    {
                        tracing::warn!(
                            attachment_id = %attachment_id,
                            error = %e,
                            "failed to persist secondary pending status; \
                             watchdog will have no in-flight signal"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        attachment_id = %attachment_id,
                        error = %e,
                        "could not acquire DB connection for pending-status write; continuing"
                    );
                }
            }

            // Hard timeout on the parallel upload — see `ANTHROPIC_UPLOAD_TIMEOUT`.
            // A timeout is treated the same as a failed upload
            // (`secondary_status = Failed`, logged), so the user-visible
            // attachment still reaches `ready` via the surrounding logic.
            let upload_fut = client.upload_file(
                ctx.clone(),
                &info.upstream_alias,
                &filename,
                validated_mime,
                bytes.clone(),
            );
            let (secondary_file_id, secondary_status) =
                match tokio::time::timeout(ANTHROPIC_UPLOAD_TIMEOUT, upload_fut).await {
                    Ok(Ok(file_ref)) => {
                        tracing::debug!(
                            attachment_id = %attachment_id,
                            anthropic_file_id = %file_ref.file_id,
                            "parallel Anthropic Files API upload succeeded"
                        );
                        // Capture cleanup coordinates so a CAS race-loss at
                        // step 6 can issue a best-effort Anthropic delete.
                        secondary_cleanup =
                            Some((info.upstream_alias.clone(), file_ref.file_id.clone()));
                        (Some(file_ref.file_id), SecondaryUploadStatus::Uploaded)
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            attachment_id = %attachment_id,
                            error = %e,
                            "parallel Anthropic Files API upload failed; \
                             load_files / image blocks will skip this attachment"
                        );
                        (None, SecondaryUploadStatus::Failed)
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            attachment_id = %attachment_id,
                            timeout_secs = ANTHROPIC_UPLOAD_TIMEOUT.as_secs(),
                            "parallel Anthropic Files API upload timed out; \
                             load_files / image blocks will skip this attachment"
                        );
                        (None, SecondaryUploadStatus::Failed)
                    }
                };

            match self.db.conn() {
                Ok(conn) => {
                    if let Err(e) = self
                        .attachment_repo
                        .set_secondary_upload(
                            &conn,
                            &scope,
                            SetSecondaryUploadParams {
                                id: attachment_id,
                                secondary_file_id,
                                secondary_status,
                                secondary_provider_kind: Some(
                                    secondary_provider_kind::ANTHROPIC.to_owned(),
                                ),
                            },
                        )
                        .await
                    {
                        tracing::warn!(
                            attachment_id = %attachment_id,
                            error = %e,
                            "failed to persist Anthropic upload outcome; \
                             row stays in pending (best-effort)"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        attachment_id = %attachment_id,
                        error = %e,
                        "could not acquire DB connection to record Anthropic upload outcome; \
                         row stays in pending/not_attempted (best-effort)"
                    );
                }
            }
        }

        // 5. Execute purpose-specific paths (each fires independently).
        // - FileSearch + document → vector store indexing
        // - CodeInterpreter → (no extra step during upload; file is used at stream time)
        // - Images → generate thumbnail (best-effort, sync)
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

        // 5b. Image thumbnail generation (best-effort, offloaded to blocking thread).
        // Thumbnail failure never blocks the upload — the attachment transitions
        // to `ready` with `img_thumbnail = null`.
        //
        // Reuses the same `Bytes` buffer extracted before step 4c. If the
        // Anthropic upload took a clone, dropping that clone here decrements
        // the `Bytes` ref-count to one — the underlying allocation is unique
        // to the thumbnail task by the time `spawn_blocking` runs.
        let thumbnail = match buffered_image_bytes {
            Some(bytes) => {
                let cfg = self.thumbnail_config.clone();
                match tokio::task::spawn_blocking(move || {
                    super::thumbnail::generate(&cfg, bytes.as_ref())
                })
                .await
                {
                    Ok(thumb) => thumb,
                    Err(e) => {
                        tracing::warn!(error = %e, "thumbnail spawn_blocking failed");
                        None
                    }
                }
            }
            None => None,
        };

        // 6. CAS: uploaded → ready (with thumbnail if available)
        {
            use crate::domain::repos::SetReadyParams;
            let (thumb_bytes, thumb_w, thumb_h) = match thumbnail {
                Some(t) => (
                    Some(t.bytes),
                    i32::try_from(t.width).ok(),
                    i32::try_from(t.height).ok(),
                ),
                None => (None, None, None),
            };
            let conn = self.db.conn().map_err(DomainError::from)?;
            let affected = self
                .attachment_repo
                .cas_set_ready(
                    &conn,
                    &scope,
                    SetReadyParams {
                        id: attachment_id,
                        img_thumbnail: thumb_bytes,
                        img_thumbnail_width: thumb_w,
                        img_thumbnail_height: thumb_h,
                    },
                )
                .await?;
            if affected == 0 {
                // P1-14: Concurrent soft-delete — best-effort cleanup of both
                // the primary file and (if uploaded) the Anthropic secondary,
                // so the upstream Files API doesn't accumulate orphans.
                tracing::warn!(attachment_id = %attachment_id, "CAS set_ready returned 0 (concurrent delete?)");
                self.spawn_delete_file(ctx.clone(), &provider_id, &provider_file_id);
                if let Some((alias, file_id)) = secondary_cleanup.take() {
                    self.spawn_delete_secondary_file(ctx.clone(), alias, file_id);
                }
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

    /// Fire-and-forget delete of a secondary-provider file (today: Anthropic
    /// Files API). Used when a CAS race-loss leaves the secondary upload
    /// orphaned at the upstream — the row is gone (or about to be), so the
    /// usual cleanup-worker path won't run.
    fn spawn_delete_secondary_file(
        &self,
        ctx: SecurityContext,
        upstream_alias: String,
        file_id: String,
    ) {
        let Some(client) = self.anthropic_files_client.clone() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(e) = client.delete_file(ctx, &upstream_alias, &file_id).await {
                tracing::warn!(
                    secondary_file_id = %file_id,
                    error = %e,
                    "fire-and-forget secondary file delete failed (orphaned)"
                );
            }
        });
    }
}

#[cfg(test)]
#[path = "attachment_service_test.rs"]
mod tests;
