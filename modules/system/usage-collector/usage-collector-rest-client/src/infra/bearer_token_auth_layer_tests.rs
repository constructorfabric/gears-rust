#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use authn_resolver_sdk::{AuthNResolverError, ClientCredentialsRequest};
use bytes::Bytes;
use http::header::AUTHORIZATION;
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::Full;
use modkit_http::HttpError;
use tower::{Layer, Service};

use super::super::test_support::MockAuthN;
use super::BearerTokenAuthLayer;

fn make_creds() -> ClientCredentialsRequest {
    ClientCredentialsRequest {
        client_id: "test-client".to_owned(),
        client_secret: "test-secret".to_owned().into(),
        scopes: vec![],
    }
}

#[derive(Clone)]
struct CapturingService {
    captured: Arc<Mutex<Option<HeaderValue>>>,
}

impl Service<Request<Full<Bytes>>> for CapturingService {
    type Response = Response<Full<Bytes>>;
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Full<Bytes>>) -> Self::Future {
        let captured = Arc::clone(&self.captured);
        let hv = req.headers().get(AUTHORIZATION).cloned();
        Box::pin(async move {
            *captured.lock().unwrap() = hv;
            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::new()))
                .unwrap())
        })
    }
}

#[tokio::test]
async fn injects_sensitive_authorization_header() {
    let captured = Arc::new(Mutex::new(None));
    let inner = CapturingService {
        captured: Arc::clone(&captured),
    };
    let layer = BearerTokenAuthLayer::new(MockAuthN::with_token("test-bearer-token"), make_creds());
    let mut svc = layer.layer(inner);

    let req = Request::builder()
        .uri("http://example.com/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    svc.call(req).await.unwrap();

    let hv = captured
        .lock()
        .unwrap()
        .clone()
        .expect("Authorization header not set");
    assert_eq!(hv.to_str().unwrap(), "Bearer test-bearer-token");
    assert!(
        hv.is_sensitive(),
        "Authorization header value must be marked sensitive"
    );
}

#[tokio::test]
async fn authn_error_propagates_as_http_transport_error() {
    let captured = Arc::new(Mutex::new(None));
    let inner = CapturingService { captured };
    let layer = BearerTokenAuthLayer::new(MockAuthN::unauthorized(), make_creds());
    let mut svc = layer.layer(inner);

    let req = Request::builder()
        .uri("http://example.com/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let err = svc.call(req).await.unwrap_err();
    assert!(
        matches!(err, HttpError::Transport(_)),
        "expected Transport error, got: {err:?}"
    );
}

#[tokio::test]
async fn authn_error_propagates_and_downcasts_as_auth_n_resolver_error() {
    let captured = Arc::new(Mutex::new(None));
    let inner = CapturingService { captured };
    let layer = BearerTokenAuthLayer::new(MockAuthN::unauthorized(), make_creds());
    let mut svc = layer.layer(inner);

    let req = Request::builder()
        .uri("http://example.com/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let err = svc.call(req).await.unwrap_err();
    if let HttpError::Transport(boxed) = err {
        let auth_err = boxed
            .downcast::<AuthNResolverError>()
            .expect("should downcast to AuthNResolverError");
        assert!(
            matches!(*auth_err, AuthNResolverError::Unauthorized(_)),
            "expected Unauthorized variant, got: {:?}",
            *auth_err
        );
    } else {
        panic!("expected Transport error");
    }
}

#[tokio::test]
async fn authn_no_plugin_propagates_as_http_transport_with_no_plugin_variant() {
    // `AuthNResolverError::NoPluginAvailable` is a permanent misconfiguration
    // (no AuthN plugin registered), but the layer still wraps it as
    // `HttpError::Transport` so the REST client's `Transport → ServiceUnavailable`
    // arm classifies it as transient at the outbox level — operators recover
    // via the per-attempt DEBUG log plus retry-rate metrics. Pin the downcast
    // so a future refactor that special-cases `NoPluginAvailable` (e.g. maps it
    // to a permanent variant) is caught here.
    let captured = Arc::new(Mutex::new(None));
    let inner = CapturingService { captured };
    let layer = BearerTokenAuthLayer::new(MockAuthN::no_plugin(), make_creds());
    let mut svc = layer.layer(inner);

    let req = Request::builder()
        .uri("http://example.com/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let err = svc.call(req).await.unwrap_err();
    if let HttpError::Transport(boxed) = err {
        let auth_err = boxed
            .downcast::<AuthNResolverError>()
            .expect("should downcast to AuthNResolverError");
        assert!(
            matches!(*auth_err, AuthNResolverError::NoPluginAvailable),
            "expected NoPluginAvailable variant, got: {:?}",
            *auth_err
        );
    } else {
        panic!("expected Transport error");
    }
}

#[tokio::test]
async fn authn_token_acquisition_failed_propagates_as_http_transport() {
    // `TokenAcquisitionFailed` covers IdP-side rejection (invalid client creds,
    // unsupported grant). Same envelope as `Unauthorized` / `NoPluginAvailable`
    // — Transport-wrapped at the layer, then mapped to ServiceUnavailable by
    // the REST client. Pin the downcast for regression coverage.
    let captured = Arc::new(Mutex::new(None));
    let inner = CapturingService { captured };
    let layer = BearerTokenAuthLayer::new(MockAuthN::token_acquisition_failed(), make_creds());
    let mut svc = layer.layer(inner);

    let req = Request::builder()
        .uri("http://example.com/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let err = svc.call(req).await.unwrap_err();
    if let HttpError::Transport(boxed) = err {
        let auth_err = boxed
            .downcast::<AuthNResolverError>()
            .expect("should downcast to AuthNResolverError");
        assert!(
            matches!(*auth_err, AuthNResolverError::TokenAcquisitionFailed(_)),
            "expected TokenAcquisitionFailed variant, got: {:?}",
            *auth_err
        );
    } else {
        panic!("expected Transport error");
    }
}

#[tokio::test]
async fn missing_bearer_token_returns_transport_error_with_internal_variant() {
    let captured = Arc::new(Mutex::new(None));
    let inner = CapturingService { captured };
    let layer = BearerTokenAuthLayer::new(MockAuthN::without_token(), make_creds());
    let mut svc = layer.layer(inner);

    let req = Request::builder()
        .uri("http://example.com/test")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let err = svc.call(req).await.unwrap_err();
    if let HttpError::Transport(boxed) = err {
        let auth_err = boxed
            .downcast::<AuthNResolverError>()
            .expect("should downcast to AuthNResolverError");
        assert!(
            matches!(*auth_err, AuthNResolverError::Internal(_)),
            "expected Internal variant, got: {:?}",
            *auth_err
        );
    } else {
        panic!("expected Transport error, got: {err:?}");
    }
}
