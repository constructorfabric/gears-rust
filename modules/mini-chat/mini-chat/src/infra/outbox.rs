use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mini_chat_sdk::{
    MiniChatAuditPluginError, PublishError, TurnMutationAuditEventType, UsageEvent,
};
use modkit_db::outbox::Outbox;
use tracing::{info, warn};

use crate::domain::error::DomainError;
use crate::domain::model::audit_envelope::AuditEnvelope;
use crate::domain::ports::{MiniChatMetricsPort, metric_labels};
use crate::domain::repos::{AttachmentCleanupEvent, ChatCleanupEvent, OutboxEnqueuer};
use crate::infra::audit_gateway::AuditGateway;

const AUDIT_PLUGIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Infrastructure implementation of [`OutboxEnqueuer`].
///
/// Serializes events to JSON and inserts into the outbox table
/// within the caller's transaction via `modkit_db::outbox::Outbox::enqueue()`.
///
/// The `Outbox` handle is set lazily via [`set_outbox`] — this allows the
/// enqueuer to be constructed in `init()` (where services need it) while the
/// outbox pipeline starts later in `start()` (after OAGW registration).
/// Enqueue is never called before `start()` because HTTP traffic doesn't arrive
/// until after all modules have started.
pub struct InfraOutboxEnqueuer {
    outbox: std::sync::OnceLock<Arc<Outbox>>,
    usage_queue_name: String,
    cleanup_queue_name: String,
    chat_cleanup_queue_name: String,
    #[allow(dead_code)]
    thread_summary_queue_name: String,
    audit_queue_name: String,
    num_partitions: u32,
}

impl InfraOutboxEnqueuer {
    pub(crate) fn new(
        usage_queue_name: String,
        cleanup_queue_name: String,
        chat_cleanup_queue_name: String,
        thread_summary_queue_name: String,
        audit_queue_name: String,
        num_partitions: u32,
    ) -> Self {
        Self {
            outbox: std::sync::OnceLock::new(),
            usage_queue_name,
            cleanup_queue_name,
            chat_cleanup_queue_name,
            thread_summary_queue_name,
            audit_queue_name,
            num_partitions,
        }
    }

    /// Set the outbox handle after the pipeline starts in `start()`.
    /// Panics if called more than once.
    pub(crate) fn set_outbox(&self, outbox: Arc<Outbox>) {
        assert!(
            self.outbox.set(outbox).is_ok(),
            "InfraOutboxEnqueuer::set_outbox called twice"
        );
    }

    fn outbox(&self) -> &Outbox {
        #[allow(clippy::expect_used)]
        self.outbox
            .get()
            .expect("outbox not set -- enqueue called before start()")
    }

    fn partition_for(&self, tenant_id: uuid::Uuid) -> u32 {
        Self::compute_partition(tenant_id, self.num_partitions)
    }

    fn compute_partition(tenant_id: uuid::Uuid, num_partitions: u32) -> u32 {
        let hash = tenant_id.as_u128();
        #[allow(clippy::cast_possible_truncation)]
        {
            (hash % u128::from(num_partitions)) as u32
        }
    }

    /// Enqueue a thread summary task event within the caller's transaction.
    ///
    /// Partitions by `chat_id` so all summary events for a given chat land in
    /// the same partition (processed in order by a single consumer).
    #[allow(dead_code)]
    pub async fn enqueue_thread_summary_task(
        &self,
        runner: &(dyn modkit_db::secure::DBRunner + Sync),
        chat_id: uuid::Uuid,
        payload: Vec<u8>,
    ) -> Result<(), DomainError> {
        let partition = Self::compute_partition(chat_id, self.num_partitions);

        self.outbox()
            .enqueue(
                runner,
                &self.thread_summary_queue_name,
                partition,
                payload,
                "application/json",
            )
            .await
            .map_err(|e| DomainError::internal(format!("outbox enqueue: {e}")))?;

        info!(
            queue = %self.thread_summary_queue_name,
            partition,
            chat_id = %chat_id,
            "thread summary task enqueued"
        );

        Ok(())
    }
}

#[async_trait]
impl OutboxEnqueuer for InfraOutboxEnqueuer {
    async fn enqueue_usage_event(
        &self,
        runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: UsageEvent,
    ) -> Result<(), DomainError> {
        let partition = self.partition_for(event.tenant_id);
        let payload = serde_json::to_vec(&event)
            .map_err(|e| DomainError::internal(format!("serialize UsageEvent: {e}")))?;

        self.outbox()
            .enqueue(
                runner,
                &self.usage_queue_name,
                partition,
                payload,
                "application/json",
            )
            .await
            .map_err(|e| DomainError::internal(format!("outbox enqueue: {e}")))?;

        info!(
            queue = %self.usage_queue_name,
            partition,
            tenant_id = %event.tenant_id,
            turn_id = %event.turn_id,
            "usage event enqueued"
        );

        Ok(())
    }

