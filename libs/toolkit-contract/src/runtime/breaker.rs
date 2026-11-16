//! The circuit breaker in generated clients.
//!
//! Every remote call a generated client makes passes a
//! [`toolkit_resilience::circuit_breaker::CircuitBreakerLayer`] that sits in
//! the transport stack, once per attempt: in the `toolkit-http` stack for
//! REST, around the tonic channel for gRPC. The classifiers here decide what
//! a backend failure is, from the transport outcome alone:
//!
//! - a connect, TLS or timeout error is a failure, and so is a proxy that
//!   refused the `CONNECT` tunnel to the backend;
//! - a 502/503/504 (REST) or `UNAVAILABLE`/`DEADLINE_EXCEEDED` (gRPC) that is
//!   not a canonical Problem is a failure: a gateway or the transport said the
//!   backend is gone;
//! - everything else is a success, including a Problem with status 503: the
//!   backend answered. Counting those would open breakers up the whole call
//!   chain when one dependency deep down is out.
//!
//! An open breaker fails the call with
//! [`TransportError::CircuitOpen`](crate::runtime::transport_error::TransportError::CircuitOpen)
//! without touching the network.

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use toolkit_resilience::circuit_breaker::{
    Bypass, CircuitBreakerLayer, Classify, Outcome, Rejected,
};
use tower::{Layer, ServiceExt};

/// Marks a request the circuit breaker lets through without counting it.
///
/// Generated clients put it on methods declared `#[circuit_breaker(skip)]`,
/// such as long polls whose timeouts say nothing about the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkipCircuitBreaker;

/// Runs after an attempt the breaker classified as a backend failure. A
/// directory-resolving client uses it to look its endpoint up again.
#[derive(Clone)]
pub struct EndpointFailureHook(
    /// The callback; the resolving client hands it to the breaker layer.
    pub(crate) Arc<dyn Fn() + Send + Sync>,
);

impl EndpointFailureHook {
    /// Wrap a callback.
    pub fn new(hook: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(hook))
    }
}

impl fmt::Debug for EndpointFailureHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EndpointFailureHook")
    }
}

/// Classifies `toolkit-http` attempts. See the [module docs](self).
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpEndpointFailure;

const PROBLEM_JSON: &str = "application/problem+json";

impl<B> Classify<http::Response<B>, toolkit_http::HttpError> for HttpEndpointFailure {
    fn classify(&self, result: &Result<http::Response<B>, toolkit_http::HttpError>) -> Outcome {
        use toolkit_http::{HttpError, ProxyErrorKind};
        match result {
            Ok(resp) => {
                let gone = matches!(resp.status().as_u16(), 502..=504);
                if gone && !is_problem(resp.headers()) {
                    Outcome::Failure
                } else {
                    Outcome::Success
                }
            }
            Err(
                HttpError::Transport(_)
                | HttpError::Tls(_)
                | HttpError::Timeout(_)
                | HttpError::DeadlineExceeded(_)
                | HttpError::Proxy {
                    kind: ProxyErrorKind::TunnelRejected,
                    ..
                },
            ) => Outcome::Failure,
            Err(_) => Outcome::Ignore,
        }
    }
}

impl<B> Bypass<http::Request<B>> for HttpEndpointFailure {
    fn bypass(&self, req: &http::Request<B>) -> bool {
        req.extensions().get::<SkipCircuitBreaker>().is_some()
    }
}

fn is_problem(headers: &http::HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.trim_start().starts_with(PROBLEM_JSON))
}

type HttpInner = tower::util::BoxCloneService<
    http::Request<Full<Bytes>>,
    http::Response<toolkit_http::ResponseBody>,
    toolkit_http::HttpError,
>;

/// When an attempt reached the transport, as opposed to when the caller
/// started it.
///
/// The auth layer, the buffer and the concurrency limiter all run before the
/// transport's own timeout starts. A caller-side timer started earlier, with
/// the same length, can fire first and drop the attempt, and the breaker never
/// sees the timeout. [`bound`](Self::bound) runs the caller's timer from this
/// mark instead, so the transport's timeout always ends a slow attempt first.
#[derive(Debug, Clone, Default)]
pub(crate) struct AttemptStart(Arc<std::sync::OnceLock<tokio::time::Instant>>);

impl AttemptStart {
    /// Record now as the start. Only the first mark counts.
    fn mark(&self) {
        self.0.set(tokio::time::Instant::now()).ok();
    }

