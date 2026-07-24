#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "db")]

//! Comprehensive tests for the #[gear] macro with the new registry/builder

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use std::sync::Arc;
use toolkit::{
    GearCtx,
    config::ConfigProvider,
    contracts::{
        ApiGatewayCapability, DatabaseCapability, Gear, OpenApiRegistry, RestApiCapability,
        RunnableCapability,
    },
    gear,
};

// Helper for tests
struct EmptyConfigProvider;
impl ConfigProvider for EmptyConfigProvider {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

fn test_gear_ctx(cancel: tokio_util::sync::CancellationToken) -> GearCtx {
    GearCtx::new(
        "test",
        Uuid::new_v4(),
        Arc::new(EmptyConfigProvider),
        Arc::new(toolkit::client_hub::ClientHub::default()),
        cancel,
    )
}

/// Minimal `OpenAPI` registry mock
#[derive(Default)]
struct TestOpenApiRegistry;
impl OpenApiRegistry for TestOpenApiRegistry {
    fn register_operation(&self, _spec: &toolkit::api::OperationSpec) {}
    fn ensure_schema_raw(
        &self,
        root_name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        root_name.to_owned()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ---------- Test gears (must be at gear scope for `inventory`) ----------

#[derive(Default)]
#[gear(name = "basic")]
struct BasicGear;

#[async_trait]
impl Gear for BasicGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
#[gear(name = "full-featured", capabilities = [db, rest, stateful])]
struct FullFeaturedGear;

#[async_trait]
impl Gear for FullFeaturedGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}
impl DatabaseCapability for FullFeaturedGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![]
    }
}
impl RestApiCapability for FullFeaturedGear {
    fn register_rest(
        &self,
        _ctx: &toolkit::context::GearCtx,
        router: axum::Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> Result<axum::Router> {
        Ok(router)
    }
}
#[async_trait]
impl RunnableCapability for FullFeaturedGear {
    async fn start(&self, _t: CancellationToken) -> Result<()> {
        Ok(())
    }
    async fn stop(&self, _t: CancellationToken) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
#[gear(name = "dependent", deps = [basic, full_featured])]
struct DependentGear;

#[async_trait]
impl Gear for DependentGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
#[gear(name = "custom-ctor", ctor = CustomCtorGear::create())]
struct CustomCtorGear {
    value: i32,
}

impl CustomCtorGear {
    fn create() -> Self {
        Self { value: 42 }
    }
}

#[async_trait]
impl Gear for CustomCtorGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
#[gear(name = "db-only", capabilities = [db])]
struct DbOnlyGear;
#[async_trait]
impl Gear for DbOnlyGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}
impl DatabaseCapability for DbOnlyGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![]
    }
}

#[derive(Default)]
#[gear(name = "rest-only", capabilities = [rest])]
struct RestOnlyGear;
#[async_trait]
impl Gear for RestOnlyGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}
impl RestApiCapability for RestOnlyGear {
    fn register_rest(
        &self,
        _ctx: &toolkit::context::GearCtx,
        router: axum::Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> Result<axum::Router> {
        Ok(router)
    }
}

#[derive(Default)]
#[gear(name = "rest-host", capabilities = [rest_host])]
struct TestApiGatewayGear {
    registry: TestOpenApiRegistry,
}

#[async_trait]
impl Gear for TestApiGatewayGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}

impl ApiGatewayCapability for TestApiGatewayGear {
    fn rest_prepare(
        &self,
        _ctx: &toolkit::context::GearCtx,
        router: axum::Router,
        _hc_registry: std::sync::Arc<toolkit::RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }

    fn rest_finalize(
        &self,
        _ctx: &toolkit::context::GearCtx,
        router: axum::Router,
        _hc_registry: std::sync::Arc<toolkit::RestHealthcheckRegistry>,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }

    fn as_registry(&self) -> &dyn OpenApiRegistry {
        &self.registry
    }
}

#[derive(Default)]
#[gear(name = "stateful-only", capabilities = [stateful])]
struct StatefulOnlyGear;
#[async_trait]
impl Gear for StatefulOnlyGear {
    async fn init(&self, _ctx: &toolkit::context::GearCtx) -> Result<()> {
        Ok(())
    }
}
#[async_trait]
impl RunnableCapability for StatefulOnlyGear {
    async fn start(&self, _t: CancellationToken) -> Result<()> {
        Ok(())
    }
    async fn stop(&self, _t: CancellationToken) -> Result<()> {
        Ok(())
    }
}

// ---------- Tests ----------

#[tokio::test]
async fn test_basic_macro_and_init() {
    assert_eq!(BasicGear::MODULE_NAME, "basic");
    let ctx = test_gear_ctx(CancellationToken::new());
    BasicGear.init(&ctx).await.unwrap();
}

#[tokio::test]
async fn test_custom_ctor_name_and_value() {
    assert_eq!(CustomCtorGear::MODULE_NAME, "custom-ctor");
    let m = CustomCtorGear::create();
    assert_eq!(m.value, 42);
}

#[tokio::test]
async fn test_full_capabilities() {
    assert_eq!(FullFeaturedGear::MODULE_NAME, "full-featured");

    let ctx = test_gear_ctx(CancellationToken::new());
    FullFeaturedGear.init(&ctx).await.unwrap();

    // REST sync phase
    let router = axum::Router::new();
    let oas = TestOpenApiRegistry;
    let _router = FullFeaturedGear.register_rest(&ctx, router, &oas).unwrap();

    // Stateful
    let token = CancellationToken::new();
    FullFeaturedGear.start(token.clone()).await.unwrap();
    FullFeaturedGear.stop(token).await.unwrap();
}

#[test]
fn test_capability_trait_markers() {
    fn assert_gear<T: Gear>(_: &T) {}
    fn assert_db<T: DatabaseCapability>(_: &T) {}
    fn assert_rest<T: RestApiCapability>(_: &T) {}
    fn assert_stateful<T: RunnableCapability>(_: &T) {}

    assert_gear(&BasicGear);
    assert_gear(&DependentGear);
    assert_gear(&CustomCtorGear::default());

    assert_db(&FullFeaturedGear);
    assert_db(&DbOnlyGear);

    assert_rest(&FullFeaturedGear);
    assert_rest(&RestOnlyGear);

    assert_stateful(&FullFeaturedGear);
    assert_stateful(&StatefulOnlyGear);
}
