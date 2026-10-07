//! Cleanup outbox handlers — remove provider resources for soft-deleted
//! attachments and chats.
//!
//! Two handlers:
//! - [`AttachmentCleanupHandler`]: per-attachment file delete (attachment-deletion API path).
//! - [`ChatCleanupHandler`]: chat-level batch cleanup + vector store deletion.
//!
//! Both run as part of the outbox pipeline (leased strategy). All replicas
//! process events in parallel. No leader election needed.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use toolkit_db::DBProvider;
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use toolkit_security::SecurityContext;
use tracing::{info, warn};

use crate::domain::ports::{FileStorageProvider, metric_labels};

type DbProvider = DBProvider<toolkit_db::DbError>;
type AttachmentRepo = crate::infra::db::repo::attachment_repo::AttachmentRepository;

// ── Per-attachment cleanup handler ──────────────────────────────────────

/// Handles per-attachment cleanup events from the `mini-chat.attachment_cleanup` queue.
///
/// Deserializes [`AttachmentCleanupEvent`], deletes the provider file via OAGW,
/// and updates the attachment's `cleanup_status`.
/// Build a tenant-scoped `SecurityContext` for OAGW proxy calls.
///
/// The OAGW uses `subject_tenant_id` for per-tenant upstream routing
/// (e.g., different Azure deployments per tenant). The bearer token / API key
/// is injected by the OAGW `apikey_auth` plugin from the credential store --
/// NOT from the `SecurityContext`.
///
/// This means cleanup handlers don't need the original user's token;
/// they just need the correct `tenant_id` for routing.
fn tenant_security_context(tenant_id: uuid::Uuid) -> SecurityContext {
    // Builder only fails if subject_id or subject_tenant_id is missing; we provide both.
    #[allow(clippy::expect_used)]
    SecurityContext::builder()
        .subject_tenant_id(tenant_id)
        .subject_id(toolkit_security::constants::DEFAULT_SUBJECT_ID)
        .build()
        .expect("tenant SecurityContext must build with tenant_id + subject_id")
}

pub struct AttachmentCleanupHandler {
    file_storage: Arc<dyn FileStorageProvider>,
    attachment_repo: AttachmentRepo,
    chat_repo: ChatRepo,
    db: Arc<DbProvider>,
    max_attempts: u32,
    metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort>,
    /// Used to issue secondary `DELETE /v1/files/{id}` against Anthropic's
    /// Files API when the payload carries a secondary entry with
    /// `provider_kind = "anthropic"`. `None` when no Anthropic provider is
    /// configured — the Anthropic-side cleanup is skipped silently.
    anthropic_files_client:
        Option<Arc<crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient>>,
}

impl AttachmentCleanupHandler {
    pub fn new(
        file_storage: Arc<dyn FileStorageProvider>,
        db: Arc<DbProvider>,
        chat_repo: ChatRepo,
        max_attempts: u32,
        metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort>,
        anthropic_files_client: Option<
            Arc<crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient>,
        >,
    ) -> Self {
        Self {
            file_storage,
            attachment_repo: crate::infra::db::repo::attachment_repo::AttachmentRepository,
            chat_repo,
            db,
            max_attempts,
            metrics,
            anthropic_files_client,
        }
    }
}

/// Wire-format of `AttachmentCleanupEvent` for deserialization.
#[derive(Debug, Deserialize)]
struct AttachmentCleanupPayload {
    #[allow(dead_code)]
    event_type: String,
    tenant_id: uuid::Uuid,
    #[allow(dead_code)]
    chat_id: uuid::Uuid,
    attachment_id: uuid::Uuid,
    provider_file_id: Option<String>,
    storage_backend: String,
    #[allow(dead_code)]
    attachment_kind: String,
    #[serde(default)]
    secondary_ref: Option<crate::domain::repos::SecondaryCleanupRef>,
}