    async fn enqueue_attachment_cleanup(
        &self,
        runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: AttachmentCleanupEvent,
    ) -> Result<(), DomainError> {
        let partition = self.partition_for(event.tenant_id);
        let payload = serde_json::to_vec(&event)
            .map_err(|e| DomainError::internal(format!("serialize AttachmentCleanupEvent: {e}")))?;

        self.outbox()
            .enqueue(
                runner,
                &self.cleanup_queue_name,
                partition,
                payload,
                "application/json",
            )
            .await
            .map_err(|e| DomainError::internal(format!("outbox enqueue: {e}")))?;

        info!(
            queue = %self.cleanup_queue_name,
            partition,
            tenant_id = %event.tenant_id,
            attachment_id = %event.attachment_id,
            "attachment cleanup event enqueued"
        );

        Ok(())
    }

    async fn enqueue_chat_cleanup(
        &self,
        runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: ChatCleanupEvent,
    ) -> Result<(), DomainError> {
        // Partition by chat_id so all cleanup messages for the same chat
        // are serialized within one partition.
        let partition = Self::compute_partition(event.chat_id, self.num_partitions);
        let payload = serde_json::to_vec(&event)
            .map_err(|e| DomainError::internal(format!("serialize ChatCleanupEvent: {e}")))?;

        self.outbox()
            .enqueue(
                runner,
                &self.chat_cleanup_queue_name,
                partition,
                payload,
                "application/json",
            )
            .await
            .map_err(|e| DomainError::internal(format!("outbox enqueue: {e}")))?;

        info!(
            queue = %self.chat_cleanup_queue_name,
            partition,
            chat_id = %event.chat_id,
            system_request_id = %event.system_request_id,
            "chat cleanup event enqueued"
        );

        Ok(())
    }

    async fn enqueue_audit_event(
        &self,
        runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: AuditEnvelope,
    ) -> Result<(), DomainError> {
        let tenant_id = match &event {
            AuditEnvelope::Turn(e) => e.tenant_id,
            AuditEnvelope::Mutation(e) => e.tenant_id,
            AuditEnvelope::Delete(e) => e.tenant_id,
        };
        let partition = self.partition_for(tenant_id);
        let payload = serde_json::to_vec(&event)
            .map_err(|e| DomainError::internal(format!("serialize AuditEnvelope: {e}")))?;

        self.outbox()
            .enqueue(
                runner,
                &self.audit_queue_name,
                partition,
                payload,
                "application/json",
            )
            .await
            .map_err(|e| DomainError::internal(format!("audit outbox enqueue: {e}")))?;

        info!(
        queue = %self.audit_queue_name,
        partition,
        %tenant_id,
        "audit event enqueued"
        );

        Ok(())
    }

    fn flush(&self) {
        // flush is a no-op if outbox isn't set yet (before start).
        if let Some(outbox) = self.outbox.get() {
            outbox.flush();
        }
    }
}

/// Trait for lazily resolving the model-policy plugin.
///
/// Production code uses `ModelPolicyGateway` (lazy GTS resolution).
/// Tests provide a direct `Arc<dyn MiniChatModelPolicyPluginClientV1>`.
#[async_trait]
pub trait PolicyPluginProvider: Send + Sync {
    async fn get_plugin(
        &self,
    ) -> Result<
        Arc<dyn mini_chat_sdk::MiniChatModelPolicyPluginClientV1>,
        crate::domain::error::DomainError,
    >;
}

#[async_trait]
impl PolicyPluginProvider for crate::infra::model_policy::ModelPolicyGateway {
    async fn get_plugin(
        &self,
    ) -> Result<
        Arc<dyn mini_chat_sdk::MiniChatModelPolicyPluginClientV1>,
        crate::domain::error::DomainError,
    > {
        self.get_policy_plugin().await
    }
}

/// Delivers usage events to the model-policy plugin via `publish_usage()`.
///
/// Deserializes `OutboxMessage.payload` into `UsageEvent`, resolves the plugin
/// lazily via [`PolicyPluginProvider`], calls `publish_usage()`, and maps
/// `PublishError` variants to outbox `MessageResult`:
/// - `Ok(())` → `Ok` (ack + advance cursor)
/// - `PublishError::Transient` → `Retry` (exponential backoff, redelivery)
/// - `PublishError::Permanent` → `Reject` (dead-letter for manual inspection)
/// - Deserialization failure → `Reject` (corrupt payload, permanent)
/// - Plugin resolution failure → `Retry` (transient - plugin may not be ready yet)
pub struct UsageEventHandler {
    pub(crate) plugin_provider: Arc<dyn PolicyPluginProvider>,
}

