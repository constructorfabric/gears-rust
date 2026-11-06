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
