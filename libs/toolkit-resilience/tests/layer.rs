//! The breaker as a tower middleware.
#![cfg(feature = "tower")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use toolkit_resilience::circuit_breaker::{
    Bypass, CallRejected, CircuitBreaker, CircuitBreakerConfig, CircuitBreakerLayer,
    CircuitBreakerObserver, Classify, ErrorsAreFailures, Outcome, Rejected, State,
};
use tower::{Layer, Service, ServiceExt, service_fn};

#[derive(Debug, thiserror::Error)]
#[error("connection refused")]
struct Refused;

fn breaker() -> Arc<CircuitBreaker> {
    Arc::new(
        CircuitBreaker::new(
            CircuitBreakerConfig::default()
                .with_minimum_calls(2)
                .with_open_for(Duration::from_secs(5)),
        )
        .unwrap(),
    )
}

#[tokio::test(start_paused = true)]
async fn the_layer_counts_failures_and_rejects_without_calling_the_backend() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = {
        let calls = Arc::clone(&calls);
        service_fn(move |_req: ()| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(Refused) }
        })
    };
    let b = breaker();
    let mut svc = CircuitBreakerLayer::new(Arc::clone(&b), ErrorsAreFailures).layer(backend);

    for _ in 0..2 {
        let err = svc.ready().await.unwrap().call(()).await.unwrap_err();
        assert!(err.downcast_ref::<Refused>().is_some());
    }
    assert_eq!(b.state(), State::Open);

    let err = svc.ready().await.unwrap().call(()).await.unwrap_err();
    let rejected = err.downcast_ref::<Rejected>().expect("a Rejected error");
    assert_eq!(rejected.retry_after(), Duration::from_secs(5));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the open breaker sent nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn the_failure_hook_runs_for_failures_only() {
    #[derive(Clone)]
    struct Even;
    impl Classify<u32, Refused> for Even {
        fn classify(&self, r: &Result<u32, Refused>) -> Outcome {
            match r {
                Ok(n) if n % 2 == 1 => Outcome::Failure,
                Ok(_) => Outcome::Success,
                Err(_) => Outcome::Ignore,
            }
        }
    }
    impl Bypass<u32> for Even {}

    let hooks = Arc::new(AtomicUsize::new(0));
    let hook = {
        let hooks = Arc::clone(&hooks);
        Arc::new(move || {
            hooks.fetch_add(1, Ordering::SeqCst);
        })
    };
    // Default thresholds: one failure in three calls does not open it.
    let b = Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default()).unwrap());
    let mut svc = CircuitBreakerLayer::new(b, Even)
        .on_failure(hook)
        .layer(service_fn(|n: u32| async move { Ok::<_, Refused>(n) }));

    for n in [1, 2, 4] {
        svc.ready().await.unwrap().call(n).await.unwrap();
    }
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn bypassed_requests_are_neither_rejected_nor_counted() {
    #[derive(Clone)]
    struct LongPoll;
    impl<Res> Classify<Res, Refused> for LongPoll {
        fn classify(&self, r: &Result<Res, Refused>) -> Outcome {
            if r.is_ok() {
                Outcome::Success
            } else {
                Outcome::Failure
            }
        }
    }
    impl Bypass<&'static str> for LongPoll {
        fn bypass(&self, req: &&'static str) -> bool {
            *req == "watch"
        }
    }

    let b = breaker();
    let mut svc = CircuitBreakerLayer::new(Arc::clone(&b), LongPoll).layer(service_fn(
        |_req: &'static str| async { Err::<(), _>(Refused) },
    ));

    for _ in 0..5 {
        assert!(svc.ready().await.unwrap().call("watch").await.is_err());
    }
    assert_eq!(
        b.state(),
        State::Closed,
        "bypassed failures are not counted"
    );

    for _ in 0..2 {
        assert!(svc.ready().await.unwrap().call("get").await.is_err());
    }
    assert_eq!(b.state(), State::Open);
    let err = svc.ready().await.unwrap().call("watch").await.unwrap_err();
    assert!(
        err.downcast_ref::<Refused>().is_some(),
        "a bypassed call still reaches the backend"
    );
}

#[tokio::test(start_paused = true)]
async fn a_rejection_in_the_layer_reaches_the_observers() {
    let rejections = Arc::new(Rejections::default());
    let b = Arc::new(
        CircuitBreaker::new(
            CircuitBreakerConfig::default()
                .with_minimum_calls(2)
                .with_open_for(Duration::from_secs(5)),
        )
        .unwrap()
        .with_observer(Arc::clone(&rejections) as Arc<dyn CircuitBreakerObserver>),
    );
    let mut svc = CircuitBreakerLayer::new(b, ErrorsAreFailures)
        .layer(service_fn(|_req: ()| async { Err::<(), _>(Refused) }));

    for _ in 0..3 {
        assert!(svc.ready().await.unwrap().call(()).await.is_err());
    }
    assert_eq!(rejections.0.load(Ordering::SeqCst), 1);
}

