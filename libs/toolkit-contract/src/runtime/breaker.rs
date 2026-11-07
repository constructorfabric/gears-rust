//! The circuit breaker in generated clients.
//!
//! Every remote call a generated client makes passes a
//! [`toolkit_resilience::circuit_breaker::CircuitBreakerLayer`] that sits in
//! the transport stack, once per attempt: in the `toolkit-http` stack for
//! REST, around the tonic channel for gRPC. The classifiers here decide what
//! a backend failure is, from the transport outcome alone:
//!
//! - a connect, TLS or timeout error is a failure;
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
        use toolkit_http::HttpError;
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
                | HttpError::DeadlineExceeded(_),
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

/// Add the config's circuit breaker to a `toolkit-http` client being built,
/// as its attempt layer. A config without a breaker leaves the builder as is.
pub(crate) fn install_http_breaker(
    builder: toolkit_http::HttpClientBuilder,
    config: &crate::runtime::config::ClientConfig,
) -> toolkit_http::HttpClientBuilder {
    let Some(breaker) = config.circuit_breaker.clone() else {
        return builder;
    };
    let mut layer = CircuitBreakerLayer::new(breaker, HttpEndpointFailure);
    if let Some(hook) = &config.on_endpoint_failure {
        layer = layer.on_failure(Arc::clone(&hook.0));
    }
    builder.with_attempt_layer(move |svc: HttpInner| {
        layer
            .layer(svc)
            .map_err(|e: toolkit_resilience::circuit_breaker::BoxError| {
                match e.downcast::<toolkit_http::HttpError>() {
                    Ok(http) => *http,
                    Err(other) => toolkit_http::HttpError::Transport(other),
                }
            })
            .boxed_clone()
    })
}

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
        assert_eq!(c(HttpError::InsecureTransport), Outcome::Ignore);
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
