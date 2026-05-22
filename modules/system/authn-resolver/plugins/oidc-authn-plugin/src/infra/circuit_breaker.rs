//! Circuit breaker for Oidc network calls.
//!
//! The breaker uses lock-free atomics for the hot path and a `parking_lot::Mutex`-protected
//! timestamp for reset timing. It enforces fail-closed semantics:
//! - Open circuit fails fast with `IdpUnreachable`.
//! - Half-open allows exactly one probe request.
//! - Closed counts consecutive `IdP` failures until threshold is reached.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;

use tracing::debug;

use crate::domain::error::AuthNError;
use crate::domain::metrics::AuthNMetrics;

/// RAII guard that resets an `AtomicBool` flag to `false` on drop, ensuring
/// the half-open probe slot is released even if the wrapped future panics.
struct ProbeGuard<'a>(&'a AtomicBool);

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Closed state code.
pub const STATE_CLOSED: u8 = 0;
/// Half-open state code.
pub const STATE_HALF_OPEN: u8 = 1;
/// Open state code.
pub const STATE_OPEN: u8 = 2;

/// Alias for the lock-free breaker state register.
pub type CircuitBreakerState = AtomicU8;

/// Normalize a URL-like identity into a host-scoped breaker key.
#[must_use]
pub(crate) fn host_key(endpoint: &reqwest::Url) -> String {
    endpoint.host_str().map_or_else(
        || endpoint.as_str().to_owned(),
        |host| match endpoint.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        },
    )
}

/// In-memory, per-process circuit breaker for IdP-facing operations.
pub struct CircuitBreaker {
    host: String,
    pub(crate) state: CircuitBreakerState,
    pub(crate) failure_count: AtomicU32,
    pub(crate) opened_at: Mutex<Option<Instant>>,
    failure_threshold: u32,
    reset_timeout: Duration,
    half_open_probe_in_flight: AtomicBool,
    /// Injected metrics handle for recording breaker state transitions.
    metrics: Arc<AuthNMetrics>,
}

impl std::fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("host", &self.host)
            .field("state", &self.state.load(Ordering::Relaxed))
            .field("failure_count", &self.failure_count.load(Ordering::Relaxed))
            .field("failure_threshold", &self.failure_threshold)
            .field("reset_timeout", &self.reset_timeout)
            .finish_non_exhaustive()
    }
}

impl CircuitBreaker {
    /// Create a new circuit breaker with host key, threshold, reset timeout, and metrics handle.
    #[must_use]
    pub fn new(
        host: impl Into<String>,
        failure_threshold: u32,
        reset_timeout_secs: u64,
        metrics: Arc<AuthNMetrics>,
    ) -> Self {
        let clamped_threshold = failure_threshold.max(1);

        if clamped_threshold != failure_threshold {
            debug!(
                configured = failure_threshold,
                clamped = clamped_threshold,
                "circuit breaker failure_threshold clamped to minimum of 1"
            );
        }

        let breaker = Self {
            host: host.into(),
            state: AtomicU8::new(STATE_CLOSED),
            failure_count: AtomicU32::new(0),
            opened_at: Mutex::new(None),
            failure_threshold: clamped_threshold,
            reset_timeout: Duration::from_secs(reset_timeout_secs),
            half_open_probe_in_flight: AtomicBool::new(false),
            metrics,
        };
        breaker.record_metrics_for_state(STATE_CLOSED);
        breaker
    }

    /// Returns the current breaker state code.
    #[must_use]
    pub fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    /// Execute an IdP-touching operation under breaker protection.
    ///
    /// # Errors
    /// Returns `AuthNError::IdpUnreachable` when the circuit rejects execution.
    pub async fn call<F, Fut, T>(&self, f: F) -> Result<T, AuthNError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, AuthNError>>,
    {
        match self.state() {
            STATE_OPEN => {
                if !self.reset_timeout_elapsed() {
                    return Err(AuthNError::IdpUnreachable);
                }
                // CAS ensures only one caller wins the OPEN->HALF_OPEN
                // transition. Losers still attempt the probe path; the CAS
                // inside `execute_half_open_probe` serializes them.
                if self
                    .state
                    .compare_exchange(
                        STATE_OPEN,
                        STATE_HALF_OPEN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    self.record_metrics_for_state(STATE_HALF_OPEN);
                }
                self.execute_half_open_probe(f).await
            }
            STATE_HALF_OPEN => self.execute_half_open_probe(f).await,
            _ => self.execute_closed_request(f).await,
        }
    }

