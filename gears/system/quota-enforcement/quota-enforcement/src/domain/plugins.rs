//! Plugin binding: select the storage plugin by vendor, discover every
//! notification sink, and resolve their scoped `ClientHub` clients.
//!
//! Storage selection goes through the types registry (`GtsPluginSelector`
//! semantics: same vendor, lowest priority wins). Sinks are not selected:
//! every registered sink of every vendor receives every event. Nothing is
//! cached here; bootstrap resolves once and keeps the handles.
//!
//! Singleton coordination is not a plugin of this gear. The platform `cluster`
//! gear provides it, and `infra::cluster_coordination` resolves it (ADR-0006).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::{
    QuotaEnforcementStoragePluginSpecV1, QuotaEnforcementStoragePluginV1,
    QuotaNotificationSinkSpecV1, QuotaNotificationSinkV1,
};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit::plugins::choose_plugin_instance;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use types_registry_sdk::{GtsInstance, InstanceQuery, TypesRegistryClient};

use super::error::{DomainError, PluginKind};

/// Default budget for the registry listing that selects a plugin instance.
pub const DEFAULT_SELECTION_DEADLINE: Duration = Duration::from_secs(10);

/// Vendor-driven plugin resolution.
#[domain_model]
pub struct PluginBinding {
    hub: Arc<ClientHub>,
    storage_vendor: String,
    deadline: Duration,
}

impl PluginBinding {
    /// Bind to the hub with the configured storage vendor.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, storage_vendor: String) -> Self {
        Self {
            hub,
            storage_vendor,
            deadline: DEFAULT_SELECTION_DEADLINE,
        }
    }

    /// Override the budget the selecting registry listing may take.
    #[must_use]
    pub const fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Resolve the active storage plugin.
    ///
    /// # Errors
    ///
    /// - [`DomainError::TypesRegistryUnavailable`] when the registry cannot answer.
    /// - [`DomainError::PluginNotFound`] when no instance matches the vendor.
    /// - [`DomainError::InvalidPluginInstance`] when an instance is malformed.
    /// - [`DomainError::PluginClientNotRegistered`] when the instance exists
    ///   but its scoped client does not.
    pub async fn resolve_storage(
        &self,
    ) -> Result<Arc<dyn QuotaEnforcementStoragePluginV1>, DomainError> {
        let kind = PluginKind::Storage;
        let gts_id = self
            .select::<QuotaEnforcementStoragePluginSpecV1>(kind, &self.storage_vendor)
            .await?;
        self.hub
            .try_get_scoped::<dyn QuotaEnforcementStoragePluginV1>(&ClientScope::gts_id(&gts_id))
            .ok_or(DomainError::PluginClientNotRegistered { kind, gts_id })
    }

    /// Resolve every registered notification sink, of every vendor, ordered
    /// by instance id. None registered is a valid, empty set.
    ///
    /// # Errors
    ///
    /// - [`DomainError::TypesRegistryUnavailable`] when the registry cannot answer.
    /// - [`DomainError::InvalidPluginInstance`] when an instance is malformed,
    ///   or its sink answers an `id()` another sink already has.
    /// - [`DomainError::PluginClientNotRegistered`] when an instance exists
    ///   but its scoped client does not.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-sink-delivery:p1
    pub async fn resolve_sinks(
        &self,
    ) -> Result<Vec<Arc<dyn QuotaNotificationSinkV1>>, DomainError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-register
        let kind = PluginKind::NotificationSink;
        let mut instances = self.list::<QuotaNotificationSinkSpecV1>().await?;
        instances.sort_by(|a, b| a.id.as_ref().cmp(b.id.as_ref()));
        let mut sinks: Vec<(String, Arc<dyn QuotaNotificationSinkV1>)> = Vec::new();
        for instance in &instances {
            let gts_id = instance.id.as_ref();
            let invalid = |reason: String| DomainError::InvalidPluginInstance {
                kind,
                gts_id: gts_id.to_owned(),
                reason,
            };
            let content: PluginV1<QuotaNotificationSinkSpecV1> =
                serde_json::from_value(instance.object.clone())
                    .map_err(|e| invalid(e.to_string()))?;
            if content.id != gts_id {
                return Err(invalid(format!("content.id is {:?}", content.id)));
            }
            let sink = self
                .hub
                .try_get_scoped::<dyn QuotaNotificationSinkV1>(&ClientScope::gts_id(gts_id))
                .ok_or_else(|| DomainError::PluginClientNotRegistered {
                    kind,
                    gts_id: gts_id.to_owned(),
                })?;
            // Deduplicating would drop a sink, and every registered sink
            // receives every event: a taken id is a deployment error.
            if let Some((owner, _)) = sinks.iter().find(|(_, other)| other.id() == sink.id()) {
                return Err(invalid(format!(
                    "sink id {:?} is already taken by {owner}",
                    sink.id()
                )));
            }
            tracing::info!(
                target: "qe.bootstrap",
                plugin = %kind,
                vendor = content.vendor,
                instance_id = gts_id,
                sink_id = sink.id(),
                "resolved notification sink"
            );
            sinks.push((gts_id.to_owned(), sink));
        }
        Ok(sinks.into_iter().map(|(_, sink)| sink).collect())
        // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-register
    }

    async fn select<P>(&self, kind: PluginKind, vendor: &str) -> Result<String, DomainError>
    where
        P: for<'de> gts::GtsDeserialize<'de> + gts::GtsSchema,
    {
        let instances = self.list::<P>().await?;
        let candidates = instances.iter().map(|e| (e.id.as_ref(), &e.object));
        let gts_id = choose_plugin_instance::<P>(vendor, candidates)
            .map_err(|e| DomainError::plugin_selection(kind, e))?;
        tracing::info!(
            target: "qe.bootstrap",
            plugin = %kind,
            vendor,
            instance_id = %gts_id,
            "selected plugin instance"
        );
        Ok(gts_id)
    }

    /// Every registered instance of the plugin spec `P`.
    async fn list<P: gts::GtsSchema>(&self) -> Result<Vec<GtsInstance>, DomainError> {
        let registry = self
            .hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| DomainError::TypesRegistryUnavailable(e.to_string()))?;
        let type_id = <P as gts::GtsSchema>::TYPE_ID;
        // A registry that never answers must fail bootstrap, not hang it.
        let mut instances = bounded(
            self.deadline,
            registry.list_instances(InstanceQuery::new().with_pattern(format!("{type_id}*"))),
        )
        .await?;
        // A registry answers the pattern query with instances of this spec. The
        // prefix filter keeps the result correct against a registry that
        // ignores the pattern, so a foreign instance never fails deserialization.
        instances.retain(|e| e.id.as_ref().starts_with(type_id));
        Ok(instances)
    }
}

/// Runs one registry call under `deadline`; a failure and an overrun are both
/// an unavailable registry.
async fn bounded<T>(
    deadline: Duration,
    call: impl Future<Output = Result<T, CanonicalError>>,
) -> Result<T, DomainError> {
    match tokio::time::timeout(deadline, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(DomainError::TypesRegistryUnavailable(err.to_string())),
        Err(_elapsed) => Err(DomainError::TypesRegistryUnavailable(format!(
            "no answer within {deadline:?}"
        ))),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "plugins_tests.rs"]
mod plugins_tests;
