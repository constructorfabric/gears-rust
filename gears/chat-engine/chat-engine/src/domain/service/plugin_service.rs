//! Plugin resolution service.
//!
//! `PluginService` is the single integration point between Chat Engine's
//! domain layer and the SDK trait `ChatEngineBackendPlugin`. Discovery is
//! delegated to ClientHub (`try_get_scoped` keyed by GTS instance ID); per-
//! `(plugin_instance_id, session_type_id)` JSONB config is loaded through
//! the `PluginConfigRepo` abstraction.
//!
//! This service deliberately holds **no** HTTP, retry, or transport state —
//! that is the plugin implementation's job. See `infra::webhook_compat` for
//! the canonical first-party HTTP-adapter plugin.
//!
//! ## Health probe routing
//!
//! `health_probe` calls `ChatEngineBackendPlugin::health_check()` on the
//! resolved plugin and turns the outcome into a `HealthStatus` value. The
//! mapping is deliberately non-blocking — only `Healthy` is logged silently;
//! every other outcome surfaces a `WARN` log but the session-type
//! configuration flow is allowed to proceed:
//!
//! | Outcome                 | Log level | Returned `HealthStatus` |
//! |-------------------------|-----------|-------------------------|
//! | `Ok(Healthy)`           | none      | `Healthy`               |
//! | `Ok(Degraded)`          | `WARN`    | `Degraded`              |
//! | `Ok(Unhealthy)`         | `WARN`    | `Unhealthy`             |
//! | `Err(PluginError)`      | `WARN`    | `Unhealthy`             |
//!
//! The `Err(PluginError)` branch is **never** propagated to the caller as an
//! `Err(ChatEngineError)` — the operator-policy decision is "health is
//! advisory; do not block configuration on it".
//
// @cpt-cf-chat-engine-plugin-service:p3

use std::sync::Arc;

use serde_json::Value as JsonValue;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit_macros::domain_model;
use tracing::warn;
use uuid::Uuid;

use chat_engine_sdk::plugin::ChatEngineBackendPlugin;

use crate::domain::HealthStatus;
use crate::domain::error::ChatEngineError;
use crate::domain::ports::PluginConfigRepo;

/// Resolution + health surface for `ChatEngineBackendPlugin` impls.
///
/// Construct once at module init (Phase 15) with the process-wide `ClientHub`
/// and a `SeaPluginConfigRepo` over the shared `DatabaseConnection`. Clone
/// freely — both fields are `Arc` and the struct itself is cheap to copy.
#[domain_model]
#[derive(Clone)]
pub struct PluginService {
    client_hub: Arc<ClientHub>,
    configs: Arc<dyn PluginConfigRepo>,
}

impl PluginService {
    /// Build a new service over a shared `ClientHub` and a `PluginConfigRepo`.
    #[must_use]
    pub fn new(client_hub: Arc<ClientHub>, configs: Arc<dyn PluginConfigRepo>) -> Self {
        Self {
            client_hub,
            configs,
        }
    }

    /// Resolve a registered plugin by its GTS instance ID.
    ///
    /// Returns `ChatEngineError::NotFound { resource: "plugin", id: ... }`
    /// when no plugin is registered under the given scope. The lookup is
    /// O(1) (HashMap on `(TypeId, ClientScope)`).
    ///
    /// # Errors
    ///
    /// - `NotFound` when the plugin instance ID is not registered.
    pub fn resolve(
        &self,
        plugin_instance_id: &str,
    ) -> Result<Arc<dyn ChatEngineBackendPlugin>, ChatEngineError> {
        let scope = ClientScope::gts_id(plugin_instance_id);
        self.client_hub
            .try_get_scoped::<dyn ChatEngineBackendPlugin>(&scope)
            .ok_or_else(|| ChatEngineError::not_found("plugin", plugin_instance_id))
    }

    /// Load the `(plugin_instance_id, session_type_id)` JSONB config. Returns
    /// `Ok(None)` when no row exists (the call still proceeds; plugins must
    /// gracefully handle absent config or surface `PluginError::invalid_input`
    /// on their own terms).
    ///
    /// # Errors
    ///
    /// - Underlying repository / database errors propagate via
    ///   `From<sea_orm::DbErr> for ChatEngineError`.
    pub async fn load_config(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
    ) -> Result<Option<JsonValue>, ChatEngineError> {
        self.configs.find(plugin_instance_id, session_type_id).await
    }

    /// Persist (insert or update) the `(plugin_instance_id, session_type_id)`
    /// JSONB config so a later [`Self::load_config`] — e.g. from
    /// `create_session` — observes it.
    ///
    /// # Errors
    ///
    /// - Underlying repository / database errors propagate via
    ///   `From<sea_orm::DbErr> for ChatEngineError`.
    pub async fn save_config(
        &self,
        plugin_instance_id: &str,
        session_type_id: Uuid,
        config: JsonValue,
    ) -> Result<(), ChatEngineError> {
        self.configs
            .upsert(plugin_instance_id, session_type_id, config)
            .await
    }

    /// Probe the plugin's `health_check()` and apply the routing matrix
    /// documented at module level. Always returns `Ok(_)` once the plugin is
    /// resolved — the only error path is `resolve` failing with `NotFound`.
    ///
    /// # Errors
    ///
    /// - `NotFound` when the plugin is not registered (propagated from
    ///   [`PluginService::resolve`]).
    pub async fn health_probe(
        &self,
        plugin_instance_id: &str,
    ) -> Result<HealthStatus, ChatEngineError> {
        let plugin = self.resolve(plugin_instance_id)?;

        match plugin.health_check().await {
            Ok(HealthStatus::Healthy) => Ok(HealthStatus::Healthy),
            Ok(HealthStatus::Degraded) => {
                warn!(
                    plugin_instance_id = %plugin_instance_id,
                    status = "degraded",
                    "plugin health check reported degraded; routing remains enabled"
                );
                Ok(HealthStatus::Degraded)
            }
            Ok(HealthStatus::Unhealthy) => {
                warn!(
                    plugin_instance_id = %plugin_instance_id,
                    status = "unhealthy",
                    "plugin health check reported unhealthy; routing remains enabled (advisory)"
                );
                Ok(HealthStatus::Unhealthy)
            }
            Err(err) => {
                // Per Phase 3 rules: do NOT propagate the PluginError —
                // treat the health probe as advisory and fold to Unhealthy.
                // Log the *kind* of failure (via its suggested status, which
                // is a 1:1 proxy for the variant), never the payload, which
                // may carry sensitive upstream detail.
                warn!(
                    plugin_instance_id = %plugin_instance_id,
                    status = "unhealthy",
                    error_status = err.suggested_status(),
                    "plugin health check returned an error; treating as unhealthy (advisory)"
                );
                Ok(HealthStatus::Unhealthy)
            }
        }
    }
}

#[cfg(test)]
#[path = "plugin_service_tests.rs"]
mod plugin_service_tests;
