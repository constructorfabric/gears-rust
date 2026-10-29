#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
use bss_orders::domain::gate::breaker::{
    Admission, Breaker, BreakerPolicy, BreakerPolicyError, BreakerState, Outcome,
};
use std::time::{Duration, Instant};

fn policy() -> BreakerPolicy {
    BreakerPolicy {
        failure_rate: 0.5,
        window: Duration::from_secs(30),
        open_for: Duration::from_secs(10),
        min_calls: 10,
    }
}
fn t0() -> Instant {
    Instant::now()
}
fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

fn drive(b: &mut Breaker, now: Instant, outcomes: &[Outcome]) -> Option<BreakerState> {
    let mut last = None;
    for o in outcomes {
        let a = b.admit(now);
        assert_ne!(a, Admission::Reject, "closed breaker must admit");
        if let Some(s) = b.record(a, *o, now) {
            last = Some(s);
        }
    }
    last
}

#[test]
fn baseline_policy_is_the_design_values() {
    let p = BreakerPolicy::BASELINE;
    assert_eq!(
        (p.failure_rate, p.window, p.open_for, p.min_calls),
        (0.5, Duration::from_secs(30), Duration::from_secs(10), 10)
    );
    p.validate().unwrap();
}

/// One validation case: its name, the policy and what `validate` must answer.
type Case = (&'static str, BreakerPolicy, Result<(), BreakerPolicyError>);

/// Each out-of-range field is refused with its own variant; both durations refuse 999 ms and
/// accept 1 s, and the rate's bounds `0.0001` and `1.0` are accepted.
#[test]
fn policy_rejects_out_of_range_values() {
    use BreakerPolicyError::{FailureRate, MinCalls, OpenFor, Window};
    let ms = Duration::from_millis;
    let cases: [Case; 13] = [
        (
            "rate 0",
            BreakerPolicy {
                failure_rate: 0.0,
                ..policy()
            },
            Err(FailureRate),
        ),
        (
            "rate 1.01",
            BreakerPolicy {
                failure_rate: 1.01,
                ..policy()
            },
            Err(FailureRate),
        ),
        // Positive but 0 basis points once rounded: a subnormal and a tiny normal rate.
        (
            "rate 1e-40",
            BreakerPolicy {
                failure_rate: 1e-40,
                ..policy()
            },
            Err(FailureRate),
        ),
        (
            "rate 0.00004",
            BreakerPolicy {
                failure_rate: 0.000_04,
                ..policy()
            },
            Err(FailureRate),
        ),
        (
            "window 0",
            BreakerPolicy {
                window: Duration::ZERO,
                ..policy()
            },
            Err(Window),
        ),
        (
            "window 999 ms",
            BreakerPolicy {
                window: ms(999),
                ..policy()
            },
            Err(Window),
        ),
        (
            "open_for 0",
            BreakerPolicy {
                open_for: Duration::ZERO,
                ..policy()
            },
            Err(OpenFor),
        ),
        (
            "open_for 999 ms",
            BreakerPolicy {
                open_for: ms(999),
                ..policy()
            },
            Err(OpenFor),
        ),
        (
            "min_calls 0",
            BreakerPolicy {
                min_calls: 0,
                ..policy()
            },
            Err(MinCalls),
        ),
        (
            "window 1 s",
            BreakerPolicy {
                window: ms(1000),
                ..policy()
            },
            Ok(()),
        ),
        (
            "open_for 1 s",
            BreakerPolicy {
                open_for: ms(1000),
                ..policy()
            },
            Ok(()),
        ),
        (
            "rate 0.0001",
            BreakerPolicy {
                failure_rate: 0.000_1,
                ..policy()
            },
            Ok(()),
        ),
        (
            "rate 1.0",
            BreakerPolicy {
                failure_rate: 1.0,
                ..policy()
            },
            Ok(()),
        ),
    ];
    for (name, candidate, expected) in cases {
        assert_eq!(candidate.validate(), expected, "{name}");
    }
}

#[test]
fn stays_closed_below_minimum_volume() {
    let t = t0();
    let mut b = Breaker::new(policy());
    assert_eq!(drive(&mut b, t, &[Outcome::Failure; 9]), None);
    assert_eq!(b.state(t), BreakerState::Closed);
}

#[test]
fn opens_at_the_failure_rate_once_volume_is_reached() {
    let t = t0();
    let mut b = Breaker::new(policy());
    let mut seq = vec![Outcome::Success; 5];
    seq.extend([Outcome::Failure; 5]);
    assert_eq!(drive(&mut b, t, &seq), Some(BreakerState::Open));
    assert_eq!(b.admit(at(t, 1)), Admission::Reject);
    assert_eq!(b.window_counts(t), (10, 5));
}

#[test]
fn stays_closed_below_the_failure_rate() {
    let t = t0();
    let mut b = Breaker::new(policy());
    let mut seq = vec![Outcome::Success; 6];
    seq.extend([Outcome::Failure; 4]);
    assert_eq!(drive(&mut b, t, &seq), None);
    assert_eq!(b.state(t), BreakerState::Closed);
}

#[test]
fn failures_older_than_the_window_do_not_count() {
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 5]);
    // 31 s later the five failures have left the window; five successes and four failures make
    // nine outcomes, which is below the minimum volume, so the breaker stays closed.
    let later = at(t, 31_000);
    assert_eq!(b.window_counts(later), (0, 0));
    let mut seq = vec![Outcome::Success; 5];
    seq.extend([Outcome::Failure; 4]);
    assert_eq!(drive(&mut b, later, &seq), None, "9 outcomes: below volume");
}

