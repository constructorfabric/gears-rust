//! Service-level tests: authorization outcomes + SPI delegation + revoke idempotency.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::error::AuthZResolverError;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use secrecy::SecretString;
use service_principal_sdk::{
    CreateServicePrincipalRequest, ServicePrincipalClientV1, ServicePrincipalCredentials,
    ServicePrincipalFailure, ServicePrincipalSummary, TenantId,
};
use toolkit::ClientHub;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use super::*;
use crate::domain::error::DomainError;

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const SUBJECT: &str = "22222222-2222-2222-2222-222222222222";

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).expect("valid test uuid")
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid(SUBJECT))
        .subject_tenant_id(uuid(TENANT))
        .build()
        .expect("valid ctx")
}

/// PDP that permits, scoping the returned constraint to the requested owner tenant.
struct AllowTenantPdp;
#[async_trait]
impl AuthZResolverClient for AllowTenantPdp {
    async fn evaluate(
        &self,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let tid = req
            .resource
            .properties
            .get(pep_properties::OWNER_TENANT_ID)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_default();
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tid],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// PDP that permits, but the returned constraint is scoped to a FIXED tenant
/// regardless of which tenant the request actually names — models a PDP that
/// grants "allow, but only for tenant A" while the caller is acting on a
/// different tenant B. The object-level (BOLA) check in
/// `authz::ensure_scope_permits` must reject the mismatch: a bare
/// `decision: true` is never sufficient on its own, the returned scope must
/// actually cover the tenant being acted on.
struct AllowOnlyTenantPdp {
    allowed_tenant: Uuid,
}
#[async_trait]
impl AuthZResolverClient for AllowOnlyTenantPdp {
    async fn evaluate(
        &self,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [self.allowed_tenant],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// PDP that always denies.
struct DenyPdp;
#[async_trait]
impl AuthZResolverClient for DenyPdp {
    async fn evaluate(
        &self,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Configurable SPI mock. `None` means "succeed"; the `Ok(())` state is never
/// needed since a configured mock only ever needs to express a failure.
#[derive(Default)]
struct MockSp {
    create_result: Option<ServicePrincipalFailure>,
    revoke_result: Option<ServicePrincipalFailure>,
}

#[async_trait]
impl ServicePrincipalClientV1 for MockSp {
    async fn create(
        &self,
        _ctx: &SecurityContext,
        req: &CreateServicePrincipalRequest,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure> {
        match &self.create_result {
            Some(e) => Err(clone_failure(e)),
            None => Ok(ServicePrincipalCredentials {
                client_id: format!("svc-{}-{}", req.tenant_id.0, req.name),
                client_secret: SecretString::from("s3cr3t".to_owned()),
                token_url: "https://idp/token".to_owned(),
                subject_id: Uuid::new_v4(),
            }),
        }
    }
    async fn rotate_secret(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: TenantId,
        client_id: &str,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure> {
        Ok(ServicePrincipalCredentials {
            client_id: client_id.to_owned(),
            client_secret: SecretString::from("rotated".to_owned()),
            token_url: "https://idp/token".to_owned(),
            subject_id: Uuid::new_v4(),
        })
    }
    async fn revoke(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: TenantId,
        _client_id: &str,
    ) -> Result<(), ServicePrincipalFailure> {
        match &self.revoke_result {
            Some(e) => Err(clone_failure(e)),
            None => Ok(()),
        }
    }
    async fn list(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: TenantId,
    ) -> Result<Vec<ServicePrincipalSummary>, ServicePrincipalFailure> {
        Ok(vec![ServicePrincipalSummary {
            client_id: "svc-x".to_owned(),
            enabled: true,
            scopes: vec!["openid".to_owned()],
        }])
    }
}

/// Hand-rolled clone: `ServicePrincipalFailure` intentionally derives only
/// `Debug` (it is not meant to be persisted/cloned in production code), so
/// the mock reconstructs an equivalent value from a shared reference.
fn clone_failure(f: &ServicePrincipalFailure) -> ServicePrincipalFailure {
    match f {
        ServicePrincipalFailure::InvalidInput { detail, field } => {
            ServicePrincipalFailure::InvalidInput {
                detail: detail.clone(),
                field: field.clone(),
            }
        }
        ServicePrincipalFailure::NotFound { detail } => ServicePrincipalFailure::NotFound {
            detail: detail.clone(),
        },
        ServicePrincipalFailure::CleanFailure { detail } => ServicePrincipalFailure::CleanFailure {
            detail: detail.clone(),
        },
        ServicePrincipalFailure::Ambiguous { detail } => ServicePrincipalFailure::Ambiguous {
            detail: detail.clone(),
        },
    }
}

fn service(pdp: Arc<dyn AuthZResolverClient>, sp: MockSp) -> Service {
    let hub = Arc::new(ClientHub::default());
    hub.register::<dyn ServicePrincipalClientV1>(Arc::new(sp));
    Service::new(PolicyEnforcer::new(pdp), hub)
}

#[tokio::test]
async fn create_authorized_returns_credentials() {
    let svc = service(Arc::new(AllowTenantPdp), MockSp::default());
    let creds = svc
        .create(&ctx(), TenantId(uuid(TENANT)), "ci".to_owned(), vec![])
        .await
        .expect("authorized create");
    assert!(creds.client_id.starts_with("svc-"));
}

/// Cross-tenant BOLA: a PDP `decision: true` scoped (via constraint) to tenant A
/// must NOT authorize a request explicitly targeting a different tenant B. This
/// exercises the full `Service::authorize` chain (not just the pure
/// `ensure_scope_permits` unit) — `access_scope_with` → `ensure_scope_permits` —
/// through a real `Service::create` call, proving the object-level check is
/// actually wired into the request path and not merely unit-testable in
/// isolation.
#[tokio::test]
async fn create_for_a_different_tenant_than_the_pdp_scope_is_access_denied() {
    let tenant_a = uuid(TENANT);
    let tenant_b = Uuid::new_v4();
    let svc = service(
        Arc::new(AllowOnlyTenantPdp {
            allowed_tenant: tenant_a,
        }),
        MockSp::default(),
    );
    let err = svc
        .create(&ctx(), TenantId(tenant_b), "ci".to_owned(), vec![])
        .await
        .expect_err("PDP scope covers tenant A only; tenant B must be denied");
    assert!(matches!(err, DomainError::AccessDenied));
}

#[tokio::test]
async fn denied_request_is_access_denied() {
    let svc = service(Arc::new(DenyPdp), MockSp::default());
    let err = svc
        .create(&ctx(), TenantId(uuid(TENANT)), "ci".to_owned(), vec![])
        .await
        .expect_err("denied");
    assert!(matches!(err, DomainError::AccessDenied));
}

#[tokio::test]
async fn revoke_is_idempotent_on_not_found() {
    let sp = MockSp {
        revoke_result: Some(ServicePrincipalFailure::NotFound {
            detail: "gone".into(),
        }),
        ..MockSp::default()
    };
    let svc = service(Arc::new(AllowTenantPdp), sp);
    svc.revoke(&ctx(), TenantId(uuid(TENANT)), "svc-x")
        .await
        .expect("revoke treats NotFound as success");
}

#[tokio::test]
async fn provider_absent_is_provider_unavailable() {
    // A hub with no SPI registration.
    let hub = Arc::new(ClientHub::default());
    let svc = Service::new(PolicyEnforcer::new(Arc::new(AllowTenantPdp)), hub);
    let err = svc
        .list(&ctx(), TenantId(uuid(TENANT)))
        .await
        .expect_err("no provider");
    assert!(matches!(err, DomainError::ProviderUnavailable));
}

#[tokio::test]
async fn create_spi_failure_maps_to_domain_invalid_input() {
    // Confirms the `.map_err(DomainError::from)` delegate-failure path on the
    // non-idempotent `create` op actually reaches the service boundary (until
    // now only `revoke`'s `NotFound` special-case was exercised here).
    let sp = MockSp {
        create_result: Some(ServicePrincipalFailure::InvalidInput {
            detail: "bad name".into(),
            field: Some("name".into()),
        }),
        ..MockSp::default()
    };
    let svc = service(Arc::new(AllowTenantPdp), sp);
    let err = svc
        .create(&ctx(), TenantId(uuid(TENANT)), "ci".to_owned(), vec![])
        .await
        .expect_err("SPI rejected the input");
    assert!(matches!(err, DomainError::InvalidInput { .. }));
}

#[tokio::test]
async fn rotate_secret_authorized_returns_credentials() {
    let svc = service(Arc::new(AllowTenantPdp), MockSp::default());
    let creds = svc
        .rotate_secret(&ctx(), TenantId(uuid(TENANT)), "svc-x")
        .await
        .expect("authorized rotate_secret");
    assert_eq!(creds.client_id, "svc-x");
}

#[tokio::test]
async fn list_authorized_returns_sp_summaries() {
    let svc = service(Arc::new(AllowTenantPdp), MockSp::default());
    let summaries = svc
        .list(&ctx(), TenantId(uuid(TENANT)))
        .await
        .expect("authorized list");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].client_id, "svc-x");
}