    async fn execute_closed_request<F, Fut, T>(&self, f: F) -> Result<T, AuthNError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, AuthNError>>,
    {
        let result = f().await;
        match &result {
            Ok(_) => {
                self.failure_count.store(0, Ordering::Release);
            }
            Err(error) if error.is_idp_failure() => {
                let failures = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;
                if failures >= self.failure_threshold {
                    self.transition_to_open();
                }
            }
            Err(_) => {
                // Non-IdP errors are neutral: they neither increment nor reset
                // the failure count, so intermittent validation errors cannot
                // mask sustained IdP degradation.
            }
        }
        result
    }

    async fn execute_half_open_probe<F, Fut, T>(&self, f: F) -> Result<T, AuthNError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, AuthNError>>,
    {
        if self
            .half_open_probe_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(AuthNError::IdpUnreachable);
        }

        // RAII guard ensures the flag is reset even if `f().await` panics,
        // preventing the breaker from permanently rejecting half-open probes.
        let _guard = ProbeGuard(&self.half_open_probe_in_flight);

        let result = f().await;

        match &result {
            Ok(_) => self.transition_to_closed(),
            Err(error) if error.is_idp_failure() => self.transition_to_open(),
            Err(_) => {
                // Token validation failure is not an IdP outage; connectivity recovered.
                self.transition_to_closed();
            }
        }

        result
    }

    fn reset_timeout_elapsed(&self) -> bool {
        let guard = self.opened_at.lock();
        guard
            .as_ref()
            .is_some_and(|opened_at| opened_at.elapsed() >= self.reset_timeout)
    }

    fn transition_to_open(&self) {
        self.state.store(STATE_OPEN, Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
        self.half_open_probe_in_flight
            .store(false, Ordering::Release);
        *self.opened_at.lock() = Some(Instant::now());
        self.record_metrics_for_state(STATE_OPEN);
    }

    fn transition_to_closed(&self) {
        self.state.store(STATE_CLOSED, Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
        self.half_open_probe_in_flight
            .store(false, Ordering::Release);
        *self.opened_at.lock() = None;
        self.record_metrics_for_state(STATE_CLOSED);
        self.metrics.increment_circuit_breaker_closed(&self.host);
    }

    fn record_metrics_for_state(&self, state: u8) {
        self.metrics
            .set_circuit_breaker_state(&self.host, f64::from(state));
        let idp_up = if state == STATE_CLOSED { 1.0 } else { 0.0 };
        self.metrics.set_idp_up(&self.host, idp_up);
    }
}

/// Host-scoped circuit-breaker registry for outbound identity dependencies.
pub struct HostCircuitBreakers {
    breakers: DashMap<String, Arc<CircuitBreaker>>,
    failure_threshold: u32,
    reset_timeout_secs: u64,
    metrics: Arc<AuthNMetrics>,
}

impl std::fmt::Debug for HostCircuitBreakers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCircuitBreakers")
            .field("hosts", &self.breakers.len())
            .field("failure_threshold", &self.failure_threshold)
            .field("reset_timeout_secs", &self.reset_timeout_secs)
            .finish_non_exhaustive()
    }
}

impl HostCircuitBreakers {
    /// Create a host-scoped breaker registry from the configured breaker policy.
    #[must_use]
    pub fn new(
        failure_threshold: u32,
        reset_timeout_secs: u64,
        metrics: Arc<AuthNMetrics>,
    ) -> Self {
        Self {
            breakers: DashMap::new(),
            failure_threshold,
            reset_timeout_secs,
            metrics,
        }
    }