/// A backend that holds callers to the `Service` contract (`call` panics
/// unless `poll_ready` said ready) and stays not ready while `up` is false.
struct Strict {
    up: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
    polled: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    reserved: bool,
}

impl Strict {
    fn new() -> Self {
        Self {
            up: Arc::new(AtomicBool::new(true)),
            waker: Arc::default(),
            polled: Arc::default(),
            calls: Arc::default(),
            reserved: false,
        }
    }

    fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::SeqCst);
        if let Some(w) = self.waker.lock().unwrap().take() {
            w.wake();
        }
    }
}

impl Clone for Strict {
    fn clone(&self) -> Self {
        Self {
            up: Arc::clone(&self.up),
            waker: Arc::clone(&self.waker),
            polled: Arc::clone(&self.polled),
            calls: Arc::clone(&self.calls),
            reserved: false,
        }
    }
}

impl Service<&'static str> for Strict {
    type Response = ();
    type Error = Refused;
    type Future = std::future::Ready<Result<(), Refused>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Refused>> {
        self.polled.fetch_add(1, Ordering::SeqCst);
        if self.up.load(Ordering::SeqCst) {
            self.reserved = true;
            Poll::Ready(Ok(()))
        } else {
            *self.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    fn call(&mut self, _req: &'static str) -> Self::Future {
        assert!(
            std::mem::take(&mut self.reserved),
            "called without poll_ready"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Err(Refused))
    }
}

#[derive(Clone)]
struct Watch;
impl<Res> Classify<Res, Refused> for Watch {
    fn classify(&self, r: &Result<Res, Refused>) -> Outcome {
        if r.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        }
    }
}
impl Bypass<&'static str> for Watch {
    fn bypass(&self, req: &&'static str) -> bool {
        *req == "watch"
    }
}

#[derive(Default)]
struct Rejections(AtomicUsize);
impl CircuitBreakerObserver for Rejections {
    fn on_rejected(&self, _e: &CallRejected<'_>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn an_open_breaker_rejects_without_waiting_for_the_backend() {
    let backend = Strict::new();
    let b = breaker();
    let mut svc = CircuitBreakerLayer::new(Arc::clone(&b), Watch).layer(backend.clone());
    for _ in 0..2 {
        assert!(svc.ready().await.unwrap().call("get").await.is_err());
    }
    assert_eq!(b.state(), State::Open);

    backend.set_up(false);
    let polled = backend.polled.load(Ordering::SeqCst);
    let ready = tokio::time::timeout(Duration::from_secs(1), svc.ready())
        .await
        .expect("ready at once, though the backend is not");
    let err = ready.unwrap().call("get").await.unwrap_err();
    assert!(err.downcast_ref::<Rejected>().is_some());
    assert_eq!(
        backend.polled.load(Ordering::SeqCst),
        polled,
        "the backend was not asked, so nothing was reserved in it"
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_bypassed_request_after_a_refusal_waits_for_the_backend() {
    let backend = Strict::new();
    let rejections = Arc::new(Rejections::default());
    let b = Arc::new(
        CircuitBreaker::new(
            CircuitBreakerConfig::default()
                .with_minimum_calls(2)
                .with_open_for(Duration::from_secs(5)),
        )
        .unwrap()
        .with_observer(Arc::clone(&rejections) as Arc<dyn CircuitBreakerObserver>),
    );
    let mut svc = CircuitBreakerLayer::new(Arc::clone(&b), Watch).layer(backend.clone());
    for _ in 0..2 {
        assert!(svc.ready().await.unwrap().call("get").await.is_err());
    }
    assert_eq!(b.state(), State::Open);

    backend.set_up(false);
    let mut call = pin!(svc.ready().await.unwrap().call("watch"));
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut call)
            .await
            .is_err(),
        "waits while the backend is not ready"
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);

    backend.set_up(true);
    let err = call.await.unwrap_err();
    assert!(
        err.downcast_ref::<Refused>().is_some(),
        "reached the backend"
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        rejections.0.load(Ordering::SeqCst),
        0,
        "the bypassed request was not reported as rejected"
    );
}

#[tokio::test(start_paused = true)]
async fn a_bypassed_request_after_an_admission_releases_the_probe_slot() {
    let backend = Strict::new();
    let b = breaker();
    let mut svc = CircuitBreakerLayer::new(Arc::clone(&b), Watch).layer(backend.clone());
    for _ in 0..2 {
        assert!(svc.ready().await.unwrap().call("get").await.is_err());
    }
    tokio::time::advance(Duration::from_secs(5)).await;

    // `poll_ready` takes the only probe slot; the bypassed request gives it
    // back, so the next request can probe.
    assert!(svc.ready().await.unwrap().call("watch").await.is_err());
    assert_eq!(b.state(), State::HalfOpen);
    let err = svc.ready().await.unwrap().call("get").await.unwrap_err();
    assert!(
        err.downcast_ref::<Refused>().is_some(),
        "the probe went out"
    );
    assert_eq!(b.state(), State::Open);
}
