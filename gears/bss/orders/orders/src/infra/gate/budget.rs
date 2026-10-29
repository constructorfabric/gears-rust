//! Bounded gate port calls (F03 §3.3, D-195 checklist).
//!
//! One deadline per port per run (D-94: "per port per run, not per line"), started at the port's
//! first call in the run and shared by every later subrequest on that port and by the retry;
//! at most two attempts per call, the second only for a transient failure and only while time
//! remains, after a fully jittered pause of at most [`RETRY_PAUSE`] cut at the deadline (D-240);
//! cooperative cancellation. Per port per run (DESIGN L7005, F03 §3.3): at most
//! [`PORT_RUN_CONCURRENCY`] calls in flight, a further call *waiting* for a slot until the port's
//! run deadline, and at most [`PORT_RUN_SUBREQUEST_CAP`] subrequests. Across the gear, each port's
//! [`PORT_BULKHEAD`] is acquired without queueing: a saturated port is backpressure at once, and
//! each port's circuit breaker (F03 §3.3, D-233; `gate.breaker`) rejects a call before any
//! subrequest while it is open. The run-level ceiling is measured by the caller through
//! [`RunBudget::elapsed`]. Every call's outcome, its retry and every breaker transition are
//! reported to the budget's [`GateSignals`] sink (03 §3.8).
use super::authority::AUTHORITY_EXCHANGE_DEADLINE;
use super::signals::GateSignals;
use crate::config::{BreakerConfig, GateConfig};
use crate::domain::gate::breaker::{Admission, Breaker, BreakerState, Outcome};
use crate::domain::gate::ports::{GatePort, Unavailable};
use crate::infra::jitter::full_jitter;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

/// Concurrent calls one port admits across the gear (the per-port bulkhead; never waits).
pub const PORT_BULKHEAD: usize = 32;
/// Concurrent calls one assessment run admits on one port (DESIGN L7005: per port per run); a
/// further call waits for a slot, bounded by the port's run deadline.
pub const PORT_RUN_CONCURRENCY: usize = 8;
/// Subrequests (attempts) one assessment run may issue on one port (DESIGN L7005: per port per
/// run, not across all ports).
pub const PORT_RUN_SUBREQUEST_CAP: u32 = 200;
/// Attempts per bounded call: the first plus one retry, both under the port's run deadline.
const MAX_ATTEMPTS: u8 = 2;
/// The upper bound of the pause before a retry (D-240): the pause is fully jittered over
/// `(0, RETRY_PAUSE]` and cut at the call's deadline, so callers that failed together do not
/// retry in lockstep. 20 ms is under 10 % of the 250 ms baseline port deadline.
pub const RETRY_PAUSE: Duration = Duration::from_millis(20);
/// Number of gate ports; the per-port tables are indexed by [`slot`].
const PORTS: usize = GatePort::ALL.len();

/// The port's index in the per-port tables: its discriminant, which equals its position in
/// [`GatePort::ALL`] (that array is in declaration order; `tests/gate_ports.rs` pins it). The
/// tables have one entry per port, so indexing cannot miss.
const fn slot(port: GatePort) -> usize {
    port as usize
}

/// Per-port deadlines, bulkheads and breakers, shared by every run, and the gate's signal sink.
/// Every port has an entry by construction (fixed arrays over [`GatePort::ALL`]), so a lookup
/// cannot miss.
#[derive(Debug)]
pub struct Budget {
    deadlines: [Duration; PORTS],
    bulkheads: [Arc<Semaphore>; PORTS],
    /// The deadline of one `AuthN` exchange (`gate.authority_exchange_deadline_ms`).
    authority_exchange: Duration,
    /// One breaker per port, gear-wide (every run shares it); `None` when `gate.breaker` is
    /// disabled. Each lock is held only for one admit, settle or state read.
    breakers: Option<[parking_lot::Mutex<Breaker>; PORTS]>,
    /// Where the gate's signals go (03 §3.8).
    signals: Arc<dyn GateSignals>,
}

