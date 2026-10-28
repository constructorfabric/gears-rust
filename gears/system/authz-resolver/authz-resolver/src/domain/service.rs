//! Domain service for the `AuthZ` resolver.
//!
//! Plugin discovery is lazy: resolved on the first API call, over the
//! database-backed catalogue of `PlatformTypesRegistryApi`.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::{
    AuthZResolverPluginClient, AuthZResolverPluginSpecV1, EvaluationRequest, EvaluationResponse,
};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_registered_plugin_instance};
use toolkit::telemetry::ThrottledLog;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::PlatformSecurityContext;
use tracing::info;
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

use super::error::DomainError;

/// Throttle interval for unavailable plugin warnings.
const UNAVAILABLE_LOG_THROTTLE: Duration = Duration::from_secs(10);

/// Bounds cold catalogue listing and concurrent selection waits, including REST
/// page retries. Expiry returns a retryable registry outage without caching.
const DISCOVERY_BUDGET: Duration = Duration::from_secs(30);

/// `AuthZ` resolver service.
///
/// The selection is process-wide, so it is read on the platform plane: no tenant
/// credential is needed to find the plugin that evaluates a request.
#[domain_model]
pub struct Service {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
    unavailable_log_throttle: ThrottledLog,
}

impl Service {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self {
            hub,
            vendor,
            selector: GtsPluginSelector::new(),
            unavailable_log_throttle: ThrottledLog::new(UNAVAILABLE_LOG_THROTTLE),
        }
    }

    async fn get_plugin(&self) -> Result<Arc<dyn AuthZResolverPluginClient>, DomainError> {
        let instance_id = bounded(self.selector.get_or_init(|| self.resolve_plugin())).await?;
        let scope = ClientScope::gts_id(instance_id.as_ref());

        if let Some(client) = self
            .hub
            .try_get_scoped::<dyn AuthZResolverPluginClient>(&scope)
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
    ///
    /// List every page and keep only locally registered scoped clients, excluding
    /// stale instances from earlier boots. Read failures are returned without caching.
    #[tracing::instrument(skip_all, fields(vendor = %self.vendor))]
    async fn resolve_plugin(&self) -> Result<String, DomainError> {
        info!("Resolving authz_resolver plugin");

        let registry = self
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;

        let plugin_type_id = AuthZResolverPluginSpecV1::gts_type_id();
        let pattern = GtsIdPattern::try_new(plugin_type_id.as_ref())
            .map_err(|e| DomainError::Internal(e.to_string()))?;
        let instances = registry
            .list_instances(
                &PlatformSecurityContext::outbound_marker(),
                ListEntitiesRequest::matching(pattern),
            )
            .await
            .map_err(registry_failure)?;

        let gts_id = choose_registered_plugin_instance::<
            AuthZResolverPluginSpecV1,
            dyn AuthZResolverPluginClient,
        >(
            &self.hub,
            &self.vendor,
            instances
                .iter()
                .map(|instance| (instance.id.as_ref(), instance.content.as_ref())),
        )?;
        info!(plugin_gts_id = %gts_id, "Selected authz_resolver plugin instance");

        Ok(gts_id)
    }

    /// Evaluate an authorization request via the selected plugin.
    ///
    /// # Errors
    ///
    /// - Plugin resolution errors
    /// - Plugin evaluation errors
    #[tracing::instrument(skip_all)]
    pub async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, DomainError> {
        let plugin = self.get_plugin().await?;
        plugin.evaluate(request).await.map_err(DomainError::from)
    }
}

/// `selection` within [`DISCOVERY_BUDGET`]; on expiry it is dropped, which caches nothing.
async fn bounded<F>(selection: F) -> Result<Arc<str>, DomainError>
where
    F: Future<Output = Result<Arc<str>, DomainError>>,
{
    tokio::time::timeout(DISCOVERY_BUDGET, selection)
        .await
        .unwrap_or_else(|_| {
            Err(DomainError::TypesRegistryUnavailable(format!(
                "plugin discovery did not finish within {DISCOVERY_BUDGET:?}"
            )))
        })
}

/// Registry availability failures map to retryable `TypesRegistryUnavailable` (503);
/// malformed pages, credential refusals and contract faults remain internal.
fn registry_failure(error: CanonicalError) -> DomainError {
    match error {
        CanonicalError::ServiceUnavailable { .. }
        | CanonicalError::DeadlineExceeded { .. }
        | CanonicalError::ResourceExhausted { .. }
        | CanonicalError::Aborted { .. } => {
            DomainError::TypesRegistryUnavailable(error.to_string())
        }
        other => DomainError::from(other),
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
