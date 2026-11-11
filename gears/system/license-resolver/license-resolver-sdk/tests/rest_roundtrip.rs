//! REST round trip of the `LicenseResolverApi` projection: the generated server
//! routes on an ephemeral axum server, driven by the generated client.

#![cfg(all(feature = "rest-client", feature = "rest-server"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use license_resolver_sdk::rest::register_license_resolver_api_rest_routes;
use license_resolver_sdk::{
    IsLicensedRequest, LicenseCheckContext, LicenseDecision, LicenseResolverApi,
    LicenseResolverApiRestClient, PrincipalSubject, SelfLicenseCheckRequest, SubjectInput,
};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

/// Grants a check only for the resource instance `gpt-4o`.
struct GrantsGpt4o;

#[async_trait]
impl LicenseResolverApi for GrantsGpt4o {
    async fn is_licensed(
        &self,
        _ctx: PlatformSecurityContext,
        request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        let kind = match request.subject {
            SubjectInput::License(_) => "license",
            SubjectInput::Principal(_) => "principal",
        };
        Ok(
            LicenseDecision::new(request.resource.id.as_deref() == Some("gpt-4o"))
                .with_diagnostic("tenant", request.context.tenant_id.to_string())
                .with_diagnostic("kind", kind),
        )
    }

    async fn is_self_licensed(
        &self,
        ctx: SecurityContext,
        request: SelfLicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        Ok(
            LicenseDecision::new(request.resource.id.as_deref() == Some("gpt-4o"))
                .with_diagnostic("subject", ctx.subject_id().to_string()),
        )
    }
}

struct Unavailable;

#[async_trait]
impl LicenseResolverApi for Unavailable {
    async fn is_licensed(
        &self,
        _ctx: PlatformSecurityContext,
        _request: IsLicensedRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        Err(CanonicalError::service_unavailable()
            .with_detail("License service temporarily unavailable")
            .create())
    }

    async fn is_self_licensed(
        &self,
        _ctx: SecurityContext,
        _request: SelfLicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        unreachable!("not exercised")
    }
}

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(7))
        .subject_tenant_id(Uuid::from_u128(42))
        .subject_type("user")
        .bearer_token("token".to_owned())
        .build()
        .unwrap()
}

fn resource(resource_id: &str) -> license_resolver_sdk::Resource {
    serde_json::from_value(serde_json::json!({
        "type": gts_id!("cf.core.lic.res.v1~cf.genai.llm_gateway.model_usage.v1~"),
        "id": resource_id,
        "metadata": { "model_vendor": "openai" },
    }))
    .unwrap()
}

fn request(resource_id: &str) -> IsLicensedRequest {
    let subject: license_resolver_sdk::Subject = serde_json::from_value(serde_json::json!({
        "type": gts_id!("cf.core.lic.subj.v1~cf.core.lic.tenant.v1~"),
        "id": "u1",
        "metadata": {},
    }))
    .unwrap();
    let context = LicenseCheckContext::builder()
        .tenant_id(Uuid::from_u128(42))
        .build()
        .unwrap();
    IsLicensedRequest::new(subject, resource(resource_id), context)
}

async fn client_for(service: Arc<dyn LicenseResolverApi>) -> LicenseResolverApiRestClient {
    // The auth middlewares normally build both contexts from the credentials;
    // here every request gets a marker and the `caller()` context.
    let secctx = axum::middleware::from_fn(
        |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
            req.extensions_mut()
                .insert(PlatformSecurityContext::outbound_marker());
            req.extensions_mut().insert(caller());
            next.run(req).await
        },
    );
    let app: Router = register_license_resolver_api_rest_routes(
        Router::new(),
        &OpenApiRegistryImpl::new(),
        service,
    )
    .layer(secctx);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    LicenseResolverApiRestClient::new(ClientConfig::new(format!("http://{addr}"))).unwrap()
}

#[tokio::test]
async fn the_decision_and_tenant_scope_cross_the_rest_boundary() {
    let client = client_for(Arc::new(GrantsGpt4o)).await;
    let marker = PlatformSecurityContext::outbound_marker;

    let granted = client
        .is_licensed(marker(), request("gpt-4o"))
        .await
        .unwrap();
    assert!(granted.granted);
    assert_eq!(
        granted.diagnostics.get("tenant").and_then(|v| v.as_str()),
        Some(Uuid::from_u128(42).to_string().as_str()),
    );

    let denied = client.is_licensed(marker(), request("o3")).await.unwrap();
    assert!(
        !denied.granted,
        "a not-granted answer is a decision, not an error"
    );
}

#[tokio::test]
async fn a_principal_subject_crosses_the_rest_boundary() {
    let client = client_for(Arc::new(GrantsGpt4o)).await;
    let context = LicenseCheckContext::builder()
        .tenant_id(Uuid::from_u128(42))
        .build()
        .unwrap();
    let request = IsLicensedRequest::new(
        PrincipalSubject::new("user", Uuid::from_u128(7)),
        resource("gpt-4o"),
        context,
    );

    let decision = client
        .is_licensed(PlatformSecurityContext::outbound_marker(), request)
        .await
        .unwrap();
    assert_eq!(
        decision.diagnostics.get("kind").and_then(|v| v.as_str()),
        Some("principal")
    );
}

#[tokio::test]
async fn the_self_check_crosses_the_rest_boundary() {
    let client = client_for(Arc::new(GrantsGpt4o)).await;

    let decision = client
        .is_self_licensed(caller(), SelfLicenseCheckRequest::new(resource("gpt-4o")))
        .await
        .unwrap();
    assert!(decision.granted);
    assert_eq!(
        decision.diagnostics.get("subject").and_then(|v| v.as_str()),
        Some(Uuid::from_u128(7).to_string().as_str()),
    );
}

#[tokio::test]
async fn an_error_keeps_its_canonical_category_over_rest() {
    let client = client_for(Arc::new(Unavailable)).await;

    let err = client
        .is_licensed(
            PlatformSecurityContext::outbound_marker(),
            request("gpt-4o"),
        )
        .await
        .expect_err("a failing resolver must surface as an error");
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
}
