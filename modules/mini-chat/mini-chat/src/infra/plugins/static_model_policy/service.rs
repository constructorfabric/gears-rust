use async_trait::async_trait;
use mini_chat_sdk::{
    KillSwitches, MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginError,
    ModelCatalogEntry, PolicySnapshot, PolicyVersionInfo, PublishError, TierLimits, UsageEvent,
    UserLicenseStatus, UserLimits,
};
use modkit_macros::domain_model;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use uuid::Uuid;

const SUPPORTED_POLICY_VERSION: u64 = 1;

/// Service holding the model catalog loaded from configuration.
#[domain_model]
pub struct Service {
    pub catalog: Vec<ModelCatalogEntry>,
    pub kill_switches: KillSwitches,
    pub default_standard_limits: TierLimits,
    pub default_premium_limits: TierLimits,
}

impl Service {
    /// Create a service with the given configuration.
    #[must_use]
    pub fn new(
        catalog: Vec<ModelCatalogEntry>,
        kill_switches: KillSwitches,
        default_standard_limits: TierLimits,
        default_premium_limits: TierLimits,
    ) -> Self {
        Self {
            catalog,
            kill_switches,
            default_standard_limits,
            default_premium_limits,
        }
    }
}

#[async_trait]
impl MiniChatModelPolicyPluginClientV1 for Service {
    async fn get_current_policy_version(
        &self,
        user_id: Uuid,
        _cancel: CancellationToken,
    ) -> Result<PolicyVersionInfo, MiniChatModelPolicyPluginError> {
        Ok(PolicyVersionInfo {
            user_id,
            policy_version: SUPPORTED_POLICY_VERSION,
            generated_at: OffsetDateTime::now_utc(),
        })
    }

    async fn get_policy_snapshot(
        &self,
        user_id: Uuid,
        policy_version: u64,
        _cancel: CancellationToken,
    ) -> Result<PolicySnapshot, MiniChatModelPolicyPluginError> {
        if policy_version != SUPPORTED_POLICY_VERSION {
            return Err(MiniChatModelPolicyPluginError::NotFound);
        }
        Ok(PolicySnapshot {
            user_id,
            policy_version,
            model_catalog: self.catalog.clone(),
            kill_switches: self.kill_switches.clone(),
        })
    }

    async fn get_user_limits(
        &self,
        user_id: Uuid,
        policy_version: u64,
        _cancel: CancellationToken,
    ) -> Result<UserLimits, MiniChatModelPolicyPluginError> {
        if policy_version != SUPPORTED_POLICY_VERSION {
            return Err(MiniChatModelPolicyPluginError::NotFound);
        }

        Ok(UserLimits {
            user_id,
            policy_version,
            standard: self.default_standard_limits.clone(),
            premium: self.default_premium_limits.clone(),
        })
    }

    async fn check_user_license(
        &self,
        _user_id: Uuid,
        _cancel: CancellationToken,
    ) -> Result<UserLicenseStatus, MiniChatModelPolicyPluginError> {
        // Static plugin assumes all users are licensed.
        Ok(UserLicenseStatus { active: true })
    }

    async fn publish_usage(
        &self,
        payload: UsageEvent,
        _cancel: CancellationToken,
    ) -> Result<(), PublishError> {
        debug!(
            turn_id = %payload.turn_id,
            tenant_id = %payload.tenant_id,
            billing_outcome = %payload.billing_outcome,
            "static plugin: publish_usage no-op"
        );
        Ok(())
    }
}
