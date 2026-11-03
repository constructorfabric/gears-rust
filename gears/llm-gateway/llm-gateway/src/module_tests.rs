//! Gear lifecycle: `init` fails closed on missing dependencies, and
//! `register_rest` requires an initialized service.

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverApi;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistryImpl;
use toolkit::config::ConfigProvider;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use uuid::Uuid;

use super::LlmGatewayModule;
use crate::domain::test_support::{CallLog, FakePdp, PdpMode};

struct StaticConfigProvider {
    root: serde_json::Value,
}

impl ConfigProvider for StaticConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.root.get(gear)
    }
}

fn make_ctx(hub: Arc<ClientHub>) -> GearCtx {
    GearCtx::new(
        LlmGatewayModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider { root: json!({}) }),
        hub,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn init_fails_when_authz_resolver_is_missing() {
    let ctx = make_ctx(Arc::new(ClientHub::new()));
    let err = LlmGatewayModule::default()
        .init(&ctx)
        .await
        .expect_err("init must not succeed without a PEP backend");

    let chain = format!("{err:#}");
    assert!(
        chain.contains("llm-gateway") && chain.contains("authz-resolver"),
        "the error names the gear and the missing dependency: {chain}"
    );
}

#[tokio::test]
async fn init_fails_when_model_registry_client_is_missing() {
    let hub = Arc::new(ClientHub::new());
    let pdp: Arc<dyn AuthZResolverApi> =
        FakePdp::new(PdpMode::Permit, Arc::new(CallLog::default()));
    hub.register::<dyn AuthZResolverApi>(pdp);
    let ctx = make_ctx(hub);

    let err = LlmGatewayModule::default()
        .init(&ctx)
        .await
        .expect_err("init must fail without a model-registry client");

    let chain = format!("{err:#}");
    assert!(
        chain.contains("llm-gateway") && chain.contains("model-registry"),
        "the error names the gear and the missing dependency: {chain}"
    );
}

#[test]
fn register_rest_before_init_fails() {
    let ctx = make_ctx(Arc::new(ClientHub::new()));
    let openapi = OpenApiRegistryImpl::new();
    let err = LlmGatewayModule::default()
        .register_rest(&ctx, axum::Router::new(), &openapi)
        .expect_err("register_rest needs the service from init");

    let msg = format!("{err}");
    assert!(
        msg.contains("llm-gateway") && msg.contains("not initialized"),
        "{msg}"
    );
}
