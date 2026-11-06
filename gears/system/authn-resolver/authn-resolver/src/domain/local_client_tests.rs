//! The local client and service forward an `AuthenticationRequest` to the
//! selected plugin unchanged, and the plugin's result back unchanged.

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthNResolverPluginClient, AuthNResolverPluginSpecV1,
    AuthenticationRequest, AuthenticationResult, CacheControl, CacheTag, ClientCredentialsRequest,
    RequestCacheControl,
};
use parking_lot::Mutex;
use secrecy::ExposeSecret as _;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

use super::{AuthNResolverLocalClient, Service};

const VENDOR: &str = "test-vendor";

/// Records what it receives and answers with a tagged, no-store result.
#[derive(Default)]
struct RecordingPlugin {
    received: Mutex<Vec<(String, RequestCacheControl)>>,
}

#[async_trait]
impl AuthNResolverPluginClient for RecordingPlugin {
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.received.lock().push((
            request.bearer_token.expose_secret().to_owned(),
            request.cache_control.clone(),
        ));
        Ok(AuthenticationResult::new(SecurityContext::anonymous())
            .with_cache_control(CacheControl::no_store().with_tag(CacheTag::new("plugin-tag"))))
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal("not used".to_owned()))
    }
}

fn local_client(plugin: Arc<RecordingPlugin>) -> AuthNResolverLocalClient {
    let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(
        "cf.test.recording_authn_resolver.plugin.v1",
    );
    let content = serde_json::to_value(PluginV1::<AuthNResolverPluginSpecV1> {
        id: instance_id.clone(),
        vendor: VENDOR.to_owned(),
        priority: 0,
        properties: AuthNResolverPluginSpecV1,
    })
    .expect("plugin instance serializes");
    let registry = MockTypesRegistryClient::new()
        .with_instances([make_test_instance(instance_id.as_ref(), content)]);

    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TypesRegistryClient>(Arc::new(registry));
    hub.register_scoped::<dyn AuthNResolverPluginClient>(
        ClientScope::gts_id(instance_id.as_ref()),
        plugin,
    );
    AuthNResolverLocalClient::new(Arc::new(Service::new(hub, VENDOR.to_owned())))
}

#[tokio::test]
async fn every_cache_control_mode_reaches_the_plugin_unchanged() {
    let plugin = Arc::new(RecordingPlugin::default());
    let client = local_client(Arc::clone(&plugin));
    let modes = [
        RequestCacheControl::AllowCached,
        RequestCacheControl::refresh(),
        RequestCacheControl::refresh_unless_newer_than(CacheTag::new("seen-tag")),
    ];

    for mode in &modes {
        let request = AuthenticationRequest::new("raw-token").with_cache_control(mode.clone());
        let result = client.authenticate(&request).await.expect("plugin answers");
        assert_eq!(
            result.cache_control.tag,
            Some(CacheTag::new("plugin-tag")),
            "the plugin's tag comes back unchanged"
        );
    }

    let received = plugin.received.lock().clone();
    let expected: Vec<_> = modes
        .into_iter()
        .map(|mode| ("raw-token".to_owned(), mode))
        .collect();
    assert_eq!(received, expected);
}
