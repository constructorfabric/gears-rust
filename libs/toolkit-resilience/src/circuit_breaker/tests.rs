use std::sync::Arc;
use std::time::Duration;

use super::*;

fn breaker(config: CircuitBreakerConfig) -> Arc<CircuitBreaker> {
    Arc::new(CircuitBreaker::new(config).unwrap())
}

fn small() -> CircuitBreakerConfig {
    CircuitBreakerConfig::default()
        .with_minimum_calls(4)
        .with_failure_ratio(0.5)
        .with_window(Duration::from_secs(10))
        .with_open_for(Duration::from_secs(5))
}

fn record(b: &Arc<CircuitBreaker>, outcomes: &[Outcome]) {
    for o in outcomes {
        b.try_acquire().unwrap().record(*o);
    }
}

use Outcome::{Failure as F, Ignore as I, Success as S};

#[tokio::test(start_paused = true)]
async fn the_ratio_is_not_evaluated_below_minimum_calls() {
    let b = breaker(small());
    record(&b, &[F, F, F]);
    assert_eq!(b.state(), State::Closed);

    record(&b, &[S]);
    assert_eq!(b.state(), State::Open, "3 of 4 failed");
}

#[tokio::test(start_paused = true)]
async fn a_failure_share_below_the_ratio_stays_closed() {
    let b = breaker(small());
    record(&b, &[F, S, S, S, F, S, S, S]);
    assert_eq!(b.state(), State::Closed);
}

#[tokio::test(start_paused = true)]
async fn a_failure_share_equal_to_the_ratio_opens() {
    // `0.55 * 100.0` is 55.000000000000007, so comparing failures with the
    // ratio times the calls would keep 55 of 100 closed.
    let config = CircuitBreakerConfig::default()
        .with_minimum_calls(100)
        .with_failure_ratio(0.55);

    let b = breaker(config.clone());
    record(&b, &[S; 46]);
    record(&b, &[F; 54]);
    assert_eq!(b.state(), State::Closed, "54 of 100");

    let b = breaker(config);
    record(&b, &[S; 45]);
    record(&b, &[F; 55]);
    assert_eq!(b.state(), State::Open, "55 of 100");
}

#[tokio::test(start_paused = true)]
async fn ignored_outcomes_and_dropped_permits_count_for_nothing() {
    let b = breaker(small());
    record(&b, &[F, F, I, I]);
    drop(b.try_acquire().unwrap());
    assert_eq!(b.state(), State::Closed, "only two calls were counted");
}

#[tokio::test(start_paused = true)]
async fn an_open_breaker_rejects_with_the_time_left() {
    let b = Arc::new(
        CircuitBreaker::new(small())
            .unwrap()
            .with_name("orders->payments"),
    );
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(2)).await;

    let rejected = b.try_acquire().unwrap_err();
    assert_eq!(rejected.retry_after(), Duration::from_secs(3));
    assert_eq!(rejected.name(), Some("orders->payments"));
    assert!(rejected.to_string().contains("orders->payments"));
}

#[tokio::test(start_paused = true)]
async fn after_open_for_one_probe_goes_through_and_closes_the_breaker() {
    let b = breaker(small());
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;

    let probe = b.try_acquire().unwrap();
    assert_eq!(b.state(), State::HalfOpen);
    assert!(b.try_acquire().is_err(), "the only probe slot is taken");

    probe.success();
    assert_eq!(b.state(), State::Closed);
    record(&b, &[F, F, F]);
    assert_eq!(b.state(), State::Closed, "the window restarted empty");
}

#[tokio::test(start_paused = true)]
async fn a_failed_probe_opens_the_breaker_again() {
    let b = breaker(small());
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;

    b.try_acquire().unwrap().failure();
    assert_eq!(b.state(), State::Open);
    assert_eq!(
        b.try_acquire().unwrap_err().retry_after(),
        Duration::from_secs(5)
    );
}

