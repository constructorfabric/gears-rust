//! Inbound (server-side) platform-plane authentication for gRPC.
//!
//! [`InternalAuthGrpcLayer`] is the gRPC counterpart of the HTTP
//! `internal_auth_middleware`: a Tower [`Layer`] installed on a tonic
//! `Server` that validates the `x-toolkit-internal-token` metadata on every
//! inbound RPC and, on success, inserts a
//! [`PlatformSecurityContext`] and a [`PeerAuthenticated`] marker into the
//! request extensions so downstream handlers can read them
//! (`cpt-cf-adr-platform-plane-auth`).
//!
//! Validation is **async** (the K8s `TokenReview` backend is an out-of-process
//! call), so this cannot use tonic's synchronous `Interceptor` trait — it is a
//! Tower service operating at the `http::Request`/`http::Response` layer, which
//! tonic propagates into the handler's [`tonic::Request`] extensions.
//!
//! # Enforcement
//!
//! - [`InternalAuthEnforcement::Required`] (default) — a non-exempt RPC without
//!   a valid token is rejected. This is the mode a platform-plane-only listener
//!   uses.
//! - [`InternalAuthEnforcement::Permissive`] — an **absent** token passes
//!   through unauthenticated (mirroring the HTTP middleware), for listeners that
//!   also serve tenant-plane / anonymous RPCs. A **present-but-invalid** token
//!   is always rejected regardless of mode.
//!
//! When no authenticator is configured the layer is a no-op pass-through
//! (Profile 1 / in-process: the process boundary is the trust root).
//!
//! # Exempt methods
//!
//! Infrastructure RPCs (gRPC health checking, server reflection) are exempt by
//! path prefix — see [`DEFAULT_EXEMPT_PREFIXES`]. The allowlist is configurable
//! via [`InternalAuthGrpcLayer::with_exempt_prefixes`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use secrecy::{ExposeSecret, SecretString};
use tonic::Status;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{
    DynInternalAuthenticator, InternalAuthNError, InternalAuthenticator, PeerAuthenticated,
    PlatformSecurityContext,
};
use tower::{Layer, Service};

/// gRPC method path prefixes exempt from platform-plane enforcement by default.
///
/// These are the infrastructure services a client or load balancer probes
/// before (or independently of) authenticating: the standard health-checking
/// service and both versions of the reflection service. The `grpc.reflection.`
/// prefix (no trailing service segment) intentionally covers both
/// `grpc.reflection.v1` and `grpc.reflection.v1alpha`.
pub const DEFAULT_EXEMPT_PREFIXES: &[&str] = &["/grpc.health.v1.Health/", "/grpc.reflection."];

/// Whether an **absent** platform-plane credential is rejected or allowed.
///
/// A present-but-invalid credential is always rejected, in either mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalAuthEnforcement {
    /// Reject a non-exempt RPC that does not present a valid token. The mode a
    /// dedicated platform-plane listener uses.
    #[default]
    Required,
    /// Let an RPC that presents **no** token through unauthenticated (the tenant
    /// plane, if any, is enforced separately). Mirrors the HTTP middleware.
    Permissive,
}

/// Immutable, shared configuration behind a [`InternalAuthGrpcLayer`].
#[derive(Clone)]
struct Config {
    /// The platform-plane validator. `None` disables enforcement entirely
    /// (Profile 1 / in-process): every RPC passes through untouched.
    authenticator: Option<DynInternalAuthenticator>,
    /// How an absent credential is treated.
    enforcement: InternalAuthEnforcement,
    /// gRPC method path prefixes exempt from enforcement.
    exempt_prefixes: Vec<String>,
}

/// Tower [`Layer`] that enforces the platform plane on inbound gRPC requests.
///
/// Install it on a tonic server with `Server::builder().layer(layer)`. The
/// layer is server-wide: it applies to every service mounted on that server
/// (tonic cannot layer an async middleware onto a single service without losing
/// `NamedService`).
#[derive(Clone)]
pub struct InternalAuthGrpcLayer {
    config: Arc<Config>,
}