#[async_trait]
impl LeasedMessageHandler for AttachmentCleanupHandler {
    #[tracing::instrument(name = "worker", skip_all, fields(worker = "attachment_cleanup"))]
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        // 1. Deserialize payload
        let event: AttachmentCleanupPayload = match serde_json::from_slice(&msg.payload) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "attachment cleanup: invalid payload");
                return MessageResult::Reject(format!("invalid payload: {e}"));
            }
        };

        tracing::debug!(
            attachment_id = %event.attachment_id,
            storage_backend = %event.storage_backend,
            has_provider_file = event.provider_file_id.is_some(),
            "attachment cleanup: processing"
        );

        // 2. Guard: if parent chat is soft-deleted, ownership transferred to
        //    chat-deletion cleanup path (DESIGN lines 1730-1732). Ack this event.
        {
            use crate::domain::repos::ChatRepository as _;
            let conn = match self.db.conn() {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "attachment cleanup: db conn failed");
                    return MessageResult::Retry;
                }
            };
            match self.chat_repo.is_deleted_system(&conn, event.chat_id).await {
                Ok(true) => {
                    tracing::debug!(
                        attachment_id = %event.attachment_id,
                        chat_id = %event.chat_id,
                        "attachment cleanup: parent chat soft-deleted - ownership transferred, acking"
                    );
                    return MessageResult::Ok;
                }
                Ok(false) => {} // chat is active — proceed
                Err(e) => {
                    warn!(error = %e, "attachment cleanup: db error checking chat");
                    return MessageResult::Retry;
                }
            }
        }

        // 3. Nothing to delete if no provider file was ever uploaded.
        let Some(ref provider_file_id) = event.provider_file_id else {
            tracing::debug!(attachment_id = %event.attachment_id, "attachment cleanup: no provider file - marking done");
            if let Err(e) = self.mark_done(event.attachment_id).await {
                warn!(attachment_id = %event.attachment_id, error = %e, "attachment cleanup: failed to mark done");
                return MessageResult::Retry;
            }
            return MessageResult::Ok;
        };

        // 4. Delete provider file via OAGW.
        //    404 counts as success (file already gone).
        let ctx = tenant_security_context(event.tenant_id);
        if let Err(e) = self
            .file_storage
            .delete_file(ctx, &event.storage_backend, provider_file_id)
            .await
        {
            warn!(
                attachment_id = %event.attachment_id,
                error = %e,
                "attachment cleanup: provider delete failed"
            );
            return self
                .record_failure(event.attachment_id, &e.to_string())
                .await;
        }

        // 4b. Secondary-provider delete (best-effort).
        //
        // Only runs when the chat performed a secondary upload that
        // succeeded — `secondary_ref` is set at enqueue time. Today only
        // `provider_kind = secondary_provider_kind::ANTHROPIC` is wired;
        // future providers add their own match arms. A failure here does NOT
        // block the primary cleanup from being marked done: the primary file
        // is already gone, and the secondary copy is orphaned but doesn't
        // affect the user's chat. Orphans need a manual reaper or a future
        // retry hook — better than blocking the user-visible cleanup on
        // upstream flakes.
        if let Some(ref sec) = event.secondary_ref {
            use crate::infra::db::entity::attachment::secondary_provider_kind;
            match sec.provider_kind.as_str() {
                secondary_provider_kind::ANTHROPIC => {
                    if let Some(client) = self.anthropic_files_client.as_ref() {
                        let anth_ctx = tenant_security_context(event.tenant_id);
                        match client
                            .delete_file(anth_ctx, &sec.upstream_alias, &sec.file_id)
                            .await
                        {
                            Ok(()) => {
                                tracing::debug!(
                                    attachment_id = %event.attachment_id,
                                    anthropic_file_id = %sec.file_id,
                                    "attachment cleanup: Anthropic file deleted"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    attachment_id = %event.attachment_id,
                                    anthropic_file_id = %sec.file_id,
                                    error = %e,
                                    "attachment cleanup: Anthropic delete failed (orphaned); \
                                     continuing with primary cleanup"
                                );
                            }
                        }
                    } else {
                        warn!(
                            attachment_id = %event.attachment_id,
                            secondary_file_id = %sec.file_id,
                            provider_kind = %sec.provider_kind,
                            "attachment cleanup: payload references anthropic secondary but no \
                             client configured; orphaned"
                        );
                        self.metrics
                            .record_secondary_cleanup_skipped(&sec.provider_kind);
                    }
                }
                other => {
                    warn!(
                        attachment_id = %event.attachment_id,
                        provider_kind = %other,
                        "attachment cleanup: unknown secondary_provider_kind; skipping"
                    );
                }
            }
        }

        // 5. Success — mark cleanup as done.
        if let Err(e) = self.mark_done(event.attachment_id).await {
            warn!(attachment_id = %event.attachment_id, error = %e, "attachment cleanup: failed to mark done after provider delete");
            return MessageResult::Retry;
        }

        self.metrics
            .record_cleanup_completed(metric_labels::resource_type::FILE);
        info!(attachment_id = %event.attachment_id, "attachment cleanup: done");
        MessageResult::Ok
    }
}

impl AttachmentCleanupHandler {
    async fn mark_done(
        &self,
        attachment_id: uuid::Uuid,
    ) -> Result<(), crate::domain::error::DomainError> {
        use crate::domain::repos::AttachmentRepository as _;
        let conn = self
            .db
            .conn()
            .map_err(crate::domain::error::DomainError::from)?;
        self.attachment_repo
            .mark_cleanup_done(&conn, attachment_id)
            .await?;
        Ok(())
    }

    #[allow(clippy::cognitive_complexity)]
    async fn record_failure(&self, attachment_id: uuid::Uuid, error: &str) -> MessageResult {
        use crate::domain::repos::{AttachmentRepository as _, CleanupOutcome};
        let conn = match self.db.conn() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "record_failure: db conn failed");
                return MessageResult::Retry;
            }
        };
        match self
            .attachment_repo
            .record_cleanup_attempt(&conn, attachment_id, error, self.max_attempts)
            .await
        {
            Ok(CleanupOutcome::TerminalFailure) => {
                warn!(attachment_id = %attachment_id, "attachment cleanup: max attempts reached -- terminal failure");
                self.metrics
                    .record_cleanup_failed(metric_labels::resource_type::FILE);
                MessageResult::Reject(format!("max attempts ({}) reached", self.max_attempts))
            }
            Ok(CleanupOutcome::AlreadyTerminal) => {
                tracing::debug!(attachment_id = %attachment_id, "attachment cleanup: already terminal (stale redelivery)");
                MessageResult::Ok
            }
            Ok(CleanupOutcome::StillPending) => {
                self.metrics.record_cleanup_retry(
                    metric_labels::resource_type::FILE,
                    metric_labels::cleanup_retry_reason::PROVIDER_ERROR,
                );
                MessageResult::Retry
            }
            Err(e) => {
                warn!(error = %e, "record_failure: db error recording attempt");
                MessageResult::Retry
            }
        }
    }
}

// ── Chat-level cleanup handler ──────────────────────────────────────────

type ChatRepo = crate::infra::db::repo::chat_repo::ChatRepository;
type VectorStoreRepo = crate::infra::db::repo::vector_store_repo::VectorStoreRepository;

/// Handles chat-level cleanup events from the `mini-chat.chat_cleanup` queue.
///
/// On each delivery:
/// 1. Guard: verify chat is soft-deleted.
/// 2. Iterate pending attachments — delete each provider file via OAGW.
/// 3. After all attachments are terminal — delete the vector store.
/// 4. Hard-delete the `chat_vector_stores` row (durable completion marker).
pub struct ChatCleanupHandler {
    file_storage: Arc<dyn FileStorageProvider>,
    vs_provider: Arc<dyn crate::domain::ports::VectorStoreProvider>,
    attachment_repo: AttachmentRepo,
    vector_store_repo: VectorStoreRepo,
    chat_repo: ChatRepo,
    db: Arc<DbProvider>,
    max_attempts: u32,
    metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort>,
    /// See [`AttachmentCleanupHandler::anthropic_files_client`].
    anthropic_files_client:
        Option<Arc<crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient>>,
}

