# Mini Chat SDK

Plugin SDK for the mini-chat gear: plugin traits, GTS specs, domain and audit models, and error types.

## Overview

The `cf-gears-mini-chat-sdk` crate provides:

- **Plugin traits** (`plugin_api.rs`)
  - `MiniChatModelPolicyPluginClientV1` — policy data (model catalog, kill switches, user limits) and usage publishing (`publish_usage`). `check_user_license` has a default body that returns `active: false`; the gear does not call it.
  - `MiniChatAuditPluginClientV1` — receives turn audit events (`emit_turn_audit`, `emit_turn_retry_audit`, `emit_turn_edit_audit`, `emit_turn_delete_audit`).
- **GTS specs and resource types** (`gts.rs`) — `MiniChatModelPolicyPluginSpecV1`, `MiniChatAuditPluginSpecV1`; `CHAT_RESOURCE_TYPE`, `MODEL_RESOURCE_TYPE`, `USER_QUOTA_RESOURCE_TYPE`
- **Policy and usage models** (`models.rs`) — `PolicyVersionInfo`, `PolicySnapshot`, `ModelCatalogEntry`, `EstimationBudgets`, `ModelGeneralConfig`, `ModelApiParams`, `ModelToolSupport`, `ModelPreference`, `ModelTier`, `KillSwitches`, `UserLimits`, `TierLimits`, `UserLicenseStatus`, `UsageEvent`, `UsageTokens`
- **Audit models** (`audit_models.rs`) — `TurnAuditEvent`, `TurnMutationAuditEvent` (aliases `TurnRetryAuditEvent`, `TurnEditAuditEvent`), `TurnDeleteAuditEvent`, their event-type enums, and `PolicyDecisions`, `QuotaDecision`, `LicenseDecision`, `AuditUsageTokens`, `LatencyMs`, `ToolCalls`, `AttachmentMetadata`, `AttachmentKind`, `QuotaScope`, `RequesterType`
- **Error types** (`error.rs`) — `MiniChatModelPolicyPluginError`, `MiniChatAuditPluginError`, `PublishError`

A plugin is a gear that depends on this crate. In `init()` it registers a GTS instance of its spec in types-registry and its client in ClientHub under that instance id (see `mini-chat/src/infra/plugins/static_audit/gear.rs`); mini-chat selects the instance by the configured `vendor`. The mini-chat crate ships static implementations of both plugin specs (`MiniChatModelPolicyPluginSpecV1`, `MiniChatAuditPluginSpecV1`) in `mini-chat/src/infra/plugins/`.

## Usage

Implement the plugin trait:

```rust
use async_trait::async_trait;
use mini_chat_sdk::{
    MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginError,
    PolicySnapshot, PolicyVersionInfo, PublishError, UsageEvent, UserLimits,
};
use uuid::Uuid;

#[async_trait]
impl MiniChatModelPolicyPluginClientV1 for MyPolicyPlugin {
    async fn get_current_policy_version(
        &self,
        user_id: Uuid,
    ) -> Result<PolicyVersionInfo, MiniChatModelPolicyPluginError> {
        // ...
    }

    async fn get_policy_snapshot(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<PolicySnapshot, MiniChatModelPolicyPluginError> {
        // ...
    }

    async fn get_user_limits(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<UserLimits, MiniChatModelPolicyPluginError> {
        // ...
    }

    // Required: no default body. Called by the usage outbox handler after
    // the finalization transaction commits.
    async fn publish_usage(&self, payload: UsageEvent) -> Result<(), PublishError> {
        // ...
    }
}
```

## License

Apache-2.0
