//! Test utilities for OAGW integration tests.

pub mod api_v1;
pub mod body;
pub mod harness;
mod mock;
pub mod request;
pub mod response;

pub use body::{IntoBody, Json};
pub use harness::{AppHarness, AppHarnessBuilder};
pub use mock::{MockBody, MockGuard, MockResponse, MockUpstream, RecordedRequest, RouteKey};
pub use request::RequestCase;
pub use response::TestResponse;

pub use crate::domain::gts_helpers::{format_route_gts, format_upstream_gts, parse_resource_gts};
pub use crate::domain::test_support::{
    APIKEY_AUTH_PLUGIN_ID, CapturingAuthZResolverClient, DenyingAuthZResolverClient,
    OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID, TestAppState,
    TestCpBuilder, TestCredStoreClient, TestDpBuilder, build_test_app_state, build_test_gateway,
    ensure_crypto_provider,
};
