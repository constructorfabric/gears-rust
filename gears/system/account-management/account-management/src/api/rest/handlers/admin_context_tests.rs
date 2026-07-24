//! Unit tests for the `GET /account-management/v1/admin/context` handler.
//!
//! Scope: pin the pure context-projection from [`super::get_admin_context`]
//! — verifies that the `subject_type` role marker drives `admin_mode`,
//! `capabilities`, and the `non_production_auth` flag, and that the subject
//! and home tenant are reflected from the
//! [`toolkit_security::SecurityContext`] into the [`super::AdminContextDto`].

use axum::Extension;
use uuid::Uuid;

use super::*;

fn ctx_with_type(subject_type: Option<&str>) -> SecurityContext {
    let mut builder = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xA11CE))
        .subject_tenant_id(Uuid::from_u128(0x007E_9A47));
    if let Some(st) = subject_type {
        builder = builder.subject_type(st);
    }
    builder.build().expect("ctx")
}

#[tokio::test]
async fn platform_admin_marker_yields_platform_mode() {
    let Json(body) = get_admin_context(Extension(ctx_with_type(Some("platform_admin"))))
        .await
        .expect("ok");

    assert_eq!(body.admin_mode, "platform");
    assert_eq!(body.subject_type.as_deref(), Some("platform_admin"));
    assert!(body.non_production_auth);
    assert!(body.capabilities.contains(&"tenants:write".to_owned()));
    assert!(body.capabilities.contains(&"gears:read".to_owned()));
}

#[tokio::test]
async fn tenant_admin_marker_yields_tenant_mode() {
    let Json(body) = get_admin_context(Extension(ctx_with_type(Some("tenant_admin"))))
        .await
        .expect("ok");

    assert_eq!(body.admin_mode, "tenant");
    assert!(body.non_production_auth);
    assert!(body.capabilities.contains(&"tenants:read".to_owned()));
    // Tenant admin must not advertise platform-only write capabilities.
    assert!(!body.capabilities.contains(&"tenants:write".to_owned()));
    assert!(!body.capabilities.contains(&"gears:read".to_owned()));
}

#[tokio::test]
async fn unknown_marker_defaults_to_least_privileged_tenant() {
    let Json(body) = get_admin_context(Extension(ctx_with_type(Some("something-else"))))
        .await
        .expect("ok");

    assert_eq!(body.admin_mode, "tenant");
    assert!(!body.non_production_auth);
    assert!(!body.capabilities.contains(&"tenants:write".to_owned()));
}

#[tokio::test]
async fn absent_marker_defaults_to_least_privileged_tenant() {
    let Json(body) = get_admin_context(Extension(ctx_with_type(None)))
        .await
        .expect("ok");

    assert_eq!(body.admin_mode, "tenant");
    assert_eq!(body.subject_type, None);
    assert!(!body.non_production_auth);
}
