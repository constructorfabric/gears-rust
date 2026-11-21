//! Top-level test harness that wires all components together.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::AuthZResolverApi;
use credstore_sdk::CredStoreClientV1;
use oagw_sdk::api::ServiceGatewayClientV1;
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::routes::test_router;
use crate::infra::proxy::streaming::StreamShutdown;

use super::api_v1::ApiV1;
use super::mock::shared_mock;
use super::{TestCpBuilder, TestDpBuilder, build_test_app_state};
use crate::domain::test_support::MockTenantResolverClient;

/// Fully-wired test environment for OAGW integration tests.
pub struct AppHarness {
    facade: Arc<dyn ServiceGatewayClientV1>,
    ctx: SecurityContext,
    router: axum::Router,
    state: crate::gear::AppState,
    stream_shutdown: StreamShutdown,
}

impl AppHarness {
    pub fn builder() -> AppHarnessBuilder {
        AppHarnessBuilder::default()
    }

    pub fn api_v1(&self) -> ApiV1<'_> {
        ApiV1::new(self)
    }

    /// Port of the shared mock server (started lazily on first call).
    pub fn mock_port(&self) -> u16 {
        shared_mock().port()
    }

    pub fn facade(&self) -> &dyn ServiceGatewayClientV1 {
        &*self.facade
    }

    pub fn security_context(&self) -> &SecurityContext {
        &self.ctx
    }

    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    /// A REST router over the same gateway whose requests run as `ctx`,
    /// for tests that call the REST API as several tenants.
    pub fn router_as(&self, ctx: SecurityContext) -> axum::Router {
        test_router(self.state.clone(), ctx)
    }

    /// End the open SSE streams and WebSocket relays, and the ones opened
    /// from now on, as the gear's `stop` does.
    pub fn end_streams(&self) {
        self.stream_shutdown.end_streams();
    }
}

/// Builder for [`AppHarness`].
#[derive(Default)]
pub struct AppHarnessBuilder {
    credentials: Vec<(String, String)>,
    request_timeout: Option<Duration>,
    authz_client: Option<Arc<dyn AuthZResolverApi>>,
    max_body_size: Option<usize>,
    skip_upstream_tls_verify: bool,
    websocket_idle_timeout: Option<Duration>,
    websocket_close_timeout: Option<Duration>,
    credstore: Option<Arc<dyn CredStoreClientV1>>,
    tenant_resolver: Option<MockTenantResolverClient>,
    metrics: Option<Arc<dyn crate::domain::ports::OagwMetricsPort>>,
    transform_plugins: Vec<(String, Arc<dyn crate::domain::plugin::TransformPlugin>)>,
    auth_plugins: Vec<(String, Arc<dyn crate::domain::plugin::AuthPlugin>)>,
    streaming_idle_timeout: Option<Duration>,
    upstream_ssrf_policy: bool,
}

impl AppHarnessBuilder {
    /// Resolve secrets through `credstore` (e.g. a `MockCredStoreClient`
    /// the test keeps) instead of a store built from [`Self::with_credentials`].
    pub fn with_credstore(mut self, credstore: Arc<dyn CredStoreClientV1>) -> Self {
        self.credstore = Some(credstore);
        self
    }

    /// Record the Data Plane's metrics in `metrics`.
    #[cfg(test)]
    pub(crate) fn with_metrics(
        mut self,
        metrics: Arc<dyn crate::domain::ports::OagwMetricsPort>,
    ) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Register a transform plugin next to the built-ins.
    #[cfg(test)]
    pub(crate) fn with_transform_plugin(
        mut self,
        plugin_id: &str,
        plugin: Arc<dyn crate::domain::plugin::TransformPlugin>,
    ) -> Self {
        self.transform_plugins.push((plugin_id.to_owned(), plugin));
        self
    }

    /// Register an auth plugin under `plugin_id`, replacing a built-in of
    /// that id: writes accept only built-in auth plugin ids.
    #[cfg(test)]
    pub(crate) fn with_auth_plugin(
        mut self,
        plugin_id: &str,
        plugin: Arc<dyn crate::domain::plugin::AuthPlugin>,
    ) -> Self {
        self.auth_plugins.push((plugin_id.to_owned(), plugin));
        self
    }

