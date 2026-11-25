// Updated: 2026-10-06 by Constructor Tech
//! Types-registry-backed selection of the active value-store plugin.
//!
//! Resolves the configured vendor lazily, over the database-backed catalogue
//! of `PlatformTypesRegistryApi`, and retrieves its scoped
//! [`CredStorePluginClientV2`] from `ClientHub`. The first resolution happens
//! on the first request, after every local plugin has bound its admitted
//! instance in `init`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::{CredStorePluginClientV2, CredStorePluginSpecV2};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{
    ChoosePluginError, GtsPluginSelector, choose_registered_plugin_instance, is_discovery_outage,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt,
};

use crate::domain::error::DomainError;
use crate::domain::ports::plugin::PluginSelector;

/// Bounds a cold catalogue listing and concurrent selection waits, including
/// page retries. Expiry is a retryable outage and is not cached.
const DISCOVERY_BUDGET: Duration = Duration::from_secs(30);

/// Resolves the active `CredStore` backend plugin via the GTS types-registry.
///
/// The selection is process-wide, so it is read on the platform plane: no tenant
/// credential is needed to find the plugin that serves a request.
pub struct GtsCredStorePluginSelector {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
    discovery_budget: Duration,
}

impl GtsCredStorePluginSelector {
    /// Creates a new selector with lazy plugin resolution.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self {
            hub,
            vendor,
            selector: GtsPluginSelector::new(),
            discovery_budget: DISCOVERY_BUDGET,
        }
    }

    /// Overrides the discovery budget so a test can expire it without waiting.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_discovery_budget(mut self, budget: Duration) -> Self {
        self.discovery_budget = budget;
        self
    }

    /// An availability failure of the registry is a retryable outage; anything
    /// else it answers (a credential refusal, a malformed page) is internal.
    /// Wire-visible details stay curated (`with_detail` contract); raw
    /// dependency errors live only in the cause chain / logs.
    fn registry_failure(error: CanonicalError) -> DomainError {
        tracing::warn!(err = %error, "types-registry list_instances failed");
        if is_discovery_outage(&error) {
            DomainError::ServiceUnavailable {
                detail: "types registry unavailable".to_owned(),
                retry_after: None,
                cause: Some(Box::new(error)),
            }
        } else {
            DomainError::Internal {
                diagnostic: "types registry answered a plugin listing with a failure".to_owned(),
                cause: Some(Box::new(error)),
            }
        }
    }

    /// Lists every registered instance of the plugin spec and keeps only those
    /// with a locally bound scoped client, so instances a previous boot left in
    /// the durable catalogue never win. Failures are returned without caching.
    async fn resolve_instance(&self) -> Result<String, DomainError> {
        let registry = self
            .hub
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| {
                tracing::warn!(err = %e, "types-registry client unavailable");
                DomainError::ServiceUnavailable {
                    detail: "types registry unavailable".to_owned(),
                    retry_after: None,
                    cause: Some(Box::new(e)),
                }
            })?;

        let type_id = CredStorePluginSpecV2::gts_type_id();
        let pattern =
            GtsIdPattern::try_new(type_id.as_ref()).map_err(|e| DomainError::Internal {
                diagnostic: format!("invalid credstore plugin type pattern: {e}"),
                cause: None,
            })?;
        let instances = registry
            .list_instances(
                &PlatformSecurityContext::outbound_marker(),
                ListEntitiesRequest::matching(pattern),
            )
            .await
            .map_err(Self::registry_failure)?;

        let gts_id = choose_registered_plugin_instance::<
            CredStorePluginSpecV2,
            dyn CredStorePluginClientV2,
        >(
            &self.hub,
            &self.vendor,
            instances
                .iter()
                .map(|instance| (instance.id.as_ref(), instance.content.as_ref())),
        )
        .map_err(|e| match e {
            ChoosePluginError::PluginNotFound { vendor, .. } => DomainError::ServiceUnavailable {
                detail: format!("no credstore plugin found for vendor '{vendor}'"),
                retry_after: None,
                cause: None,
            },
            ChoosePluginError::InvalidPluginInstance { gts_id, reason } => DomainError::Internal {
                diagnostic: format!("invalid credstore plugin instance '{gts_id}': {reason}"),
                cause: None,
            },
        })?;

        Ok(gts_id)
    }
}

#[async_trait]
impl PluginSelector for GtsCredStorePluginSelector {
    async fn resolve(&self) -> Result<Arc<dyn CredStorePluginClientV2>, DomainError> {
        let instance_id = self
            .selector
            .get_or_init_with_timeout(
                self.discovery_budget,
                || self.resolve_instance(),
                |detail| {
                    tracing::warn!(%detail, "credstore plugin discovery timed out");
                    DomainError::ServiceUnavailable {
                        detail: "types registry unavailable".to_owned(),
                        retry_after: None,
                        cause: None,
                    }
                },
            )
            .await?;

        let scope = ClientScope::gts_id(instance_id.as_ref());

        self.hub
            .try_get_scoped::<dyn CredStorePluginClientV2>(&scope)
            .ok_or_else(|| DomainError::ServiceUnavailable {
                detail: format!("credstore plugin client not registered yet for '{instance_id}'"),
                retry_after: None,
                cause: None,
            })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "plugin_select_tests.rs"]
mod plugin_select_tests;
