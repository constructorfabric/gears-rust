//! Domain service for the `AuthN` resolver.
//!
//! Plugin discovery is lazy: resolved on first API call after
//! types-registry is ready.

use std::sync::Arc;
use std::time::Duration;

use authn_resolver_sdk::{
    AuthNResolverPluginClient, AuthNResolverPluginSpecV1, AuthenticationResult,
    ClientCredentialsRequest,
};
use modkit::client_hub::{ClientHub, ClientScope};
use modkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use modkit::telemetry::ThrottledLog;
use modkit_macros::domain_model;
use tracing::info;
use types_registry_sdk::{ListQuery, TypesRegistryClient};

use super::error::DomainError;

/// Throttle interval for unavailable plugin warnings.
const UNAVAILABLE_LOG_THROTTLE: Duration = Duration::from_secs(10);

/// `AuthN` resolver service.
///
/// Discovers plugins via types-registry and delegates authentication calls.
#[domain_model]
pub struct Service {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
    unavailable_log_throttle: ThrottledLog,
}

impl Service {
    /// Creates a new service with lazy plugin resolution.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self {
            hub,
            vendor,
            selector: GtsPluginSelector::new(),
            unavailable_log_throttle: ThrottledLog::new(UNAVAILABLE_LOG_THROTTLE),
        }
    }

    /// Lazily resolves and returns the plugin client.
    async fn get_plugin(&self) -> Result<Arc<dyn AuthNResolverPluginClient>, DomainError> {
        let instance_id = self.selector.get_or_init(|| self.resolve_plugin()).await?;
        let scope = ClientScope::gts_id(instance_id.as_ref());

        if let Some(client) = self
            .hub
            .try_get_scoped::<dyn AuthNResolverPluginClient>(&scope)
        {
            Ok(client)
        } else {
            if self.unavailable_log_throttle.should_log() {
                tracing::warn!(
                    plugin_gts_id = %instance_id,
                    vendor = %self.vendor,
                    "Plugin client not registered yet"
                );
            }
            Err(DomainError::PluginUnavailable {
                gts_id: instance_id.to_string(),
                reason: "client not registered yet".into(),
            })
        }
    }

    /// Resolves the plugin instance from types-registry.
    #[tracing::instrument(skip_all, fields(vendor = %self.vendor))]
    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        info!("Resolving authn_resolver plugin");

        let registry = self
            .hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;

        let plugin_type_id = AuthNResolverPluginSpecV1::gts_schema_id().clone();

        let instances = registry
            .list(
                ListQuery::new()
                    .with_pattern(format!("{plugin_type_id}*"))
                    .with_is_type(false),
            )
            .await?;

        let gts_id = choose_plugin_instance::<AuthNResolverPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.gts_id.as_str(), &e.content)),
        )?;
        info!(plugin_gts_id = %gts_id, "Selected authn_resolver plugin instance");

        Ok(gts_id)
    }

    /// Authenticate a bearer token via the selected plugin.
    ///
    /// # Errors
    ///
    /// - `Unauthorized` if the token is invalid
    /// - Plugin resolution errors
    #[tracing::instrument(skip_all)]
    pub async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticationResult, DomainError> {
        let plugin = self.get_plugin().await?;
        plugin
            .authenticate(bearer_token)
            .await
            .map_err(DomainError::from)
    }

    /// Exchange client credentials for a `SecurityContext` via the selected plugin.
    ///
    /// # Errors
    ///
    /// - `TokenAcquisitionFailed` if credentials are invalid
    /// - Plugin resolution errors
    #[tracing::instrument(skip_all, fields(client_id = %request.client_id))]
    pub async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, DomainError> {
        let plugin = self.get_plugin().await?;
        plugin
            .exchange_client_credentials(request)
            .await
            .map_err(DomainError::from)
    }
}