/// One breaker per port from validated `config` (`None` when disabled).
fn breakers(config: &BreakerConfig) -> Option<[parking_lot::Mutex<Breaker>; PORTS]> {
    let policy = config.policy();
    config
        .enabled
        .then(|| std::array::from_fn(|_| parking_lot::Mutex::new(Breaker::new(policy))))
}

impl Budget {
    /// Baseline deadlines overridden by configuration, one 32-permit bulkhead and (unless
    /// `gate.breaker.enabled` is false) one breaker per port.
    ///
    /// `config` is already validated: `OrdersConfig::validate` runs `GateConfig::validate` once
    /// at init, so a deadline outside `1..=60000` ms or an out-of-range breaker value never
    /// reaches here. Each enabled breaker's initial `closed` state is reported to `signals` once, as an initial
    /// state, not a transition.
    #[must_use]
    pub fn from_config(config: &GateConfig, signals: Arc<dyn GateSignals>) -> Self {
        let deadlines = GatePort::ALL.map(|port| {
            config.deadline_ms.get(&port).map_or_else(
                || port.baseline_deadline(),
                |ms| Duration::from_millis(u64::from(*ms)),
            )
        });
        let bulkheads = GatePort::ALL.map(|_| Arc::new(Semaphore::new(PORT_BULKHEAD)));
        let authority_exchange = config
            .authority_exchange_deadline_ms
            .map_or(AUTHORITY_EXCHANGE_DEADLINE, |ms| {
                Duration::from_millis(u64::from(ms))
            });
        let breakers = breakers(&config.breaker);
        if breakers.is_some() {
            for port in GatePort::ALL {
                signals.breaker_initial(port, BreakerState::Closed);
            }
        }
        Self {
            deadlines,
            bulkheads,
            authority_exchange,
            breakers,
            signals,
        }
    }

    /// Baseline deadlines with no configured override, one 32-permit bulkhead and one baseline
    /// breaker per port, reporting to [`NoGateSignals`](super::signals::NoGateSignals) (test
    /// composition roots only).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn baseline() -> Self {
        Self {
            deadlines: GatePort::ALL.map(GatePort::baseline_deadline),
            bulkheads: GatePort::ALL.map(|_| Arc::new(Semaphore::new(PORT_BULKHEAD))),
            authority_exchange: AUTHORITY_EXCHANGE_DEADLINE,
            breakers: breakers(&BreakerConfig::default()),
            signals: Arc::new(super::signals::NoGateSignals),
        }
    }

    /// The gate's signal sink.
    #[must_use]
    pub fn signals(&self) -> &Arc<dyn GateSignals> {
        &self.signals
    }

    /// The port's breaker state at `now`; `None` when `gate.breaker` is disabled.
    #[must_use]
    pub fn breaker_state(&self, port: GatePort, now: Instant) -> Option<BreakerState> {
        self.breakers
            .as_ref()
            .map(|b| b[slot(port)].lock().state(now.into_std()))
    }

    /// Ask the port's breaker to admit one call; [`Admission::Admit`] when disabled. A granted
    /// probe is logged and reported as the transition to half-open.
    fn admit(&self, port: GatePort, now: Instant) -> Admission {
        let Some(breakers) = &self.breakers else {
            return Admission::Admit;
        };
        let admission = breakers[slot(port)].lock().admit(now.into_std());
        if admission == Admission::Probe {
            tracing::info!(
                target: "orders.gate.breaker",
                port = port.token(),
                state = BreakerState::HalfOpen.token(),
                "bss-orders: gate breaker half-open, probing"
            );
            self.signals.breaker(port, BreakerState::HalfOpen);
        }
        admission
    }

    /// Settle one admission exactly once: `Some(outcome)` is recorded in the window (or decides
    /// a probe); `None` (a call that ended without a counted outcome: cancelled, backpressure,
    /// subrequest cap, a repeated deadline of one run) records nothing, but releases a `Probe`
    /// by reopening the breaker from `now`, so the next probe waits another `open_for`. A
    /// transition, the reopening of a released probe included, is logged at `warn` and reported.
    fn settle(&self, port: GatePort, admission: Admission, outcome: Option<Outcome>, now: Instant) {
        let Some(breakers) = &self.breakers else {
            return;
        };
        let now = now.into_std();
        let transition = {
            let mut breaker = breakers[slot(port)].lock();
            if let Some(outcome) = outcome {
                breaker.record(admission, outcome, now)
            } else {
                breaker.abandon(admission, now);
                // An abandoned probe reopens the breaker (`Breaker::abandon`).
                (admission == Admission::Probe).then_some(BreakerState::Open)
            }
        };
        if let Some(state) = transition {
            tracing::warn!(
                target: "orders.gate.breaker",
                port = port.token(),
                state = state.token(),
                "bss-orders: gate breaker changed state"
            );
            self.signals.breaker(port, state);
        }
    }

    /// The deadline of one `AuthN` client-credentials exchange (D-194; baseline
    /// [`AUTHORITY_EXCHANGE_DEADLINE`]).
    #[must_use]
    pub fn authority_exchange_deadline(&self) -> Duration {
        self.authority_exchange
    }

    /// The port's per-run deadline.
    #[must_use]
    pub fn deadline(&self, port: GatePort) -> Duration {
        self.deadlines[slot(port)]
    }
}

