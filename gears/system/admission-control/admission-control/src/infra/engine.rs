//! Resolution of the configured admission engine.
//!
//! The registered instances of [`AdmissionEnginePluginSpecV1`] are listed, the
//! pinned instance is taken when `instance_id` is set (it must be registered
//! and belong to `vendor`), else the vendor's lowest-priority instance is
//! chosen (ties broken by the smallest identifier), and its scoped client is
//! taken from `ClientHub`.
//!
//! [`LazyEngine`] does this on first use and caches the result (one caller
//! resolves at a time; a failure is not cached, so the next call retries), so
//! a call that arrives before the serve phase resolves the engine itself
//! instead of finding none. The serve phase still resolves it once eagerly, so
//! a configured engine that cannot be resolved fails startup.

use std::sync::Arc;

use admission_control_sdk::{AdmissionEnginePluginClientV1, AdmissionEnginePluginSpecV1};
use async_trait::async_trait;
use tokio::sync::OnceCell;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{ChoosePluginError, choose_plugin_instance};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};

use crate::config::EngineConfig;
use crate::domain::service::{EngineHandle, EngineSource, EngineUnavailable};

/// Why a configured engine could not be resolved.
#[derive(Debug, thiserror::Error)]
pub enum EngineResolveError {
    /// The types registry could not list engine plugin instances.
    #[error("types-registry could not list admission engine plugin instances")]
    Registry(#[source] CanonicalError),
    /// The pinned instance identifier is not registered.
    #[error("configured engine instance `{0}` is not registered")]
    PinnedInstanceNotFound(String),
    /// No registered instance matches the selection.
    #[error("no usable admission engine plugin instance for the configured selection")]
    Selection(#[source] ChoosePluginError),
    /// The selected instance has no scoped client registered in `ClientHub`.
    #[error("admission engine plugin `{0}` has no client registered in ClientHub")]
    ClientNotRegistered(String),
}

/// Resolves the configured engine.
///
/// # Errors
///
/// [`EngineResolveError`] when the configured engine cannot be resolved.
pub async fn resolve_engine(
    hub: &ClientHub,
    registry: &dyn TypesRegistryClient,
    selection: &EngineConfig,
) -> Result<EngineHandle, EngineResolveError> {
    let type_id = <AdmissionEnginePluginSpecV1 as gts::GtsSchema>::TYPE_ID;
    let mut instances = registry
        .list_instances(InstanceQuery::new().with_pattern(format!("{type_id}*")))
        .await
        .map_err(EngineResolveError::Registry)?;
    instances.retain(|instance| AsRef::<str>::as_ref(&instance.id).starts_with(type_id));
    instances.sort_by(|a, b| AsRef::<str>::as_ref(&a.id).cmp(AsRef::<str>::as_ref(&b.id)));

    if let Some(pinned) = &selection.instance_id {
        instances.retain(|instance| AsRef::<str>::as_ref(&instance.id) == pinned);
        if instances.is_empty() {
            return Err(EngineResolveError::PinnedInstanceNotFound(pinned.clone()));
        }
    }
    let chosen = choose_plugin_instance::<AdmissionEnginePluginSpecV1>(
        selection.vendor.trim(),
        instances
            .iter()
            .map(|instance| (instance.id.as_ref(), &instance.object)),
    )
    .map_err(EngineResolveError::Selection)?;

    let plugin: Arc<dyn AdmissionEnginePluginClientV1> = hub
        .try_get_scoped::<dyn AdmissionEnginePluginClientV1>(&ClientScope::gts_id(&chosen))
        .ok_or_else(|| EngineResolveError::ClientNotRegistered(chosen.clone()))?;
    tracing::info!(engine_id = %chosen, "admission engine resolved");
    Ok(EngineHandle { id: chosen, plugin })
}

/// The configured engine, resolved on first use. See the module
/// documentation.
pub struct LazyEngine {
    hub: Arc<ClientHub>,
    registry: Arc<dyn TypesRegistryClient>,
    selection: EngineConfig,
    engine: OnceCell<EngineHandle>,
}

impl std::fmt::Debug for LazyEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyEngine")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

impl LazyEngine {
    /// An engine resolved through `registry` and `hub` on first use.
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        registry: Arc<dyn TypesRegistryClient>,
        selection: EngineConfig,
    ) -> Self {
        Self {
            hub,
            registry,
            selection,
            engine: OnceCell::new(),
        }
    }

    /// The engine, resolved once and cached.
    ///
    /// # Errors
    ///
    /// [`EngineResolveError`] when the engine cannot be resolved now.
    pub async fn resolve(&self) -> Result<EngineHandle, EngineResolveError> {
        self.engine
            .get_or_try_init(|| resolve_engine(&self.hub, self.registry.as_ref(), &self.selection))
            .await
            .cloned()
    }
}

#[async_trait]
impl EngineSource for LazyEngine {
    async fn engine(&self) -> Result<EngineHandle, EngineUnavailable> {
        self.resolve().await.map_err(|error| {
            tracing::warn!(error = %error, "admission engine cannot be resolved yet");
            EngineUnavailable
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "engine_tests.rs"]
mod engine_tests;