impl std::fmt::Debug for InternalAuthGrpcLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalAuthGrpcLayer")
            .field("enforced", &self.config.authenticator.is_some())
            .field("enforcement", &self.config.enforcement)
            .field("exempt_prefixes", &self.config.exempt_prefixes)
            .finish()
    }
}

impl InternalAuthGrpcLayer {
    /// Build a layer around an optional `authenticator`.
    ///
    /// When `authenticator` is `Some`, enforcement defaults to
    /// [`InternalAuthEnforcement::Required`] with the
    /// [`DEFAULT_EXEMPT_PREFIXES`] allowlist. When `None`, the layer is a
    /// no-op pass-through (Profile 1 / in-process).
    #[must_use]
    pub fn new(authenticator: Option<DynInternalAuthenticator>) -> Self {
        Self {
            config: Arc::new(Config {
                authenticator,
                enforcement: InternalAuthEnforcement::Required,
                exempt_prefixes: DEFAULT_EXEMPT_PREFIXES
                    .iter()
                    .map(|p| (*p).to_owned())
                    .collect(),
            }),
        }
    }

    /// Override how an absent credential is treated (default:
    /// [`InternalAuthEnforcement::Required`]).
    #[must_use]
    pub fn with_enforcement(mut self, enforcement: InternalAuthEnforcement) -> Self {
        Arc::make_mut(&mut self.config).enforcement = enforcement;
        self
    }

    /// Replace the exempt method-path allowlist (default:
    /// [`DEFAULT_EXEMPT_PREFIXES`]).
    ///
    /// Each entry is matched as a prefix of the gRPC method path
    /// (`/<package>.<Service>/<Method>`). Pass an empty vector to enforce on
    /// every method.
    #[must_use]
    pub fn with_exempt_prefixes(mut self, prefixes: Vec<String>) -> Self {
        Arc::make_mut(&mut self.config).exempt_prefixes = prefixes;
        self
    }
}

impl<S> Layer<S> for InternalAuthGrpcLayer {
    type Service = InternalAuthGrpcService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        InternalAuthGrpcService {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// The [`Service`] produced by [`InternalAuthGrpcLayer`].
#[derive(Clone)]
pub struct InternalAuthGrpcService<S> {
    inner: S,
    config: Arc<Config>,
}

/// Outcome of reading the internal-token header off an inbound request.
enum TokenOutcome {
    /// A non-empty token was present.
    Present(SecretString),
    /// No internal-token header was present.
    Missing,
    /// The header was present but malformed (non-ASCII) or empty.
    Invalid,
}

/// Read the `x-toolkit-internal-token` header from an inbound request.
fn read_token(headers: &http::HeaderMap) -> TokenOutcome {
    let Some(value) = headers.get(INTERNAL_TOKEN_HEADER) else {
        return TokenOutcome::Missing;
    };
    let Ok(raw) = value.to_str() else {
        return TokenOutcome::Invalid;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        TokenOutcome::Invalid
    } else {
        TokenOutcome::Present(SecretString::from(trimmed))
    }
}

/// Map a neutral [`InternalAuthNError`] onto a gRPC [`Status`].
///
/// The token and any provider-specific detail are never surfaced on the wire.
fn authn_error_to_status(err: &InternalAuthNError) -> Status {
    match err {
        InternalAuthNError::InvalidToken => Status::unauthenticated("invalid internal token"),
        InternalAuthNError::Unavailable => Status::unavailable("internal-auth backend unavailable"),
        // `Other` (and, defensively, any future neutral variant) is an
        // unexpected infrastructure failure. `InternalAuthNError` is
        // `#[non_exhaustive]`, so the wildcard is required.
        _ => Status::internal("internal authentication failure"),
    }
}

impl<S, ReqBody, ResBody> Service<http::Request<ReqBody>> for InternalAuthGrpcService<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = http::Response<ResBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<ReqBody>) -> Self::Future {
        let config = Arc::clone(&self.config);
        // Tower readiness contract: `poll_ready` was called on `self.inner`, so
        // the readiness reservation belongs to it. Move that instance into the
        // future and leave a fresh clone behind for the next call.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            // No authenticator configured: enforcement disabled (Profile 1).
            let Some(authenticator) = config.authenticator.as_ref() else {
                return inner.call(req).await;
            };