/// One assessment run's caps, each per port (DESIGN L7005): concurrent calls, subrequests, and
/// the port's run deadline.
#[derive(Debug)]
pub struct RunBudget {
    /// Each port's in-run concurrency limiter; a call waits for a permit until the deadline.
    in_flight: [Semaphore; PORTS],
    /// Each port's subrequests issued in this run, never past [`PORT_RUN_SUBREQUEST_CAP`].
    subrequests: [AtomicU32; PORTS],
    /// Each port's deadline instant, fixed at the port's first call in this run (D-94).
    deadlines: [OnceLock<Instant>; PORTS],
    /// Each port's first admitted `Deadline` in this run already counted for the breaker (M3-2).
    deadline_counted: [AtomicBool; PORTS],
    started: Instant,
}

impl RunBudget {
    /// A fresh run: per port 8 concurrent calls and 200 subrequests, no port deadline started.
    #[must_use]
    pub fn new() -> Self {
        Self {
            in_flight: std::array::from_fn(|_| Semaphore::new(PORT_RUN_CONCURRENCY)),
            subrequests: std::array::from_fn(|_| AtomicU32::new(0)),
            deadlines: std::array::from_fn(|_| OnceLock::new()),
            deadline_counted: std::array::from_fn(|_| AtomicBool::new(false)),
            started: Instant::now(),
        }
    }

    /// Time since the run started. The run-level ceiling is the caller's to enforce with this;
    /// [`bounded`] enforces only the per-port run deadlines.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The port's deadline in this run: started now on the port's first call, reused afterwards.
    fn port_deadline(&self, budget: &Budget, port: GatePort) -> Instant {
        *self.deadlines[slot(port)].get_or_init(|| Instant::now() + budget.deadline(port))
    }

    /// Subrequests issued so far on `port` in this run (capped at the per-port run limit).
    #[must_use]
    pub fn subrequests(&self, port: GatePort) -> u32 {
        self.subrequests[slot(port)].load(Ordering::Acquire)
    }

    /// `true` for the first call of this run to report a `Deadline` on `port`, `false` after.
    fn first_deadline(&self, port: GatePort) -> bool {
        !self.deadline_counted[slot(port)].swap(true, Ordering::AcqRel)
    }

    /// Count one subrequest on `port`; `false` once the port's cap is reached (the counter never
    /// passes it).
    fn take_subrequest(&self, port: GatePort) -> bool {
        self.subrequests[slot(port)]
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < PORT_RUN_SUBREQUEST_CAP).then_some(n + 1)
            })
            .is_ok()
    }
}

impl Default for RunBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a bounded call produced no answer.
#[derive(Debug)]
pub enum CallError {
    /// The port's deadline elapsed.
    Deadline,
    /// A transient callee failure that was not (or could not be) retried to success.
    Transient(CanonicalError),
    /// A callee failure no retry can fix.
    Permanent(CanonicalError),
    /// The port's gear-wide bulkhead had no free permit.
    Backpressure,
    /// The caller cancelled.
    Cancelled,
    /// The run already issued its 200 subrequests on the port.
    SubrequestCapExceeded,
    /// The port's breaker is open (D-233): no subrequest was spent.
    BreakerOpen,
}

