//! Host-scoped circuit breakers for `IdP` network calls.
//!
//! The state machine is `toolkit-resilience`'s [`CircuitBreaker`]; this module
//! adds what is specific to the plugin: one breaker per `IdP` host, the mapping
//! of [`AuthNError`] to a breaker [`Outcome`], and an observer that keeps the
//! plugin's own metrics (`authn.circuit_breaker.*`, `authn.idp_up`).
//!
//! Semantics, fail-closed:
//! - Open circuit fails fast with `IdpUnreachable`.
//! - Half-open lets exactly one probe through.
//! - Closed opens after `failure_threshold` consecutive `IdP` failures.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dashmap::DashMap;
use toolkit_resilience::circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerObserver, ConfigError, Lifecycle, Outcome,
    State, StateChange,
};

use crate::domain::error::AuthNError;
use crate::domain::metrics::AuthNMetrics;

/// Normalize a URL-like identity into a host-scoped breaker key.
#[must_use]
pub(crate) fn host_key(endpoint: &url::Url) -> String {
    endpoint.host_str().map_or_else(
        || endpoint.as_str().to_owned(),
        |host| match endpoint.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        },
    )
}

/// Value of the state gauge: 0 = closed, 1 = half-open, 2 = open.
fn state_code(state: State) -> f64 {
    match state {
        State::Closed => 0.0,
        State::HalfOpen => 1.0,
        State::Open => 2.0,
    }
}

/// Reports one breaker's events through the plugin's metrics, labelled by host.
struct AuthnMetricsObserver {
    host: Arc<str>,
    metrics: Arc<AuthNMetrics>,
    /// Highest `StateChange::generation` applied. Events arrive after the
    /// breaker's lock is released and can be reordered; an older one must not
    /// overwrite the gauges. Held while the gauges are written, so a newer
    /// event cannot write between an older event's check and its writes.
    last_generation: Mutex<u64>,
}

impl AuthnMetricsObserver {
    fn new(host: Arc<str>, metrics: Arc<AuthNMetrics>) -> Self {
        Self {
            host,
            metrics,
            last_generation: Mutex::new(0),
        }
    }

    fn set_state(&self, state: State) {
        self.metrics
            .set_circuit_breaker_state(&self.host, state_code(state));
        let idp_up = if state == State::Closed { 1.0 } else { 0.0 };
        self.metrics.set_idp_up(&self.host, idp_up);
    }
}

impl CircuitBreakerObserver for AuthnMetricsObserver {
    fn on_attach(&self, event: &Lifecycle<'_>) {
        let _applied = self
            .last_generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.set_state(event.state);
    }

    fn on_state_change(&self, event: &StateChange<'_>) {
        // A counter, not a gauge: every close counts, whatever the order.
        if event.to == State::Closed {
            self.metrics.increment_circuit_breaker_closed(&self.host);
        }
        let mut applied = self
            .last_generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *applied < event.generation {
            *applied = event.generation;
            self.set_state(event.to);
        }
    }
}

/// Host-scoped circuit-breaker registry for outbound identity dependencies.
pub struct HostCircuitBreakers {
    breakers: DashMap<String, Arc<CircuitBreaker>>,
    config: CircuitBreakerConfig,
    metrics: Arc<AuthNMetrics>,
}

