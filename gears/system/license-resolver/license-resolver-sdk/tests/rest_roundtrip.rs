//! REST round trip of the `LicenseResolverApi` projection: the generated server
//! routes on an ephemeral axum server, driven by the generated client.

#![cfg(all(feature = "rest-client", feature = "rest-server"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use license_resolver_sdk::rest::register_license_resolver_api_rest_routes;
use license_resolver_sdk::{
    LicenseCheckContext, LicenseCheckRequest, LicenseDecision, LicenseResolverApi,
    LicenseResolverApiRestClient,
};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_gts::gts_id;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

/// Grants a check only for the resource instance `gpt-4o`.
struct GrantsGpt4o;

#[async_trait]
impl LicenseResolverApi for GrantsGpt4o {
    async fn is_licensed(
        &self,
        _ctx: PlatformSecurityContext,
        request: LicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        Ok(
            LicenseDecision::new(request.resource.id.as_deref() == Some("gpt-4o"))
                .with_diagnostic("tenant", request.context.tenant_id.to_string()),
        )
    }
}

struct Unavailable;

#[async_trait]
impl LicenseResolverApi for Unavailable {
    async fn is_licensed(
        &self,
        _ctx: PlatformSecurityContext,
        _request: LicenseCheckRequest,
    ) -> Result<LicenseDecision, CanonicalError> {
        Err(CanonicalError::service_unavailable()
            .with_detail("License service temporarily unavailable")
            .create())
    }
}

fn request(resource_id: &str) -> LicenseCheckRequest {
    let subject = serde_json::from_value(serde_json::json!({
        "type": gts_id!("cf.core.lic.subj.v1~cf.core.lic.user.v1~"),
        "id": "u1",
        "metadata": {},
    }))
    .unwrap();
    let resource = serde_json::from_value(serde_json::json!({
        "type": gts_id!("cf.core.lic.res.v1~cf.genai.llm_gateway.model_usage.v1~"),
        "id": resource_id,
        "metadata": { "model_vendor": "openai" },
    }))
    .unwrap();
    let context = LicenseCheckContext::builder()
        .tenant_id(Uuid::from_u128(42))
        .build()
        .unwrap();
    LicenseCheckRequest::new(subject, resource, context)
}

async fn client_for(service: Arc<dyn LicenseResolverApi>) -> LicenseResolverApiRestClient {
    // `internal_auth_middleware` normally builds the `PlatformSecurityContext`
    // from the service credential; here every request gets a marker.
    let secctx = axum::middleware::from_fn(
        |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
            req.extensions_mut()
                .insert(PlatformSecurityContext::outbound_marker());
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