/// How a bounded call's result counts for the breaker (D-233): a success or a permanent answer
/// is a success, a deadline or a transient failure is a failure, and an exit the callee did not
/// answer (backpressure, cancellation, the subrequest cap) is not counted. A `Deadline` counts
/// only when `first_deadline` (M3-2: the run's port deadline is cumulative, so the calls of one
/// run that hit it are one failure); a later one in the same run is not counted.
fn breaker_outcome<T>(result: &Result<T, CallError>, first_deadline: bool) -> Option<Outcome> {
    match result {
        Ok(_) | Err(CallError::Permanent(_)) => Some(Outcome::Success),
        Err(CallError::Deadline) => first_deadline.then_some(Outcome::Failure),
        Err(CallError::Transient(_)) => Some(Outcome::Failure),
        Err(
            CallError::Backpressure
            | CallError::Cancelled
            | CallError::SubrequestCapExceeded
            | CallError::BreakerOpen,
        ) => None,
    }
}

/// The `outcome` label of a bounded call (D-233): one variant per [`CallError`] exit plus `Ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    /// The callee answered.
    Ok,
    /// [`CallError::Deadline`].
    Deadline,
    /// [`CallError::Transient`].
    Transient,
    /// [`CallError::Permanent`].
    Permanent,
    /// [`CallError::Backpressure`].
    Backpressure,
    /// [`CallError::Cancelled`].
    Cancelled,
    /// [`CallError::SubrequestCapExceeded`].
    SubrequestCap,
    /// [`CallError::BreakerOpen`].
    BreakerOpen,
}

impl CallOutcome {
    /// Every outcome, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::Ok,
        Self::Deadline,
        Self::Transient,
        Self::Permanent,
        Self::Backpressure,
        Self::Cancelled,
        Self::SubrequestCap,
        Self::BreakerOpen,
    ];

    /// The `outcome` label value (D-233).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Deadline => "deadline",
            Self::Transient => "transient",
            Self::Permanent => "permanent",
            Self::Backpressure => "backpressure",
            Self::Cancelled => "cancelled",
            Self::SubrequestCap => "subrequest_cap",
            Self::BreakerOpen => "breaker_open",
        }
    }

    /// The outcome of a bounded call's result.
    #[must_use]
    pub fn of<T>(result: &Result<T, CallError>) -> Self {
        match result {
            Ok(_) => Self::Ok,
            Err(CallError::Deadline) => Self::Deadline,
            Err(CallError::Transient(_)) => Self::Transient,
            Err(CallError::Permanent(_)) => Self::Permanent,
            Err(CallError::Backpressure) => Self::Backpressure,
            Err(CallError::Cancelled) => Self::Cancelled,
            Err(CallError::SubrequestCapExceeded) => Self::SubrequestCap,
            Err(CallError::BreakerOpen) => Self::BreakerOpen,
        }
    }
}

/// Whether a callee error is worth one retry: unavailable, callee deadline, internal, or an abort
/// that carries no business code. "No business code" means an empty `ctx.reason`; an `Aborted`
/// with a reason is a business answer. The SDK builder always sets a reason, so in practice a
/// locally built `Aborted` is never transient; the branch stays for wire-decoded errors.
fn is_transient(e: &CanonicalError) -> bool {
    match e {
        CanonicalError::ServiceUnavailable { .. }
        | CanonicalError::DeadlineExceeded { .. }
        | CanonicalError::Internal { .. } => true,
        CanonicalError::Aborted { ctx, .. } => ctx.reason.trim().is_empty(),
        _ => false,
    }
}

