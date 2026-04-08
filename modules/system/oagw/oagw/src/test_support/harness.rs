//! Top-level test harness that wires all components together.

use std::sync::Arc;
use std::time::Duration;

use authz_resolver_sdk::AuthZResolverClient;
use modkit::client_hub::ClientHub;
use modkit_security::SecurityContext;
use oagw_sdk::api::ServiceGatewayClientV1;
use uuid::Uuid;

use crate::api::rest::routes::test_router;

use super::api_v1::ApiV1;
use super::mock::shared_mock;
use super::{TestCpBuilder, TestDpBuilder, build_test_app_state};

/// Fully-wired test environment for OAGW integration tests.
pub struct AppHarness {
    facade: Arc<dyn ServiceGatewayClientV1>,
    ctx: SecurityContext,
    router: axum::Router,
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

    pub(crate) fn router(&self) -> &axum::Router {
        &self.router
    }
}

/// Builder for [`AppHarness`].
#[derive(Default)]
pub struct AppHarnessBuilder {
    credentials: Vec<(String, String)>,
    request_timeout: Option<Duration>,
    authz_client: Option<Arc<dyn AuthZResolverClient>>,
    max_body_size: Option<usize>,
    skip_upstream_tls_verify: bool,
}

impl AppHarnessBuilder {
    pub fn with_credentials(mut self, creds: Vec<(String, String)>) -> Self {
        self.credentials = creds;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Override the AuthZ client used by the data plane (useful for authz tests).
    pub fn with_authz_client(mut self, client: Arc<dyn AuthZResolverClient>) -> Self {
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

    pub async fn build(self) -> AppHarness {
        let hub = ClientHub::new();

        let mut cp_builder = TestCpBuilder::new();
        if !self.credentials.is_empty() {
            cp_builder = cp_builder.with_credentials(self.credentials);
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
        dp_builder =
            dp_builder.with_token_http_config(modkit_http::HttpClientConfig::for_testing());

        let app_state = build_test_app_state(&hub, cp_builder, dp_builder);

        let ctx = SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context");

        let router = test_router(app_state.state, ctx.clone());

        AppHarness {
            facade: app_state.facade,
            ctx,
            router,
        }
    }
}
