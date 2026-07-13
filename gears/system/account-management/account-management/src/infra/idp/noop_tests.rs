use super::*;
use account_management_sdk::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpDeprovisionUserRequest,
    IdpListUsersRequest, IdpNewUser, IdpProvisionFailure, IdpProvisionTenantRequest,
    IdpProvisionUserRequest, IdpTenantContext, IdpUserOperationFailure, IdpUserPagination,
};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

fn sample_tenant_context() -> IdpTenantContext {
    IdpTenantContext::new(
        Uuid::nil(),
        "t",
        gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")),
        None,
    )
}

fn sec_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

#[tokio::test]
async fn noop_provider_reports_unsupported_operation_on_provision_tenant() {
    let p = NoopIdpProvider;
    let req = IdpProvisionTenantRequest::for_root(
        Uuid::nil(),
        "t",
        gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")),
    );
    let err = p
        .provision_tenant(&sec_ctx(), &req)
        .await
        .expect_err("noop must err");
    assert!(matches!(
        err,
        IdpProvisionFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn noop_provider_deprovision_tenant_reports_unsupported_operation() {
    let p = NoopIdpProvider;
    let req = IdpDeprovisionTenantRequest::new(sample_tenant_context());
    let err = p
        .deprovision_tenant(&sec_ctx(), &req)
        .await
        .expect_err("noop must err");
    assert!(matches!(
        err,
        IdpDeprovisionFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn noop_provider_provision_user_reports_unsupported_operation() {
    let p = NoopIdpProvider;
    let req = IdpProvisionUserRequest::new(sample_tenant_context(), IdpNewUser::new("alice"));
    let err = p
        .provision_user(&sec_ctx(), &req)
        .await
        .expect_err("noop must err");
    assert!(matches!(
        err,
        IdpUserOperationFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn noop_provider_deprovision_user_reports_unsupported_operation() {
    let p = NoopIdpProvider;
    let req = IdpDeprovisionUserRequest::new(sample_tenant_context(), Uuid::nil());
    let err = p
        .deprovision_user(&sec_ctx(), &req)
        .await
        .expect_err("noop must err");
    assert!(matches!(
        err,
        IdpUserOperationFailure::UnsupportedOperation { .. }
    ));
}

#[tokio::test]
async fn noop_provider_list_users_reports_unsupported_operation() {
    let p = NoopIdpProvider;
    let req = IdpListUsersRequest::new(sample_tenant_context(), IdpUserPagination::default());
    let err = p
        .list_users(&sec_ctx(), &req)
        .await
        .expect_err("noop must err");
    assert!(matches!(
        err,
        IdpUserOperationFailure::UnsupportedOperation { .. }
    ));
}
