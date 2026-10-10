//! Generated platform REST servers require internalToken before dispatch
//! (`cpt-cf-adr-two-plane-auth`), for owned and borrowed `PlatformSecurityContext`.

#![cfg(all(feature = "rest-client", feature = "rest-server"))]
#![expect(
    clippy::unwrap_used,
    reason = "test setup; a failed bind or request should fail the test"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
use toolkit_contract::runtime::config::{ClientConfig, InternalTokenProvider};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::{contract, rest_contract};
use toolkit_http_middleware::{RouteAuth, RouteAuthPolicy, layer_route_auth};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{
    DynInternalAuthenticator, InternalAuthNError, InternalAuthenticator, PlatformIdentity,
    PlatformSecurityContext,
};

const GOOD_TOKEN: &str = "good-internal-token";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct Echo {
    pub id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SysError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
}

impl axum::response::IntoResponse for SysError {
    fn into_response(self) -> axum::response::Response {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            self.to_string(),
        )
            .into_response()
    }
}

mod _api_dto_markers {
    use super::Echo;
    use toolkit::api::api_dto::ResponseApiDto;

    impl ResponseApiDto for Echo {}
}

#[contract(gear = "sys", version = "v1")]
pub trait SysApi: Send + Sync {
    #[idempotency(SafeRead)]
    async fn owned(&self, ctx: PlatformSecurityContext, id: String) -> Result<Echo, SysError>;

    #[idempotency(SafeRead)]
    async fn borrowed(&self, ctx: &PlatformSecurityContext, id: String) -> Result<Echo, SysError>;
}

#[rest_contract(base_path = "/api/sys/v1")]
pub trait SysApiRest: SysApi {
    #[get("/owned/{id}")]
    async fn owned(&self, ctx: PlatformSecurityContext, id: String) -> Result<Echo, SysError>;

    #[get("/borrowed/{id}")]
    async fn borrowed(&self, ctx: &PlatformSecurityContext, id: String) -> Result<Echo, SysError>;
}

/// Counts dispatches, so a refusal is shown to happen before the service.
struct SysService(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl SysApi for SysService {
    async fn owned(&self, _ctx: PlatformSecurityContext, id: String) -> Result<Echo, SysError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Echo { id })
    }

    async fn borrowed(&self, _ctx: &PlatformSecurityContext, id: String) -> Result<Echo, SysError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Echo { id })
    }
}

/// Accepts `GOOD_TOKEN` only.
struct StubInternalAuthenticator;

impl InternalAuthenticator for StubInternalAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        if token == GOOD_TOKEN {
            Ok(PlatformIdentity::Shared {
                name: "caller".to_owned(),
            })
        } else {
            Err(InternalAuthNError::InvalidToken)
        }
    }
}

/// Generated routes behind the `OoP` auth stack, with policy derived from registered specs.
async fn start_server() -> (String, serde_json::Value, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let svc: Arc<dyn SysApi> = Arc::new(SysService(calls.clone()));
    let openapi = OpenApiRegistryImpl::new();
    let router = register_sys_api_rest_routes(Router::new(), &openapi, svc);

    let policy: RouteAuthPolicy = openapi
        .operation_specs
        .iter()
        .map(|entry| {
            let spec = entry.value();
            assert_eq!(spec.auth, RouteAuth::Platform, "{}", spec.path);
            (spec.method.clone(), spec.path.clone(), spec.auth)
        })
        .collect();
    let app = layer_route_auth(
        router,
        policy,
        None,
        Some(DynInternalAuthenticator::new(StubInternalAuthenticator)),
    );

    let doc = openapi.build_openapi(&OpenApiInfo::default()).unwrap();
    let spec = serde_json::to_value(&doc).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), spec, calls)
}

fn client(base_url: &str, token: Option<&str>) -> SysApiRestClient {
    let mut cfg = ClientConfig::new(base_url.to_owned());
    if let Some(token) = token {
        cfg = cfg.with_internal_token_provider(InternalTokenProvider::from_token(
            SecretString::from(token.to_owned()),
        ));
    }
    SysApiRestClient::new(cfg).unwrap()
}

#[tokio::test]
async fn platform_operations_require_the_internal_token_scheme() {
    let (_, spec, _) = start_server().await;
    for path in ["/api/sys/v1/owned/{id}", "/api/sys/v1/borrowed/{id}"] {
        assert_eq!(
            spec["paths"][path]["get"]["security"],
            serde_json::json!([{ "internalToken": [] }]),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_validated_internal_token_reaches_the_service() {
    let (base_url, _, calls) = start_server().await;
    let client = client(&base_url, Some(GOOD_TOKEN));
    let marker = PlatformSecurityContext::outbound_marker();

    let owned = SysApi::owned(&client, marker.clone(), "a".to_owned())
        .await
        .unwrap();
    let borrowed = SysApi::borrowed(&client, &marker, "b".to_owned())
        .await
        .unwrap();

    assert_eq!((owned.id.as_str(), borrowed.id.as_str()), ("a", "b"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_missing_or_forged_token_is_refused_before_the_service() {
    let (base_url, _, calls) = start_server().await;
    let http = toolkit_http::HttpClientBuilder::new().build().unwrap();

    for path in ["owned", "borrowed"] {
        for (token, reason) in [
            (None, "MISSING_INTERNAL_TOKEN"),
            (Some("forged"), "INTERNAL_AUTH_FAILED"),
        ] {
            let mut request = http.get(&format!("{base_url}/api/sys/v1/{path}/a"));
            if let Some(token) = token {
                request = request.header(INTERNAL_TOKEN_HEADER, token);
            }
            let response = request.send().await.unwrap();
            let status = response.status();
            let body = String::from_utf8_lossy(&response.bytes().await.unwrap()).into_owned();
            assert_eq!(
                status,
                http::StatusCode::UNAUTHORIZED,
                "{path}, token {token:?}: {body}"
            );
            assert!(body.contains(reason), "{path}, token {token:?}: {body}");
        }
    }

    // The generated client surfaces the same refusal as an error.
    let marker = PlatformSecurityContext::outbound_marker();
    let client = client(&base_url, Some("forged"));
    assert!(
        SysApi::borrowed(&client, &marker, "b".to_owned())
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
