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
use modkit_db::DBProvider;
use modkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use modkit_security::SecurityContext;
use serde::Deserialize;
use tracing::{info, warn};

use crate::domain::ports::{FileStorageProvider, metric_labels};

type DbProvider = DBProvider<modkit_db::DbError>;
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
        .subject_id(modkit_security::constants::DEFAULT_SUBJECT_ID)
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
}

impl AttachmentCleanupHandler {
    pub fn new(
        file_storage: Arc<dyn FileStorageProvider>,
        db: Arc<DbProvider>,
        chat_repo: ChatRepo,
        max_attempts: u32,
        metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort>,
    ) -> Self {
        Self {
            file_storage,
            attachment_repo: crate::infra::db::repo::attachment_repo::AttachmentRepository,
            chat_repo,
            db,
            max_attempts,
            metrics,
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
}

#[async_trait]
impl LeasedMessageHandler for AttachmentCleanupHandler {
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
        //    RagHttpClient.delete() is best-effort (404 = success).
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
                self.metrics
                    .record_cleanup_retry(metric_labels::resource_type::FILE, error);
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
}

#[async_trait]
impl LeasedMessageHandler for ChatCleanupHandler {
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
                                &error_str,
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

        // 6. Vector store cleanup — only after all attachments are terminal
        let vs_row = match self
            .vector_store_repo
            .find_by_chat_system(&conn, chat_id)
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
                    let reason = format!("vector store delete failed: {e}");
                    warn!(chat_id = %chat_id, vector_store_id = vs_id, error = %e, "chat cleanup: vector store delete failed");
                    self.metrics
                        .record_cleanup_retry(metric_labels::resource_type::VECTOR_STORE, &reason);
                    return MessageResult::Retry;
                }

                info!(chat_id = %chat_id, vector_store_id = vs_id, "chat cleanup: vector store deleted on provider");
            }

            // Hard-delete the chat_vector_stores row (durable completion marker)
            if let Err(e) = self.vector_store_repo.delete_system(&conn, vs_row.id).await {
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
// ── Tests ───────────────────────────────────────────────────────────────
#[cfg(test)]
#[path = "cleanup_worker_tests.rs"]
mod tests;
