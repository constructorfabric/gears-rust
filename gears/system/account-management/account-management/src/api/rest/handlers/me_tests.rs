//! Unit tests for the `GET /account-management/v1/me` handler.
//!
//! Scope: pin the pure context-projection from [`super::get_me`] —
//! verifies that all three fields (`subject_id`, `subject_type`,
//! `subject_tenant_id`) are correctly reflected from the
//! [`toolkit_security::SecurityContext`] into the [`super::MeDto`] body.

use axum::Extension;
use uuid::Uuid;

use super::*;

#[tokio::test]
async fn get_me_projects_context_into_body() {
    let subject = Uuid::from_u128(0xA11CE);
    let home = Uuid::from_u128(0x007E_9A47);
    let ctx = SecurityContext::builder()
        .subject_id(subject)
        .subject_type("service")
        .subject_tenant_id(home)
        .build()
        .expect("ctx");

    let Json(body) = get_me(Extension(ctx)).await.expect("ok");

    assert_eq!(body.subject_id, subject);
    assert_eq!(body.subject_type.as_deref(), Some("service"));
    assert_eq!(body.subject_tenant_id, home);
}
