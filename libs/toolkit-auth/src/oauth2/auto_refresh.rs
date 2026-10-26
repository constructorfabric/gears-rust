//! Reactive bearer-token auto-refresh tower layer.
//!
//! Wraps an outbound HTTP service with bearer-header injection plus a single
//! retry triggered by a configurable response predicate (default: HTTP 401).
//! On a triggering response the layer invalidates the provider's cached token,
//! re-reads it, and re-issues the original request once with the refreshed
//! credential.
//!
//! This is the Rust counterpart of go-appkit's `AuthBearerRoundTripper`
//! (`github.com/acronis/go-appkit/httpclient`). The contract - passthrough on
//! pre-set auth header, throttled invalidation, single retry, no-retry on
//! unchanged token - is preserved across the port.
//!
//! # Cost
//!
//! Auto-refresh requires the request body type `B` to be [`Clone`] because the
//! layer keeps a copy of the original request to replay on retry. With
//! `toolkit_http`'s default body (`http_body_util::Full<bytes::Bytes>`) this is
//! a cheap reference-counted clone, but it is still strictly more expensive
//! than [`super::layer::BearerAuthLayer`]. Pick the plain layer when the
//! upstream is known not to revoke tokens out-of-band.

use parking_lot::Mutex;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use http::header::{AUTHORIZATION, HeaderName};
use http::{HeaderValue, Request, Response, StatusCode};
use tower::{Layer, Service, ServiceExt};

use super::error::TokenError;
use super::token::TokenProvider;
use toolkit_http::HttpError;
use toolkit_utils::SecretString;

/// Closure awaiting a cached credential or shared acquisition.
///
/// Implemented by wrapping [`TokenProvider::get`] in production; tests substitute a
/// closure backed by an in-memory state machine to drive the layer's
/// expiration / invalidation branches without waiting on
/// [`TokenProvider`]'s background watcher.
type GetTokenFn = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<SecretString, TokenError>> + Send>>
        + Send
        + Sync,
>;

/// Closure forcing the cached credential to be discarded and re-fetched.
///
/// Implemented by wrapping [`TokenProvider::invalidate`] in production. The boxed
/// future allows the layer to stay generic over the concrete `TokenProvider` type
/// without exposing it as a type parameter, which keeps the public API
/// (`BearerAuthAutoRefreshLayer`, `BearerAuthAutoRefreshService<S>`)
/// monomorphic and stable.
type InvalidateTokenFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), TokenError>> + Send>> + Send + Sync>;

/// Default minimum interval between [`TokenProvider::invalidate`] calls.
///
/// Mirrors go-appkit's `DefaultAuthProviderMinInvalidationInterval`. Throttles
/// bursts of 401 responses so the token endpoint is not hammered when many
/// in-flight requests fail simultaneously.
pub const DEFAULT_MIN_INVALIDATION_INTERVAL: Duration = Duration::from_mins(15);

/// Predicate deciding whether a downstream response should trigger a token
/// invalidation + single retry.
///
/// Receives the response status. Defaults to "true on HTTP 401 only".
pub type ShouldRefreshFn = Arc<dyn Fn(StatusCode) -> bool + Send + Sync>;

/// Options for [`BearerAuthAutoRefreshLayer`].
///
/// Mirrors go-appkit's `AuthBearerRoundTripperOpts`. All fields are public so
/// callers can spread-update via [`BearerAuthAutoRefreshOpts::default`].
#[derive(Clone)]
pub struct BearerAuthAutoRefreshOpts {
    /// Minimum interval between `TokenProvider::invalidate` calls.
    ///
    /// The slot is consumed at the moment a triggering response arrives,
    /// before the invalidate completes - so a transient token-endpoint
    /// failure also burns the slot. Subsequent triggering responses inside
    /// the window will surface the original failure status without retrying
    /// `TokenProvider::invalidate`. Lower this value (e.g. 30-60 seconds) when
    /// faster recovery from token-endpoint blips matters more than burst
    /// protection.
    ///
    /// See [`DEFAULT_MIN_INVALIDATION_INTERVAL`].
    pub min_invalidation_interval: Duration,

