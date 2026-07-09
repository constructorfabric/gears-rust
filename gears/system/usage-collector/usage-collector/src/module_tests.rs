//! Lifecycle tests for the `UsageCollectorModule` gear.
//!
//! Covers the three failure branches that previously had no direct
//! coverage:
//!
//! 1. `init` returns an error when no `AuthZResolverClient` is registered
//!    in `ClientHub` — and the `ClientHubError` is preserved as the
//!    `anyhow::Error` source (regression for RUST-ERR-001).
//! 2. A second `init` call surfaces the `OnceLock` "already initialized"
//!    guard.
//! 3. `register_rest` invoked before `init` surfaces the
//!    "Service not initialized" guard.

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverClient;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistryImpl;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use uuid::Uuid;

use super::UsageCollectorModule;
use crate::domain::test_support::CountingAllowAllResolver;

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

fn make_ctx(hub: Arc<ClientHub>) -> GearCtx {
    let cfg = json!({
        "usage-collector": { "vendor": "test-vendor" }
    });
    GearCtx::new(
        UsageCollectorModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider { root: cfg }),
        hub,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn init_fails_when_authz_resolver_missing() {
    let hub = Arc::new(ClientHub::new());
    let ctx = make_ctx(hub);
    let module = UsageCollectorModule::default();

    let err = module
        .init(&ctx)
        .await
        .expect_err("init must fail when no authz-resolver client is registered");

    let top = format!("{err}");
    assert!(
        top.contains("usage-collector") && top.contains("authz-resolver"),
        "top-level message should name the gear and dependency, got: {top}"
    );

    // RUST-ERR-001 regression: the underlying `ClientHubError` MUST be
    // preserved as `source()` so the `{:#}` chain renders both the
    // contextual message and the not-found cause.
    let source = err
        .source()
        .expect("anyhow::Context::with_context must preserve the ClientHubError source");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("usage-collector") && chain.contains("not found"),
        "alternate-formatted chain should include both context and ClientHubError cause, got: {chain}"
    );
    // Source itself is a `ClientHubError::NotFound`; touch it to keep the
    // assertion hard against API-shape changes.
    let _ = source.to_string();
}

#[tokio::test]
async fn init_fails_when_already_initialized() {
    let hub = Arc::new(ClientHub::new());
    let resolver: Arc<dyn AuthZResolverClient> = CountingAllowAllResolver::new();
    hub.register::<dyn AuthZResolverClient>(resolver);

    let ctx = make_ctx(hub);
    let module = UsageCollectorModule::default();

    module.init(&ctx).await.expect("first init must succeed");

    let err = module
        .init(&ctx)
        .await
        .expect_err("second init must fail with the OnceLock guard");

    let msg = format!("{err}");
    assert!(
        msg.contains("usage-collector") && msg.contains("already initialized"),
        "second-init error should name the gear and the guard, got: {msg}"
    );
}

#[test]
fn register_rest_fails_when_service_not_initialized() {
    let hub = Arc::new(ClientHub::new());
    let ctx = make_ctx(hub);
    let module = UsageCollectorModule::default();
    let openapi = OpenApiRegistryImpl::new();

    let err = module
        .register_rest(&ctx, axum::Router::new(), &openapi)
        .expect_err("register_rest must fail when init has not run");

    let msg = format!("{err}");
    assert!(
        msg.contains("usage-collector") && msg.contains("not initialized"),
        "register_rest error should report the missing Service, got: {msg}"
    );
}