    /// Run `fut` for at most `budget` after the attempt reached the
    /// transport, or after `started` while it has not. `None` when it ran out.
    pub(crate) async fn bound<F: std::future::Future>(
        &self,
        started: tokio::time::Instant,
        budget: std::time::Duration,
        fut: F,
    ) -> Option<F::Output> {
        let deadline = || self.0.get().copied().unwrap_or(started) + budget;
        let mut fut = std::pin::pin!(fut);
        let mut armed = deadline();
        loop {
            if let Ok(out) = tokio::time::timeout_at(armed, fut.as_mut()).await {
                return Some(out);
            }
            // The attempt can reach the transport after the timer was set.
            let later = deadline();
            if later <= armed {
                return None;
            }
            armed = later;
        }
    }
}

/// Add the config's circuit breaker to a `toolkit-http` client being built,
/// as its attempt layer, behind a layer that marks each attempt's
/// [`AttemptStart`]. A config without a breaker gets the marking layer only.
///
/// Public for tests that build their own client around a custom auth layer;
/// generated clients get it from
/// [`build_default_http_client`](crate::runtime::client::build_default_http_client).
#[must_use]
pub fn install_http_breaker(
    builder: toolkit_http::HttpClientBuilder,
    config: &crate::runtime::config::ClientConfig,
) -> toolkit_http::HttpClientBuilder {
    let Some(breaker) = config.circuit_breaker.clone() else {
        return builder.with_attempt_layer(|svc: HttpInner| mark_attempt_start(svc).boxed_clone());
    };
    let mut layer = CircuitBreakerLayer::new(breaker, HttpEndpointFailure);
    if let Some(hook) = &config.on_endpoint_failure {
        layer = layer.on_failure(Arc::clone(&hook.0));
    }
    builder.with_attempt_layer(move |svc: HttpInner| {
        let guarded =
            layer
                .layer(svc)
                .map_err(|e: toolkit_resilience::circuit_breaker::BoxError| {
                    match e.downcast::<toolkit_http::HttpError>() {
                        Ok(http) => *http,
                        Err(other) => toolkit_http::HttpError::Transport(other),
                    }
                });
        mark_attempt_start(guarded).boxed_clone()
    })
}

/// Mark the request's [`AttemptStart`] when `svc` is called. `call` runs the
/// breaker and the transport's timeout setup synchronously below it, so the
/// mark and the transport timer start together.
fn mark_attempt_start<S>(svc: S) -> tower::util::MapRequest<S, fn(HttpRequest) -> HttpRequest>
where
    S: tower::Service<HttpRequest>,
{
    fn mark(req: HttpRequest) -> HttpRequest {
        if let Some(start) = req.extensions().get::<AttemptStart>() {
            start.mark();
        }
        req
    }
    svc.map_request(mark as fn(HttpRequest) -> HttpRequest)
}

type HttpRequest = http::Request<Full<Bytes>>;