/// Run `call` under the port's budget.
///
/// # Errors
/// [`CallError`] as documented on its variants. The deadline is the port's run deadline: it
/// starts at the port's first call in `run` and covers every later call on the port in the
/// same run, the wait for an in-run slot and both attempts of this call; the retry is taken at
/// most once, only for a transient failure, after a jittered pause of at most [`RETRY_PAUSE`]
/// cut at the deadline, and only while the deadline has not passed ([`retry_transient`]).
///
/// The order is: cancellation, the in-run slot, the gear-wide bulkhead, the breaker, the
/// attempts. The in-run slot is awaited (a deadline that passes while waiting is
/// [`CallError::Deadline`]); the gear-wide port bulkhead is then `try_acquire`d, so a saturated
/// port is [`CallError::Backpressure`] at once, never a queue; an open breaker is
/// [`CallError::BreakerOpen`] before any subrequest. An admitted call settles its breaker
/// admission exactly once, on the attempts' result ([`breaker_outcome`]), or as not counted
/// when the call is dropped before it ends.
///
/// One deadline per run per port counts for the breaker (M3-2): the port's run deadline is
/// cumulative, so concurrent calls of one run that all hit it are one failure, not one per call.
/// The first admitted call of the run to end in [`CallError::Deadline`] on the port records a
/// failure; a later one on the same port in the same run is settled as not counted (a probe is
/// released, which reopens the breaker as a failed probe would). A call that finds the deadline
/// already passed, or passes it waiting for an in-run slot, is never admitted and never counted.
///
/// Every call, admitted or not, is reported once to the budget's signals as
/// [`GateSignals::port_call`] with its [`CallOutcome`] and, when an attempt was started, its whole
/// duration from entry (the retry included); the second attempt as [`GateSignals::port_retry`]. A call dropped before it
/// ends is reported as `cancelled`. Every call is counted; the duration is reported only for a
/// call that started an attempt.
///
/// `outcome="deadline"` is one token for two situations: an admitted attempt that timed out, and
/// a call that found the run's port deadline already elapsed (or passed it waiting for an
/// in-run slot) and was never admitted. The whole-call duration is reported only for a call that
/// started an attempt, so the latter has none ([`GateSignals::port_call`]).
pub async fn bounded<T, F, Fut>(
    budget: &Budget,
    run: &RunBudget,
    port: GatePort,
    cancel: &CancellationToken,
    call: F,
) -> Result<T, CallError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, CanonicalError>>,
{
    let attempted = AtomicBool::new(false);
    let report = Unreported {
        signals: budget.signals.as_ref(),
        port,
        started: Instant::now(),
        attempted: &attempted,
        outcome: CallOutcome::Cancelled,
    };
    let result = admitted(budget, run, port, cancel, &attempted, call).await;
    report.finish(CallOutcome::of(&result));
    result
}

/// A bounded call's report: sent on [`Unreported::finish`] with the result's outcome, or on drop
/// as `cancelled` (the `bounded` future dropped mid-call, or a callee panic unwinding through
/// it: the panic is reported as `cancelled`). The duration is sent only when `attempted` was set
/// by the first attempt.
struct Unreported<'a> {
    signals: &'a dyn GateSignals,
    port: GatePort,
    started: Instant,
    attempted: &'a AtomicBool,
    outcome: CallOutcome,
}

impl Unreported<'_> {
    fn finish(mut self, outcome: CallOutcome) {
        self.outcome = outcome;
    }
}

impl Drop for Unreported<'_> {
    fn drop(&mut self) {
        let duration = self
            .attempted
            .load(Ordering::Acquire)
            .then(|| self.started.elapsed());
        self.signals.port_call(self.port, self.outcome, duration);
    }
}

