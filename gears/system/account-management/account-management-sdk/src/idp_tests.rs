//! Tests for the SDK `IdP` provisioner contract -- trait default impl
//! and the metric-label constants on the failure enums.

use super::*;

use crate::{IdpNewUser, IdpTenantContext, IdpUserPagination};
use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Minimal stub implementing no trait methods. Pins the
/// `IdpPluginClient` defaults: every method MUST return its
/// category-appropriate `UnsupportedOperation`.
struct Stub;

#[async_trait]
impl IdpPluginClient for Stub {}

fn sample_tenant_context() -> IdpTenantContext {
    IdpTenantContext::new(
        Uuid::nil(),
        "t",
        gts::GtsTypeId::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"),
        None,
    )
}

fn sample_security_context() -> SecurityContext {
    SecurityContext::anonymous()
}

#[tokio::test]
async fn deprovision_default_impl_returns_unsupported_operation() {
    let s = Stub;
    let req = IdpDeprovisionTenantRequest::new(sample_tenant_context());
    let err = s
        .deprovision_tenant(&sample_security_context(), &req)
        .await
        .expect_err("default impl must err");
    assert!(matches!(
        err,
        IdpDeprovisionFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn provision_tenant_default_impl_returns_unsupported_operation() {
    let s = Stub;
    let req = IdpProvisionTenantRequest::for_root(
        Uuid::nil(),
        "t",
        gts::GtsTypeId::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"),
    );
    let err = s
        .provision_tenant(&sample_security_context(), &req)
        .await
        .expect_err("default impl must err");
    assert!(matches!(
        err,
        IdpProvisionFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn provision_user_default_impl_returns_unsupported_operation() {
    let s = Stub;
    let req = IdpProvisionUserRequest::new(sample_tenant_context(), IdpNewUser::new("alice"));
    let err = s
        .provision_user(&sample_security_context(), &req)
        .await
        .expect_err("default impl must err");
    assert!(matches!(
        err,
        IdpUserOperationFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn deprovision_user_default_impl_returns_unsupported_operation() {
    let s = Stub;
    let req = IdpDeprovisionUserRequest {
        tenant_context: sample_tenant_context(),
        user_id: Uuid::nil(),
    };
    let err = s
        .deprovision_user(&sample_security_context(), &req)
        .await
        .expect_err("default impl must err");
    assert!(matches!(
        err,
        IdpUserOperationFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn list_users_default_impl_returns_unsupported_operation() {
    let s = Stub;
    let req = IdpListUsersRequest::new(sample_tenant_context(), IdpUserPagination::default());
    let err = s
        .list_users(&sample_security_context(), &req)
        .await
        .expect_err("default impl must err");
    assert!(matches!(
        err,
        IdpUserOperationFailure::UnsupportedOperation { .. }
    ));
}

#[test]
fn parent_context_defaults_none_and_builder_sets_it() {
    let tt = gts::GtsTypeId::new("gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~");
    let req = IdpProvisionTenantRequest::new(Uuid::nil(), Uuid::nil(), "t", tt.clone());
    assert!(
        req.parent_context.is_none(),
        "new() must default parent_context to None"
    );

    let pctx = IdpTenantContext::new(
        Uuid::nil(),
        "parent",
        tt,
        Some(serde_json::json!({"k":"v"})),
    );
    let req2 = req.with_parent_context(pctx);
    assert!(
        req2.parent_context.is_some(),
        "with_parent_context must set the field"
    );
}

#[test]
fn provision_failure_metric_labels_are_stable() {
    assert_eq!(
        IdpProvisionFailure::CleanFailure {
            detail: String::new()
        }
        .as_metric_label(),
        "clean_failure"
    );
    assert_eq!(
        IdpProvisionFailure::Ambiguous {
            detail: String::new()
        }
        .as_metric_label(),
        "ambiguous"
    );
    assert_eq!(
        IdpProvisionFailure::UnsupportedOperation {
            detail: String::new()
        }
        .as_metric_label(),
        "unsupported_operation"
    );
    // `InvalidInput` carries an additional `field` for canonical-error
    // `field_violations` attribution; the metric label is independent
    // of that and stays stable so dashboards don't fragment.
    assert_eq!(
        IdpProvisionFailure::InvalidInput {
            detail: String::new(),
            field: Some("provisioning_metadata.realm_name".into()),
        }
        .as_metric_label(),
        "invalid_input"
    );
    // And with `field: None` — for plugins that can't localise the
    // violation to a specific key. Same label.
    assert_eq!(
        IdpProvisionFailure::InvalidInput {
            detail: String::new(),
            field: None,
        }
        .as_metric_label(),
        "invalid_input"
    );
}

#[test]
fn deprovision_failure_metric_labels_are_stable() {
    assert_eq!(
        IdpDeprovisionFailure::Terminal {
            detail: String::new()
        }
        .as_metric_label(),
        "terminal"
    );
    assert_eq!(
        IdpDeprovisionFailure::Retryable {
            detail: String::new()
        }
        .as_metric_label(),
        "retryable"
    );
    assert_eq!(
        IdpDeprovisionFailure::UnsupportedOperation {
            detail: String::new()
        }
        .as_metric_label(),
        "unsupported_operation"
    );
    assert_eq!(
        IdpDeprovisionFailure::NotFound {
            detail: String::new()
        }
        .as_metric_label(),
        "already_absent"
    );
}

#[test]
fn provision_failure_detail_and_display() {
    // `detail()` returns the raw provider string verbatim across every
    // variant so audit / redaction consumers do not have to repeat the
    // match arms themselves.
    let f = IdpProvisionFailure::Ambiguous {
        detail: "vendor timeout".to_owned(),
    };
    assert_eq!(f.detail(), "vendor timeout");
    // `Display` is `"<metric_label>: <detail>"` so trace lines and
    // `Box<dyn Error>` propagation produce a stable, grep-able shape.
    assert_eq!(f.to_string(), "ambiguous: vendor timeout");
    let f2 = IdpProvisionFailure::CleanFailure {
        detail: "refused".to_owned(),
    };
    assert_eq!(f2.to_string(), "clean_failure: refused");
    // InvalidInput Display uses the same label-prefixed shape; the
    // `field` is intentionally NOT in `Display` (it's a structured
    // attribute consumed by `into_domain_error`, not part of the
    // free-text trace line). Detail still round-trips verbatim so
    // operator log greps stay simple.
    let f3 = IdpProvisionFailure::InvalidInput {
        detail: "realm_name must be non-empty".to_owned(),
        field: Some("provisioning_metadata.realm_name".into()),
    };
    assert_eq!(f3.detail(), "realm_name must be non-empty");
    assert_eq!(
        f3.to_string(),
        "invalid_input: realm_name must be non-empty"
    );
}

#[test]
fn deprovision_failure_detail_and_display() {
    let f = IdpDeprovisionFailure::NotFound {
        detail: "gone".to_owned(),
    };
    assert_eq!(f.detail(), "gone");
    // `NotFound`'s metric label is `already_absent` (see
    // `IdpDeprovisionFailure::as_metric_label`) — `Display` preserves the
    // operational label, not the variant name.
    assert_eq!(f.to_string(), "already_absent: gone");
}

#[test]
fn failure_enums_implement_std_error_trait() {
    // The `IdP` failure enums must implement `core::error::Error`
    // so plugin authors can `?`-propagate them through `Box<dyn Error>`
    // / `thiserror::Error(#[from])` paths without writing manual
    // conversions. A `&dyn core::error::Error` coercion is the
    // compile-time witness.
    let provision: IdpProvisionFailure = IdpProvisionFailure::Ambiguous {
        detail: String::new(),
    };
    let deprovision: IdpDeprovisionFailure = IdpDeprovisionFailure::Terminal {
        detail: String::new(),
    };
    let _: &dyn core::error::Error = &provision;
    let _: &dyn core::error::Error = &deprovision;
}
