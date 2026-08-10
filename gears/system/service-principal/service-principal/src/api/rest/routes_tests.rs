//! Route-registration + behavioral tests for the service-principal REST surface.
//!
//! Builds the router via [`register_routes`] against a *real* `OpenApiRegistryImpl`
//! (mirrors `credstore`/`token-issuer` `routes_tests.rs` — there is no separate
//! "Noop" registry test double in this toolkit version) and drives it end-to-end
//! with `tower::ServiceExt::oneshot`, injecting `SecurityContext` as a request
//! extension the way the real auth middleware would.

use std::sync::Arc;

use authz_resolver_sdk::AuthZResolverClient;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::error::AuthZResolverError;
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::pep::PolicyEnforcer;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use secrecy::SecretString;
use service_principal_sdk::{
    CreateServicePrincipalRequest, ServicePrincipalClientV1, ServicePrincipalCredentials,
    ServicePrincipalFailure, ServicePrincipalSummary, TenantId,
};
use toolkit::ClientHub;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::{SecurityContext, pep_properties};
use tower::ServiceExt as _;
use uuid::Uuid;

use super::*;
use crate::domain::service::Service;

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const SUBJECT: &str = "22222222-2222-2222-2222-222222222222";

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).expect("valid test uuid")
}

fn test_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid(SUBJECT))
        .subject_tenant_id(uuid(TENANT))
        .build()
        .expect("valid ctx")
}

/// PDP that permits, scoping the returned constraint to whatever owner tenant the
/// request carries — i.e. it always authorizes the resource actually being acted
/// on (mirrors `domain::service_tests::AllowTenantPdp`).
struct AllowTenantPdp;
#[async_trait::async_trait]
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

/// PDP that always denies.
struct DenyPdp;
#[async_trait::async_trait]
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

/// Configurable SPI mock — fixed success responses by default, with an
/// overridable `rotate_result` so a single test can drive a domain failure
/// through the real HTTP stack (mirrors `domain::service_tests::MockSp`).
#[derive(Default)]
struct MockSp {
    rotate_result: Option<ServicePrincipalFailure>,
}

