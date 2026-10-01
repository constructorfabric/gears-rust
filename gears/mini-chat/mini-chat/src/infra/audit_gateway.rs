use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mini_chat_sdk::{MiniChatAuditPluginClientV1, MiniChatAuditPluginSpecV1};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{ChoosePluginError, GtsPluginSelector, choose_plugin_instance};
use tracing::warn;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};

/// Outcome of an instance lookup that must not be cached by the selector.
enum ResolveError {
    /// No audit plugin instance is registered (yet).
    NoPlugin,
    Failed(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ResolveError {
    fn from(e: E) -> Self {
        Self::Failed(e.into())
    }
}

/// Where the audit plugin instance id comes from.
enum InstanceLookup {
    /// GTS types-registry, filtered by vendor.
    Registry,
    /// Fixed answer for unit tests; `None` means "no plugin registered".
    #[cfg(test)]
    Fixed(Box<dyn Fn() -> Option<String> + Send + Sync>),
}

/// Resolves and dispatches to the registered audit plugin instance.
///
/// The instance id is resolved via GTS types-registry on first use and
/// cached once found. "No plugin registered" is not cached: every delivery
/// looks again, so a plugin registered later is picked up.
/// Used exclusively by `AuditEventHandler` in the outbox layer.
pub struct AuditGateway {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
    lookup: InstanceLookup,
    /// Set after the "no audit plugin" warning, cleared once a plugin is
    /// found, so the steady no-plugin state logs once.
    no_plugin_warned: AtomicBool,
}

impl AuditGateway {
    pub(crate) fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self::with_parts(
            hub,
            vendor,
            GtsPluginSelector::new(),
            InstanceLookup::Registry,
        )
    }

    fn with_parts(
        hub: Arc<ClientHub>,
        vendor: String,
        selector: GtsPluginSelector,
        lookup: InstanceLookup,
    ) -> Self {
        Self {
            hub,
            vendor,
            selector,
            lookup,
            no_plugin_warned: AtomicBool::new(false),
        }
    }

    /// Create a no-op gateway for tests: no plugin is ever registered, so
    /// `get_plugin()` returns `Ok(None)` without touching types-registry.
    #[cfg(test)]
    pub(crate) fn noop() -> Arc<Self> {
        Self::with_lookup(Arc::new(ClientHub::new()), || None)
    }

    /// Create a gateway whose instance lookup is `lookup` (`None` = no plugin
    /// registered) and whose clients come from `hub` — for unit tests.
    #[cfg(test)]
    pub(crate) fn with_lookup(
        hub: Arc<ClientHub>,
        lookup: impl Fn() -> Option<String> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self::with_parts(
            hub,
            String::new(),
            GtsPluginSelector::new(),
            InstanceLookup::Fixed(Box::new(lookup)),
        ))
    }

    /// Create a gateway pre-loaded with a concrete plugin instance — for unit tests.
    ///
    /// The supplied plugin is registered in a fresh `ClientHub` under a
    /// fixed synthetic instance ID.  The selector is pre-cached so
    /// `get_plugin()` returns the plugin immediately without any
    /// types-registry round-trip.
    #[cfg(test)]
    pub(crate) fn from_plugin(plugin: Arc<dyn MiniChatAuditPluginClientV1>) -> Arc<Self> {
        const MOCK_INSTANCE_ID: &str = "test.audit.plugin.v1~test._.mock.v1";
        let hub = Arc::new(ClientHub::new());
        hub.register_scoped::<dyn MiniChatAuditPluginClientV1>(
            ClientScope::gts_id(MOCK_INSTANCE_ID),
            plugin,
        );
        Self::new_preconfigured(
            hub,
            String::new(),
            GtsPluginSelector::pre_cached(MOCK_INSTANCE_ID.to_owned()),
        )
    }

    /// Create a gateway with explicit fields — for tests that pre-warm the
    /// selector and register the plugin directly in the hub.
    #[cfg(test)]
    pub(crate) fn new_preconfigured(
        hub: Arc<ClientHub>,
        vendor: String,
        selector: GtsPluginSelector,
    ) -> Arc<Self> {
        Arc::new(Self::with_parts(
            hub,
            vendor,
            selector,
            InstanceLookup::Registry,
        ))
    }

