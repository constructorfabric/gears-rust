//! Per-port circuit breaker (DESIGN 03 §3.3 "Ports are bounded by deadline, breaker and
//! bulkhead": 0.5 failures over 30 s, open 10 s; D-233). Pure: time is a parameter. The
//! window is `window / BUCKETS` wide buckets of (outcomes, failures); a bucket older than the
//! window is cleared on touch. The infra wrapper owns one `Breaker` per port behind a mutex.
use std::time::{Duration, Instant};
use toolkit_macros::domain_model;

/// How many buckets the sliding window has: each is `window / 30` wide, one second at the
/// 30 s baseline.
const BUCKET_COUNT: u32 = 30;
/// `BUCKET_COUNT` as the array length (the single source of truth is `BUCKET_COUNT`).
const BUCKETS: usize = BUCKET_COUNT as usize;

/// The breaker's tunables (`gate.breaker` config; D-233).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BreakerPolicy {
    /// Failures / outcomes in the window at or above which a closed breaker opens; `(0, 1]` and
    /// at least one basis point once rounded (the breaker judges the rate in basis points).
    pub failure_rate: f32,
    /// The sliding window.
    pub window: Duration,
    /// How long an open breaker rejects before admitting one probe.
    pub open_for: Duration,
    /// Outcomes the window must hold before the rate is judged (unratified, D-233).
    pub min_calls: u32,
}

/// An out-of-range policy value.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BreakerPolicyError {
    #[error("gate.breaker.failure_rate must be in (0, 1] and round to at least one basis point")]
    FailureRate,
    #[error("gate.breaker.window_seconds must be at least 1")]
    Window,
    #[error("gate.breaker.open_seconds must be at least 1")]
    OpenFor,
    #[error("gate.breaker.min_calls must be at least 1")]
    MinCalls,
}

impl BreakerPolicy {
    /// The 03 §3.3 values with the unratified minimum volume.
    pub const BASELINE: Self = Self {
        failure_rate: 0.5,
        window: Duration::from_secs(30),
        open_for: Duration::from_secs(10),
        min_calls: 10,
    };

    /// # Errors
    /// The first out-of-range field, in declaration order. A positive `failure_rate` that rounds
    /// to 0 basis points (a tiny or subnormal value) is out of range: the breaker would judge it
    /// as 0 and open on the first `min_calls` outcomes whatever their failures.
    pub fn validate(&self) -> Result<(), BreakerPolicyError> {
        if !(self.failure_rate > 0.0 && self.failure_rate <= 1.0)
            || (f64::from(self.failure_rate) * 10_000.0).round() < 1.0
        {
            return Err(BreakerPolicyError::FailureRate);
        }
        if self.window < Duration::from_secs(1) {
            return Err(BreakerPolicyError::Window);
        }
        if self.open_for < Duration::from_secs(1) {
            return Err(BreakerPolicyError::OpenFor);
        }
        if self.min_calls == 0 {
            return Err(BreakerPolicyError::MinCalls);
        }
        Ok(())
    }
}

/// The three breaker states (metric value 0 / 2 / 1 as the OIDC plugin's gauge; D-233).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

impl BreakerState {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half-open",
        }
    }

    /// The gauge value: 0 closed, 1 half-open, 2 open.
    #[must_use]
    pub const fn gauge(self) -> u64 {
        match self {
            Self::Closed => 0,
            Self::HalfOpen => 1,
            Self::Open => 2,
        }
    }
}

/// What `admit` decided for one call.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Closed: the call proceeds and its outcome counts in the window.
    Admit,
    /// Half-open: this call is the single probe; its outcome decides the next state.
    Probe,
    /// Open, or a probe is already in flight: the call is not made.
    Reject,
}

/// How an admitted call ended, as the breaker sees it (D-233 item 2).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    start: Option<Instant>,
    outcomes: u32,
    failures: u32,
}

const EMPTY: Bucket = Bucket {
    start: None,
    outcomes: 0,
    failures: 0,
};

#[derive(Debug, Clone, Copy)]
enum Phase {
    Closed,
    Open { since: Instant },
    HalfOpen { probe_in_flight: bool },
}

/// One port's breaker.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Breaker {
    policy: BreakerPolicy,
    /// The first instant any method saw; bucket boundaries are measured from it (`Instant` has
    /// no epoch).
    origin: Option<Instant>,
    /// The latest instant any mutating method saw; an earlier `now` (a caller that read the
    /// clock before taking the lock) is clamped to it so time never runs backwards.
    last_seen: Option<Instant>,
    /// The failure rate in basis points, so the boundary is judged in integers.
    rate_bp: u32,
    buckets: [Bucket; BUCKETS],
    phase: Phase,
}