            // Infrastructure methods (health, reflection) bypass enforcement.
            let path = req.uri().path();
            if config
                .exempt_prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
            {
                return inner.call(req).await;
            }

            match read_token(req.headers()) {
                TokenOutcome::Present(token) => {
                    match authenticator.authenticate(token.expose_secret()).await {
                        Ok(identity) => {
                            let name = identity.peer_name().to_owned();
                            tracing::debug!(peer = %name, "platform-plane gRPC call authenticated");
                            req.extensions_mut().insert(PeerAuthenticated { name });
                            req.extensions_mut()
                                .insert(PlatformSecurityContext::new(identity));
                            inner.call(req).await
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "platform-plane gRPC authentication failed");
                            Ok(authn_error_to_status(&err).into_http())
                        }
                    }
                }
                // A malformed / empty credential is rejected in either mode.
                TokenOutcome::Invalid => {
                    Ok(Status::unauthenticated("invalid internal token").into_http())
                }
                TokenOutcome::Missing => match config.enforcement {
                    InternalAuthEnforcement::Permissive => inner.call(req).await,
                    InternalAuthEnforcement::Required => {
                        Ok(Status::unauthenticated("missing internal token").into_http())
                    }
                },
            }
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::future::{Ready, ready};

    use toolkit_security::PlatformIdentity;

    /// Header the [`Echo`] inner service sets to report whether the request
    /// carried a validated [`PlatformSecurityContext`] extension by the time it
    /// was called.
    const HAD_CTX_HEADER: &str = "x-test-had-ctx";
    /// Header reporting the [`PeerAuthenticated`] name the inner service saw.
    const PEER_HEADER: &str = "x-test-peer";

    /// Terminal inner service: records what extensions the request carried into
    /// response headers so tests can assert the middleware populated them.
    #[derive(Clone)]
    struct Echo;