impl std::fmt::Debug for HostCircuitBreakers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCircuitBreakers")
            .field("hosts", &self.breakers.len())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl HostCircuitBreakers {
    /// Create a host-scoped breaker registry from the configured breaker policy.
    ///
    /// # Errors
    /// Returns [`ConfigError`] for a zero `failure_threshold` or `reset_timeout_secs`.
    pub fn new(
        failure_threshold: u32,
        reset_timeout_secs: u64,
        metrics: Arc<AuthNMetrics>,
    ) -> Result<Self, ConfigError> {
        Self::with_open_for(
            failure_threshold,
            Duration::from_secs(reset_timeout_secs),
            metrics,
        )
    }

    /// [`new`](Self::new) with the open period given as a `Duration`.
    pub(crate) fn with_open_for(
        failure_threshold: u32,
        open_for: Duration,
        metrics: Arc<AuthNMetrics>,
    ) -> Result<Self, ConfigError> {
        let config = CircuitBreakerConfig::default()
            .with_consecutive_failures(failure_threshold)
            .with_open_for(open_for)
            .with_half_open_calls(1);
        config.validate()?;
        Ok(Self {
            breakers: DashMap::new(),
            config,
            metrics,
        })
    }

    /// Execute an IdP-touching operation under the breaker for the given host.
    ///
    /// Only `IdP` failures count against the breaker. Other errors (a bad token,
    /// say) are neutral while closed, so intermittent validation errors cannot
    /// mask sustained `IdP` degradation; as a half-open probe they show the
    /// `IdP` answered, so they close the circuit. A request the local client
    /// refused (`HttpClientUnavailable`) never reached the `IdP` and counts for
    /// nothing, even as a probe.
    ///
    /// # Errors
    /// Returns `AuthNError::IdpUnreachable` when that host's breaker rejects execution.
    pub async fn call<F, Fut, T>(&self, host: &str, f: F) -> Result<T, AuthNError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, AuthNError>>,
    {
        // The config was validated in `new`, so this does not fail; if it did,
        // fail closed rather than run the call unguarded.
        let breaker = self
            .breaker_for(host)
            .map_err(|_| AuthNError::IdpUnreachable)?;
        let permit = breaker
            .try_acquire()
            .map_err(|_| AuthNError::IdpUnreachable)?;
        let probing = permit.is_probe();

        // A permit dropped without an outcome (panic, cancelled future)
        // counts for nothing and frees the probe slot.
        let result = f().await;
        let outcome = match &result {
            Ok(_) => Outcome::Success,
            Err(error) if error.is_idp_failure() => Outcome::Failure,
            // The local client refused the call: the IdP was not asked, so
            // even a probe says nothing about it.
            Err(AuthNError::HttpClientUnavailable(_)) => Outcome::Ignore,
            Err(_) if probing => Outcome::Success,
            Err(_) => Outcome::Ignore,
        };
        permit.record(outcome);
        result
    }

    fn breaker_for(&self, host: &str) -> Result<Arc<CircuitBreaker>, ConfigError> {
        if let Some(existing) = self.breakers.get(host) {
            return Ok(Arc::clone(&existing));
        }
        let entry = self
            .breakers
            .entry(host.to_owned())
            .or_try_insert_with(|| self.build_breaker(host).map(Arc::new))?;
        Ok(Arc::clone(&entry))
    }

    fn build_breaker(&self, host: &str) -> Result<CircuitBreaker, ConfigError> {
        let host: Arc<str> = Arc::from(host);
        let observer = AuthnMetricsObserver::new(Arc::clone(&host), Arc::clone(&self.metrics));
        Ok(CircuitBreaker::new(self.config.clone())?
            .with_name(host)
            .with_observer(observer))
    }

    /// Return the current state for a host if its breaker has been created.
    #[cfg(test)]
    pub(crate) fn state_for_host(&self, host: &str) -> Option<State> {
        self.breakers.get(host).map(|breaker| breaker.state())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::oneshot;

    use crate::domain::metrics::test_harness::MetricsHarness;

    use super::*;

    const HOST: &str = "test-idp.example.com";
    /// Short enough to wait out in a test, long enough not to race the assertions.
    const SHORT_OPEN: Duration = Duration::from_millis(40);

    fn breakers(failure_threshold: u32, open_for: Duration) -> HostCircuitBreakers {
        HostCircuitBreakers::with_open_for(
            failure_threshold,
            open_for,
            MetricsHarness::new().metrics(),
        )
        .expect("test breaker config should be valid")
    }

    async fn fail(breakers: &HostCircuitBreakers) -> Result<(), AuthNError> {
        breakers
            .call(HOST, || async { Err::<(), _>(AuthNError::IdpUnreachable) })
            .await
    }

    async fn succeed(breakers: &HostCircuitBreakers) -> Result<(), AuthNError> {
        breakers
            .call(HOST, || async { Ok::<_, AuthNError>(()) })
            .await
    }

    async fn wait_out_open_period() {
        tokio::time::sleep(SHORT_OPEN * 2).await;
    }

    #[test]
    fn host_key_uses_host_and_port_without_path_query_or_fragment() {
        let with_port =
            url::Url::parse("https://idp.example.com:8443/realms/a?tenant_secret=x#frag")
                .expect("test URL with port should parse");
        let without_port = url::Url::parse("https://idp.example.com/realms/b")
            .expect("test URL without port should parse");

        assert_eq!(host_key(&with_port), "idp.example.com:8443");
        assert_eq!(host_key(&without_port), "idp.example.com");
    }

    #[tokio::test]
    async fn host_registry_isolates_open_breaker_by_host() {
        let breakers = HostCircuitBreakers::new(1, 30, MetricsHarness::new().metrics())
            .expect("test breaker config should be valid");

        let first = breakers
            .call("idp-a.example.com", || async {
                Err::<(), _>(AuthNError::IdpUnreachable)
            })
            .await;
        assert!(matches!(first, Err(AuthNError::IdpUnreachable)));
        assert_eq!(
            breakers.state_for_host("idp-a.example.com"),
            Some(State::Open)
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
            Some(State::Closed)
        );
    }

    #[tokio::test]
    async fn host_registry_records_metrics_per_host() {
        let harness = MetricsHarness::new();
        let breakers = HostCircuitBreakers::new(1, 30, harness.metrics())
            .expect("test breaker config should be valid");

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
        let b = breakers(2, Duration::from_secs(30));

        assert!(matches!(fail(&b).await, Err(AuthNError::IdpUnreachable)));
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));

        assert!(matches!(fail(&b).await, Err(AuthNError::IdpUnreachable)));
        assert_eq!(b.state_for_host(HOST), Some(State::Open));
    }

    #[tokio::test]
    async fn open_circuit_rejects_without_running_the_call() {
        let b = breakers(1, Duration::from_secs(30));
        drop(fail(&b).await);

        let ran = AtomicUsize::new(0);
        let result = b
            .call(HOST, || async {
                ran.fetch_add(1, Ordering::AcqRel);
                Ok::<_, AuthNError>(())
            })
            .await;

        assert!(matches!(result, Err(AuthNError::IdpUnreachable)));
        assert_eq!(ran.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn open_transitions_to_half_open_after_timeout() {
        let b = Arc::new(breakers(1, SHORT_OPEN));
        drop(fail(&b).await);
        assert_eq!(b.state_for_host(HOST), Some(State::Open));
        wait_out_open_period().await;

        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (entered_tx, entered_rx) = oneshot::channel::<()>();

        let cloned = Arc::clone(&b);
        let join = tokio::spawn(async move {
            cloned
                .call(HOST, || async move {
                    let _entered = entered_tx.send(());
                    drop(release_rx.await);
                    Ok::<(), AuthNError>(())
                })
                .await
        });

        drop(entered_rx.await);
        assert_eq!(b.state_for_host(HOST), Some(State::HalfOpen));
        let _released = release_tx.send(());
        assert!(join.await.expect("probe task should join").is_ok());
    }

    #[tokio::test]
    async fn half_open_success_closes_circuit() {
        let b = breakers(1, SHORT_OPEN);
        drop(fail(&b).await);
        wait_out_open_period().await;

        assert!(succeed(&b).await.is_ok());
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));
    }

    #[tokio::test]
    async fn half_open_failure_reopens_circuit() {
        let b = breakers(1, SHORT_OPEN);
        drop(fail(&b).await);
        wait_out_open_period().await;

        assert!(matches!(fail(&b).await, Err(AuthNError::IdpUnreachable)));
        assert_eq!(b.state_for_host(HOST), Some(State::Open));
    }

    #[tokio::test]
    async fn half_open_non_idp_error_closes_circuit() {
        let b = breakers(1, SHORT_OPEN);
        drop(fail(&b).await);
        wait_out_open_period().await;

        let probe = b
            .call(HOST, || async {
                Err::<(), _>(AuthNError::SignatureInvalid)
            })
            .await;

        assert!(matches!(probe, Err(AuthNError::SignatureInvalid)));
        assert_eq!(
            b.state_for_host(HOST),
            Some(State::Closed),
            "the IdP answered, so connectivity recovered"
        );
    }

    #[tokio::test]
    async fn half_open_allows_only_one_concurrent_probe() {
        let b = Arc::new(breakers(1, SHORT_OPEN));
        drop(fail(&b).await);
        wait_out_open_period().await;

        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (entered_tx, entered_rx) = oneshot::channel::<()>();

        let probe_breakers = Arc::clone(&b);
        let first = tokio::spawn(async move {
            probe_breakers
                .call(HOST, || async move {
                    let _entered = entered_tx.send(());
                    drop(release_rx.await);
                    Ok::<(), AuthNError>(())
                })
                .await
        });

        drop(entered_rx.await);
        assert!(matches!(succeed(&b).await, Err(AuthNError::IdpUnreachable)));

        let _released = release_tx.send(());
        assert!(first.await.expect("first probe task should join").is_ok());
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));
    }

    #[tokio::test]
    async fn non_idp_error_does_not_reset_failure_count() {
        let b = breakers(2, Duration::from_secs(30));

        drop(fail(&b).await);
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));

        // A non-IdP error must not reset the consecutive-failure count.
        drop(
            b.call(HOST, || async {
                Err::<(), _>(AuthNError::SignatureInvalid)
            })
            .await,
        );
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));

        drop(fail(&b).await);
        assert_eq!(b.state_for_host(HOST), Some(State::Open));
    }

    #[tokio::test]
    async fn success_resets_failure_count() {
        let b = breakers(2, Duration::from_secs(30));

        drop(fail(&b).await);
        drop(succeed(&b).await);
        drop(fail(&b).await);

        assert_eq!(b.state_for_host(HOST), Some(State::Closed));
    }

    #[test]
    fn rejects_zero_threshold_and_zero_open_period() {
        let metrics = MetricsHarness::new().metrics();
        assert!(HostCircuitBreakers::new(0, 30, Arc::clone(&metrics)).is_err());
        assert!(HostCircuitBreakers::new(1, 0, metrics).is_err());
    }

    #[tokio::test]
    async fn cancelled_call_counts_for_nothing() {
        let b = Arc::new(breakers(1, Duration::from_secs(30)));
        let started = Arc::new(tokio::sync::Notify::new());

        let task_b = Arc::clone(&b);
        let task_started = Arc::clone(&started);
        let task = tokio::spawn(async move {
            task_b
                .call(HOST, || async move {
                    task_started.notify_one();
                    std::future::pending::<Result<(), AuthNError>>().await
                })
                .await
        });
        started.notified().await;
        task.abort();
        drop(task.await);

        assert_eq!(b.state_for_host(HOST), Some(State::Closed));
    }

    #[tokio::test]
    async fn metrics_track_breaker_state_transitions() {
        let harness = MetricsHarness::new();
        let attrs = [("host", HOST)];
        let b = HostCircuitBreakers::with_open_for(1, SHORT_OPEN, harness.metrics())
            .expect("test breaker config should be valid");

        // The breaker is created on first use; a success keeps it closed.
        drop(succeed(&b).await);
        harness.force_flush();
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs),
            Some(0.0),
            "new breaker should start closed"
        );
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs),
            Some(1.0)
        );

        drop(fail(&b).await);
        harness.force_flush();
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs),
            Some(2.0),
            "failure threshold should open the breaker"
        );
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs),
            Some(0.0)
        );

        wait_out_open_period().await;
        drop(succeed(&b).await);
        harness.force_flush();
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs),
            Some(0.0),
            "successful probe should close the breaker"
        );
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs),
            Some(1.0)
        );
        assert_eq!(
            harness.counter_value(
                crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_CLOSED_TOTAL,
                &attrs
            ),
            1,
            "successful probe should increment close transition counter"
        );
    }

    #[tokio::test]
    async fn cancelled_half_open_probe_frees_the_slot() {
        let b = Arc::new(breakers(1, SHORT_OPEN));
        drop(fail(&b).await);
        wait_out_open_period().await;

        let started = Arc::new(tokio::sync::Notify::new());
        let task_b = Arc::clone(&b);
        let task_started = Arc::clone(&started);
        let probe = tokio::spawn(async move {
            task_b
                .call(HOST, || async move {
                    task_started.notify_one();
                    std::future::pending::<Result<(), AuthNError>>().await
                })
                .await
        });
        started.notified().await;
        assert_eq!(b.state_for_host(HOST), Some(State::HalfOpen));
        probe.abort();
        drop(probe.await);

        assert!(
            succeed(&b).await.is_ok(),
            "the next call takes the freed probe slot"
        );
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));
    }

    #[tokio::test]
    async fn metrics_report_half_open_and_close_on_non_idp_probe_error() {
        let harness = MetricsHarness::new();
        let attrs = [("host", HOST)];
        let b = Arc::new(
            HostCircuitBreakers::with_open_for(1, SHORT_OPEN, harness.metrics())
                .expect("test breaker config should be valid"),
        );
        drop(fail(&b).await);
        wait_out_open_period().await;

        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (entered_tx, entered_rx) = oneshot::channel::<()>();
        let probe_b = Arc::clone(&b);
        let probe = tokio::spawn(async move {
            probe_b
                .call(HOST, || async move {
                    let _entered = entered_tx.send(());
                    drop(release_rx.await);
                    Err::<(), _>(AuthNError::SignatureInvalid)
                })
                .await
        });
        drop(entered_rx.await);

        harness.force_flush();
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs),
            Some(1.0)
        );
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs),
            Some(0.0)
        );

        let _released = release_tx.send(());
        drop(probe.await.expect("probe task should join"));
        harness.force_flush();
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs),
            Some(0.0)
        );
        assert_eq!(
            harness.counter_value(
                crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_CLOSED_TOTAL,
                &attrs
            ),
            1
        );
    }

    #[test]
    fn observer_ignores_state_changes_older_than_the_last_applied() {
        let harness = MetricsHarness::new();
        let host: Arc<str> = Arc::from(HOST);
        let observer = AuthnMetricsObserver::new(Arc::clone(&host), harness.metrics());
        let attrs = [("host", HOST)];

        // Generation 3 (closed -> open) arrives before generation 2 (half-open -> closed).
        observer.on_state_change(&StateChange::new(
            Some(&host),
            State::Closed,
            State::Open,
            3,
        ));
        observer.on_state_change(&StateChange::new(
            Some(&host),
            State::HalfOpen,
            State::Closed,
            2,
        ));

        harness.force_flush();
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_STATE, &attrs),
            Some(2.0)
        );
        assert_eq!(
            harness.gauge_value(crate::domain::metrics::AUTHN_IDP_UP, &attrs),
            Some(0.0)
        );
        assert_eq!(
            harness.counter_value(
                crate::domain::metrics::AUTHN_CIRCUIT_BREAKER_CLOSED_TOTAL,
                &attrs
            ),
            1,
            "a late close still counts"
        );
    }

    #[tokio::test]
    async fn local_client_failure_on_a_probe_leaves_the_breaker_half_open() {
        let b = Arc::new(breakers(1, SHORT_OPEN));
        drop(fail(&b).await);
        wait_out_open_period().await;

        let probe = b
            .call(HOST, || async {
                Err::<(), _>(AuthNError::HttpClientUnavailable("overloaded".to_owned()))
            })
            .await;

        assert!(
            matches!(probe, Err(AuthNError::HttpClientUnavailable(_))),
            "{probe:?}"
        );
        assert_eq!(
            b.state_for_host(HOST),
            Some(State::HalfOpen),
            "the IdP was not asked, so the probe neither closes nor reopens"
        );
        assert!(succeed(&b).await.is_ok(), "the probe slot was freed");
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));
    }

    /// Many callers hitting an open breaker whose period has passed must run
    /// exactly one probe.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_open_to_half_open_allows_only_one_probe() {
        let b = Arc::new(breakers(1, SHORT_OPEN));
        drop(fail(&b).await);
        wait_out_open_period().await;

        let probe_entered = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
        let start_barrier = Arc::new(tokio::sync::Barrier::new(10));
        let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<bool>();

        let mut handles = Vec::new();
        for _ in 0..10 {
            let b = Arc::clone(&b);
            let entered = Arc::clone(&probe_entered);
            let barrier = Arc::clone(&start_barrier);
            let rx = Arc::clone(&release_rx);
            let done = done_tx.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let result = b
                    .call(HOST, || async {
                        entered.fetch_add(1, Ordering::AcqRel);
                        let rx = rx.lock().await.take();
                        if let Some(rx) = rx {
                            drop(rx.await);
                        }
                        Ok::<(), AuthNError>(())
                    })
                    .await;
                let _reported = done.send(result.is_ok());
            }));
        }
        drop(done_tx);

        let loser_results = tokio::time::timeout(Duration::from_secs(10), async {
            let mut results = Vec::new();
            while results.len() < 9 {
                results.push(done_rx.recv().await.expect("caller should report"));
            }
            results
        })
        .await
        .expect("nine callers should return while the probe is held open");

        assert_eq!(
            probe_entered.load(Ordering::Acquire),
            1,
            "exactly one probe closure should have entered while it is held open"
        );
        assert!(
            loser_results.iter().all(|ok| !ok),
            "callers returning while the probe is held open should be rejected"
        );

        let _released = release_tx.send(());
        assert!(done_rx.recv().await.expect("winning caller should report"));
        for handle in handles {
            handle.await.expect("task should not panic");
        }
        assert_eq!(b.state_for_host(HOST), Some(State::Closed));
    }
}