/// [`bounded`] without its report: the discipline itself.
async fn admitted<T, F, Fut>(
    budget: &Budget,
    run: &RunBudget,
    port: GatePort,
    cancel: &CancellationToken,
    attempted: &AtomicBool,
    call: F,
) -> Result<T, CallError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, CanonicalError>>,
{
    if cancel.is_cancelled() {
        return Err(CallError::Cancelled);
    }
    let deadline = run.port_deadline(budget, port);
    if Instant::now() >= deadline {
        // The port's run deadline passed on an earlier call: no subrequest is spent.
        return Err(CallError::Deadline);
    }
    // The in-run slot first: a waiting call holds no gear-wide permit.
    let _run_permit = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(CallError::Cancelled),
        r = tokio::time::timeout_at(deadline, run.in_flight[slot(port)].acquire()) => match r {
            Err(_elapsed) => return Err(CallError::Deadline),
            // The run's semaphores are never closed.
            Ok(Err(_closed)) => return Err(CallError::Backpressure),
            Ok(Ok(permit)) => permit,
        },
    };
    let _port_permit = Arc::clone(&budget.bulkheads[slot(port)])
        .try_acquire_owned()
        .map_err(|_| CallError::Backpressure)?;
    let admission = budget.admit(port, Instant::now());
    if admission == Admission::Reject {
        return Err(CallError::BreakerOpen);
    }
    let pending = Unsettled {
        budget,
        port,
        admission: Some(admission),
    };
    let result = attempts(budget, run, port, cancel, deadline, attempted, &call).await;
    // Only an admitted `Deadline` claims the run's one counted deadline on the port.
    let first_deadline = matches!(result, Err(CallError::Deadline)) && run.first_deadline(port);
    pending.settle(breaker_outcome(&result, first_deadline));
    result
}

/// An admission not yet settled. [`Unsettled::settle`] settles it on the attempts' result; a
/// call that never reaches it (the `bounded` future dropped mid-call, or a panicking callee)
/// settles it on drop as not counted, so a probe is always released.
struct Unsettled<'a> {
    budget: &'a Budget,
    port: GatePort,
    admission: Option<Admission>,
}

impl Unsettled<'_> {
    fn settle(mut self, outcome: Option<Outcome>) {
        if let Some(admission) = self.admission.take() {
            self.budget
                .settle(self.port, admission, outcome, Instant::now());
        }
    }
}

impl Drop for Unsettled<'_> {
    fn drop(&mut self) {
        if let Some(admission) = self.admission.take() {
            self.budget
                .settle(self.port, admission, None, Instant::now());
        }
    }
}

/// The attempts of one admitted call under the port's run deadline (see [`bounded`]): each
/// attempt spends one of the run's subrequests on the port, and the second is reported as a
/// retry.
async fn attempts<T, F, Fut>(
    budget: &Budget,
    run: &RunBudget,
    port: GatePort,
    cancel: &CancellationToken,
    deadline: Instant,
    attempted: &AtomicBool,
    call: &F,
) -> Result<T, CallError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, CanonicalError>>,
{
    retry_transient(deadline, cancel, call, |attempt| {
        if !run.take_subrequest(port) {
            return Err(CallError::SubrequestCapExceeded);
        }
        if attempt > 1 {
            budget.signals.port_retry(port);
        }
        attempted.store(true, Ordering::Release);
        Ok(())
    })
    .await
}

/// The one retry loop of gate ports and acceptance commands: at most [`MAX_ATTEMPTS`] attempts
/// of `call` under `deadline`, the second only after a transient failure that left time, and
/// never after a deadline. Before the retry it waits a fully jittered pause of at most
/// [`RETRY_PAUSE`] (the `toolkit-contract` `retry_with_backoff` and `toolkit-transport-grpc`
/// `rpc_retry` full jitter), cut at `deadline`; a retry the pause brought to the deadline is not
/// taken. `before(attempt)` runs just before each attempt (1-based) and may refuse it.
/// Cancellation wins over the call and over the pause.
async fn retry_transient<T, F, Fut>(
    deadline: Instant,
    cancel: &CancellationToken,
    call: &F,
    mut before: impl FnMut(u8) -> Result<(), CallError>,
) -> Result<T, CallError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, CanonicalError>>,
{
    let mut attempt: u8 = 0;
    loop {
        attempt += 1;
        before(attempt)?;
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(CallError::Cancelled),
            r = tokio::time::timeout_at(deadline, call()) => r,
        };
        let error = match outcome {
            Err(_elapsed) => return Err(CallError::Deadline),
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(error)) => error,
        };
        if !is_transient(&error) {
            return Err(CallError::Permanent(error));
        }
        let now = Instant::now();
        if attempt >= MAX_ATTEMPTS || now >= deadline {
            return Err(CallError::Transient(error));
        }
        let pause = full_jitter(RETRY_PAUSE).min(deadline.saturating_duration_since(now));
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(CallError::Cancelled),
            () = tokio::time::sleep(pause) => {}
        }
        if Instant::now() >= deadline {
            return Err(CallError::Transient(error));
        }
    }
}