    /// Predicate that decides whether to invalidate and retry on a given
    /// response status. Default: `status == 401`.
    pub should_refresh: ShouldRefreshFn,

    /// Header name to stamp the bearer credential into. Default:
    /// `Authorization`.
    pub header_name: HeaderName,
}

impl fmt::Debug for BearerAuthAutoRefreshOpts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BearerAuthAutoRefreshOpts")
            .field("min_invalidation_interval", &self.min_invalidation_interval)
            .field("header_name", &self.header_name)
            .finish_non_exhaustive()
    }
}

fn default_should_refresh(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED
}

impl Default for BearerAuthAutoRefreshOpts {
    fn default() -> Self {
        let should_refresh: ShouldRefreshFn = Arc::new(default_should_refresh);
        Self {
            min_invalidation_interval: DEFAULT_MIN_INVALIDATION_INTERVAL,
            should_refresh,
            header_name: AUTHORIZATION,
        }
    }
}

/// Tower layer that injects a bearer credential and reactively refreshes it
/// on a triggering response (default: HTTP 401).
///
/// Behaves like [`super::layer::BearerAuthLayer`] on the happy path. On a
/// response matching [`BearerAuthAutoRefreshOpts::should_refresh`] it
/// invalidates the provider's cached token (throttled by
/// [`BearerAuthAutoRefreshOpts::min_invalidation_interval`]), re-reads the
/// token, and re-sends the request once.
///
/// # Behavior corners
///
/// * If the request already carries the configured auth header, the layer is
///   a passthrough - no token is read and no retry is performed.
/// * If `TokenProvider::get` fails before the first send, the call returns
///   [`HttpError::Transport`] immediately.
/// * If `TokenProvider::get` fails after invalidation, or the new token equals the
///   previous one, the original triggering response is surfaced unchanged.
/// * Concurrent triggering responses share the throttle; only the first one
///   in the window calls `TokenProvider::invalidate`. Other concurrent calls re-read
///   the token and either retry with the refreshed credential (if it has
///   become available) or surface their original response.
/// * The throttle scope is per-layer-instance: two separate
///   `BearerAuthAutoRefreshLayer::new(...)` calls against the same `TokenProvider`
///   keep independent throttle state. Build one layer per upstream and
///   share it across clients to keep the throttle effective.
/// * If replacement fails or produces the same credential, the
///   slot is still consumed (see
///   [`BearerAuthAutoRefreshOpts::min_invalidation_interval`]).
#[derive(Clone)]
pub struct BearerAuthAutoRefreshLayer {
    get_token: GetTokenFn,
    invalidate_token: InvalidateTokenFn,
    opts: BearerAuthAutoRefreshOpts,
    last_invalidation: Arc<Mutex<Option<Instant>>>,
}

impl fmt::Debug for BearerAuthAutoRefreshLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BearerAuthAutoRefreshLayer")
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl BearerAuthAutoRefreshLayer {
    /// Create a layer with default options (`Authorization` header,
    /// 401-triggers-refresh, 15-minute throttle).
    #[must_use]
    pub fn new(token: TokenProvider) -> Self {
        Self::with_opts(token, BearerAuthAutoRefreshOpts::default())
    }

    /// Create a layer with explicit options.
    #[must_use]
    pub fn with_opts(token: TokenProvider, opts: BearerAuthAutoRefreshOpts) -> Self {
        let token_for_get = token.clone();
        let get_token: GetTokenFn = Arc::new(move || {
            let token = token_for_get.clone();
            Box::pin(async move { token.get().await })
        });
        let invalidate_token: InvalidateTokenFn = Arc::new(move || {
            let t = token.clone();
            Box::pin(async move { t.invalidate().await })
        });
        Self::from_fns(get_token, invalidate_token, opts)
    }

    fn from_fns(
        get_token: GetTokenFn,
        invalidate_token: InvalidateTokenFn,
        opts: BearerAuthAutoRefreshOpts,
    ) -> Self {
        Self {
            get_token,
            invalidate_token,
            opts,
            last_invalidation: Arc::new(Mutex::new(None)),
        }
    }
}