    /// Resolve the audit plugin client.
    ///
    /// - `Ok(Some(plugin))` — plugin resolved and ready.
    /// - `Ok(None)` — no audit plugin is registered; audit is optional, caller should skip.
    /// - `Err(e)` — transient failure, including an instance that is
    ///   registered in types-registry but whose client is not (yet) in
    ///   `ClientHub`; caller should retry.
    pub(crate) async fn get_plugin(
        &self,
    ) -> Result<Option<Arc<dyn MiniChatAuditPluginClientV1>>, anyhow::Error> {
        let instance_id = match self
            .selector
            .get_or_init(|| self.resolve_audit_plugin())
            .await
        {
            Ok(id) => id,
            Err(ResolveError::NoPlugin) => {
                if !self.no_plugin_warned.swap(true, Ordering::Relaxed) {
                    warn!(
                        vendor = %self.vendor,
                        "no mini-chat audit plugin registered; audit events will be dropped"
                    );
                }
                return Ok(None);
            }
            Err(ResolveError::Failed(e)) => return Err(e),
        };
        self.no_plugin_warned.store(false, Ordering::Relaxed);

        let scope = ClientScope::gts_id(instance_id.as_ref());
        if let Some(client) = self
            .hub
            .try_get_scoped::<dyn MiniChatAuditPluginClientV1>(&scope)
        {
            return Ok(Some(client));
        }

        // The instance may belong to a plugin that has not registered its
        // client yet, or may have been replaced: look it up again next time.
        self.selector.reset().await;
        Err(anyhow::anyhow!(
            "audit plugin instance '{instance_id}' has no client in ClientHub"
        ))
    }