    /// Execute an IdP-touching operation under the breaker for the given host.
    ///
    /// # Errors
    /// Returns `AuthNError::IdpUnreachable` when that host's breaker rejects execution.
    pub async fn call<F, Fut, T>(&self, host: &str, f: F) -> Result<T, AuthNError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, AuthNError>>,
    {
        self.breaker_for(host).call(f).await
    }

    fn breaker_for(&self, host: &str) -> Arc<CircuitBreaker> {
        self.breakers
            .entry(host.to_owned())
            .or_insert_with(|| {
                Arc::new(CircuitBreaker::new(
                    host,
                    self.failure_threshold,
                    self.reset_timeout_secs,
                    Arc::clone(&self.metrics),
                ))
            })
            .clone()
    }

    /// Return the current state for a host if its breaker has been created.
    #[cfg(test)]
    pub(crate) fn state_for_host(&self, host: &str) -> Option<u8> {
        self.breakers.get(host).map(|breaker| breaker.state())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::oneshot;

    use crate::domain::metrics::test_harness::MetricsHarness;

    use super::*;

    fn breaker(failure_threshold: u32, reset_timeout_secs: u64) -> CircuitBreaker {
        CircuitBreaker::new(
            "test-idp.example.com",
            failure_threshold,
            reset_timeout_secs,
            MetricsHarness::new().metrics(),
        )
    }

    #[test]
    fn host_key_uses_host_and_port_without_path_query_or_fragment() {
        let with_port =
            reqwest::Url::parse("https://idp.example.com:8443/realms/a?tenant_secret=x#frag")
                .expect("test URL with port should parse");
        let without_port = reqwest::Url::parse("https://idp.example.com/realms/b")
            .expect("test URL without port should parse");

        assert_eq!(host_key(&with_port), "idp.example.com:8443");
        assert_eq!(host_key(&without_port), "idp.example.com");
    }

    #[tokio::test]
    async fn host_registry_isolates_open_breaker_by_host() {
        let breakers = HostCircuitBreakers::new(1, 30, MetricsHarness::new().metrics());

        let first = breakers
            .call("idp-a.example.com", || async {
                Err::<(), _>(AuthNError::IdpUnreachable)
            })
            .await;
        assert!(matches!(first, Err(AuthNError::IdpUnreachable)));
        assert_eq!(
            breakers.state_for_host("idp-a.example.com"),
            Some(STATE_OPEN)
        );

        let unaffected = breakers
            .call("idp-b.example.com", || async { Ok::<_, AuthNError>(()) })
            .await;
        assert!(
            unaffected.is_ok(),
            "opening one host must not reject a different host"
        );
        assert_eq!(
            breakers.state_for_host("idp-b.example.com"),
            Some(STATE_CLOSED)
        );
    }

    #[tokio::test]
    async fn host_registry_records_metrics_per_host() {
        let harness = MetricsHarness::new();
        let breakers = HostCircuitBreakers::new(1, 30, harness.metrics());

        drop(
            breakers
                .call("idp-a.example.com", || async {
                    Err::<(), _>(AuthNError::IdpUnreachable)
                })
                .await,
        );
        let unaffected = breakers
            .call("idp-b.example.com", || async { Ok::<_, AuthNError>(()) })
            .await;
        assert!(unaffected.is_ok());

        harness.force_flush();
        assert_eq!(
            harness.gauge_value(
                crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE,
                &[("host", "idp-a.example.com")]
            ),
            Some(2.0)
        );
        assert_eq!(
            harness.gauge_value(
                crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE,
                &[("host", "idp-b.example.com")]
            ),
            Some(0.0)
        );
        assert_eq!(
            harness.gauge_value(
                crate::domain::metrics::AUTHN_IDP_UP,
                &[("host", "idp-a.example.com")]
            ),
            Some(0.0)
        );
        assert_eq!(
            harness.gauge_value(
                crate::domain::metrics::AUTHN_IDP_UP,
                &[("host", "idp-b.example.com")]
            ),
            Some(1.0)
        );
    }

    #[tokio::test]
    async fn closed_to_open_after_threshold_failures() {
        let breaker = breaker(2, 30);

        let first = breaker
            .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
            .await;
        assert!(matches!(first, Err(AuthNError::IdpUnreachable)));
        assert_eq!(breaker.state(), STATE_CLOSED);

        let second = breaker
            .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
            .await;
        assert!(matches!(second, Err(AuthNError::IdpUnreachable)));
        assert_eq!(breaker.state(), STATE_OPEN);
    }

    #[tokio::test]
    async fn open_transitions_to_half_open_after_timeout() {
        let breaker = Arc::new(CircuitBreaker::new(
            "test-idp.example.com",
            1,
            0,
            MetricsHarness::new().metrics(),
        ));
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_OPEN);

        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (entered_tx, entered_rx) = oneshot::channel::<()>();

        let cloned = breaker.clone();
        let join = tokio::spawn(async move {
            cloned
                .call(|| async move {
                    let _entered = entered_tx.send(());
                    drop(release_rx.await);
                    Ok::<(), AuthNError>(())
                })
                .await
        });

        // Ensure probe closure has started and breaker is in half-open.
        drop(entered_rx.await);
        assert_eq!(breaker.state(), STATE_HALF_OPEN);
        let _released = release_tx.send(());
        let result = join.await.expect("probe task should join");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn half_open_success_closes_circuit() {
        let breaker = breaker(1, 0);
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_OPEN);

        let probe = breaker.call(|| async { Ok::<_, AuthNError>(()) }).await;
        assert!(probe.is_ok());
        assert_eq!(breaker.state(), STATE_CLOSED);
    }

