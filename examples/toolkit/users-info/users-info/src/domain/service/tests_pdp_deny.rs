#![allow(clippy::unwrap_used, clippy::expect_used)]

use uuid::Uuid;

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::service::ServiceConfig;
use crate::test_support::{
    DenyAllAuthZResolver, build_services, build_services_with_authz, ctx_allow_tenants, inmem_db,
    seed_user,
};
use users_info_sdk::{NewAddress, NewCity, NewUser};

// ---------------------------------------------------------------------------
// Explicit PDP denial tests (decision=false → EnforcerError::AccessDenied → Forbidden)
// ---------------------------------------------------------------------------

/// When the PDP explicitly denies access, `list_users_page` must return
/// `DomainError::Forbidden` (not `InternalError` or any other variant).
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_list_users() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(&conn, Uuid::new_v4(), tenant_id, "u@example.com", "U").await;

    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services
        .users
        .list_users_page(&ctx, &toolkit_odata::ODataQuery::default())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden from explicit PDP denial, got: {err:?}"
    );
}

/// Explicit PDP denial on `get_user` → `DomainError::Forbidden`.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_get_user() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(&conn, user_id, tenant_id, "get@example.com", "Get User").await;

    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services.users.get_user(&ctx, user_id).await.unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for get_user, got: {err:?}"
    );
}

/// Explicit PDP denial on `create_user` → `DomainError::Forbidden`.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_create_user() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();

    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services
        .users
        .create_user(
            &ctx,
            NewUser {
                id: None,
                tenant_id,
                email: "new@example.com".to_owned(),
                display_name: "New User".to_owned(),
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for create_user, got: {err:?}"
    );
}

/// Explicit PDP denial on `update_user` → `DomainError::Forbidden`.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_update_user() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(&conn, user_id, tenant_id, "upd@example.com", "Upd User").await;

    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services
        .users
        .update_user(
            &ctx,
            user_id,
            users_info_sdk::UserPatch {
                email: Some("updated@example.com".to_owned()),
                display_name: None,
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for update_user, got: {err:?}"
    );
}

/// Explicit PDP denial on `delete_user` → `DomainError::Forbidden`.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_delete_user() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(&conn, user_id, tenant_id, "del@example.com", "Del User").await;

    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services.users.delete_user(&ctx, user_id).await.unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for delete_user, got: {err:?}"
    );
}

/// Explicit PDP denial on `list_addresses_page` → `DomainError::Forbidden`.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_list_addresses() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();

    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services
        .addresses
        .list_addresses_page(&ctx, &toolkit_odata::ODataQuery::default())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for list_addresses, got: {err:?}"
    );
}

/// Explicit PDP denial on `get_address` → `DomainError::Forbidden`.
/// The service prefetches the address (`allow_all`), then calls the enforcer
/// which must propagate the denial.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_get_address() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(
        &conn,
        user_id,
        tenant_id,
        "addr-get@example.com",
        "Addr Get",
    )
    .await;

    // First create the address with a permissive resolver
    let permissive = build_services(db.clone(), ServiceConfig::default());
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let city = permissive
        .cities
        .create_city(
            &ctx,
            NewCity {
                id: None,
                tenant_id,
                name: "Deny City".to_owned(),
                country: "DC".to_owned(),
            },
        )
        .await
        .unwrap();

    let addr = permissive
        .addresses
        .create_address(
            &ctx,
            NewAddress {
                id: None,
                tenant_id,
                user_id,
                city_id: city.id,
                street: "Deny St".to_owned(),
                postal_code: "00000".to_owned(),
            },
        )
        .await
        .unwrap();

    // Now use the denying resolver
    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );

    let err = services
        .addresses
        .get_address(&ctx, addr.id)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for get_address, got: {err:?}"
    );
}

/// Explicit PDP denial on `create_address` → `DomainError::Forbidden`.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_create_address() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(&conn, user_id, tenant_id, "addr-cr@example.com", "Addr Cr").await;

    // Create a city with a permissive resolver first
    let permissive = build_services(db.clone(), ServiceConfig::default());
    let pctx = ctx_allow_tenants(&[tenant_id]);

    let city = permissive
        .cities
        .create_city(
            &pctx,
            NewCity {
                id: None,
                tenant_id,
                name: "Cr City".to_owned(),
                country: "CC".to_owned(),
            },
        )
        .await
        .unwrap();

    // Now use the denying resolver
    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services
        .addresses
        .create_address(
            &ctx,
            NewAddress {
                id: None,
                tenant_id,
                user_id,
                city_id: city.id,
                street: "Denied St".to_owned(),
                postal_code: "11111".to_owned(),
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for create_address, got: {err:?}"
    );
}

/// Explicit PDP denial on `delete_address` → `DomainError::Forbidden`.
#[tokio::test]
async fn pdp_denied_returns_forbidden_for_delete_address() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(
        &conn,
        user_id,
        tenant_id,
        "addr-del@example.com",
        "Addr Del",
    )
    .await;

    // Create address with permissive resolver
    let permissive = build_services(db.clone(), ServiceConfig::default());
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let city = permissive
        .cities
        .create_city(
            &ctx,
            NewCity {
                id: None,
                tenant_id,
                name: "Del City".to_owned(),
                country: "DL".to_owned(),
            },
        )
        .await
        .unwrap();

    let addr = permissive
        .addresses
        .create_address(
            &ctx,
            NewAddress {
                id: None,
                tenant_id,
                user_id,
                city_id: city.id,
                street: "Del St".to_owned(),
                postal_code: "22222".to_owned(),
            },
        )
        .await
        .unwrap();

    // Now use the denying resolver
    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );

    let err = services
        .addresses
        .delete_address(&ctx, addr.id)
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden for delete_address, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Additional PDP denial tests (decision=false → EnforcerError::AccessDenied → Forbidden)
// ---------------------------------------------------------------------------

/// PDP returns `decision=false` → `DomainError::Forbidden`.
#[tokio::test]
async fn decision_false_returns_forbidden_for_list_users() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();

    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services
        .users
        .list_users_page(&ctx, &toolkit_odata::ODataQuery::default())
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden from decision=false, got: {err:?}"
    );
}

/// PDP returns `decision=false` on `create_address` → `DomainError::Forbidden`.
#[tokio::test]
async fn decision_false_returns_forbidden_for_create_address() {
    let db = inmem_db().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let conn = db.conn().unwrap();
    seed_user(&conn, user_id, tenant_id, "dec@example.com", "Dec User").await;

    // Create a city with a permissive resolver first
    let permissive = build_services(db.clone(), ServiceConfig::default());
    let pctx = ctx_allow_tenants(&[tenant_id]);

    let city = permissive
        .cities
        .create_city(
            &pctx,
            NewCity {
                id: None,
                tenant_id,
                name: "Dec City".to_owned(),
                country: "DF".to_owned(),
            },
        )
        .await
        .unwrap();

    // Now use the decision-denied resolver
    let services = build_services_with_authz(
        db.clone(),
        ServiceConfig::default(),
        Arc::new(DenyAllAuthZResolver),
    );
    let ctx = ctx_allow_tenants(&[tenant_id]);

    let err = services
        .addresses
        .create_address(
            &ctx,
            NewAddress {
                id: None,
                tenant_id,
                user_id,
                city_id: city.id,
                street: "Dec St".to_owned(),
                postal_code: "33333".to_owned(),
            },
        )
        .await
        .unwrap_err();

    assert!(
        matches!(err, DomainError::Forbidden),
        "Expected DomainError::Forbidden from decision=false, got: {err:?}"
    );
}