    async fn resolve_audit_plugin(&self) -> Result<String, ResolveError> {
        match &self.lookup {
            InstanceLookup::Registry => {}
            #[cfg(test)]
            InstanceLookup::Fixed(lookup) => return lookup().ok_or(ResolveError::NoPlugin),
        }

        let registry = self.hub.get::<dyn TypesRegistryClient>()?;
        let plugin_type_id = MiniChatAuditPluginSpecV1::gts_type_id().clone();
        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{plugin_type_id}*")))
            .await?;

        match choose_plugin_instance::<MiniChatAuditPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.id.as_ref(), &e.object)),
        ) {
            Ok(gts_id) => Ok(gts_id),
            Err(ChoosePluginError::PluginNotFound { .. }) => Err(ResolveError::NoPlugin),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::sync::atomic::AtomicU32;

    use async_trait::async_trait;
    use mini_chat_sdk::{
        MiniChatAuditPluginError, TurnAuditEvent, TurnDeleteAuditEvent, TurnEditAuditEvent,
        TurnRetryAuditEvent,
    };

    use super::*;

    struct NoopPlugin;

    #[async_trait]
    impl MiniChatAuditPluginClientV1 for NoopPlugin {
        async fn emit_turn_audit(&self, _: TurnAuditEvent) -> Result<(), MiniChatAuditPluginError> {
            Ok(())
        }
        async fn emit_turn_retry_audit(
            &self,
            _: TurnRetryAuditEvent,
        ) -> Result<(), MiniChatAuditPluginError> {
            Ok(())
        }
        async fn emit_turn_edit_audit(
            &self,
            _: TurnEditAuditEvent,
        ) -> Result<(), MiniChatAuditPluginError> {
            Ok(())
        }
        async fn emit_turn_delete_audit(
            &self,
            _: TurnDeleteAuditEvent,
        ) -> Result<(), MiniChatAuditPluginError> {
            Ok(())
        }
    }

    const INSTANCE_ID: &str = "test.audit.plugin.v1~test._.late.v1";

    #[tokio::test]
    async fn no_plugin_is_not_cached_and_late_plugin_is_used() {
        let hub = Arc::new(ClientHub::new());
        let registered = Arc::new(AtomicBool::new(false));
        let lookups = Arc::new(AtomicU32::new(0));
        let gateway = {
            let registered = Arc::clone(&registered);
            let lookups = Arc::clone(&lookups);
            AuditGateway::with_lookup(Arc::clone(&hub), move || {
                lookups.fetch_add(1, Ordering::SeqCst);
                registered
                    .load(Ordering::SeqCst)
                    .then(|| INSTANCE_ID.to_owned())
            })
        };

        assert!(gateway.get_plugin().await.unwrap().is_none());
        assert!(gateway.get_plugin().await.unwrap().is_none());
        assert_eq!(
            lookups.load(Ordering::SeqCst),
            2,
            "none must be re-resolved"
        );

        hub.register_scoped::<dyn MiniChatAuditPluginClientV1>(
            ClientScope::gts_id(INSTANCE_ID),
            Arc::new(NoopPlugin),
        );
        registered.store(true, Ordering::SeqCst);

        assert!(gateway.get_plugin().await.unwrap().is_some());
        assert!(gateway.get_plugin().await.unwrap().is_some());
        assert_eq!(
            lookups.load(Ordering::SeqCst),
            3,
            "a found instance is cached"
        );
    }

    #[tokio::test]
    async fn instance_without_client_is_an_error_and_re_resolved() {
        let hub = Arc::new(ClientHub::new());
        let lookups = Arc::new(AtomicU32::new(0));
        let gateway = {
            let lookups = Arc::clone(&lookups);
            AuditGateway::with_lookup(Arc::clone(&hub), move || {
                lookups.fetch_add(1, Ordering::SeqCst);
                Some(INSTANCE_ID.to_owned())
            })
        };

        assert!(gateway.get_plugin().await.is_err());
        assert!(gateway.get_plugin().await.is_err());
        assert_eq!(lookups.load(Ordering::SeqCst), 2);

        hub.register_scoped::<dyn MiniChatAuditPluginClientV1>(
            ClientScope::gts_id(INSTANCE_ID),
            Arc::new(NoopPlugin),
        );
        assert!(gateway.get_plugin().await.unwrap().is_some());
    }

    /// Types-registry that lists no instances and counts `list_instances` calls.
    #[derive(Default)]
    struct EmptyRegistry {
        list_calls: AtomicU32,
    }

    fn unused() -> toolkit_canonical_errors::CanonicalError {
        toolkit_canonical_errors::CanonicalError::internal("not used by AuditGateway").create()
    }

    #[async_trait]
    impl TypesRegistryClient for EmptyRegistry {
        async fn register(
            &self,
            _: Vec<serde_json::Value>,
        ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn register_type_schemas(
            &self,
            _: Vec<serde_json::Value>,
        ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn get_type_schema(
            &self,
            _: &str,
        ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn get_type_schema_by_uuid(
            &self,
            _: uuid::Uuid,
        ) -> Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn get_type_schemas(
            &self,
            _: Vec<String>,
        ) -> std::collections::HashMap<
            String,
            Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
        > {
            std::collections::HashMap::new()
        }
        async fn get_type_schemas_by_uuid(
            &self,
            _: Vec<uuid::Uuid>,
        ) -> std::collections::HashMap<
            uuid::Uuid,
            Result<types_registry_sdk::GtsTypeSchema, toolkit_canonical_errors::CanonicalError>,
        > {
            std::collections::HashMap::new()
        }
        async fn list_type_schemas(
            &self,
            _: types_registry_sdk::TypeSchemaQuery,
        ) -> Result<Vec<types_registry_sdk::GtsTypeSchema>, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn register_instances(
            &self,
            _: Vec<serde_json::Value>,
        ) -> Result<Vec<types_registry_sdk::RegisterResult>, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn get_instance(
            &self,
            _: &str,
        ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn get_instance_by_uuid(
            &self,
            _: uuid::Uuid,
        ) -> Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>
        {
            Err(unused())
        }
        async fn get_instances(
            &self,
            _: Vec<String>,
        ) -> std::collections::HashMap<
            String,
            Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
        > {
            std::collections::HashMap::new()
        }
        async fn get_instances_by_uuid(
            &self,
            _: Vec<uuid::Uuid>,
        ) -> std::collections::HashMap<
            uuid::Uuid,
            Result<types_registry_sdk::GtsInstance, toolkit_canonical_errors::CanonicalError>,
        > {
            std::collections::HashMap::new()
        }
        async fn list_instances(
            &self,
            _: InstanceQuery,
        ) -> Result<Vec<types_registry_sdk::GtsInstance>, toolkit_canonical_errors::CanonicalError>
        {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn registry_without_instances_yields_no_plugin_and_is_not_cached() {
        let hub = Arc::new(ClientHub::new());
        let registry = Arc::new(EmptyRegistry::default());
        hub.register::<dyn TypesRegistryClient>(
            Arc::clone(&registry) as Arc<dyn TypesRegistryClient>
        );
        let gateway = AuditGateway::new(hub, "acme".to_owned());

        // PluginNotFound maps to Ok(None), not an error.
        assert!(gateway.get_plugin().await.unwrap().is_none());
        assert!(gateway.get_plugin().await.unwrap().is_none());
        assert_eq!(
            registry.list_calls.load(Ordering::SeqCst),
            2,
            "no-plugin must be re-resolved"
        );
    }

    #[tokio::test]
    async fn registry_missing_from_hub_is_an_error() {
        let gateway = AuditGateway::new(Arc::new(ClientHub::new()), "acme".to_owned());
        assert!(gateway.get_plugin().await.is_err());
    }
}