impl ChatCleanupHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        file_storage: Arc<dyn FileStorageProvider>,
        vs_provider: Arc<dyn crate::domain::ports::VectorStoreProvider>,
        db: Arc<DbProvider>,
        chat_repo: ChatRepo,
        max_attempts: u32,
        metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort>,
        anthropic_files_client: Option<
            Arc<crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient>,
        >,
    ) -> Self {
        Self {
            file_storage,
            vs_provider,
            attachment_repo: crate::infra::db::repo::attachment_repo::AttachmentRepository,
            vector_store_repo: crate::infra::db::repo::vector_store_repo::VectorStoreRepository,
            chat_repo,
            db,
            max_attempts,
            metrics,
            anthropic_files_client,
        }
    }
}

/// Wire-format of `ChatCleanupEvent` for deserialization.
/// Uses the domain `CleanupReason` enum directly for type-safe matching.
#[derive(Debug, Deserialize)]
struct ChatCleanupPayload {
    reason: crate::domain::repos::CleanupReason,
    tenant_id: uuid::Uuid,
    chat_id: uuid::Uuid,
    #[allow(dead_code)]
    system_request_id: uuid::Uuid,
    #[serde(default)]
    secondary_upstream_alias: Option<String>,
}

#[async_trait]
impl LeasedMessageHandler for ChatCleanupHandler {
    #[tracing::instrument(name = "worker", skip_all, fields(worker = "chat_cleanup"))]
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        use crate::domain::repos::{
            AttachmentRepository as _, ChatRepository as _, VectorStoreRepository as _,
        };