    impl Service<http::Request<()>> for Echo {
        type Response = http::Response<()>;
        type Error = Infallible;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<()>) -> Self::Future {
            let had_ctx = req.extensions().get::<PlatformSecurityContext>().is_some();
            let peer = req
                .extensions()
                .get::<PeerAuthenticated>()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let mut resp = http::Response::new(());
            resp.headers_mut().insert(
                HAD_CTX_HEADER,
                if had_ctx { "1" } else { "0" }.parse().unwrap(),
            );
            resp.headers_mut().insert(
                PEER_HEADER,
                peer.parse().unwrap_or_else(|_| "".parse().unwrap()),
            );
            ready(Ok(resp))
        }
    }

    /// A fake platform-plane validator: `"good"` authenticates as `peer-x`,
    /// `"down"` is a backend outage, anything else is an invalid token.
    struct FakeAuth;

    impl InternalAuthenticator for FakeAuth {
        async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
            match token {
                "good" => Ok(PlatformIdentity::Shared {
                    name: "peer-x".to_owned(),
                }),
                "down" => Err(InternalAuthNError::Unavailable),
                _ => Err(InternalAuthNError::InvalidToken),
            }
        }
    }

    fn authed_layer() -> InternalAuthGrpcLayer {
        InternalAuthGrpcLayer::new(Some(DynInternalAuthenticator::new(FakeAuth)))
    }

    fn request(path: &str, token: Option<&str>) -> http::Request<()> {
        let mut builder = http::Request::builder().uri(path);
        if let Some(token) = token {
            builder = builder.header(INTERNAL_TOKEN_HEADER, token);
        }
        builder.body(()).unwrap()
    }

    /// Drive one request through the layered service.
    async fn call(layer: &InternalAuthGrpcLayer, req: http::Request<()>) -> http::Response<()> {
        let mut svc = layer.clone().layer(Echo);
        svc.call(req).await.unwrap()
    }

    fn grpc_status(resp: &http::Response<()>) -> Option<i32> {
        resp.headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
    }

    #[tokio::test]
    async fn none_authenticator_passes_through() {
        let layer = InternalAuthGrpcLayer::new(None);
        // No token, Required-by-default — but with no authenticator it is a no-op.
        let resp = call(&layer, request("/pkg.Svc/Method", None)).await;
        assert!(grpc_status(&resp).is_none(), "must not reject");
        assert_eq!(resp.headers().get(HAD_CTX_HEADER).unwrap(), "0");
    }

    #[tokio::test]
    async fn required_rejects_missing_token() {
        let resp = call(&authed_layer(), request("/pkg.Svc/Method", None)).await;
        // gRPC Unauthenticated == 16.
        assert_eq!(grpc_status(&resp), Some(16));
    }

    #[tokio::test]
    async fn valid_token_populates_extensions() {
        let resp = call(&authed_layer(), request("/pkg.Svc/Method", Some("good"))).await;
        assert!(grpc_status(&resp).is_none(), "valid token must not reject");
        assert_eq!(resp.headers().get(HAD_CTX_HEADER).unwrap(), "1");
        assert_eq!(resp.headers().get(PEER_HEADER).unwrap(), "peer-x");
    }

    #[tokio::test]
    async fn invalid_token_is_rejected() {
        let resp = call(&authed_layer(), request("/pkg.Svc/Method", Some("nope"))).await;
        assert_eq!(grpc_status(&resp), Some(16));
    }

    #[tokio::test]
    async fn empty_token_is_rejected_even_when_permissive() {
        let layer = authed_layer().with_enforcement(InternalAuthEnforcement::Permissive);
        let resp = call(&layer, request("/pkg.Svc/Method", Some("   "))).await;
        assert_eq!(grpc_status(&resp), Some(16));
    }

    #[tokio::test]
    async fn backend_unavailable_maps_to_unavailable() {
        let resp = call(&authed_layer(), request("/pkg.Svc/Method", Some("down"))).await;
        // gRPC Unavailable == 14.
        assert_eq!(grpc_status(&resp), Some(14));
    }

    #[tokio::test]
    async fn permissive_allows_missing_token() {
        let layer = authed_layer().with_enforcement(InternalAuthEnforcement::Permissive);
        let resp = call(&layer, request("/pkg.Svc/Method", None)).await;
        assert!(
            grpc_status(&resp).is_none(),
            "permissive must allow anonymous"
        );
        assert_eq!(resp.headers().get(HAD_CTX_HEADER).unwrap(), "0");
    }

    #[tokio::test]
    async fn exempt_path_bypasses_enforcement() {
        // Health check with no token is allowed even under Required.
        let resp = call(
            &authed_layer(),
            request("/grpc.health.v1.Health/Check", None),
        )
        .await;
        assert!(
            grpc_status(&resp).is_none(),
            "exempt method must pass through"
        );
        assert_eq!(resp.headers().get(HAD_CTX_HEADER).unwrap(), "0");
    }

    #[tokio::test]
    async fn custom_exempt_prefixes_replace_defaults() {
        let layer = authed_layer().with_exempt_prefixes(vec!["/my.Svc/".to_owned()]);
        // The custom prefix is now exempt.
        let resp = call(&layer, request("/my.Svc/Ping", None)).await;
        assert!(grpc_status(&resp).is_none());
        // The former default (health) is no longer exempt.
        let resp = call(&layer, request("/grpc.health.v1.Health/Check", None)).await;
        assert_eq!(grpc_status(&resp), Some(16));
    }
}
