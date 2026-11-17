use std::sync::Arc;

use license_resolver_sdk::{LicenseResolverApi, PlatformLicenseResolverApi};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::{OpenApiRegistry, OpenApiRegistryImpl};
use toolkit::client_hub::ClientHubError;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use uuid::Uuid;

use super::{LicenseResolver, refuse_remote_wiring};

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
async fn init_registers_both_apis_and_register_rest_mounts_them() {
    let ctx = make_ctx();
    let gear = LicenseResolver;

    gear.init(&ctx).await.expect("init must succeed");
    ctx.client_hub()
        .get::<dyn PlatformLicenseResolverApi>()
        .expect("platform api must be registered in the hub");
    let platform = ctx
        .client_hub()
        .get::<dyn PlatformLicenseResolverApi>()
        .unwrap();
    let tenant = ctx
        .client_hub()
        .get::<dyn LicenseResolverApi>()
        .expect("tenant api must be registered in the hub");
    assert_eq!(
        Arc::as_ptr(&platform).cast::<()>(),
        Arc::as_ptr(&tenant).cast::<()>(),
        "both contracts are served by one local client"
    );
    assert!(
        ctx.client_hub()
            .try_get::<crate::domain::LicenseResolverLocalClient>()
            .is_none(),
        "the client is held in the hub only while wiring"
    );

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

fn provider(gear_config: serde_json::Value) -> StaticConfigProvider {
    StaticConfigProvider {
        root: serde_json::Value::Object(
            [("license-resolver".to_owned(), gear_config)]
                .into_iter()
                .collect(),
        ),
    }
}

#[test]
fn a_remote_binding_for_a_provided_contract_is_refused() {
    for key in ["platform_license_resolver_api", "license_resolver_api"] {
        let cfg = provider(json!({
            "clients": { "provides": { key: { "transport": "rest", "endpoint": "http://r:1" } } }
        }));
        let err = refuse_remote_wiring(&cfg, "license-resolver")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("clients.provides.{key}")),
            "names the entry: {err}"
        );
    }
}

#[test]
fn local_or_absent_wiring_is_accepted() {
    for cfg in [
        provider(json!({ "vendor": "acme" })),
        provider(json!({
            "clients": { "provides": { "license_resolver_api": { "transport": "local" } } }
        })),
    ] {
        refuse_remote_wiring(&cfg, "license-resolver").unwrap();
    }
}
