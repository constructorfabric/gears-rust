//! The breaker as a tower middleware.
#![cfg(feature = "tower")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use toolkit_resilience::circuit_breaker::{
    Bypass, CircuitBreaker, CircuitBreakerConfig, CircuitBreakerLayer, Classify, ErrorsAreFailures,
    Outcome, Rejected, State,
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