/// The breaker rejection inside an error chain, if the call was refused.
pub(crate) fn find_rejected<'a>(
    err: &'a (dyn std::error::Error + 'static),
) -> Option<&'a Rejected> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(r) = e.downcast_ref::<Rejected>() {
            return Some(r);
        }
        cur = e.source();
    }
    None
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_http::HttpError;

    use std::time::Duration;

    use tokio::time::{Instant, sleep};

    const BUDGET: Duration = Duration::from_millis(100);

    #[tokio::test(start_paused = true)]
    async fn bound_runs_from_started_while_the_attempt_is_unmarked() {
        let start = AttemptStart::default();
        let started = Instant::now();

        let out = start.bound(started, BUDGET, sleep(BUDGET * 2)).await;

        assert!(out.is_none());
        assert_eq!(started.elapsed(), BUDGET);
    }

    #[tokio::test(start_paused = true)]
    async fn bound_moves_to_a_late_mark() {
        let start = AttemptStart::default();
        let started = Instant::now();
        let late = start.clone();
        let fut = async move {
            sleep(BUDGET / 2).await;
            late.mark();
            sleep(BUDGET * 7 / 10).await;
        };

        let out = start.bound(started, BUDGET, fut).await;

        assert!(
            out.is_some(),
            "the budget runs from the mark, not from the start"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_mark_after_the_budget_ran_out_is_too_late() {
        let start = AttemptStart::default();
        let started = Instant::now();
        let late = start.clone();
        let fut = async move {
            sleep(BUDGET * 3 / 2).await;
            late.mark();
        };

        let out = start.bound(started, BUDGET, fut).await;

        assert!(out.is_none());
        assert_eq!(started.elapsed(), BUDGET);
    }

    #[tokio::test(start_paused = true)]
    async fn bound_ends_a_budget_after_the_mark() {
        let start = AttemptStart::default();
        let started = Instant::now();
        let late = start.clone();
        let fut = async move {
            sleep(BUDGET / 2).await;
            late.mark();
            sleep(BUDGET * 2).await;
        };

        let out = start.bound(started, BUDGET, fut).await;

        assert!(out.is_none());
        assert_eq!(started.elapsed(), BUDGET * 3 / 2);
    }

    fn resp(status: u16, content_type: Option<&str>) -> Result<http::Response<()>, HttpError> {
        let mut b = http::Response::builder().status(status);
        if let Some(ct) = content_type {
            b = b.header(http::header::CONTENT_TYPE, ct);
        }
        b.body(()).map_err(HttpError::RequestBuild)
    }

    #[test]
    fn gateway_errors_without_a_problem_are_failures() {
        for status in [502, 503, 504] {
            assert_eq!(
                HttpEndpointFailure.classify(&resp(status, None)),
                Outcome::Failure
            );
            assert_eq!(
                HttpEndpointFailure.classify(&resp(status, Some("text/html"))),
                Outcome::Failure
            );
        }
    }

    #[test]
    fn a_problem_from_the_backend_is_an_answer() {
        let r = resp(503, Some("application/problem+json; charset=utf-8"));
        assert_eq!(HttpEndpointFailure.classify(&r), Outcome::Success);
    }

    #[test]
    fn business_statuses_and_throttling_are_answers() {
        for status in [200, 400, 404, 409, 429, 500] {
            assert_eq!(
                HttpEndpointFailure.classify(&resp(status, None)),
                Outcome::Success
            );
        }
    }

    #[test]
    fn transport_errors_are_failures_and_local_ones_are_not_counted() {
        let c = |e: HttpError| {
            Classify::<http::Response<()>, HttpError>::classify(&HttpEndpointFailure, &Err(e))
        };
        assert_eq!(c(HttpError::Transport("refused".into())), Outcome::Failure);
        assert_eq!(
            c(HttpError::Timeout(std::time::Duration::from_secs(1))),
            Outcome::Failure
        );
        assert_eq!(c(HttpError::Overloaded), Outcome::Ignore);
    }

    /// A proxy that answers every `CONNECT` with `status`, once per connection.
    async fn refusing_proxy(status: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut conn, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                drop(conn.read(&mut buf).await);
                let reply = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n");
                drop(conn.write_all(reply.as_bytes()).await);
            }
        });
        format!("http://{addr}")
    }

    async fn error_through_proxy(status: &'static str) -> HttpError {
        drop(toolkit_security::tls::configured_provider().install_default());
        let client = toolkit_http::HttpClientBuilder::new()
            .retry(None)
            .outbound_proxy(toolkit_http::ExplicitProxy::all(
                refusing_proxy(status).await,
            ))
            .build()
            .unwrap();
        client
            .get("https://backend.example/")
            .send()
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn a_refused_tunnel_is_a_failure_and_proxy_auth_is_not_counted() {
        let c = |e: HttpError| {
            Classify::<http::Response<()>, HttpError>::classify(&HttpEndpointFailure, &Err(e))
        };
        let refused = error_through_proxy("502 Bad Gateway").await;
        assert!(
            matches!(
                refused,
                HttpError::Proxy {
                    kind: toolkit_http::ProxyErrorKind::TunnelRejected,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(c(refused), Outcome::Failure);

        let auth = error_through_proxy("407 Proxy Authentication Required").await;
        assert!(
            matches!(
                auth,
                HttpError::Proxy {
                    kind: toolkit_http::ProxyErrorKind::AuthRequired,
                    ..
                }
            ),
            "{auth:?}"
        );
        assert_eq!(c(auth), Outcome::Ignore);
    }

    #[test]
    fn the_skip_marker_bypasses() {
        let plain = http::Request::new(());
        let mut marked = http::Request::new(());
        marked.extensions_mut().insert(SkipCircuitBreaker);
        assert!(!HttpEndpointFailure.bypass(&plain));
        assert!(HttpEndpointFailure.bypass(&marked));
    }
}
