//! The breaker as a tower middleware.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use pin_project_lite::pin_project;
use tower::util::Oneshot;
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
            admission: None,
        }
    }
}

/// A service guarded by a [`CircuitBreaker`]. See [`CircuitBreakerLayer`].
///
/// The breaker decides in `poll_ready`, as tower's `load_shed` does. When it
/// refuses, the service reports ready without asking the inner service, so a
/// caller waiting on readiness gets [`Rejected`] at once and nothing is
/// reserved below the breaker for a call that never reaches it. When it
/// admits, the service is ready once the inner service is, and the permit is
/// held until `call`.
///
/// A request picked by [`Bypass`] arrives at `call`, after that decision. If
/// the breaker had admitted, it goes to the inner service, which is ready,
/// and the permit is released unused. If the breaker had refused, it goes to
/// a clone of the inner service, which is polled ready first.
pub struct CircuitBreakerService<S, P> {
    inner: S,
    breaker: Arc<CircuitBreaker>,
    policy: P,
    on_failure: Option<FailureHook>,
    /// The breaker's decision for the next `call`, made in `poll_ready`.
    admission: Option<Result<Permit, Rejected>>,
}

impl<S: Clone, P: Clone> Clone for CircuitBreakerService<S, P> {
    fn clone(&self) -> Self {
        // A clone has not been polled ready.
        Self {
            inner: self.inner.clone(),
            breaker: Arc::clone(&self.breaker),
            policy: self.policy.clone(),
            on_failure: self.on_failure.clone(),
            admission: None,
        }
    }
}

impl<S, P, Req> Service<Req> for CircuitBreakerService<S, P>
where
    S: Service<Req> + Clone,
    S::Error: Into<BoxError>,
    P: Classify<S::Response, S::Error> + Bypass<Req> + Clone,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ResponseFuture<S, Req, P>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let breaker = &self.breaker;
        let admission = self.admission.get_or_insert_with(|| breaker.admit());
        if admission.is_err() {
            return Poll::Ready(Ok(()));
        }
        match self.inner.poll_ready(cx) {
            Poll::Ready(Err(e)) => {
                // The permit goes back unused: the call will not be made.
                self.admission = None;
                Poll::Ready(Err(e.into()))
            }
            other => other.map_err(Into::into),
        }
    }

    fn call(&mut self, req: Req) -> Self::Future {
        // Without a `poll_ready` first the caller broke the `Service`
        // contract; decide now rather than panic.
        let admission = self
            .admission
            .take()
            .unwrap_or_else(|| self.breaker.admit());
        let kind = match (self.policy.bypass(&req), admission) {
            (true, Ok(permit)) => {
                drop(permit);
                Kind::Bypass {
                    fut: self.inner.call(req),
                }
            }
            (true, Err(_)) => Kind::BypassUnready {
                fut: Oneshot::new(self.inner.clone(), req),
            },
            (false, Ok(permit)) => Kind::Guarded {
                fut: self.inner.call(req),
                permit: Some(permit),
                policy: self.policy.clone(),
                on_failure: self.on_failure.clone(),
            },
            (false, Err(rejected)) => {
                self.breaker.report_rejected(&rejected);
                Kind::Rejected { rejected }
            }
        };
        ResponseFuture { kind }
    }
}

pin_project! {
    /// Future of [`CircuitBreakerService`].
    pub struct ResponseFuture<S, Req, P>
    where
        S: Service<Req>,
    {
        #[pin]
        kind: Kind<S, Req, P>,
    }
}

pin_project! {
    #[project = KindProj]
    enum Kind<S, Req, P>
    where
        S: Service<Req>,
    {
        Bypass { #[pin] fut: S::Future },
        BypassUnready { #[pin] fut: Oneshot<S, Req> },
        Guarded {
            #[pin]
            fut: S::Future,
            permit: Option<Permit>,
            policy: P,
            on_failure: Option<FailureHook>,
        },
        Rejected { rejected: Rejected },
    }
}

impl<S, Req, P> Future for ResponseFuture<S, Req, P>
where
    S: Service<Req>,
    S::Error: Into<BoxError>,
    P: Classify<S::Response, S::Error>,
{
    type Output = Result<S::Response, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().kind.project() {
            KindProj::Bypass { fut } => fut.poll(cx).map_err(Into::into),
            KindProj::BypassUnready { fut } => fut.poll(cx).map_err(Into::into),
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
