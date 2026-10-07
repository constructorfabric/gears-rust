use async_trait::async_trait;
use uuid::Uuid;

use crate::audit_models::{
    TurnAuditEvent, TurnDeleteAuditEvent, TurnEditAuditEvent, TurnRetryAuditEvent,
};
use crate::error::{MiniChatAuditPluginError, MiniChatModelPolicyPluginError, PublishError};
use crate::models::{PolicySnapshot, PolicyVersionInfo, UsageEvent, UserLicenseStatus, UserLimits};

/// Plugin API trait for mini-chat model policy implementations.
///
/// Plugins implement this trait to provide model catalog and policy data.
/// The mini-chat gear discovers plugins via GTS types-registry and
/// delegates policy queries to the selected plugin.
///
/// Cancellation is handled by the framework: in-flight futures are dropped
/// on shutdown or lease expiry - no explicit `CancellationToken` needed.
#[async_trait]
pub trait MiniChatModelPolicyPluginClientV1: Send + Sync {
    /// Get the current policy version for a user.
    async fn get_current_policy_version(
        &self,
        user_id: Uuid,
    ) -> Result<PolicyVersionInfo, MiniChatModelPolicyPluginError>;

    /// Get the full policy snapshot for a given version, including
    /// model catalog and kill switches.
    async fn get_policy_snapshot(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<PolicySnapshot, MiniChatModelPolicyPluginError>;

    /// Get per-user credit limits for a specific policy version.
    async fn get_user_limits(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<UserLimits, MiniChatModelPolicyPluginError>;

    /// Check whether a user holds an active `MiniChat` license in the caller's tenant.
    ///
    /// Returns `active: true` when the user's status is `active`.
    /// Returns `active: false` for any other status (`invited`, `deactivated`,
    /// `deleted`) or when the user is not found - this is not an error condition.
    ///
    /// The default implementation returns `active: false` so that existing
    /// out-of-tree V1 plugins remain compatible without code changes.
    async fn check_user_license(
        &self,
        _user_id: Uuid,
    ) -> Result<UserLicenseStatus, MiniChatModelPolicyPluginError> {
        Ok(UserLicenseStatus { active: false })
    }

    /// Publish a usage event after turn finalization.
    ///
    /// Called by the outbox processor after the finalization transaction
    /// commits. Plugins can forward the event to external billing systems.
    async fn publish_usage(&self, payload: UsageEvent) -> Result<(), PublishError>;
}

/// Plugin API trait for mini-chat audit event publishing.
///
/// Plugins implement this trait to receive audit events from the mini-chat
/// gear. The gear lists the plugin instances in the GTS types-registry and
/// selects one of them by the configured `vendor`; events go only to that
/// instance. If no instance matches, audit events are dropped.
///
/// # Content
///
/// In P1 the gear leaves the content fields of `TurnAuditEvent` empty
/// (`prompt` and `response` are `None`, `attachments` is empty,
/// `policy_decisions.license` and `policy_decisions.quota.quota_scope` are
/// `None`) and performs no redaction or truncation (ADR-0009). Redaction must
/// be added in the gear before these fields are filled.
///
/// # Delivery semantics
///
/// Audit emission uses the transactional outbox with leased processing: at
/// least once, so a plugin can see the same event more than once. Dedupe on
/// `(tenant_id, event_type, request_id)` for `turn_completed`, `turn_failed`
/// and `turn_delete`, and on `(tenant_id, event_type, new_request_id)` for
/// `turn_retry` and `turn_edit`; a redelivery is byte-identical (the same
/// stored outbox payload).
/// `Transient` and `PluginTimeout` (30 s deadline) make the outbox retry the
/// event; `Permanent` dead-letters it. Cancellation is handled by the
/// framework via future dropping - no explicit `CancellationToken` needed.
#[async_trait]
pub trait MiniChatAuditPluginClientV1: Send + Sync {
    /// Emit a turn audit event (turn completed or failed).
    async fn emit_turn_audit(&self, event: TurnAuditEvent) -> Result<(), MiniChatAuditPluginError>;

    /// Emit a turn-retry audit event.
    async fn emit_turn_retry_audit(
        &self,
        event: TurnRetryAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError>;

    /// Emit a turn-edit audit event.
    async fn emit_turn_edit_audit(
        &self,
        event: TurnEditAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError>;

    /// Emit a turn-delete audit event.
    async fn emit_turn_delete_audit(
        &self,
        event: TurnDeleteAuditEvent,
    ) -> Result<(), MiniChatAuditPluginError>;
}
