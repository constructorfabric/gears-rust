//! The breaker as a tower middleware.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use pin_project_lite::pin_project;
use tower::{Layer, Service};

use super::{CircuitBreaker, Outcome, Permit, Rejected};

/// The error type of [`CircuitBreakerService`]: the inner service's error, or
/// a [`Rejected`] when the breaker refused the call.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// What a call's result says about the backend.
pub trait Classify<Res, Err> {
    /// Classify one result.
    fn classify(&self, result: &Result<Res, Err>) -> Outcome;
}

/// Which requests skip the breaker.
pub trait Bypass<Req> {
    /// `true` sends the request past the breaker: never rejected, never
    /// counted. For calls whose failures say nothing about the backend, such
    /// as long polls that are expected to time out.
    fn bypass(&self, _req: &Req) -> bool {
        false
    }
}

/// Every error is a failure, every response a success; nothing bypasses.
#[derive(Debug, Clone, Copy, Default)]
pub struct ErrorsAreFailures;

impl<Res, Err> Classify<Res, Err> for ErrorsAreFailures {
    fn classify(&self, result: &Result<Res, Err>) -> Outcome {
        if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        }
    }
}

impl<Req> Bypass<Req> for ErrorsAreFailures {}

type FailureHook = Arc<dyn Fn() + Send + Sync>;

/// Wraps a service in a [`CircuitBreaker`]. Clones of the layer and of the
/// services it builds share the one breaker.
#[derive(Clone)]
pub struct CircuitBreakerLayer<P> {
    breaker: Arc<CircuitBreaker>,
    policy: P,
    on_failure: Option<FailureHook>,
}

impl<P> CircuitBreakerLayer<P> {
    /// Guard calls with `breaker`; `policy` classifies results and picks
    /// requests that bypass the breaker.
    #[must_use]
    pub fn new(breaker: Arc<CircuitBreaker>, policy: P) -> Self {
        Self {
            breaker,
            policy,
            on_failure: None,
        }
    }

    /// Run `hook` after every call classified as a failure, for example to
    /// make a resolving client look its endpoint up again.
    #[must_use]
    pub fn on_failure(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.on_failure = Some(hook);
        self
    }
}

impl<S, P: Clone> Layer<S> for CircuitBreakerLayer<P> {
    type Service = CircuitBreakerService<S, P>;

    fn layer(&self, inner: S) -> Self::Service {
        CircuitBreakerService {
            inner,
            breaker: Arc::clone(&self.breaker),
            policy: self.policy.clone(),
            on_failure: self.on_failure.clone(),
        }
    }
}

/// A service guarded by a [`CircuitBreaker`]. See [`CircuitBreakerLayer`].
#[derive(Clone)]
pub struct CircuitBreakerService<S, P> {
    inner: S,
    breaker: Arc<CircuitBreaker>,
    policy: P,
    on_failure: Option<FailureHook>,
}

impl<S, P, Req> Service<Req> for CircuitBreakerService<S, P>
where
    S: Service<Req>,
    S::Error: Into<BoxError>,
    P: Classify<S::Response, S::Error> + Bypass<Req> + Clone,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ResponseFuture<S::Future, P>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        if self.policy.bypass(&req) {
            return ResponseFuture {
                kind: Kind::Bypass {
                    fut: self.inner.call(req),
                },
            };
        }
        match self.breaker.try_acquire() {
            Ok(permit) => ResponseFuture {
                kind: Kind::Guarded {
                    fut: self.inner.call(req),
                    permit: Some(permit),
                    policy: self.policy.clone(),
                    on_failure: self.on_failure.clone(),
                },
            },
            Err(rejected) => ResponseFuture {
                kind: Kind::Rejected { rejected },
            },
        }
    }
}

pin_project! {
    /// Future of [`CircuitBreakerService`].
    pub struct ResponseFuture<F, P> {
        #[pin]
        kind: Kind<F, P>,
    }
}

pin_project! {
    #[project = KindProj]
    enum Kind<F, P> {
        Bypass { #[pin] fut: F },
        Guarded {
            #[pin]
            fut: F,
            permit: Option<Permit>,
            policy: P,
            on_failure: Option<FailureHook>,
        },
        Rejected { rejected: Rejected },
    }
}

impl<F, P, Res, Err> Future for ResponseFuture<F, P>
where
    F: Future<Output = Result<Res, Err>>,
    Err: Into<BoxError>,
    P: Classify<Res, Err>,
{
    type Output = Result<Res, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().kind.project() {
            KindProj::Bypass { fut } => fut.poll(cx).map_err(Into::into),
            KindProj::Rejected { rejected } => Poll::Ready(Err(Box::new(rejected.clone()))),
            KindProj::Guarded {
                fut,
                permit,
                policy,
                on_failure,
            } => {
                let result = ready!(fut.poll(cx));
                let outcome = policy.classify(&result);
                if let Some(permit) = permit.take() {
                    permit.record(outcome);
                }
                if outcome == Outcome::Failure
                    && let Some(hook) = on_failure
                {
                    hook();
                }
                Poll::Ready(result.map_err(Into::into))
            }
        }
    }
}
