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
use toolkit::gts_publication::GtsPublicationCapability;
use toolkit::{ClientHub, ConfigProvider, Gear, GearCtx};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing_platform::{Call, MockTypesRegistry};
use uuid::Uuid;

use super::CredStoreGear;

/// A config provider answering the gear from one `gears.credstore-v2` section.
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
    let registry = Arc::new(MockTypesRegistry::new());
    registry.install(&hub);

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
    // The declarations are published by the runtime before `init`: `init`
    // itself registers nothing in the registry.
    assert_eq!(registry.calls(Call::Register), 0);
}

/// `init` fails closed without the platform registry API: no secret type can
/// be resolved, so the gear does not come up.
#[tokio::test]
async fn init_fails_without_the_platform_registry_api() {
    let dsn = format!(
        "sqlite:file:credstore_gear_{}?mode=memory&cache=shared",
        Uuid::new_v4()
    );
    let db = connect_db(&dsn, ConnectOpts::default())
        .await
        .expect("connect sqlite");

    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(Arc::new(UnusedTenantResolver));

    let ctx = GearCtx::new(
        CredStoreGear::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(json!({ "config": {} }))),
        hub,
        CancellationToken::default(),
    )
    .with_db(DBProvider::new(db));

    let err = CredStoreGear::default()
        .init(&ctx)
        .await
        .expect_err("init without the registry API");
    assert!(
        format!("{err:#}").contains("PlatformTypesRegistryApi"),
        "{err:#}"
    );
}

/// The publication covers the SDK's credential types and plugin spec, the
/// permissions, and the audit topic and event type.
#[test]
fn the_manifest_declares_the_types_permissions_and_audit_entities() {
    let manifest = CredStoreGear::default()
        .descriptor()
        .build_gts_manifest(&FixedConfig(json!({ "config": {} })))
        .expect("manifest");
    let ids: Vec<&str> = manifest
        .documents()
        .iter()
        .map(|(id, _)| id.as_ref())
        .collect();

    for expected in [
        credstore_sdk::CREDENTIAL_RESOURCE_TYPE,
        <credstore_sdk::CredStorePluginSpecV2 as gts::GtsSchema>::TYPE_ID,
        crate::infra::audit::AUDIT_TOPIC_ID,
        crate::infra::audit::SECRET_AUDIT_EVENT_TYPE_ID,
        toolkit_gts::gts_id!("cf.toolkit.authz.permission.v1~cf.core.credstore.credential_read.v1"),
    ] {
        assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
    }
}