impl<S> Layer<S> for BearerAuthAutoRefreshLayer {
    type Service = BearerAuthAutoRefreshService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BearerAuthAutoRefreshService {
            inner,
            get_token: Arc::clone(&self.get_token),
            invalidate_token: Arc::clone(&self.invalidate_token),
            opts: self.opts.clone(),
            last_invalidation: Arc::clone(&self.last_invalidation),
        }
    }
}

/// Tower service produced by [`BearerAuthAutoRefreshLayer`].
///
/// All clones of one service share the same throttle state, so spawning
/// multiple clients off a single layer instance still produces a single
/// rate-limited invalidation source per token.
#[derive(Clone)]
pub struct BearerAuthAutoRefreshService<S> {
    inner: S,
    get_token: GetTokenFn,
    invalidate_token: InvalidateTokenFn,
    opts: BearerAuthAutoRefreshOpts,
    last_invalidation: Arc<Mutex<Option<Instant>>>,
}

impl<S: fmt::Debug> fmt::Debug for BearerAuthAutoRefreshService<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BearerAuthAutoRefreshService")
            .field("inner", &self.inner)
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

/// Build a `Bearer <token>` header value with the sensitive flag set.
///
/// The plaintext is held in a zeroizing buffer until [`HeaderValue::from_str`]
/// copies it; the returned value is marked sensitive so it is redacted by
/// `http`'s `Debug` and skipped from HTTP/2 HPACK indexing.
fn build_bearer_value(token: &str) -> Result<HeaderValue, http::header::InvalidHeaderValue> {
    let raw = zeroize::Zeroizing::new(format!("Bearer {token}"));
    let mut value = HeaderValue::from_str(&raw)?;
    value.set_sensitive(true);
    Ok(value)
}

/// Try to acquire the throttle slot. Returns `true` if the caller should
/// proceed with `TokenProvider::invalidate`, `false` if a recent invalidation is
/// still inside the configured window.
///
/// Holds a `parking_lot::Mutex` guard for one read+write - never across an
/// `.await`.
fn try_acquire_invalidation_slot(
    last_invalidation: &Mutex<Option<Instant>>,
    min_interval: Duration,
) -> bool {
    let mut guard = last_invalidation.lock();
    let invalidate = match *guard {
        Some(last) => last.elapsed() >= min_interval,
        None => true,
    };
    if invalidate {
        *guard = Some(Instant::now());
    }
    invalidate
}