/// The baseline deadline of one Pricing acceptance command (submit phase B, M2-6):
/// `gate.acceptance_deadline_ms` overrides it.
pub const ACCEPTANCE_DEADLINE: Duration = Duration::from_millis(500);

/// The budget of submit phase B's Pricing acceptance commands (Pricing's `SellabilityV1` `check` command, M2-6).
/// Not a gate port: [`GatePort::ALL`] and the per-run tables are unchanged. Each line command has
/// its own deadline covering both attempts, retries once only for a transient failure while time
/// remains (after the same jittered pause as a gate port, D-240), is cancel-aware and takes a
/// gear-wide bulkhead permit without queueing. A retry is logged at `warn` with the line and its
/// command key; it has no instrument (D-240: D-233's instrument table has no acceptance row).
#[derive(Debug, Clone)]
pub struct AcceptanceBudget {
    deadline: Duration,
    bulkhead: Arc<Semaphore>,
}

impl AcceptanceBudget {
    /// The configured deadline (baseline [`ACCEPTANCE_DEADLINE`]) and a [`PORT_BULKHEAD`]-permit
    /// bulkhead. `config` is already validated (no zero deadline).
    #[must_use]
    pub fn from_config(config: &GateConfig) -> Self {
        Self::with_deadline(
            config
                .acceptance_deadline_ms
                .map_or(ACCEPTANCE_DEADLINE, |ms| {
                    Duration::from_millis(u64::from(ms))
                }),
        )
    }

    /// A budget with `deadline` per command.
    #[must_use]
    pub fn with_deadline(deadline: Duration) -> Self {
        Self {
            deadline,
            bulkhead: Arc::new(Semaphore::new(PORT_BULKHEAD)),
        }
    }

    /// The per-command deadline.
    #[must_use]
    pub fn deadline(&self) -> Duration {
        self.deadline
    }

    /// Run `line_id`'s acceptance command, whose command key is `key`, under the budget (type
    /// doc).
    ///
    /// # Errors
    /// [`CallError`]: the deadline, a transient failure not retried to success, a permanent
    /// callee failure (a business refusal included), backpressure or cancellation.
    pub async fn bounded<T, F, Fut>(
        &self,
        line_id: Uuid,
        key: &str,
        cancel: &CancellationToken,
        call: F,
    ) -> Result<T, CallError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, CanonicalError>>,
    {
        if cancel.is_cancelled() {
            return Err(CallError::Cancelled);
        }
        let deadline = Instant::now() + self.deadline;
        let _permit = Arc::clone(&self.bulkhead)
            .try_acquire_owned()
            .map_err(|_| CallError::Backpressure)?;
        retry_transient(deadline, cancel, &call, |attempt| {
            if attempt > 1 {
                tracing::warn!(
                    target: "orders.gate.submit",
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    line_id = %line_id,
                    key,
                    "bss-orders: retrying a transient acceptance command"
                );
            }
            Ok(())
        })
        .await
    }
}

/// The typed unavailability a failed bounded call becomes.
#[must_use]
pub fn to_unavailable(e: &CallError) -> Unavailable {
    match e {
        CallError::Deadline => Unavailable::Deadline,
        CallError::Permanent(CanonicalError::PermissionDenied { .. }) => Unavailable::Denied,
        CallError::Permanent(CanonicalError::InvalidArgument { .. }) => Unavailable::Malformed,
        CallError::Permanent(_)
        | CallError::Transient(_)
        | CallError::Backpressure
        | CallError::Cancelled
        | CallError::SubrequestCapExceeded
        | CallError::BreakerOpen => Unavailable::Outage,
    }
}