        // 1. Deserialize payload
        let event: ChatCleanupPayload = match serde_json::from_slice(&msg.payload) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "chat cleanup: invalid payload");
                return MessageResult::Reject(format!("invalid payload: {e}"));
            }
        };

        let chat_id = event.chat_id;
        let tenant_id = event.tenant_id;
        tracing::debug!(chat_id = %chat_id, tenant_id = %tenant_id, reason = ?event.reason, "chat cleanup: processing");

        // 2. Acquire DB connection
        let conn = match self.db.conn() {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "chat cleanup: db conn failed");
                return MessageResult::Retry;
            }
        };

        // 3. Guard: verify chat is actually soft-deleted
        match self.chat_repo.is_deleted_system(&conn, chat_id).await {
            Ok(true) => {} // expected
            Ok(false) => {
                warn!(chat_id = %chat_id, "chat cleanup: chat is not soft-deleted -- rejecting");
                return MessageResult::Reject("chat is not soft-deleted".to_owned());
            }
            Err(e) => {
                warn!(chat_id = %chat_id, error = %e, "chat cleanup: db error checking chat");
                return MessageResult::Retry;
            }
        }

        // 4. Load and process pending attachments
        let pending = match self
            .attachment_repo
            .find_pending_cleanup_by_chat(&conn, chat_id)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                warn!(chat_id = %chat_id, error = %e, "chat cleanup: db error loading attachments");
                return MessageResult::Retry;
            }
        };

        let mut any_still_pending = false;
        for att in &pending {
            // Attempt provider file delete
            if let Some(ref provider_file_id) = att.provider_file_id {
                let ctx = tenant_security_context(event.tenant_id);
                if let Err(e) = self
                    .file_storage
                    .delete_file(ctx, &att.storage_backend, provider_file_id)
                    .await
                {
                    warn!(
                        chat_id = %chat_id,
                        attachment_id = %att.id,
                        error = %e,
                        "chat cleanup: provider file delete failed"
                    );
                    let error_str = e.to_string();
                    match self
                        .attachment_repo
                        .record_cleanup_attempt(&conn, att.id, &error_str, self.max_attempts)
                        .await
                    {
                        Ok(crate::domain::repos::CleanupOutcome::StillPending) => {
                            self.metrics.record_cleanup_retry(
                                metric_labels::resource_type::FILE,
                                metric_labels::cleanup_retry_reason::PROVIDER_ERROR,
                            );
                            any_still_pending = true;
                        }
                        Ok(crate::domain::repos::CleanupOutcome::TerminalFailure) => {
                            self.metrics
                                .record_cleanup_failed(metric_labels::resource_type::FILE);
                            warn!(chat_id = %chat_id, attachment_id = %att.id, "chat cleanup: attachment terminal failure");
                        }
                        Ok(crate::domain::repos::CleanupOutcome::AlreadyTerminal) => {
                            tracing::debug!(chat_id = %chat_id, attachment_id = %att.id, "chat cleanup: attachment already terminal (stale)");
                        }
                        Err(db_err) => {
                            warn!(chat_id = %chat_id, attachment_id = %att.id, error = %db_err, "chat cleanup: db error recording attempt");
                            any_still_pending = true;
                        }
                    }
                    continue;
                }
            }

            // Secondary-provider delete (best-effort).
            //
            // The chat-cleanup payload carries `secondary_upstream_alias`
            // resolved at chat-delete time; the attachment row carries
            // `secondary_file_id` and `secondary_provider_kind` from the
            // parallel upload. Failure is logged but does NOT block marking
            // this attachment done — the primary cleanup gates the
            // user-visible state, and a secondary orphan needs a separate
            // reaper. Today only `provider_kind = "anthropic"` is wired.
            if let (Some(file_id), Some(provider_kind), Some(alias)) = (
                att.secondary_file_id.as_deref(),
                att.secondary_provider_kind.as_deref(),
                event.secondary_upstream_alias.as_deref(),
            ) {
                use crate::infra::db::entity::attachment::{
                    SecondaryUploadStatus, secondary_provider_kind,
                };
                if att.secondary_status == SecondaryUploadStatus::Uploaded {
                    match provider_kind {
                        secondary_provider_kind::ANTHROPIC => {
                            if let Some(client) = self.anthropic_files_client.as_ref() {
                                let anth_ctx = tenant_security_context(event.tenant_id);
                                match client.delete_file(anth_ctx, alias, file_id).await {
                                    Ok(()) => {
                                        tracing::debug!(
                                            chat_id = %chat_id,
                                            attachment_id = %att.id,
                                            anthropic_file_id = %file_id,
                                            "chat cleanup: Anthropic file deleted"
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            chat_id = %chat_id,
                                            attachment_id = %att.id,
                                            anthropic_file_id = %file_id,
                                            error = %e,
                                            "chat cleanup: Anthropic delete failed (orphaned); \
                                             continuing"
                                        );
                                    }
                                }
                            } else {
                                warn!(
                                    chat_id = %chat_id,
                                    attachment_id = %att.id,
                                    secondary_file_id = %file_id,
                                    provider_kind = %provider_kind,
                                    "chat cleanup: attachment references anthropic secondary but \
                                     no client configured; orphaned"
                                );
                                self.metrics.record_secondary_cleanup_skipped(provider_kind);
                            }
                        }
                        other => {
                            warn!(
                                chat_id = %chat_id,
                                attachment_id = %att.id,
                                provider_kind = %other,
                                "chat cleanup: unknown secondary_provider_kind on attachment; skipping"
                            );
                        }
                    }
                }
            }

            // Success — mark done
            if let Err(e) = self.attachment_repo.mark_cleanup_done(&conn, att.id).await {
                warn!(chat_id = %chat_id, attachment_id = %att.id, error = %e, "chat cleanup: failed to mark done");
                any_still_pending = true;
                continue;
            }

            // Only count as completed file cleanup if a provider file was actually deleted.
            if att.provider_file_id.is_some() {
                self.metrics
                    .record_cleanup_completed(metric_labels::resource_type::FILE);
            }
            tracing::debug!(chat_id = %chat_id, attachment_id = %att.id, "chat cleanup: attachment done");
        }

        // 5. If any attachments are still pending → retry later
        if any_still_pending {
            return MessageResult::Retry;
        }

        // 6. Vector store cleanup — only after all attachments are terminal.
        //    Every `chat_vector_stores` query carries the event's tenant.
        let vs_scope = toolkit_security::AccessScope::for_tenant(tenant_id);
        let vs_row = match self
            .vector_store_repo
            .find_by_chat(&conn, &vs_scope, chat_id)
            .await
        {
            Ok(vs) => vs,
            Err(e) => {
                warn!(chat_id = %chat_id, error = %e, "chat cleanup: db error loading vector store");
                return MessageResult::Retry;
            }
        };

        if let Some(vs_row) = vs_row {
            // Double-check: no pending attachments left
            match self
                .attachment_repo
                .find_pending_cleanup_by_chat(&conn, chat_id)
                .await
            {
                Ok(still) if !still.is_empty() => {
                    return MessageResult::Retry;
                }
                Err(e) => {
                    warn!(chat_id = %chat_id, error = %e, "chat cleanup: db error re-checking attachments");
                    return MessageResult::Retry;
                }
                _ => {}
            }

            // Check for failed attachments → log warning (metric in Phase 5)
            let failed_count = match self
                .attachment_repo
                .count_failed_cleanup_by_chat(&conn, chat_id)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    warn!(chat_id = %chat_id, error = %e, "chat cleanup: db error counting failed attachments");
                    return MessageResult::Retry;
                }
            };
            if failed_count > 0 {
                warn!(
                    chat_id = %chat_id,
                    failed_count,
                    "chat cleanup: deleting vector store with failed attachment cleanup"
                );
                self.metrics.record_cleanup_vs_with_failed_attachments();
            }

            // Delete provider vector store if it has an ID
            if let Some(ref vs_id) = vs_row.vector_store_id {
                let vs_ctx = tenant_security_context(event.tenant_id);
                if let Err(e) = self
                    .vs_provider
                    .delete_vector_store(vs_ctx, &vs_row.provider, vs_id)
                    .await
                {
                    warn!(chat_id = %chat_id, vector_store_id = vs_id, error = %e, "chat cleanup: vector store delete failed");
                    self.metrics.record_cleanup_retry(
                        metric_labels::resource_type::VECTOR_STORE,
                        metric_labels::cleanup_retry_reason::VECTOR_STORE_DELETE_FAILED,
                    );
                    let result =
                        bound_vector_store_retry(msg.attempts, self.max_attempts, chat_id, vs_id);
                    if matches!(result, MessageResult::Reject(_)) {
                        self.metrics
                            .record_cleanup_failed(metric_labels::resource_type::VECTOR_STORE);
                    }
                    return result;
                }

                info!(chat_id = %chat_id, vector_store_id = vs_id, "chat cleanup: vector store deleted on provider");
            }

            // Hard-delete the chat_vector_stores row (durable completion marker)
            if let Err(e) = self
                .vector_store_repo
                .delete(&conn, &vs_scope, vs_row.id)
                .await
            {
                warn!(chat_id = %chat_id, error = %e, "chat cleanup: failed to delete VS row");
                return MessageResult::Retry;
            }

            // Record metric only after durable completion (avoids double-counting on retry).
            if vs_row.vector_store_id.is_some() {
                self.metrics
                    .record_cleanup_completed(metric_labels::resource_type::VECTOR_STORE);
            }
            info!(chat_id = %chat_id, "chat cleanup: vector store row removed");
        }

        info!(chat_id = %chat_id, "chat cleanup: complete");
        MessageResult::Ok
    }
}

