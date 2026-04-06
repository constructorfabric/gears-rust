use super::*;

impl AuditGateway {
    /// Create a no-op gateway for tests.
    ///
    /// The selector is pre-warmed with the empty-string sentinel so
    /// `get_plugin()` immediately returns `Ok(None)` and audit events
    /// are silently dropped without hitting the types-registry.
    pub(crate) fn noop() -> Arc<Self> {
        Self::new_preconfigured(
            Arc::new(ClientHub::new()),
            String::new(),
            GtsPluginSelector::pre_cached(String::new()),
        )
    }

    /// Create a gateway pre-loaded with a concrete plugin instance for unit tests.
    ///
    /// The supplied plugin is registered in a fresh `ClientHub` under a
    /// fixed synthetic instance ID. The selector is pre-cached so
    /// `get_plugin()` returns the plugin immediately without any
    /// types-registry round-trip.
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

    /// Create a gateway with explicit fields for tests that pre-warm the
    /// selector and register the plugin directly in the hub.
    pub(crate) fn new_preconfigured(
        hub: Arc<ClientHub>,
        vendor: String,
        selector: GtsPluginSelector,
    ) -> Arc<Self> {
        Arc::new(Self {
            hub,
            vendor,
            selector,
        })
    }
}