#[async_trait::async_trait]
impl ServicePrincipalClientV1 for MockSp {
    async fn create(
        &self,
        _ctx: &SecurityContext,
        req: &CreateServicePrincipalRequest,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure> {
        Ok(ServicePrincipalCredentials {
            client_id: format!("svc-{}-{}", req.tenant_id.0, req.name),
            client_secret: SecretString::from("s3cr3t".to_owned()),
            token_url: "https://idp/token".to_owned(),
            subject_id: Uuid::new_v4(),
        })
    }
    async fn rotate_secret(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: TenantId,
        client_id: &str,
    ) -> Result<ServicePrincipalCredentials, ServicePrincipalFailure> {
        match &self.rotate_result {
            Some(e) => Err(clone_failure(e)),
            None => Ok(ServicePrincipalCredentials {
                client_id: client_id.to_owned(),
                client_secret: SecretString::from("rotated".to_owned()),
                token_url: "https://idp/token".to_owned(),
                subject_id: Uuid::new_v4(),
            }),
        }
    }
    async fn revoke(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: TenantId,
        _client_id: &str,
    ) -> Result<(), ServicePrincipalFailure> {
        Ok(())
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
/// `Debug` (it is not meant to be persisted/cloned in production code), so the
/// mock reconstructs an equivalent value from a shared reference (mirrors
/// `domain::service_tests::clone_failure`).
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

/// Build a router wired to a `Service` backed by the given PDP and SPI mock.
fn router_with_pdp_and_sp(
    pdp: Arc<dyn AuthZResolverClient>,
    sp: MockSp,
) -> (Router, Arc<OpenApiRegistryImpl>) {
    let hub = Arc::new(ClientHub::default());
    hub.register::<dyn ServicePrincipalClientV1>(Arc::new(sp));
    let svc = Arc::new(Service::new(PolicyEnforcer::new(pdp), hub));
    let openapi = Arc::new(OpenApiRegistryImpl::new());
    let router = register_routes(Router::new(), openapi.as_ref(), svc);
    (router, openapi)
}

/// Build a router wired to a `Service` backed by the given PDP, with a
/// default (all-success) `MockSp` registered in the `ClientHub`.
fn router_with_pdp(pdp: Arc<dyn AuthZResolverClient>) -> (Router, Arc<OpenApiRegistryImpl>) {
    router_with_pdp_and_sp(pdp, MockSp::default())
}

fn authorized_router() -> Router {
    router_with_pdp(Arc::new(AllowTenantPdp)).0
}

/// Inject the `SecurityContext` extension the way the real auth middleware would.
fn request_with_ctx(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let body = match body {
        Some(json) => Body::from(serde_json::to_vec(&json).expect("serializable body")),
        None => Body::empty(),
    };
    let mut req = builder.body(body).expect("valid request");
    req.extensions_mut().insert(test_ctx());
    req
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 1024 * 64).await.expect("body");
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

#[test]
fn all_four_routes_register_in_the_openapi_registry() {
    let (_router, openapi) = router_with_pdp(Arc::new(AllowTenantPdp));
    let keys: std::collections::BTreeSet<String> = openapi
        .operation_specs
        .iter()
        .map(|e| e.key().clone())
        .collect();
    let expected: std::collections::BTreeSet<String> = [
        "POST:/service-principal/v1/tenants/{tenant_id}/service-principals",
        "GET:/service-principal/v1/tenants/{tenant_id}/service-principals",
        "POST:/service-principal/v1/tenants/{tenant_id}/service-principals/{client_id}/rotate-secret",
        "DELETE:/service-principal/v1/tenants/{tenant_id}/service-principals/{client_id}",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        keys, expected,
        "exactly the four documented operations must be registered"
    );
}

#[tokio::test]
async fn create_returns_201_with_location_and_no_store_and_secret_body() {
    let router = authorized_router();
    let uri = format!("/service-principal/v1/tenants/{TENANT}/service-principals");
    let req = request_with_ctx("POST", &uri, Some(serde_json::json!({ "name": "ci" })));
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    // Pull the headers we need into owned values before `body_json` consumes
    // `resp` (it takes the response, not a reference, to drain the body).
    let cache_control = resp
        .headers()
        .get(axum::http::header::CACHE_CONTROL)
        .expect("Cache-Control header present")
        .to_str()
        .expect("ascii")
        .to_owned();
    assert!(
        cache_control.contains("no-store"),
        "secret responses must not be cached"
    );
    // A 201 MUST carry `Location` pointing at the new resource — asserted
    // below once we know the `client_id` from the body.
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .expect("Location header present")
        .to_str()
        .expect("ascii")
        .to_owned();

    let body = body_json(resp).await;
    assert_eq!(body["client_secret"], "s3cr3t");
    let client_id = body["client_id"].as_str().expect("client_id").to_owned();
    assert!(client_id.starts_with("svc-"));

    assert!(
        location.ends_with(&format!("/service-principals/{client_id}")),
        "Location {location:?} must end with /service-principals/{client_id}"
    );
}

#[tokio::test]
async fn list_returns_200_with_summaries() {
    let router = authorized_router();
    let uri = format!("/service-principal/v1/tenants/{TENANT}/service-principals");
    let req = request_with_ctx("GET", &uri, None);
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["service_principals"][0]["client_id"], "svc-x");
    assert_eq!(body["service_principals"][0]["enabled"], true);
    assert_eq!(body["service_principals"][0]["scopes"][0], "openid");
}

#[tokio::test]
async fn rotate_secret_returns_200_with_no_store_and_new_secret() {
    let router = authorized_router();
    let uri =
        format!("/service-principal/v1/tenants/{TENANT}/service-principals/svc-x/rotate-secret");
    let req = request_with_ctx("POST", &uri, None);
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let cache_control = resp
        .headers()
        .get(axum::http::header::CACHE_CONTROL)
        .expect("Cache-Control header present")
        .to_str()
        .expect("ascii");
    assert!(cache_control.contains("no-store"));
    let body = body_json(resp).await;
    assert_eq!(body["client_secret"], "rotated");
}

#[tokio::test]
async fn revoke_returns_204() {
    let router = authorized_router();
    let uri = format!("/service-principal/v1/tenants/{TENANT}/service-principals/svc-x");
    let req = request_with_ctx("DELETE", &uri, None);
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn denied_request_returns_403() {
    let router = router_with_pdp(Arc::new(DenyPdp)).0;
    let uri = format!("/service-principal/v1/tenants/{TENANT}/service-principals");
    let req = request_with_ctx("GET", &uri, None);
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// A domain failure (SPI `NotFound`) must render as an RFC 9457
/// `application/problem+json` envelope through the *real* HTTP stack — not just
/// the right status code. This exercises the full chain: handler `?` →
/// `From<DomainError> for CanonicalError` → `Problem::from_error` →
/// `IntoResponse`.
#[tokio::test]
async fn rotate_secret_not_found_renders_canonical_problem() {
    let sp = MockSp {
        rotate_result: Some(ServicePrincipalFailure::NotFound {
            detail: "no such client".to_owned(),
        }),
    };
    let router = router_with_pdp_and_sp(Arc::new(AllowTenantPdp), sp).0;
    let uri = format!(
        "/service-principal/v1/tenants/{TENANT}/service-principals/svc-ghost/rotate-secret"
    );
    let req = request_with_ctx("POST", &uri, None);
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("Content-Type header present")
        .to_str()
        .expect("ascii");
    assert!(
        content_type.contains("problem+json"),
        "canonical errors must render as RFC 9457 problem+json, got {content_type:?}"
    );

    let body = body_json(resp).await;
    assert_eq!(body["status"], 404);
    assert!(
        body["type"].as_str().is_some_and(|t| !t.is_empty()),
        "problem envelope must carry a non-empty `type` URI, got {body:?}"
    );
    assert!(
        body["title"].as_str().is_some_and(|t| !t.is_empty()),
        "problem envelope must carry a `title`, got {body:?}"
    );
}