/// Retry a failed vector-store delete until the message has been delivered
/// `max_attempts` times, then dead-letter it. Deliveries spent waiting on
/// pending attachments count toward the same budget. The
/// `chat_vector_stores` row stays in place, so a replayed dead letter
/// retries the delete.
fn bound_vector_store_retry(
    attempts: i16,
    max_attempts: u32,
    chat_id: uuid::Uuid,
    vector_store_id: &str,
) -> MessageResult {
    let this_attempt = u32::try_from(attempts).unwrap_or(0).saturating_add(1);
    if this_attempt < max_attempts {
        return MessageResult::Retry;
    }
    warn!(
        chat_id = %chat_id,
        vector_store_id,
        max_attempts,
        "chat cleanup: vector store delete failed on the last attempt -- dead-lettering"
    );
    MessageResult::Reject(format!(
        "vector store delete: max attempts ({max_attempts}) reached"
    ))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn make_msg() -> OutboxMessage {
        OutboxMessage {
            partition_id: 1,
            seq: 1,
            payload: b"{}".to_vec(),
            payload_type: "application/json".to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0i16,
        }
    }

    fn make_cleanup_payload(provider_file_id: Option<&str>) -> OutboxMessage {
        let event = serde_json::json!({
            "event_type": "attachment_deleted",
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "chat_id": "00000000-0000-0000-0000-000000000002",
            "attachment_id": "00000000-0000-0000-0000-000000000003",
            "provider_file_id": provider_file_id,
            "vector_store_id": null,
            "storage_backend": "openai",
            "attachment_kind": "document",
            "deleted_at": "2026-01-01T00:00:00Z"
        });
        OutboxMessage {
            partition_id: 1,
            seq: 1,
            payload: serde_json::to_vec(&event).unwrap(),
            payload_type: "application/json".to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0i16,
        }
    }

    #[tokio::test]
    async fn attachment_handler_rejects_invalid_payload() {
        use crate::domain::service::test_helpers::inmem_db;

        let db = inmem_db().await;
        let db_provider = crate::domain::service::test_helpers::mock_db_provider(db);
        let handler = AttachmentCleanupHandler::new(
            Arc::new(crate::domain::service::test_helpers::NoopFileStorage),
            db_provider,
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            }),
            5,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            None, // anthropic_files_client — Anthropic cleanup is exercised separately
        );

        let msg = make_msg(); // payload is "{}" — missing required fields
        let result = handler.handle(&msg).await;
        assert!(
            matches!(result, MessageResult::Reject(_)),
            "invalid payload should be rejected"
        );
    }

    #[tokio::test]
    async fn attachment_handler_succeeds_no_provider_file() {
        use crate::domain::service::test_helpers::inmem_db;

        let db = inmem_db().await;
        let db_provider = crate::domain::service::test_helpers::mock_db_provider(db);
        let handler = AttachmentCleanupHandler::new(
            Arc::new(crate::domain::service::test_helpers::NoopFileStorage),
            db_provider,
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            }),
            5,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            None, // anthropic_files_client — Anthropic cleanup is exercised separately
        );

        let msg = make_cleanup_payload(None);
        let result = handler.handle(&msg).await;
        // mark_done will fail (attachment doesn't exist in DB) → Retry
        // but the important thing is it doesn't Reject for missing provider_file_id
        assert!(
            matches!(result, MessageResult::Ok | MessageResult::Retry),
            "no provider file should not reject"
        );
    }

    #[tokio::test]
    async fn deserialize_cleanup_payload() {
        let msg = make_cleanup_payload(Some("file-abc123"));
        let payload: AttachmentCleanupPayload =
            serde_json::from_slice(&msg.payload).expect("deserialization should succeed");
        assert_eq!(
            payload.attachment_id.to_string(),
            "00000000-0000-0000-0000-000000000003"
        );
        assert_eq!(payload.provider_file_id.as_deref(), Some("file-abc123"));
        assert_eq!(payload.storage_backend, "openai");
    }

    // ── Chat cleanup handler tests ──────────────────────────────────

    fn make_chat_cleanup_payload(chat_id: uuid::Uuid, tenant_id: uuid::Uuid) -> OutboxMessage {
        let event = serde_json::json!({
            "reason": "chat_soft_delete",
            "tenant_id": tenant_id.to_string(),
            "chat_id": chat_id.to_string(),
            "system_request_id": uuid::Uuid::new_v4().to_string(),
            "chat_deleted_at": "2026-01-01T00:00:00+00:00",
        });
        OutboxMessage {
            partition_id: 1,
            seq: 1,
            payload: serde_json::to_vec(&event).unwrap(),
            payload_type: "application/json".to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0i16,
        }
    }

    fn build_chat_handler(db_provider: Arc<DbProvider>) -> ChatCleanupHandler {
        use crate::domain::service::test_helpers::{NoopFileStorage, NoopVectorStoreProvider};
        ChatCleanupHandler::new(
            Arc::new(NoopFileStorage),
            Arc::new(NoopVectorStoreProvider),
            db_provider,
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            }),
            5,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            None, // anthropic_files_client — Anthropic cleanup is exercised separately
        )
    }

    #[tokio::test]
    async fn chat_cleanup_rejects_invalid_payload() {
        use crate::domain::service::test_helpers::inmem_db;

        let db = inmem_db().await;
        let handler =
            build_chat_handler(crate::domain::service::test_helpers::mock_db_provider(db));

        let msg = make_msg(); // "{}" — missing fields
        let result = handler.handle(&msg).await;
        assert!(
            matches!(result, MessageResult::Reject(_)),
            "invalid payload should be rejected"
        );
    }

    #[tokio::test]
    async fn chat_cleanup_rejects_active_chat() {
        use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};

        let db = inmem_db().await;
        let handler = build_chat_handler(mock_db_provider(db));

        // Non-existent chat → is_deleted_system returns false
        let msg = make_chat_cleanup_payload(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let result = handler.handle(&msg).await;
        assert!(
            matches!(result, MessageResult::Reject(_)),
            "active/non-existent chat should be rejected"
        );
    }

    #[tokio::test]
    async fn chat_cleanup_succeeds_empty_chat() {
        use crate::domain::repos::ChatRepository as _;
        use crate::domain::service::test_helpers::{inmem_db, mock_db_provider};

        let db = inmem_db().await;
        let db_provider = mock_db_provider(db.clone());

        // Create and soft-delete a chat
        let chat_repo =
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            });
        let tenant_id = uuid::Uuid::new_v4();
        let user_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();
        let scope = toolkit_security::AccessScope::allow_all();
        let conn = db_provider.conn().unwrap();

        let chat = crate::domain::models::Chat {
            id: chat_id,
            tenant_id,
            user_id,
            model: "test-model".to_owned(),
            title: Some("test".to_owned()),
            is_temporary: false,
            created_at: time::OffsetDateTime::now_utc(),
            updated_at: time::OffsetDateTime::now_utc(),
        };
        chat_repo.create(&conn, &scope, chat).await.unwrap();
        chat_repo.soft_delete(&conn, &scope, chat_id).await.unwrap();

        let handler = build_chat_handler(db_provider);
        let msg = make_chat_cleanup_payload(chat_id, uuid::Uuid::new_v4());
        let result = handler.handle(&msg).await;
        assert!(
            matches!(result, MessageResult::Ok),
            "empty soft-deleted chat should succeed, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn deserialize_chat_cleanup_payload() {
        let chat_id = uuid::Uuid::new_v4();
        let msg = make_chat_cleanup_payload(chat_id, uuid::Uuid::new_v4());
        let payload: ChatCleanupPayload =
            serde_json::from_slice(&msg.payload).expect("deserialization should succeed");
        assert_eq!(payload.chat_id, chat_id);
        assert_eq!(
            payload.reason,
            crate::domain::repos::CleanupReason::ChatSoftDelete
        );
    }

    // ── State-machine tests with seeded DB ──────────────────────────────

    /// Insert a minimal attachment row with `cleanup_status` = 'pending'
    /// and `deleted_at` set (soft-deleted).
    async fn seed_pending_attachment(
        db: &Arc<DbProvider>,
        chat_id: uuid::Uuid,
        tenant_id: uuid::Uuid,
        provider_file_id: Option<&str>,
    ) -> uuid::Uuid {
        use crate::domain::repos::{AttachmentRepository as _, InsertAttachmentParams};
        let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
        let scope = toolkit_security::AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let att_id = uuid::Uuid::new_v4();
        // Insert in pending status
        repo.insert(
            &conn,
            &scope,
            InsertAttachmentParams {
                id: att_id,
                tenant_id,
                chat_id,
                uploaded_by_user_id: uuid::Uuid::new_v4(),
                filename: "test.txt".to_owned(),
                content_type: "text/plain".to_owned(),
                size_bytes: 100,
                storage_backend: "openai".to_owned(),
                attachment_kind: "document".to_owned(),
                for_file_search: false,
                for_code_interpreter: false,
            },
        )
        .await
        .expect("insert attachment");

        // If provider_file_id is set, transition to uploaded
        if let Some(pfid) = provider_file_id {
            use crate::domain::repos::SetUploadedParams;
            repo.cas_set_uploaded(
                &conn,
                &scope,
                SetUploadedParams {
                    id: att_id,
                    provider_file_id: pfid.to_owned(),
                    size_bytes: 100,
                },
            )
            .await
            .expect("set uploaded");
        }

        // Mark cleanup pending BEFORE soft-deleting (mimics the chat-deletion TX
        // where attachments are NOT individually soft-deleted, only marked pending).
        repo.mark_attachments_pending_for_chat(&conn, chat_id)
            .await
            .expect("mark pending");

        att_id
    }

    /// Create a soft-deleted chat in the DB.
    async fn seed_deleted_chat(db: &Arc<DbProvider>) -> (uuid::Uuid, uuid::Uuid) {
        use crate::domain::repos::ChatRepository as _;
        let chat_repo =
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            });
        let tenant_id = uuid::Uuid::new_v4();
        let chat_id = uuid::Uuid::new_v4();
        let scope = toolkit_security::AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let chat = crate::domain::models::Chat {
            id: chat_id,
            tenant_id,
            user_id: uuid::Uuid::new_v4(),
            model: "test-model".to_owned(),
            title: Some("test".to_owned()),
            is_temporary: false,
            created_at: time::OffsetDateTime::now_utc(),
            updated_at: time::OffsetDateTime::now_utc(),
        };
        chat_repo.create(&conn, &scope, chat).await.unwrap();
        chat_repo.soft_delete(&conn, &scope, chat_id).await.unwrap();
        (chat_id, tenant_id)
    }

    #[tokio::test]
    async fn chat_cleanup_processes_pending_attachment_success() {
        use crate::domain::repos::AttachmentRepository as _;
        use crate::domain::service::test_helpers::inmem_db;

        let db = inmem_db().await;
        let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());

        let (chat_id, tenant_id) = seed_deleted_chat(&db_provider).await;
        seed_pending_attachment(&db_provider, chat_id, tenant_id, Some("file-123")).await;

        let handler = build_chat_handler(Arc::clone(&db_provider));
        let msg = make_chat_cleanup_payload(chat_id, tenant_id);
        let result = handler.handle(&msg).await;

        assert!(
            matches!(result, MessageResult::Ok),
            "should succeed with NoopFileStorage, got: {result:?}"
        );

        // Verify attachment is now 'done'
        let conn = db_provider.conn().unwrap();
        let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
        let pending = repo
            .find_pending_cleanup_by_chat(&conn, chat_id)
            .await
            .unwrap();
        assert!(pending.is_empty(), "no attachments should remain pending");
    }

    #[tokio::test]
    async fn chat_cleanup_retries_on_provider_failure() {
        use crate::domain::repos::AttachmentRepository as _;
        use crate::domain::service::test_helpers::{
            FailingFileStorage, NoopVectorStoreProvider, inmem_db,
        };

        let db = inmem_db().await;
        let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());

        let (chat_id, tenant_id) = seed_deleted_chat(&db_provider).await;
        seed_pending_attachment(&db_provider, chat_id, tenant_id, Some("file-456")).await;

        // Use FailingFileStorage — provider always errors
        let handler = ChatCleanupHandler::new(
            Arc::new(FailingFileStorage),
            Arc::new(NoopVectorStoreProvider),
            Arc::clone(&db_provider),
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            }),
            5, // max_attempts
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            None, // anthropic_files_client
        );

        let msg = make_chat_cleanup_payload(chat_id, tenant_id);
        let result = handler.handle(&msg).await;

        assert!(
            matches!(result, MessageResult::Retry),
            "should retry on provider failure, got: {result:?}"
        );

        // Verify attachment is still pending with incremented attempts
        let conn = db_provider.conn().unwrap();
        let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
        let pending = repo
            .find_pending_cleanup_by_chat(&conn, chat_id)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1, "attachment should still be pending");
        assert_eq!(
            pending[0].cleanup_attempts, 1,
            "attempts should be incremented"
        );
        assert!(
            pending[0].last_cleanup_error.is_some(),
            "error should be recorded"
        );
    }

    #[tokio::test]
    async fn chat_cleanup_terminal_failure_at_max_attempts() {
        use crate::domain::repos::AttachmentRepository as _;
        use crate::domain::service::test_helpers::{
            FailingFileStorage, NoopVectorStoreProvider, inmem_db,
        };

        let db = inmem_db().await;
        let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());

        let (chat_id, tenant_id) = seed_deleted_chat(&db_provider).await;
        seed_pending_attachment(&db_provider, chat_id, tenant_id, Some("file-789")).await;

        // max_attempts = 1 → first failure is terminal
        let handler = ChatCleanupHandler::new(
            Arc::new(FailingFileStorage),
            Arc::new(NoopVectorStoreProvider),
            Arc::clone(&db_provider),
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            }),
            1, // max_attempts = 1 → immediately terminal
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            None, // anthropic_files_client
        );

        let msg = make_chat_cleanup_payload(chat_id, tenant_id);
        let result = handler.handle(&msg).await;

        // All attachments terminal (failed) → handler proceeds to VS check → Success
        assert!(
            matches!(result, MessageResult::Ok),
            "all attachments terminal -> should succeed, got: {result:?}"
        );

        // Verify attachment is now 'failed'
        // Need the attachment ID — re-seed returns it
        // Actually we need to find it. Let's use find_pending which should return empty.
        let conn = db_provider.conn().unwrap();
        let repo = crate::infra::db::repo::attachment_repo::AttachmentRepository;
        let pending = repo
            .find_pending_cleanup_by_chat(&conn, chat_id)
            .await
            .unwrap();
        assert!(
            pending.is_empty(),
            "no pending attachments -- the one we had should be 'failed'"
        );

        // Also verify count_failed returns 1
        let failed = repo
            .count_failed_cleanup_by_chat(&conn, chat_id)
            .await
            .unwrap();
        assert_eq!(
            failed, 1,
            "one attachment should be in terminal failed state"
        );
    }

    #[test]
    fn bound_vector_store_retry_rejects_on_last_attempt() {
        let chat_id = uuid::Uuid::new_v4();
        assert!(matches!(
            bound_vector_store_retry(0, 3, chat_id, "vs-1"),
            MessageResult::Retry
        ));
        assert!(matches!(
            bound_vector_store_retry(1, 3, chat_id, "vs-1"),
            MessageResult::Retry
        ));
        assert!(matches!(
            bound_vector_store_retry(2, 3, chat_id, "vs-1"),
            MessageResult::Reject(_)
        ));
        assert!(matches!(
            bound_vector_store_retry(7, 3, chat_id, "vs-1"),
            MessageResult::Reject(_)
        ));
    }

    /// Vector store provider whose delete always fails.
    struct FailingVectorStoreDelete;

    #[async_trait]
    impl crate::domain::ports::VectorStoreProvider for FailingVectorStoreDelete {
        async fn create_vector_store(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
        ) -> Result<String, crate::domain::ports::FileStorageError> {
            Ok("vs-unused".to_owned())
        }

        async fn add_file_to_vector_store(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
            _params: crate::domain::ports::AddFileToVectorStoreParams,
        ) -> Result<
            crate::domain::ports::VectorStoreFileStatus,
            crate::domain::ports::FileStorageError,
        > {
            Ok(crate::domain::ports::VectorStoreFileStatus::Completed)
        }

        async fn get_vector_store_file_status(
            &self,
            _ctx: toolkit_security::SecurityContext,
            _provider_id: &str,
            _vector_store_id: &str,
            _provider_file_id: &str,
        ) -> Result<
            crate::domain::ports::VectorStoreFileStatus,
            crate::domain::ports::FileStorageError,
        > {
            Ok(crate::domain::ports::VectorStoreFileStatus::Completed)
        }

        async fn delete_vector_store(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
            _vector_store_id: &str,
        ) -> Result<(), crate::domain::ports::FileStorageError> {
            Err(crate::domain::ports::FileStorageError::Unavailable {
                message: "delete returned 500".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn chat_cleanup_vector_store_delete_failure_is_bounded() {
        use crate::domain::repos::{InsertVectorStoreParams, VectorStoreRepository as _};
        use crate::domain::service::test_helpers::{NoopFileStorage, inmem_db};

        let db = inmem_db().await;
        let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());
        let (chat_id, tenant_id) = seed_deleted_chat(&db_provider).await;

        let vs_repo = crate::infra::db::repo::vector_store_repo::VectorStoreRepository;
        let scope = toolkit_security::AccessScope::allow_all();
        let conn = db_provider.conn().unwrap();
        let row = vs_repo
            .insert(
                &conn,
                &scope,
                InsertVectorStoreParams {
                    id: uuid::Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    provider: "openai".to_owned(),
                },
            )
            .await
            .expect("insert vector store row");
        vs_repo
            .cas_set_vector_store_id(&conn, &scope, row.id, "vs-abc")
            .await
            .expect("set vector store id");

        let handler = ChatCleanupHandler::new(
            Arc::new(NoopFileStorage),
            Arc::new(FailingVectorStoreDelete),
            Arc::clone(&db_provider),
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            }),
            3, // max_attempts
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            None,
        );

        let mut msg = make_chat_cleanup_payload(chat_id, tenant_id);
        for attempts in 0..2 {
            msg.attempts = attempts;
            let result = handler.handle(&msg).await;
            assert!(
                matches!(result, MessageResult::Retry),
                "delivery {attempts}: expected Retry, got {result:?}"
            );
        }
        msg.attempts = 2;
        let result = handler.handle(&msg).await;
        assert!(
            matches!(result, MessageResult::Reject(ref r) if r.contains("max attempts (3)")),
            "last delivery: expected Reject, got {result:?}"
        );

        // The row stays so a replayed dead letter retries the delete.
        let remaining = vs_repo
            .find_by_chat(&conn, &toolkit_security::AccessScope::allow_all(), chat_id)
            .await
            .expect("load vector store row");
        assert!(remaining.is_some(), "chat_vector_stores row must stay");
    }

    /// Vector store provider that records every delete call.
    #[derive(Default)]
    struct RecordingVectorStoreDelete {
        deleted: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl crate::domain::ports::VectorStoreProvider for RecordingVectorStoreDelete {
        async fn create_vector_store(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
        ) -> Result<String, crate::domain::ports::FileStorageError> {
            Ok("vs-unused".to_owned())
        }

        async fn add_file_to_vector_store(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
            _params: crate::domain::ports::AddFileToVectorStoreParams,
        ) -> Result<
            crate::domain::ports::VectorStoreFileStatus,
            crate::domain::ports::FileStorageError,
        > {
            Ok(crate::domain::ports::VectorStoreFileStatus::Completed)
        }

        async fn get_vector_store_file_status(
            &self,
            _ctx: toolkit_security::SecurityContext,
            _provider_id: &str,
            _vector_store_id: &str,
            _provider_file_id: &str,
        ) -> Result<
            crate::domain::ports::VectorStoreFileStatus,
            crate::domain::ports::FileStorageError,
        > {
            Ok(crate::domain::ports::VectorStoreFileStatus::Completed)
        }

        async fn delete_vector_store(
            &self,
            _ctx: SecurityContext,
            _provider_id: &str,
            vector_store_id: &str,
        ) -> Result<(), crate::domain::ports::FileStorageError> {
            self.deleted
                .lock()
                .unwrap()
                .push(vector_store_id.to_owned());
            Ok(())
        }
    }

    #[tokio::test]
    async fn chat_cleanup_ignores_vector_store_row_of_other_tenant() {
        use crate::domain::repos::{InsertVectorStoreParams, VectorStoreRepository as _};
        use crate::domain::service::test_helpers::{NoopFileStorage, inmem_db};

        let db = inmem_db().await;
        let db_provider = crate::domain::service::test_helpers::mock_db_provider(db.clone());
        let (chat_id, tenant_a) = seed_deleted_chat(&db_provider).await;
        let tenant_b = uuid::Uuid::new_v4();

        let vs_repo = crate::infra::db::repo::vector_store_repo::VectorStoreRepository;
        let scope = toolkit_security::AccessScope::allow_all();
        let conn = db_provider.conn().unwrap();
        let row_a = vs_repo
            .insert(
                &conn,
                &scope,
                InsertVectorStoreParams {
                    id: uuid::Uuid::new_v4(),
                    tenant_id: tenant_a,
                    chat_id,
                    provider: "openai".to_owned(),
                },
            )
            .await
            .expect("insert tenant-A vector store row");
        vs_repo
            .cas_set_vector_store_id(&conn, &scope, row_a.id, "vs-tenant-a")
            .await
            .expect("set vector store id");
        // Same chat_id, different tenant.
        let row_b = vs_repo
            .insert(
                &conn,
                &scope,
                InsertVectorStoreParams {
                    id: uuid::Uuid::new_v4(),
                    tenant_id: tenant_b,
                    chat_id,
                    provider: "openai".to_owned(),
                },
            )
            .await
            .expect("insert tenant-B vector store row");
        vs_repo
            .cas_set_vector_store_id(&conn, &scope, row_b.id, "vs-tenant-b")
            .await
            .expect("set vector store id");

        let vs_provider = Arc::new(RecordingVectorStoreDelete::default());
        let handler = ChatCleanupHandler::new(
            Arc::new(NoopFileStorage),
            Arc::clone(&vs_provider) as Arc<dyn crate::domain::ports::VectorStoreProvider>,
            Arc::clone(&db_provider),
            crate::infra::db::repo::chat_repo::ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            }),
            3,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            None,
        );

        let result = handler
            .handle(&make_chat_cleanup_payload(chat_id, tenant_a))
            .await;
        assert!(
            matches!(result, MessageResult::Ok),
            "expected Ok, got {result:?}"
        );

        assert_eq!(
            *vs_provider.deleted.lock().unwrap(),
            vec!["vs-tenant-a".to_owned()],
            "only the tenant-A vector store is deleted at the provider"
        );
        let row_a_left = vs_repo
            .find_by_chat(
                &conn,
                &toolkit_security::AccessScope::for_tenant(tenant_a),
                chat_id,
            )
            .await
            .expect("load tenant-A vector store row");
        assert!(row_a_left.is_none(), "tenant-A row must be removed");
        let remaining = vs_repo
            .find_by_chat(
                &conn,
                &toolkit_security::AccessScope::for_tenant(tenant_b),
                chat_id,
            )
            .await
            .expect("load tenant-B vector store row");
        let remaining = remaining.expect("tenant-B chat_vector_stores row must stay");
        assert_eq!(remaining.id, row_b.id);
        assert_eq!(remaining.vector_store_id.as_deref(), Some("vs-tenant-b"));
    }
}