#[async_trait]
impl modkit_db::outbox::LeasedMessageHandler for UsageEventHandler {
    async fn handle(
        &self,
        msg: &modkit_db::outbox::OutboxMessage,
    ) -> modkit_db::outbox::MessageResult {
        let event = match serde_json::from_slice::<UsageEvent>(&msg.payload) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    payload_len = msg.payload.len(),
                    "usage event deserialization failed: {e}"
                );
                return modkit_db::outbox::MessageResult::Reject(format!(
                    "deserialization failed: {e}"
                ));
            }
        };

        info!(
            tenant_id = %event.tenant_id,
            user_id = %event.user_id,
            turn_id = %event.turn_id,
            request_id = %event.request_id,
            effective_model = %event.effective_model,
            billing_outcome = ?event.billing_outcome,
            settlement_method = ?event.settlement_method,
            actual_credits_micro = event.actual_credits_micro,
            partition_id = msg.partition_id,
            seq = msg.seq,
            "publishing usage event to plugin"
        );

        let plugin = match self.plugin_provider.get_plugin().await {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    error = %e,
                    "failed to resolve policy plugin - will retry"
                );
                return modkit_db::outbox::MessageResult::Retry;
            }
        };

        match plugin.publish_usage(event).await {
            Ok(()) => modkit_db::outbox::MessageResult::Ok,
            Err(PublishError::Transient(reason)) => {
                warn!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    %reason,
                    "publish_usage transient failure - will retry"
                );
                modkit_db::outbox::MessageResult::Retry
            }
            Err(PublishError::Permanent(reason)) => {
                tracing::error!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    %reason,
                    "publish_usage permanent failure - dead-lettering"
                );
                modkit_db::outbox::MessageResult::Reject(reason)
            }
        }
    }
}

/// Delivers audit events to the audit plugin via [`AuditGateway`].
///
/// Deserializes `OutboxMessage.payload` into [`AuditEnvelope`], resolves the
/// plugin via `AuditGateway`, dispatches to the correct `emit_*` method, and
/// maps [`MiniChatAuditPluginError`] to outbox `MessageResult`:
/// - `Ok(())` → `Ok`
/// - `Transient` → `Retry`
/// - `Permanent` → `Reject` (dead-letter)
/// - Deserialization failure → `Reject` (corrupt payload)
/// - Plugin not configured → `Ok` (audit is optional; skip silently)
/// - Plugin resolution error → `Retry` (transient; plugin may not be ready yet)
pub struct AuditEventHandler {
    pub(crate) audit_gateway: Arc<AuditGateway>,
    pub(crate) metrics: Arc<dyn MiniChatMetricsPort>,
}

#[async_trait]
impl modkit_db::outbox::LeasedMessageHandler for AuditEventHandler {
    async fn handle(
        &self,
        msg: &modkit_db::outbox::OutboxMessage,
    ) -> modkit_db::outbox::MessageResult {
        let plugin = match self.audit_gateway.get_plugin().await {
            Ok(Some(p)) => p,
            Ok(None) => {
                // No audit plugin registered - audit is optional; ack and advance.
                return modkit_db::outbox::MessageResult::Ok;
            }
            Err(e) => {
                warn!(error = %e, "audit plugin resolution failed - will retry");
                return modkit_db::outbox::MessageResult::Retry;
            }
        };

        let envelope = match serde_json::from_slice::<AuditEnvelope>(&msg.payload) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    payload_len = msg.payload.len(),
                    "audit event deserialization failed: {e}"
                );
                return modkit_db::outbox::MessageResult::Reject(format!(
                    "deserialization failed: {e}"
                ));
            }
        };

        let result: Result<(), MiniChatAuditPluginError> = match &envelope {
            AuditEnvelope::Turn(evt) => {
                tokio::time::timeout(AUDIT_PLUGIN_TIMEOUT, plugin.emit_turn_audit(evt.clone()))
                    .await
                    .unwrap_or(Err(MiniChatAuditPluginError::PluginTimeout))
            }
            AuditEnvelope::Mutation(evt) => match evt.event_type {
                TurnMutationAuditEventType::TurnRetry => tokio::time::timeout(
                    AUDIT_PLUGIN_TIMEOUT,
                    plugin.emit_turn_retry_audit(evt.clone()),
                )
                .await
                .unwrap_or(Err(MiniChatAuditPluginError::PluginTimeout)),
                TurnMutationAuditEventType::TurnEdit => tokio::time::timeout(
                    AUDIT_PLUGIN_TIMEOUT,
                    plugin.emit_turn_edit_audit(evt.clone()),
                )
                .await
                .unwrap_or(Err(MiniChatAuditPluginError::PluginTimeout)),
            },
            AuditEnvelope::Delete(evt) => tokio::time::timeout(
                AUDIT_PLUGIN_TIMEOUT,
                plugin.emit_turn_delete_audit(evt.clone()),
            )
            .await
            .unwrap_or(Err(MiniChatAuditPluginError::PluginTimeout)),
        };

        match result {
            Ok(()) => {
                self.metrics.record_audit_emit(metric_labels::result::OK);
                modkit_db::outbox::MessageResult::Ok
            }
            Err(e) if e.is_transient() => {
                warn!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    error = %e,
                    "audit emit transient failure - will retry"
                );
                self.metrics.record_audit_emit(metric_labels::result::RETRY);
                modkit_db::outbox::MessageResult::Retry
            }
            Err(e) => {
                tracing::error!(
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    error = %e,
                    "audit emit permanent failure - dead-lettering"
                );
                self.metrics
                    .record_audit_emit(metric_labels::result::REJECT);
                modkit_db::outbox::MessageResult::Reject(e.to_string())
            }
        }
    }
}
#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;