    /// Resolve tenants through `resolver` (for hierarchy tests).
    pub fn with_tenant_resolver(mut self, resolver: MockTenantResolverClient) -> Self {
        self.tenant_resolver = Some(resolver);
        self
    }

    pub fn with_credentials(mut self, creds: Vec<(String, String)>) -> Self {
        self.credentials = creds;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Override the AuthZ client used by the data plane (useful for authz tests).
    pub fn with_authz_client(mut self, client: Arc<dyn AuthZResolverApi>) -> Self {
        self.authz_client = Some(client);
        self
    }

    /// Override the maximum request body size (useful for body-limit tests).
    pub fn with_max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = Some(size);
        self
    }

    /// Skip upstream TLS certificate verification. **Test use only.**
    pub fn with_skip_upstream_tls_verify(mut self, allow: bool) -> Self {
        self.skip_upstream_tls_verify = allow;
        self
    }

    /// Override the WebSocket idle timeout (useful for idle-timeout tests).
    pub fn with_websocket_idle_timeout(mut self, timeout: Duration) -> Self {
        self.websocket_idle_timeout = Some(timeout);
        self
    }

    /// Override the WebSocket Close frame handshake timeout.
    pub fn with_websocket_close_timeout(mut self, timeout: Duration) -> Self {
        self.websocket_close_timeout = Some(timeout);
        self
    }

    /// Override the SSE streaming idle timeout.
    pub fn with_streaming_idle_timeout(mut self, timeout: Duration) -> Self {
        self.streaming_idle_timeout = Some(timeout);
        self
    }

    /// Resolve upstream hosts under the default SSRF policy, which refuses
    /// loopback and private addresses, instead of none. Writes are not
    /// checked.
    pub fn with_upstream_ssrf_policy(mut self) -> Self {
        self.upstream_ssrf_policy = true;
        self
    }

    pub async fn build(self) -> AppHarness {
        let hub = ClientHub::new();

        let mut cp_builder = TestCpBuilder::new();
        if !self.credentials.is_empty() {
            cp_builder = cp_builder.with_credentials(self.credentials);
        }
        if let Some(credstore) = self.credstore {
            cp_builder = cp_builder.with_credstore(credstore);
        }
        if let Some(resolver) = self.tenant_resolver {
            cp_builder = cp_builder.with_tenant_resolver(resolver);
        }

        let mut dp_builder = TestDpBuilder::new();
        if let Some(timeout) = self.request_timeout {
            dp_builder = dp_builder.with_request_timeout(timeout);
        }
        if let Some(client) = self.authz_client {
            dp_builder = dp_builder.with_authz_client(client);
        }
        if let Some(size) = self.max_body_size {
            dp_builder = dp_builder.with_max_body_size(size);
        }
        dp_builder = dp_builder.with_skip_upstream_tls_verify(self.skip_upstream_tls_verify);
        if let Some(timeout) = self.websocket_idle_timeout {
            dp_builder = dp_builder.with_websocket_idle_timeout(timeout);
        }
        if let Some(timeout) = self.websocket_close_timeout {
            dp_builder = dp_builder.with_websocket_close_timeout(timeout);
        }
        if let Some(metrics) = self.metrics {
            dp_builder = dp_builder.with_metrics(metrics);
        }
        for (plugin_id, plugin) in self.transform_plugins {
            dp_builder = dp_builder.with_transform_plugin(&plugin_id, plugin);
        }
        for (plugin_id, plugin) in self.auth_plugins {
            dp_builder = dp_builder.with_auth_plugin(&plugin_id, plugin);
        }
        if let Some(timeout) = self.streaming_idle_timeout {
            dp_builder = dp_builder.with_streaming_idle_timeout(timeout);
        }
        if self.upstream_ssrf_policy {
            dp_builder = dp_builder.with_upstream_ssrf_policy();
        }
        dp_builder =
            dp_builder.with_token_http_config(toolkit_http::HttpClientConfig::for_testing());

        let app_state = build_test_app_state(&hub, cp_builder, dp_builder);

        let ctx = SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context");

        let router = test_router(app_state.state.clone(), ctx.clone());

        AppHarness {
            facade: app_state.facade,
            ctx,
            router,
            state: app_state.state,
            stream_shutdown: app_state.stream_shutdown,
        }
    }
}