impl Breaker {
    /// `policy` must have passed [`BreakerPolicy::validate`]; an unvalidated policy (a zero
    /// window, say) degrades silently rather than failing here.
    #[must_use]
    pub fn new(policy: BreakerPolicy) -> Self {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the rate is validated to (0, 1] and scaled to basis points, so the rounded value is in 1..=10_000"
        )]
        let rate_bp = (f64::from(policy.failure_rate) * 10_000.0).round() as u32;
        Self {
            policy,
            origin: None,
            last_seen: None,
            rate_bp,
            buckets: [EMPTY; BUCKETS],
            phase: Phase::Closed,
        }
    }

    fn clamp(&self, now: Instant) -> Instant {
        self.last_seen.map_or(now, |l| now.max(l))
    }

    fn bucket_width(&self) -> Duration {
        self.policy.window / BUCKET_COUNT
    }

    /// The bucket for `now`, reset if it holds an older interval. `ordinal` counts bucket widths
    /// since `origin`; `u32::try_from(ordinal).unwrap_or(u32::MAX)` saturates only after
    /// `u32::MAX` widths, about 136 years of uptime at the 30 s baseline (one-second buckets).
    fn touch(&mut self, now: Instant) -> &mut Bucket {
        let origin = *self.origin.get_or_insert(now);
        let width = self.bucket_width();
        let since = now.saturating_duration_since(origin);
        let ordinal = since.as_nanos().checked_div(width.as_nanos()).unwrap_or(0);
        let idx = usize::try_from(ordinal % u128::from(BUCKET_COUNT)).unwrap_or(0);
        let bucket_start = origin + width * u32::try_from(ordinal).unwrap_or(u32::MAX);
        let b = &mut self.buckets[idx];
        if b.start != Some(bucket_start) {
            *b = Bucket {
                start: Some(bucket_start),
                outcomes: 0,
                failures: 0,
            };
        }
        b
    }

    /// Sum the buckets whose start lies within the window ending at `now`.
    #[must_use]
    pub fn window_counts(&self, now: Instant) -> (u32, u32) {
        let now = self.clamp(now);
        let mut outcomes = 0;
        let mut failures = 0;
        for b in &self.buckets {
            if b.start
                .is_some_and(|start| now.saturating_duration_since(start) < self.policy.window)
            {
                outcomes += b.outcomes;
                failures += b.failures;
            }
        }
        (outcomes, failures)
    }

    #[must_use]
    pub fn state(&self, now: Instant) -> BreakerState {
        let now = self.clamp(now);
        match self.phase {
            Phase::Closed => BreakerState::Closed,
            Phase::Open { since }
                if now.saturating_duration_since(since) >= self.policy.open_for =>
            {
                BreakerState::HalfOpen
            }
            Phase::Open { .. } => BreakerState::Open,
            Phase::HalfOpen { .. } => BreakerState::HalfOpen,
        }
    }

    /// Every `Probe` must end in exactly one [`Breaker::record`] or [`Breaker::abandon`];
    /// otherwise the probe slot stays taken and the breaker rejects everything.
    pub fn admit(&mut self, now: Instant) -> Admission {
        let now = self.clamp(now);
        self.last_seen = Some(now);
        match self.state(now) {
            BreakerState::Closed => Admission::Admit,
            BreakerState::Open => Admission::Reject,
            BreakerState::HalfOpen => {
                if self.probe_in_flight() {
                    Admission::Reject
                } else {
                    self.phase = Phase::HalfOpen {
                        probe_in_flight: true,
                    };
                    Admission::Probe
                }
            }
        }
    }

    /// Whether a probe holds the half-open slot: only then does a `Probe` settlement act.
    fn probe_in_flight(&self) -> bool {
        matches!(
            self.phase,
            Phase::HalfOpen {
                probe_in_flight: true
            }
        )
    }

    /// Returns the new state on a transition. A `Probe` settles only while its probe holds the
    /// half-open slot; a stale or repeated `Probe` settlement is a no-op returning `None`.
    pub fn record(
        &mut self,
        admission: Admission,
        outcome: Outcome,
        now: Instant,
    ) -> Option<BreakerState> {
        let now = self.clamp(now);
        self.last_seen = Some(now);
        match admission {
            Admission::Reject => None,
            Admission::Probe if !self.probe_in_flight() => None,
            Admission::Probe => match outcome {
                Outcome::Success => {
                    self.clear();
                    self.phase = Phase::Closed;
                    Some(BreakerState::Closed)
                }
                Outcome::Failure => {
                    self.phase = Phase::Open { since: now };
                    Some(BreakerState::Open)
                }
            },
            Admission::Admit => {
                let b = self.touch(now);
                b.outcomes += 1;
                if outcome == Outcome::Failure {
                    b.failures += 1;
                }
                let (outcomes, failures) = self.window_counts(now);
                if matches!(self.phase, Phase::Closed)
                    && outcomes >= self.policy.min_calls
                    && u64::from(failures) * 10_000 >= u64::from(self.rate_bp) * u64::from(outcomes)
                {
                    self.phase = Phase::Open { since: now };
                    return Some(BreakerState::Open);
                }
                None
            }
        }
    }

    /// Release the slot of an admission whose call ended without a counted outcome (cancelled,
    /// backpressure, panic): a `Probe` reopens the breaker from `now`, so the next probe waits
    /// another `open_for`; `Admit` and `Reject` hold nothing. Like [`Breaker::record`], a `Probe`
    /// acts only while its probe holds the half-open slot.
    pub fn abandon(&mut self, admission: Admission, now: Instant) {
        let now = self.clamp(now);
        self.last_seen = Some(now);
        if admission == Admission::Probe && self.probe_in_flight() {
            self.phase = Phase::Open { since: now };
        }
    }

    fn clear(&mut self) {
        self.buckets = [EMPTY; BUCKETS];
    }
}