#[test]
fn half_open_admits_exactly_one_probe_after_open_for() {
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 10]);
    assert_eq!(b.admit(at(t, 9_999)), Admission::Reject);
    let probe_at = at(t, 10_000);
    assert_eq!(b.state(probe_at), BreakerState::HalfOpen);
    assert_eq!(b.admit(probe_at), Admission::Probe);
    assert_eq!(
        b.admit(probe_at),
        Admission::Reject,
        "second caller during the probe"
    );
}

#[test]
fn probe_success_closes_and_clears_the_window() {
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 10]);
    let p = at(t, 10_000);
    let a = b.admit(p);
    assert_eq!(b.record(a, Outcome::Success, p), Some(BreakerState::Closed));
    assert_eq!(b.window_counts(p), (0, 0));
    assert_eq!(b.admit(p), Admission::Admit);
}

#[test]
fn probe_failure_reopens_for_another_open_for() {
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 10]);
    let p = at(t, 10_000);
    let a = b.admit(p);
    assert_eq!(b.record(a, Outcome::Failure, p), Some(BreakerState::Open));
    assert_eq!(b.admit(at(t, 19_999)), Admission::Reject);
    assert_eq!(b.admit(at(t, 20_000)), Admission::Probe);
}

#[test]
fn recording_a_rejected_admission_is_a_no_op() {
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 10]);
    assert_eq!(
        b.record(Admission::Reject, Outcome::Success, at(t, 1)),
        None
    );
    assert_eq!(b.state(at(t, 1)), BreakerState::Open);
}

#[test]
fn state_tokens_are_stable() {
    assert_eq!(BreakerState::Closed.token(), "closed");
    assert_eq!(BreakerState::Open.token(), "open");
    assert_eq!(BreakerState::HalfOpen.token(), "half-open");
}

#[test]
fn opens_at_a_non_dyadic_rate_boundary() {
    let p = BreakerPolicy {
        failure_rate: 0.3,
        ..policy()
    };
    let t = t0();
    let mut b = Breaker::new(p);
    let mut seq = vec![Outcome::Success; 7];
    seq.extend([Outcome::Failure; 3]);
    assert_eq!(drive(&mut b, t, &seq), Some(BreakerState::Open));

    let mut b = Breaker::new(p);
    let mut seq = vec![Outcome::Success; 8];
    seq.extend([Outcome::Failure; 2]);
    assert_eq!(drive(&mut b, t, &seq), None);
    assert_eq!(b.state(t), BreakerState::Closed);
}

#[test]
fn time_going_backwards_does_not_reset_live_buckets() {
    // A backwards `now` that lands on a wrapped bucket index (a full window earlier) would
    // reset the bucket holding the live outcome; the breaker clamps the clock instead.
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 5]);
    let later = at(t, 30_000);
    let a = b.admit(later);
    assert_eq!(b.record(a, Outcome::Success, later), None);
    assert_eq!(
        b.window_counts(later),
        (1, 0),
        "the five failures left the window"
    );
    let a = b.admit(t);
    assert_eq!(b.record(a, Outcome::Success, t), None);
    assert_eq!(
        b.window_counts(later),
        (2, 0),
        "the backwards call joins the live bucket"
    );
}

#[test]
fn an_abandoned_probe_reopens_and_the_next_probe_waits_open_for() {
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 10]);
    let p = at(t, 10_000);
    let a = b.admit(p);
    assert_eq!(a, Admission::Probe);
    b.abandon(a, p);
    assert_eq!(b.admit(at(t, 19_999)), Admission::Reject);
    assert_eq!(b.admit(at(t, 20_000)), Admission::Probe);
}

#[test]
fn the_smallest_basis_point_rate_is_accepted() {
    BreakerPolicy {
        failure_rate: 0.0001,
        ..policy()
    }
    .validate()
    .unwrap();
}

#[test]
fn a_stale_probe_settlement_is_ignored() {
    let t = t0();
    let mut b = Breaker::new(policy());
    drive(&mut b, t, &[Outcome::Failure; 10]);
    let p = at(t, 10_000);
    let a = b.admit(p);
    assert_eq!(a, Admission::Probe);
    assert_eq!(b.record(a, Outcome::Success, p), Some(BreakerState::Closed));
    // The probe already settled: a second settlement must not reopen the closed breaker.
    assert_eq!(b.record(Admission::Probe, Outcome::Failure, p), None);
    assert_eq!(b.state(p), BreakerState::Closed);
    b.abandon(Admission::Probe, p);
    assert_eq!(b.state(p), BreakerState::Closed);
    assert_eq!(b.admit(p), Admission::Admit);
}