impl<S, B, ResBody> Service<Request<B>> for BearerAuthAutoRefreshService<S>
where
    S: Service<Request<B>, Response = Response<ResBody>, Error = HttpError>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
    B: Clone + Send + 'static,
    ResBody: Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Response<ResBody>, HttpError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        // Mirrors go-appkit's RoundTrip first branch: if the caller already
        // set the auth header we never read or invalidate the cached token.
        if req.headers().contains_key(&self.opts.header_name) {
            let clone = self.inner.clone();
            let mut inner = std::mem::replace(&mut self.inner, clone);
            return Box::pin(async move { inner.call(req).await });
        }

        // Clone-swap: `inner` is the clone already polled ready by the
        // outer caller and is consumed for the first send; `retry_inner`
        // is a separate clone whose readiness is re-established via
        // `oneshot` if we need it.
        let outer_clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, outer_clone);
        let retry_inner = self.inner.clone();

        let get_token = Arc::clone(&self.get_token);
        let invalidate_token = Arc::clone(&self.invalidate_token);
        let opts = self.opts.clone();
        let last_invalidation = Arc::clone(&self.last_invalidation);
        Box::pin(async move {
            let initial_secret = get_token()
                .await
                .map_err(|e| HttpError::Transport(Box::new(e)))?;
            let bearer_value = build_bearer_value(initial_secret.expose())
                .map_err(HttpError::InvalidHeaderValue)?;
            let retry_req = req.clone();
            req.headers_mut()
                .insert(opts.header_name.clone(), bearer_value);
            let initial_secret_for_compare = initial_secret;
            let response = inner.call(req).await?;

            if !(opts.should_refresh)(response.status()) {
                return Ok(response);
            }

            // Throttle so a burst of triggering responses produces at most
            // one `TokenProvider::invalidate` call per window. Later concurrent
            // callers skip the invalidate but still re-read the token in
            // case the first invalidator already swapped a fresh one in.
            let did_invalidate =
                try_acquire_invalidation_slot(&last_invalidation, opts.min_invalidation_interval);
            if did_invalidate {
                tracing::info!(
                    status = response.status().as_u16(),
                    header = %opts.header_name,
                    "OAuth2 auto-refresh: invalidating token after auth-failure response"
                );
                if let Err(error) = invalidate_token().await {
                    tracing::warn!(%error, "OAuth2 auto-refresh: replacement failed");
                    return Ok(response);
                }
            }

            // Join pending replacement if another caller invalidated the token.
            // If acquisition fails, preserve the original upstream response.
            let new_secret = match get_token().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        status = response.status().as_u16(),
                        header = %opts.header_name,
                        "OAuth2 auto-refresh: token unavailable after invalidate; surfacing original response"
                    );
                    return Ok(response);
                }
            };

            // Avoid a 401 loop when the token endpoint keeps minting the
            // same value (or the throttle prevented another invalidation).
            if new_secret.expose() == initial_secret_for_compare.expose() {
                if did_invalidate {
                    tracing::warn!(
                        status = response.status().as_u16(),
                        header = %opts.header_name,
                        "OAuth2 auto-refresh: token unchanged after invalidate; surfacing original response"
                    );
                }
                return Ok(response);
            }

            let new_value = match build_bearer_value(new_secret.expose()) {
                Ok(v) => v,
                Err(e) => return Err(HttpError::InvalidHeaderValue(e)),
            };

            // We cannot drain the original response body because `ResBody`
            // is not bound on `http_body::Body`; the production stack's
            // `toolkit_http::ResponseBody` already buffers, so dropping is
            // safe there.
            drop(response);

            let mut retry_req = retry_req;
            retry_req
                .headers_mut()
                .insert(opts.header_name.clone(), new_value);
            // `oneshot` polls readiness on the separate clone we captured
            // before the swap and consumes it for the single retry.
            retry_inner.oneshot(retry_req).await
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{Method, Request, Response, StatusCode};
    use http_body_util::Full;
    use httpmock::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use toolkit_utils::SecretString;
    use url::Url;

    use crate::oauth2::config::OAuthClientConfig;

    fn test_config(server: &MockServer) -> OAuthClientConfig {
        OAuthClientConfig {
            token_endpoint: Some(
                Url::parse(&format!("http://localhost:{}/token", server.port())).unwrap(),
            ),
            client_id: "test-client".into(),
            client_secret: SecretString::new("test-secret"),
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            jitter_max: Duration::from_millis(0),
            min_refresh_period: Duration::from_millis(100),
            ..Default::default()
        }
    }

    fn token_json(token: &str, expires_in: u64) -> String {
        format!(r#"{{"access_token":"{token}","expires_in":{expires_in},"token_type":"Bearer"}}"#)
    }

    fn empty_req() -> Request<Full<Bytes>> {
        Request::builder()
            .method(Method::GET)
            .uri("http://example.com/api")
            .body(Full::new(Bytes::new()))
            .unwrap()
    }

    type Script = Arc<Mutex<Vec<(Option<String>, StatusCode)>>>;

    /// Inner mock service: returns a queued `(expected_auth, status)` pair on
    /// every call, asserting the inbound auth header matches. Tracks the
    /// number of calls.
    #[derive(Clone)]
    struct ScriptedService {
        script: Script,
        header_name: HeaderName,
        calls: Arc<AtomicUsize>,
    }

    impl ScriptedService {
        fn new(header_name: HeaderName, script: Vec<(Option<&str>, StatusCode)>) -> Self {
            let owned: Vec<(Option<String>, StatusCode)> = script
                .into_iter()
                .map(|(h, s)| (h.map(std::borrow::ToOwned::to_owned), s))
                .collect();
            Self {
                script: Arc::new(Mutex::new(owned)),
                header_name,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Service<Request<Full<Bytes>>> for ScriptedService {
        type Response = Response<Full<Bytes>>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request<Full<Bytes>>) -> Self::Future {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let actual = req
                .headers()
                .get(&self.header_name)
                .map(|v| v.to_str().unwrap().to_owned());
            let mut script = self.script.lock();
            assert!(
                !script.is_empty(),
                "ScriptedService called more times than scripted"
            );
            let (expected, status) = script.remove(0);
            assert_eq!(actual, expected, "auth header mismatch on call");
            Box::pin(async move {
                Ok(Response::builder()
                    .status(status)
                    .body(Full::new(Bytes::new()))
                    .unwrap())
            })
        }
    }

    // -- trait assertions -----------------------------------------------------

    #[test]
    fn auto_refresh_is_send_sync_clone() {
        fn assert_traits<T: Send + Sync + Clone>() {}
        assert_traits::<BearerAuthAutoRefreshLayer>();
        assert_traits::<BearerAuthAutoRefreshService<ScriptedService>>();
    }

    // -- happy path -----------------------------------------------------------

    /// A successful upstream response needs no invalidation or replay.
    #[tokio::test]
    async fn happy_path_no_401() {
        let server = MockServer::start();
        let token_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-A", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        let inner =
            ScriptedService::new(AUTHORIZATION, vec![(Some("Bearer tok-A"), StatusCode::OK)]);
        let calls = Arc::clone(&inner.calls);

        let layer = BearerAuthAutoRefreshLayer::new(token);
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry on 2xx");
        assert_eq!(token_mock.calls(), 1, "no extra token fetch on success");
    }

    // -- main refresh-on-401 path --------------------------------------------

    /// An unauthorized response triggers replacement and one authenticated replay.
    #[tokio::test]
    async fn refresh_on_401_then_success() {
        let server = MockServer::start();

        // First mock returns tok-A; we will swap it after the initial fetch.
        let mut initial_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-A", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();
        assert_eq!(initial_mock.calls(), 1, "initial fetch");

        // Swap in tok-B for the post-401 invalidate fetch.
        initial_mock.delete();
        let refreshed_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-B", 3600));
        });

        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![
                (Some("Bearer tok-A"), StatusCode::UNAUTHORIZED),
                (Some("Bearer tok-B"), StatusCode::OK),
            ],
        );
        let calls = Arc::clone(&inner.calls);

        let layer = BearerAuthAutoRefreshLayer::new(token);
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "exactly one retry");
        assert_eq!(refreshed_mock.calls(), 1, "exactly one invalidate fetch");
    }

    // -- pre-set Authorization header -> passthrough --------------------------

    /// Caller-provided authorization bypasses provider acquisition and refresh.
    #[tokio::test]
    async fn existing_authorization_header_passes_through() {
        let server = MockServer::start();
        let token_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-unused", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        assert_eq!(token_mock.calls(), 0, "construction is idle");

        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![(Some("Bearer manual"), StatusCode::UNAUTHORIZED)],
        );
        let calls = Arc::clone(&inner.calls);

        let layer = BearerAuthAutoRefreshLayer::new(token);
        let mut svc = layer.layer(inner);

        let mut req = empty_req();
        req.headers_mut()
            .insert(AUTHORIZATION, HeaderValue::from_static("Bearer manual"));
        let resp = Service::call(&mut svc, req).await.unwrap();

        // Layer must NOT retry: passthrough surfaces the inner 401 verbatim.
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "passthrough - no retry");
        assert_eq!(
            token_mock.calls(),
            0,
            "passthrough never starts acquisition"
        );
    }

    // -- token did not change after invalidate -> no retry --------------------

    /// An identical replacement credential does not trigger another upstream request.
    #[tokio::test]
    async fn unchanged_token_after_invalidate_does_not_loop() {
        let server = MockServer::start();
        // Endpoint always returns the same token value.
        let token_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-same", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();
        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![(Some("Bearer tok-same"), StatusCode::UNAUTHORIZED)],
        );
        let calls = Arc::clone(&inner.calls);

        let layer = BearerAuthAutoRefreshLayer::new(token);
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "no retry - original 401 surfaces"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "inner called exactly once");
        assert_eq!(
            token_mock.calls(),
            2,
            "initial fetch + one invalidate fetch"
        );
    }

    // -- throttle blocks burst invalidations ---------------------------------

    /// Releases all 5 inner calls only once every task has reached the
    /// barrier, so the layer sees genuinely concurrent 401s instead of the
    /// scheduling order produced by single-threaded `tokio::spawn`.
    #[derive(Clone)]
    struct GatedService {
        gate: Arc<tokio::sync::Barrier>,
        calls: Arc<AtomicUsize>,
    }

    impl Service<Request<Full<Bytes>>> for GatedService {
        type Response = Response<Full<Bytes>>;
        type Error = HttpError;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let gate = Arc::clone(&self.gate);
            Box::pin(async move {
                gate.wait().await;
                Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .body(Full::new(Bytes::new()))
                    .unwrap())
            })
        }
    }

    /// Concurrent failures respect the shared invalidation throttle.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn throttle_blocks_burst_invalidations() {
        const BURST: usize = 5;

        let server = MockServer::start();
        let token_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-burst", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();

        // All 5 inner calls block at the barrier until the last one arrives,
        // forcing the burst to be observed concurrently by the layer.
        let inner = GatedService {
            gate: Arc::new(tokio::sync::Barrier::new(BURST)),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let calls = Arc::clone(&inner.calls);

        let layer = BearerAuthAutoRefreshLayer::new(token);
        let svc = layer.layer(inner);

        let mut handles = Vec::new();
        for _ in 0..BURST {
            let mut s = svc.clone();
            handles.push(tokio::spawn(async move {
                Service::call(&mut s, empty_req()).await
            }));
        }
        for h in handles {
            let resp = h.await.unwrap().unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }

        // Every concurrent caller hits the inner exactly once. Token endpoint
        // is hit exactly twice: the initial fetch + one invalidate from the
        // single caller that wins the throttle CAS. Any other count means
        // the throttle is broken.
        assert_eq!(
            calls.load(Ordering::SeqCst),
            BURST,
            "no retries - token unchanged"
        );
        assert_eq!(
            token_mock.calls(),
            2,
            "throttle must produce exactly initial + one invalidate fetch"
        );
    }

    // -- custom predicate variants -------------------------------------------

    /// A custom refresh predicate can handle forbidden responses.
    #[tokio::test]
    async fn custom_predicate_retries_on_403() {
        let server = MockServer::start();
        let mut initial_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-A", 3600));
        });
        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();

        initial_mock.delete();
        let _refreshed_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-B", 3600));
        });

        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![
                (Some("Bearer tok-A"), StatusCode::FORBIDDEN),
                (Some("Bearer tok-B"), StatusCode::OK),
            ],
        );

        let opts = BearerAuthAutoRefreshOpts {
            should_refresh: Arc::new(|s| s == StatusCode::FORBIDDEN),
            ..BearerAuthAutoRefreshOpts::default()
        };
        let layer = BearerAuthAutoRefreshLayer::with_opts(token, opts);
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Responses outside the refresh predicate are returned unchanged.
    #[tokio::test]
    async fn custom_predicate_does_not_retry_on_500() {
        let server = MockServer::start();
        let token_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-A", 3600));
        });
        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();

        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![(Some("Bearer tok-A"), StatusCode::INTERNAL_SERVER_ERROR)],
        );
        let calls = Arc::clone(&inner.calls);

        let layer = BearerAuthAutoRefreshLayer::new(token);
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry on 500");
        assert_eq!(token_mock.calls(), 1, "no invalidate on 500");
    }

    // -- custom header name --------------------------------------------------

    /// Both initial and retried requests use the configured authorization header.
    #[tokio::test]
    async fn custom_header_name_is_used() {
        let server = MockServer::start();
        let mut initial_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-A", 3600));
        });
        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();

        initial_mock.delete();
        let _refreshed_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-B", 3600));
        });

        let custom = HeaderName::from_static("x-api-key");
        let inner = ScriptedService::new(
            custom.clone(),
            vec![
                (Some("Bearer tok-A"), StatusCode::UNAUTHORIZED),
                (Some("Bearer tok-B"), StatusCode::OK),
            ],
        );

        let opts = BearerAuthAutoRefreshOpts {
            header_name: custom,
            ..BearerAuthAutoRefreshOpts::default()
        };
        let layer = BearerAuthAutoRefreshLayer::with_opts(token, opts);
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // -- token unavailable on initial get ------------------------------------

    /// Deterministic fake `TokenProvider` used by tests that need to drive
    /// `get`/`invalidate` outcomes without waiting on a real `TokenWatcher`
    /// and its TTL clock.
    ///
    /// `next_swaps` is a queue: each `invalidate()` consumes the front
    /// entry and stores it as `current` (`None` simulates an empty cache).
    #[derive(Clone)]
    struct FakeToken {
        state: Arc<Mutex<FakeTokenState>>,
    }

    struct FakeTokenState {
        current: Option<String>,
        next_swaps: std::collections::VecDeque<Option<String>>,
        invalidate_calls: usize,
    }

    impl FakeToken {
        fn new(initial: Option<&str>, swaps: Vec<Option<&str>>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeTokenState {
                    current: initial.map(str::to_owned),
                    next_swaps: swaps.into_iter().map(|o| o.map(str::to_owned)).collect(),
                    invalidate_calls: 0,
                })),
            }
        }

        fn invalidate_calls(&self) -> usize {
            self.state.lock().invalidate_calls
        }

        fn build_layer(&self) -> BearerAuthAutoRefreshLayer {
            self.build_layer_with_opts(BearerAuthAutoRefreshOpts::default())
        }

        fn build_layer_with_opts(
            &self,
            opts: BearerAuthAutoRefreshOpts,
        ) -> BearerAuthAutoRefreshLayer {
            let s_get = Arc::clone(&self.state);
            let s_inv = Arc::clone(&self.state);
            let get_token: GetTokenFn = Arc::new(move || {
                let s = s_get.lock();
                let result = match &s.current {
                    Some(t) => Ok(SecretString::new(t)),
                    None => Err(TokenError::Unavailable("fake token unavailable".into())),
                };
                Box::pin(std::future::ready(result))
            });
            let invalidate_token: InvalidateTokenFn = Arc::new(move || {
                let state = Arc::clone(&s_inv);
                Box::pin(async move {
                    let mut s = state.lock();
                    s.invalidate_calls += 1;
                    if let Some(next) = s.next_swaps.pop_front() {
                        s.current = next;
                    }
                    Ok(())
                })
            });
            BearerAuthAutoRefreshLayer::from_fns(get_token, invalidate_token, opts)
        }
    }

    /// Acquisition failure prevents sending an unauthenticated request.
    #[tokio::test]
    async fn token_unavailable_on_first_get() {
        // Cache empty from the start - the layer must surface Transport
        // and never invoke the inner service.
        let token = FakeToken::new(None, vec![]);
        let inner = ScriptedService::new(AUTHORIZATION, vec![]);
        let calls = Arc::clone(&inner.calls);

        let layer = token.build_layer();
        let mut svc = layer.layer(inner);

        let err = Service::call(&mut svc, empty_req()).await.unwrap_err();
        assert!(
            matches!(err, HttpError::Transport(_)),
            "expected Transport error, got: {err:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "inner never invoked");
        assert_eq!(token.invalidate_calls(), 0, "no invalidate before any send");
    }

    // -- failed invalidate, token unchanged ----------------------------------

    /// Failed replacement surfaces the original response without using the discarded token.
    #[tokio::test]
    async fn failed_invalidate_surfaces_original_response() {
        let server = MockServer::start();

        // Initial token has a long TTL so the layer's first `TokenProvider::get`
        // succeeds; the failure we want to model is that the *invalidate*
        // call cannot fetch a replacement.
        let mut success_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-A", 3600));
        });
        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();

        // Swap the endpoint to fail so the layer's post-401 invalidate
        // cannot mint a new credential. Per `TokenProvider::invalidate` semantics,
        // the cached credential is discarded and the layer surfaces the
        // original 401 without retrying with the rejected credential.
        success_mock.delete();
        let fail_mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(500)
                .header("content-type", "application/json")
                .body(r#"{"error":"server_error"}"#);
        });

        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![(Some("Bearer tok-A"), StatusCode::UNAUTHORIZED)],
        );
        let calls = Arc::clone(&inner.calls);

        let layer = BearerAuthAutoRefreshLayer::new(token);
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "original 401 surfaces when invalidate fails"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "inner called exactly once");
        assert!(
            fail_mock.calls() >= 1,
            "the failed invalidate must have hit the token endpoint"
        );
    }

    // -- token unavailable after invalidate (post-401 re-read fails) --------

    #[tokio::test]
    async fn token_unavailable_after_invalidate_surfaces_original_response() {
        // Initial get returns tok-A; invalidate clears the cache so the
        // post-401 re-read fails - exercising the `get_token` Err branch
        // after invalidation.
        let token = FakeToken::new(Some("tok-A"), vec![None]);
        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![(Some("Bearer tok-A"), StatusCode::UNAUTHORIZED)],
        );
        let calls = Arc::clone(&inner.calls);

        let layer = token.build_layer();
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "original 401 surfaces when post-invalidate get returns Err"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "inner called exactly once");
        assert_eq!(token.invalidate_calls(), 1, "invalidate fired exactly once");
    }

    // -- single-retry contract: retry response is also 401 -------------------

    /// A rejected replay is returned without another refresh cycle.
    #[tokio::test]
    async fn retry_response_also_401_surfaces_without_second_invalidate() {
        // TokenProvider rotates A -> B on invalidate, but the upstream rejects
        // tok-B too. The layer must surface the second 401 verbatim and
        // must NOT fire a second invalidate from inside the same call.
        let token = FakeToken::new(Some("tok-A"), vec![Some("tok-B")]);
        let inner = ScriptedService::new(
            AUTHORIZATION,
            vec![
                (Some("Bearer tok-A"), StatusCode::UNAUTHORIZED),
                (Some("Bearer tok-B"), StatusCode::UNAUTHORIZED),
            ],
        );
        let calls = Arc::clone(&inner.calls);

        let layer = token.build_layer();
        let mut svc = layer.layer(inner);

        let resp = Service::call(&mut svc, empty_req()).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "retry's 401 surfaces verbatim - no second retry"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2, "exactly one retry");
        assert_eq!(
            token.invalidate_calls(),
            1,
            "no second invalidate after retry-401"
        );
    }

    // -- debug safety --------------------------------------------------------

    /// Debug output omits credentials and bearer values.
    #[tokio::test]
    async fn debug_does_not_reveal_tokens() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("super-secret-auto", 3600));
        });
        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();
        let layer = BearerAuthAutoRefreshLayer::new(token);
        let dbg = format!("{layer:?}");
        assert!(
            !dbg.contains("super-secret-auto"),
            "Debug must not reveal token: {dbg}"
        );
    }

    #[test]
    fn opts_debug_does_not_reveal_predicate() {
        let opts = BearerAuthAutoRefreshOpts::default();
        let dbg = format!("{opts:?}");
        // Should mention header_name and interval but no closure pointer.
        assert!(dbg.contains("min_invalidation_interval"));
        assert!(dbg.contains("header_name"));
        assert!(!dbg.contains("should_refresh"), "predicate field elided");
    }
}
