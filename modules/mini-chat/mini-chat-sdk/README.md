# Mini Chat SDK

Plugin SDK for the mini-chat module: policy plugin traits, domain models, and error types.

## Overview

The `cf-mini-chat-sdk` crate provides:

- **Plugin trait** — `MiniChatModelPolicyPluginClientV1` for policy data providers (model catalog, kill switches, user limits)
- **GTS spec** — `MiniChatModelPolicyPluginSpecV1` for types-registry discovery
- **Domain models** — `PolicySnapshot`, `ModelCatalogEntry`, `UserLimits`, `KillSwitches`, `ModelTier`
- **Error type** — `MiniChatModelPolicyPluginError`

Plugin implementations (e.g. `cf-static-model-policy-plugin`) depend on this crate and register via `inventory`.

## Usage

Implement the plugin trait:

```rust
use async_trait::async_trait;
use mini_chat_sdk::{
    MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginError,
    PolicySnapshot, PolicyVersionInfo, UserLimits,
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
}
```

## License

Apache-2.0
