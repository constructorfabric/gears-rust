//! GTS plugin selector for the signing port.
//!
//! [`GtsSigningPluginSelector`] resolves the active signing-plugin instance via
//! the types-registry (mirroring the credstore selector) and *itself implements*
//! [`SigningClientV1`], lazily delegating each call to the scoped plugin client
//! resolved from the [`ClientHub`]. This lets it be injected directly as the
//! `Service`'s signer.

use std::sync::Arc;

use async_trait::async_trait;
use token_issuer_sdk::{
    PublicKeyVersion, SignatureResult, SigningClientV1, SigningError, SigningKeyRef,
    SigningPluginSpecV1,
};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use toolkit_security::SecurityContext;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};

/// Resolves the active signing-plugin instance via the GTS types-registry and
/// delegates [`SigningClientV1`] calls to the scoped plugin client.
pub struct GtsSigningPluginSelector {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
}

impl GtsSigningPluginSelector {
    /// Creates a new selector with lazy plugin resolution.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self {
            hub,
            vendor,
            selector: GtsPluginSelector::new(),
        }
    }

    /// Lists the registered signing-plugin instances and picks the one matching
    /// the configured vendor (lowest priority wins). The selected instance id is
    /// cached after the first successful resolution.
    async fn resolve_instance(&self) -> Result<String, SigningError> {
        let registry = self.hub.get::<dyn TypesRegistryClient>().map_err(|e| {
            SigningError::ServiceUnavailable {
                detail: format!("types-registry unavailable: {e}"),
                retry_after: None,
            }
        })?;

        let type_id = SigningPluginSpecV1::gts_type_id();
        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{type_id}*")))
            .await
            .map_err(|e| SigningError::ServiceUnavailable {
                detail: format!("types-registry list_instances failed: {e}"),
                retry_after: None,
            })?;

        choose_plugin_instance::<SigningPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.id.as_ref(), &e.object)),
        )
        .map_err(|e| match e {
            toolkit::plugins::ChoosePluginError::PluginNotFound { .. } => {
                SigningError::NoPluginAvailable
            }
            toolkit::plugins::ChoosePluginError::InvalidPluginInstance { gts_id, reason } => {
                SigningError::Internal(format!(
                    "invalid signing plugin instance '{gts_id}': {reason}"
                ))
            }
        })
    }

    /// Resolves the scoped [`SigningClientV1`] for the active plugin instance.
    async fn resolve(&self) -> Result<Arc<dyn SigningClientV1>, SigningError> {
        let instance_id = self
            .selector
            .get_or_init(|| self.resolve_instance())
            .await?;

        let scope = ClientScope::gts_id(instance_id.as_ref());

        if let Some(client) = self.hub.try_get_scoped::<dyn SigningClientV1>(&scope) {
            return Ok(client);
        }
        // The cached instance id may be stale (plugin re-registered under a new
        // id). Drop it so the next call re-resolves.
        self.selector.reset().await;
        Err(SigningError::ServiceUnavailable {
            detail: format!("signing plugin client not registered yet for '{instance_id}'"),
            retry_after: None,
        })
    }
}

#[async_trait]
impl SigningClientV1 for GtsSigningPluginSelector {
    async fn sign(
        &self,
        ctx: &SecurityContext,
        key: &SigningKeyRef,
        signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> {
        self.resolve().await?.sign(ctx, key, signing_input).await
    }

    async fn public_keys(
        &self,
        ctx: &SecurityContext,
        key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> {
        self.resolve().await?.public_keys(ctx, key).await
    }
}

#[cfg(test)]
#[path = "plugin_select_tests.rs"]
mod tests;
