//! In-process `AccountManagementClient` coverage for tenant-scoped service
//! accounts.
//!
//! The REST suite proves wire behavior. This suite pins the sibling-gear path:
//! global `ClientHub` resolution must expose the same four operations and route
//! them through `ServiceAccountService`, rather than requiring an HTTP call back
//! into Account Management.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use account_management::AccountManagementClientImpl;
use account_management_sdk::AccountManagementClient;
use secrecy::ExposeSecret;
use toolkit::ClientHub;
use uuid::Uuid;

use common::*;

fn register_client(hub: &ClientHub, services: &TestServices) {
    let client: Arc<dyn AccountManagementClient> = Arc::new(AccountManagementClientImpl::new(
        Arc::clone(&services.tenant_service),
        Arc::clone(&services.user_service),
        Arc::clone(&services.service_account_service),
        Arc::clone(&services.metadata_service),
    ));
    hub.register::<dyn AccountManagementClient>(client);
}

// @cpt-begin:cpt-cf-account-management-dod-service-accounts-contract-trait-surface:p1:inst-dod-sa-am-client-trait-object-test
#[tokio::test]
async fn client_hub_trait_object_exposes_all_service_account_operations() {
    let harness = setup_sqlite().await.expect("sqlite");
    let tenant_id = Uuid::new_v4();
    seed_root(&harness, tenant_id).await;

    let services = build_services_full(
        &harness,
        fake_idp(),
        empty_metadata_registry(),
        types_registry_for_users(),
    );
    let hub = ClientHub::new();
    register_client(&hub, &services);
    let client = hub
        .get::<dyn AccountManagementClient>()
        .expect("AccountManagementClient registered");
    let ctx = ctx_for(tenant_id);

    let credentials = client
        .create_service_account(
            &ctx,
            tenant_id,
            "ci".to_owned(),
            vec!["platform.read".to_owned()],
        )
        .await
        .expect("create through ClientHub");
    assert_eq!(
        credentials.client_secret.expose_secret(),
        FAKE_CLIENT_SECRET
    );
    let client_id = credentials.client_id;

    let listed = client
        .list_service_accounts(&ctx, tenant_id)
        .await
        .expect("list through ClientHub");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].client_id, client_id);
    assert_eq!(listed[0].name, "ci");
    assert_eq!(listed[0].scopes, ["platform.read"]);

    let rotated = client
        .rotate_service_account_secret(&ctx, tenant_id, &client_id)
        .await
        .expect("rotate through ClientHub");
    assert_eq!(rotated.client_id, client_id);
    assert_eq!(rotated.client_secret.expose_secret(), FAKE_CLIENT_SECRET);

    client
        .revoke_service_account(&ctx, tenant_id, &client_id)
        .await
        .expect("revoke through ClientHub");
    assert!(
        client
            .list_service_accounts(&ctx, tenant_id)
            .await
            .expect("list after revoke")
            .is_empty()
    );
}
// @cpt-end:cpt-cf-account-management-dod-service-accounts-contract-trait-surface:p1:inst-dod-sa-am-client-trait-object-test

#[tokio::test]
async fn client_surface_preserves_canonical_tenant_errors() {
    let harness = setup_sqlite().await.expect("sqlite");
    let visible_tenant = Uuid::new_v4();
    seed_root(&harness, visible_tenant).await;

    let services = build_services(&harness);
    let hub = ClientHub::new();
    register_client(&hub, &services);
    let client = hub
        .get::<dyn AccountManagementClient>()
        .expect("AccountManagementClient registered");

    let absent_tenant = Uuid::new_v4();
    let err = client
        .list_service_accounts(&ctx_for(visible_tenant), absent_tenant)
        .await
        .expect_err("missing tenant must remain a canonical not-found");
    assert_eq!(err.status_code(), 404);
}
