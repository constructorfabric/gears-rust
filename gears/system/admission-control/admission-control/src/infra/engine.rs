//! Resolution of the configured policy engine, once, in the serve phase.
//!
//! The registered instances of [`AdmissionEnginePluginSpecV1`] are listed, the
//! pinned instance is taken when `instance_id` is set (it must be registered
//! and belong to `vendor`), else the vendor's lowest-priority instance is
//! chosen (ties broken by the smallest identifier), and its scoped client is
//! taken from `ClientHub`.

use std::sync::Arc;

use admission_control_sdk::{AdmissionEnginePluginClientV1, AdmissionEnginePluginSpecV1};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{ChoosePluginError, choose_plugin_instance};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};

use crate::config::EngineConfig;
use crate::domain::service::EngineHandle;

/// Why a configured engine could not be resolved; every variant fails startup.
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "engine_tests.rs"]
mod engine_tests;
