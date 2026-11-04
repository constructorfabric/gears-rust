//! Gear-level tests for [`CredStoreGear`].
#![allow(clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverClient,
    TenantResolverError,
};
use tokio_util::sync::CancellationToken;
use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::MockTypesRegistryClient;
use uuid::Uuid;

use super::CredStoreGear;

/// A config provider answering the gear from one `gears.credstore` section.
struct FixedConfig(Value);

impl ConfigProvider for FixedConfig {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        (gear == CredStoreGear::MODULE_NAME).then_some(&self.0)
    }
}

/// `init` only stores the tenant resolver; nothing here is called.
struct UnusedTenantResolver;

#[async_trait]
impl TenantResolverClient for UnusedTenantResolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }
}

/// `AuthZ` is a consumed contract, wired after `init` (locally or remotely), so
/// `init` must not look it up.
#[tokio::test]
async fn init_does_not_need_authz_in_the_hub() {
    let dsn = format!(
        "sqlite:file:credstore_gear_{}?mode=memory&cache=shared",
        Uuid::new_v4()
    );
    let db = connect_db(&dsn, ConnectOpts::default())
        .await
        .expect("connect sqlite");

    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(Arc::new(UnusedTenantResolver));
    hub.register::<dyn TypesRegistryClient>(Arc::new(MockTypesRegistryClient::new()));

    let ctx = GearCtx::new(
        CredStoreGear::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": {} }))),
        hub,
        CancellationToken::default(),
    )
    .with_db(DBProvider::new(db));

    CredStoreGear::default()
        .init(&ctx)
        .await
        .expect("init without AuthZ in the hub");
}
