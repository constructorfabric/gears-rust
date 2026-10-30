use std::sync::Arc;

use license_resolver_sdk::LicenseResolverApi;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::{OpenApiRegistry, OpenApiRegistryImpl};
use toolkit::client_hub::ClientHubError;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use uuid::Uuid;

use super::LicenseResolver;

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

fn make_ctx() -> GearCtx {
    let cfg = json!({ "license-resolver": { "vendor": "acme" } });
    GearCtx::new(
        LicenseResolver::MODULE_NAME,
        Uuid::from_u128(1),
        Arc::new(StaticConfigProvider { root: cfg }),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn init_registers_the_api_and_register_rest_mounts_it() {
    let ctx = make_ctx();
    let gear = LicenseResolver;

    gear.init(&ctx).await.expect("init must succeed");
    ctx.client_hub()
        .get::<dyn LicenseResolverApi>()
        .expect("resolver api must be registered in the hub");

    let openapi: &dyn OpenApiRegistry = &OpenApiRegistryImpl::new();
    let _router = gear
        .register_rest(&ctx, axum::Router::new(), openapi)
        .expect("register_rest must mount the route");
}

#[test]
fn register_rest_fails_before_init() {
    let openapi: &dyn OpenApiRegistry = &OpenApiRegistryImpl::new();
    let err = LicenseResolver
        .register_rest(&make_ctx(), axum::Router::new(), openapi)
        .expect_err("no api in the hub, no route");
    assert!(
        matches!(
            err.downcast_ref::<ClientHubError>(),
            Some(ClientHubError::NotFound { .. })
        ),
        "got: {err:?}"
    );
}