#[tokio::test(start_paused = true)]
async fn every_probe_must_succeed_before_closing() {
    let b = breaker(small().with_half_open_calls(2));
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;

    let (p1, p2) = (b.try_acquire().unwrap(), b.try_acquire().unwrap());
    assert!(b.try_acquire().is_err());
    p1.success();
    assert_eq!(b.state(), State::HalfOpen);
    assert!(
        b.try_acquire().is_err(),
        "a used slot is not handed out again"
    );
    p2.success();
    assert_eq!(b.state(), State::Closed);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_probe_returns_its_slot() {
    let b = breaker(small());
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;

    drop(b.try_acquire().unwrap());
    assert_eq!(b.state(), State::HalfOpen);
    b.try_acquire().unwrap().success();
    assert_eq!(b.state(), State::Closed);
}

#[tokio::test(start_paused = true)]
async fn a_late_result_from_before_a_state_change_is_ignored() {
    let b = breaker(small());
    let slow_call = b.try_acquire().unwrap();
    record(&b, &[F, F, F, F]);
    assert_eq!(b.state(), State::Open);

    // Started while closed, finished after the breaker opened.
    slow_call.success();
    assert_eq!(b.state(), State::Open);
}

#[tokio::test(start_paused = true)]
async fn a_late_probe_from_an_earlier_cycle_takes_no_slot_now() {
    let b = breaker(small().with_half_open_calls(2));
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;
    let (late, failing) = (b.try_acquire().unwrap(), b.try_acquire().unwrap());
    failing.failure();
    tokio::time::advance(Duration::from_secs(5)).await;

    let current = (b.try_acquire().unwrap(), b.try_acquire().unwrap());
    // The earlier cycle's probe finishes now: its slot goes back to the old
    // cycle and its result is stale.
    late.success();
    assert!(
        b.try_acquire().is_err(),
        "the current cycle has no spare slot"
    );
    assert_eq!(b.state(), State::HalfOpen);
    current.0.success();
    current.1.success();
    assert_eq!(b.state(), State::Closed);
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_get_exactly_the_probe_slots() {
    let b = breaker(small().with_half_open_calls(3));
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;

    let handles: Vec<_> = (0..32)
        .map(|_| {
            let b = Arc::clone(&b);
            tokio::spawn(async move { b.try_acquire().ok() })
        })
        .collect();
    let mut admitted = Vec::new();
    for h in handles {
        if let Some(p) = h.await.unwrap() {
            admitted.push(p);
        }
    }
    assert_eq!(admitted.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn failures_outside_the_window_are_forgotten() {
    let b = breaker(small());
    record(&b, &[F, F, F]);
    tokio::time::advance(Duration::from_secs(11)).await;
    record(&b, &[F, S, S, S]);
    assert_eq!(b.state(), State::Closed, "1 of 4 failed within the window");
}

#[tokio::test(start_paused = true)]
async fn consecutive_failures_trip_regardless_of_volume() {
    let b = breaker(small().with_consecutive_failures(3));
    record(&b, &[F, F, S, F, F]);
    assert_eq!(b.state(), State::Closed, "a success resets the run");
    record(&b, &[F]);
    assert_eq!(b.state(), State::Open);
}

#[tokio::test(start_paused = true)]
async fn a_disabled_breaker_never_rejects() {
    let b = breaker(CircuitBreakerConfig::disabled().with_minimum_calls(1));
    record(&b, &[F, F, F, F, F]);
    assert_eq!(b.state(), State::Closed);
    assert!(b.try_acquire().is_ok());
}

#[test]
fn invalid_configs_are_rejected() {
    let cases = [
        (small().with_failure_ratio(0.0), "failure_ratio"),
        (small().with_failure_ratio(1.5), "failure_ratio"),
        (small().with_failure_ratio(f64::NAN), "failure_ratio"),
        (small().with_minimum_calls(0), "minimum_calls"),
        (small().with_window(Duration::ZERO), "window"),
        (small().with_open_for(Duration::ZERO), "open_for"),
        (small().with_half_open_calls(0), "half_open_calls"),
        (small().with_consecutive_failures(0), "consecutive_failures"),
    ];
    for (config, field) in cases {
        let err = CircuitBreaker::new(config).unwrap_err().to_string();
        assert!(err.contains(field), "{field}: {err}");
    }
}

#[cfg(feature = "serde")]
#[test]
fn config_deserializes_with_defaults_and_humantime() {
    let c: CircuitBreakerConfig =
        serde_json::from_value(serde_json::json!({ "window": "1m", "open_for": "500ms" })).unwrap();
    assert_eq!(c.window(), Duration::from_secs(60));
    assert_eq!(c.open_for(), Duration::from_millis(500));
    assert_eq!(c.minimum_calls(), 10);
    assert!(c.enabled());

    let off: CircuitBreakerConfig =
        serde_json::from_value(serde_json::json!({ "enabled": false })).unwrap();
    assert!(!off.enabled());
}

#[cfg(feature = "serde")]
#[test]
fn config_rejects_unknown_keys_and_bad_values() {
    let typo = serde_json::from_value::<CircuitBreakerConfig>(serde_json::json!({ "open": "1s" }));
    assert!(typo.unwrap_err().to_string().contains("unknown field"));

    let bad =
        serde_json::from_value::<CircuitBreakerConfig>(serde_json::json!({ "failure_ratio": 2.0 }));
    assert!(bad.unwrap_err().to_string().contains("failure_ratio"));
}

use crate::metrics::circuit_breaker::{outcome, state};

/// Records every event as a short string.
#[derive(Default)]
struct Recorder(parking_lot::Mutex<Vec<String>>);

impl Recorder {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock())
    }
}

fn name(n: Option<&Arc<str>>) -> &str {
    n.map_or("-", |n| &**n)
}

impl CircuitBreakerObserver for Recorder {
    fn on_attach(&self, e: &Lifecycle<'_>) {
        self.0
            .lock()
            .push(format!("attach {} {}", name(e.name), state::of(e.state)));
    }

    fn on_detach(&self, e: &Lifecycle<'_>) {
        self.0
            .lock()
            .push(format!("detach {} {}", name(e.name), state::of(e.state)));
    }

    fn on_state_change(&self, e: &StateChange<'_>) {
        self.0.lock().push(format!(
            "{}: {}->{}",
            name(e.name),
            state::of(e.from),
            state::of(e.to)
        ));
    }

    fn on_call(&self, e: &CallRecorded<'_>) {
        let counted = if e.counted { "" } else { " (not counted)" };
        self.0
            .lock()
            .push(format!("call {}{counted}", outcome::of(e.outcome)));
    }

    fn on_rejected(&self, e: &CallRejected<'_>) {
        self.0.lock().push(format!(
            "rejected {} {}s",
            state::of(e.state),
            e.retry_after.as_secs()
        ));
    }
}

fn observed(config: CircuitBreakerConfig) -> (Arc<CircuitBreaker>, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let b = CircuitBreaker::new(config)
        .unwrap()
        .with_name("b")
        .with_observer(Arc::clone(&recorder));
    recorder.take(); // the attach event
    (Arc::new(b), recorder)
}

#[tokio::test(start_paused = true)]
async fn observers_see_calls_transitions_and_rejections_in_order() {
    let (b, events) = observed(small());
    record(&b, &[S, I, F, F, F]);
    assert!(b.try_acquire().is_err());
    tokio::time::advance(Duration::from_secs(5)).await;
    let probe = b.try_acquire().unwrap();
    assert!(b.try_acquire().is_err(), "the only probe slot is taken");
    probe.success();

    assert_eq!(
        events.take(),
        [
            "call success",
            "call ignored (not counted)",
            "call failure",
            "call failure",
            "call failure",
            "b: closed->open",
            "rejected open 5s",
            "b: open->half_open",
            "rejected half_open 1s",
            "call success",
            "b: half_open->closed",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_late_result_is_reported_as_not_counted() {
    let (b, events) = observed(small());
    let late = b.try_acquire().unwrap();
    record(&b, &[F, F, F, F]);
    events.take();

    late.success();
    assert_eq!(events.take(), ["call success (not counted)"]);
    assert_eq!(b.state(), State::Open);
}

#[tokio::test(start_paused = true)]
async fn dropped_permits_and_disabled_breakers_report_nothing() {
    let (b, events) = observed(small());
    drop(b.try_acquire().unwrap());
    assert!(events.take().is_empty());

    let recorder = Arc::new(Recorder::default());
    let disabled = Arc::new(CircuitBreaker::disabled().with_observer(Arc::clone(&recorder)));
    disabled.try_acquire().unwrap().failure();
    assert!(recorder.take().is_empty());
}

/// Observers run after the breaker's lock is released, so one may call back
/// into the breaker. Run on a thread with a deadline: re-locking would hang.
#[test]
fn an_observer_may_call_back_into_the_breaker() {
    struct Reentrant(std::sync::OnceLock<std::sync::Weak<CircuitBreaker>>);
    impl CircuitBreakerObserver for Reentrant {
        fn on_state_change(&self, _e: &StateChange<'_>) {
            if let Some(b) = self.0.get().and_then(std::sync::Weak::upgrade) {
                assert_eq!(b.state(), State::Open);
                assert!(b.try_acquire().is_err());
            }
        }
        fn on_rejected(&self, _e: &CallRejected<'_>) {
            if let Some(b) = self.0.get().and_then(std::sync::Weak::upgrade) {
                assert_eq!(b.state(), State::Open);
            }
        }
    }

    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let observer = Arc::new(Reentrant(std::sync::OnceLock::new()));
        let b = Arc::new(
            CircuitBreaker::new(small())
                .unwrap()
                .with_observer(Arc::clone(&observer)),
        );
        observer.0.set(Arc::downgrade(&b)).unwrap();
        record(&b, &[F, F, F, F]);
        done.send(b.state()).unwrap();
    });
    let state = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("an observer calling back into the breaker deadlocked");
    assert_eq!(state, State::Open);
}

#[tokio::test(start_paused = true)]
async fn a_failed_probe_reports_half_open_to_open() {
    let (b, events) = observed(small());
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;
    b.try_acquire().unwrap().failure();
    assert_eq!(
        &events.take()[4..],
        [
            "b: closed->open",
            "b: open->half_open",
            "call failure",
            "b: half_open->open"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn state_changes_carry_a_growing_generation() {
    #[derive(Default)]
    struct Generations(parking_lot::Mutex<Vec<u64>>);
    impl CircuitBreakerObserver for Generations {
        fn on_state_change(&self, e: &StateChange<'_>) {
            self.0.lock().push(e.generation);
        }
    }
    let generations = Arc::new(Generations::default());
    let b = Arc::new(
        CircuitBreaker::new(small())
            .unwrap()
            .with_observer(Arc::clone(&generations)),
    );
    record(&b, &[F, F, F, F]);
    tokio::time::advance(Duration::from_secs(5)).await;
    b.try_acquire().unwrap().success();
    assert_eq!(*generations.0.lock(), [1, 2, 3]);
}

#[tokio::test(start_paused = true)]
async fn attach_rename_remove_and_drop_are_reported() {
    let recorder = Arc::new(Recorder::default());
    let b = CircuitBreaker::new(small())
        .unwrap()
        .with_observer(Arc::clone(&recorder))
        .with_name("b");
    assert_eq!(
        recorder.take(),
        ["attach - closed", "detach - closed", "attach b closed"]
    );

    let b = Arc::new(b);
    record(&b, &[F, F, F, F]);
    recorder.take();
    drop(b);
    assert_eq!(recorder.take(), ["detach b open"]);

    let removed = CircuitBreaker::new(small())
        .unwrap()
        .with_observer(Arc::clone(&recorder))
        .without_observers();
    drop(removed);
    assert_eq!(recorder.take(), ["attach - closed", "detach - closed"]);
}

#[tokio::test(start_paused = true)]
async fn every_observer_gets_every_event_and_they_can_be_removed() {
    let first = Arc::new(Recorder::default());
    let second = Arc::new(Recorder::default());
    let b = Arc::new(
        CircuitBreaker::new(small())
            .unwrap()
            .with_observer(Arc::clone(&first))
            .with_observer(Arc::clone(&second)),
    );
    record(&b, &[S]);
    assert_eq!(first.take(), ["attach - closed", "call success"]);
    assert_eq!(second.take(), ["attach - closed", "call success"]);

    let quiet = Arc::new(
        CircuitBreaker::new(small())
            .unwrap()
            .with_observer(Arc::clone(&first))
            .without_observers(),
    );
    assert_eq!(first.take(), ["attach - closed", "detach - closed"]);
    record(&quiet, &[S]);
    assert!(first.take().is_empty());
}