    #[tokio::test]
    async fn half_open_failure_reopens_circuit() {
        let breaker = breaker(1, 0);
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_OPEN);

        let probe = breaker
            .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
            .await;
        assert!(matches!(probe, Err(AuthNError::IdpUnreachable)));
        assert_eq!(breaker.state(), STATE_OPEN);
    }

    #[tokio::test]
    async fn half_open_allows_only_one_concurrent_probe() {
        let breaker = Arc::new(CircuitBreaker::new(
            "test-idp.example.com",
            1,
            0,
            MetricsHarness::new().metrics(),
        ));
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_OPEN);

        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (entered_tx, entered_rx) = oneshot::channel::<()>();

        let probe_breaker = breaker.clone();
        let first = tokio::spawn(async move {
            probe_breaker
                .call(|| async move {
                    let _entered = entered_tx.send(());
                    drop(release_rx.await);
                    Ok::<(), AuthNError>(())
                })
                .await
        });

        drop(entered_rx.await);
        let second = breaker.call(|| async { Ok::<(), AuthNError>(()) }).await;
        assert!(matches!(second, Err(AuthNError::IdpUnreachable)));

        let _released = release_tx.send(());
        let first_result = first.await.expect("first probe task should join");
        assert!(first_result.is_ok());
        assert_eq!(breaker.state(), STATE_CLOSED);
    }

    #[tokio::test]
    async fn non_idp_error_does_not_reset_failure_count() {
        let breaker = breaker(2, 30);

        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_CLOSED);

        // Non-IdP error should NOT reset the failure counter.
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::SignatureInvalid) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_CLOSED);

        // Second IdP failure should open the circuit (threshold=2).
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_OPEN);
    }

    #[tokio::test]
    async fn zero_threshold_clamps_to_one() {
        let breaker = breaker(0, 30);
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(
            breaker.state(),
            STATE_OPEN,
            "threshold clamped to 1 should open after a single failure"
        );
    }

    #[tokio::test]
    async fn metrics_track_breaker_state_transitions() {
        let harness = MetricsHarness::new();
        let host = "idp.example.com";
        let attrs = [("host", host)];
        let breaker = CircuitBreaker::new(host, 1, 0, harness.metrics());

        harness.force_flush();
        let initial_state = harness
            .gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs)
            .expect("initial state should be present");
        let initial_idp = harness
            .gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs)
            .expect("initial idp should be present");
        assert!(
            (initial_state - 0.0).abs() < f64::EPSILON,
            "new breaker should start closed"
        );
        assert!(
            (initial_idp - 1.0).abs() < f64::EPSILON,
            "closed breaker should mark idp up"
        );

        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );

        harness.force_flush();
        let open_state = harness
            .gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs)
            .expect("open state should be present");
        let open_idp = harness
            .gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs)
            .expect("open idp should be present");
        assert!(
            (open_state - 2.0).abs() < f64::EPSILON,
            "failure threshold should open the breaker"
        );
        assert!(
            (open_idp - 0.0).abs() < f64::EPSILON,
            "open breaker should mark idp down"
        );

        drop(breaker.call(|| async { Ok::<(), AuthNError>(()) }).await);

        harness.force_flush();
        let closed_state = harness
            .gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs)
            .expect("closed state should be present");
        let closed_idp = harness
            .gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs)
            .expect("closed idp should be present");
        let closed_transitions = harness.counter_value(
            crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_CLOSED_TOTAL,
            &attrs,
        );
        assert!(
            (closed_state - 0.0).abs() < f64::EPSILON,
            "successful probe should close the breaker"
        );
        assert!(
            (closed_idp - 1.0).abs() < f64::EPSILON,
            "closed breaker should mark idp up"
        );
        assert_eq!(
            closed_transitions, 1,
            "successful probe should increment close transition counter"
        );
    }

    /// Regression test: multiple callers entering the OPEN path concurrently
    /// must not both execute a probe. Before the CAS fix,
    /// `transition_to_half_open` reset `half_open_probe_in_flight`, allowing
    /// the second caller to pass the CAS in `execute_half_open_probe`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_open_to_half_open_allows_only_one_probe() {
        use std::sync::atomic::AtomicUsize;
        use tokio::sync::Barrier;

        let breaker = Arc::new(CircuitBreaker::new(
            "test-idp.example.com",
            1,
            0,
            MetricsHarness::new().metrics(),
        ));

        // Trip the breaker to OPEN.
        drop(
            breaker
                .call(|| async { Err::<(), _>(AuthNError::IdpUnreachable) })
                .await,
        );
        assert_eq!(breaker.state(), STATE_OPEN);

        let probe_entered = Arc::new(AtomicUsize::new(0));

        // Hold the winning probe open so concurrent callers observe the
        // `half_open_probe_in_flight` flag as `true`.
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));

        // Barrier ensures all 10 tasks call the breaker at the same instant.
        let start_barrier = Arc::new(Barrier::new(10));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let b = breaker.clone();
            let pe = probe_entered.clone();
            let bar = start_barrier.clone();
            let rx = release_rx.clone();
            handles.push(tokio::spawn(async move {
                bar.wait().await;
                b.call(|| async {
                    pe.fetch_add(1, Ordering::AcqRel);
                    // The first probe to enter takes the receiver and waits;
                    // later entrants (if the bug existed) would find None and
                    // proceed immediately.
                    if let Some(rx) = rx.lock().await.take() {
                        drop(rx.await);
                    }
                    Ok::<(), AuthNError>(())
                })
                .await
            }));
        }

        // Give the runtime time to schedule all tasks and attempt the probes.
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            probe_entered.load(Ordering::Acquire),
            1,
            "exactly one probe closure should have entered while it is held open"
        );

        // Release the probe so it completes.
        let _released = release_tx.send(());

        let mut ok_count = 0usize;
        for handle in handles {
            if handle.await.expect("task should not panic").is_ok() {
                ok_count += 1;
            }
        }

        assert_eq!(ok_count, 1, "exactly one caller should succeed");
        assert_eq!(breaker.state(), STATE_CLOSED);
    }
}
