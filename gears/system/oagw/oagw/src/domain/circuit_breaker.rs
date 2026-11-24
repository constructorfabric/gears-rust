//! Upstream circuit breakers (`cpt-cf-oagw-adr-circuit-breaker`), one
//! registry per replica.
//!
//! A breaker belongs to an upstream, not to a caller: every tenant that
//! resolves to the upstream shares it, as Envoy's outlier detection shares a
//! host's state between all downstreams. Only failures the upstream caused
//! count ([`outcome_of_status`], [`outcome_of_error`],
//! [`UpstreamPermit::timed_out`]): not the gateway's own errors, not a
//! deadline the client used up uploading its body, not a request pinned to
//! one endpoint of several ([`UpstreamPermit::pinned_to_one_endpoint`]).
//! A caller can still make the upstream fail with what it asks for (a path
//! that answers 503, a request slower than the timeout); per-tenant isolation
//! is the rate limits' job.
//!
//! The key is the upstream a request resolved to and the upstream whose level
//! set the effective config: tenants that resolve different configs on one
//! upstream (a `private` config its owner sees and a descendant does not) get
//! separate breakers instead of rebuilding one on every switch. State is never
//! shared between replicas: each opens on the failures it sees itself.
//!
//! A breaker is rebuilt closed when the config, the alias or the endpoints it
//! was built for change, on the next request on every replica. A request that
//! sees no config leaves the breaker alone: under a `private` config the owner
//! and a descendant resolving to the same upstream see different configs. The
//! Control Plane drops an upstream's breakers on the replica that took a write
//! changing or removing its `circuit_breaker`, or deleting it;
//! [`CircuitBreakers::evict_idle`] reclaims the rest. Another replica does not
//! see a removal, so an identical config re-added there before the idle
//! eviction continues the old state, open for at most `timeout_seconds`.
//!
//! While two snapshots of one upstream are both in use (proxy config cache
//! lag, a write in progress), each switch between them rebuilds the breaker
//! closed, so it may not open until the snapshots converge. This is
//! transient.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use http::StatusCode;
use tokio::time::Instant;
use toolkit_macros::domain_model;
use toolkit_resilience::circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig as BreakerConfig, CircuitBreakerObserver, Outcome, Permit,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::{CircuitBreakerConfig, Endpoint, Upstream};

/// How long past its `timeout_seconds` an unused breaker is kept. An open one
/// would have let a probe through long before.
const IDLE_GRACE: Duration = Duration::from_secs(10 * 60);

/// The gateway's own steps before the exchange (CredStore read, endpoint
/// selection, an auth plugin's signing phase) may use up to
/// `1 / GATEWAY_SHARE` of the request timeout for a deadline hit to still
/// count against the upstream.
const GATEWAY_SHARE: u32 = 10;

/// The upstreams' breakers on this replica.
#[domain_model]
pub struct CircuitBreakers {
    /// `(resolved upstream, config owner)` → breaker.
    breakers: DashMap<(Uuid, Uuid), Breaker>,
    /// Sees every breaker's events (metrics); none in tests.
    observer: Option<Arc<dyn CircuitBreakerObserver>>,
    /// Zero point of [`Breaker::last_used`].
    epoch: Instant,
}

#[allow(
    unknown_lints,
    de0309_must_have_domain_model,
    reason = "runtime state, not a domain entity"
)]
struct Breaker {
    breaker: Arc<CircuitBreaker>,
    /// What the breaker was built for; a request that resolves to anything
    /// else rebuilds it.
    alias: Arc<str>,
    endpoints: Vec<Endpoint>,
    /// Milliseconds after [`CircuitBreakers::epoch`] of the last request.
    last_used: AtomicU64,
}

impl Default for CircuitBreakers {
    fn default() -> Self {
        Self {
            breakers: DashMap::new(),
            observer: None,
            epoch: Instant::now(),
        }
    }
}

impl CircuitBreakers {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Report every breaker built from now on to `observer`.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn CircuitBreakerObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Ask the breaker of `upstream`, the effective upstream of a proxy
    /// target, to let one request through to it.
    ///
    /// # Errors
    ///
    /// [`DomainError::CircuitBreakerOpen`] while the breaker is open, or
    /// half-open with every probe slot taken.
    pub fn admit(
        &self,
        upstream: &Upstream,
        instance: &str,
    ) -> Result<UpstreamPermit, DomainError> {
        let Some(breaker) = self.breaker_for(upstream) else {
            return Ok(UpstreamPermit::new(None));
        };
        breaker
            .try_acquire()
            .map(|permit| UpstreamPermit::new(Some(permit)))
            .map_err(|rejected| DomainError::CircuitBreakerOpen {
                detail: format!("circuit breaker is open for upstream '{}'", upstream.alias),
                instance: instance.to_owned(),
                retry_after: rejected.retry_after(),
            })
    }

    /// Fail fast while the breaker of `upstream` would reject the request,
    /// without keeping a probe slot: the permit is released at once. Moves an
    /// open breaker whose timeout has passed to half-open, as
    /// [`admit`](Self::admit) would.
    ///
    /// So at the open → half-open edge every request passes until probes
    /// hold every slot through `admit`; those that passed before then do the
    /// gateway's steps between the two calls and are rejected by `admit`.
    /// The window is at most the time the first request after the timeout
    /// takes from the check to `admit`, once per `timeout_seconds`. Holding
    /// the slot from the check on would hold it through the auth plugin, the
    /// rate limits and the CredStore read.
    ///
    /// # Errors
    ///
    /// As [`admit`](Self::admit).
    pub fn check(&self, upstream: &Upstream, instance: &str) -> Result<(), DomainError> {
        self.admit(upstream, instance).map(drop)
    }

    /// The breaker for requests to `upstream`; `None` without an enabled
    /// circuit breaker.
    fn breaker_for(&self, upstream: &Upstream) -> Option<Arc<CircuitBreaker>> {
        let cb = upstream.circuit_breaker.as_ref()?;
        let owner = cb.owner_id.unwrap_or(upstream.id);
        let key = (upstream.id, owner);
        if !cb.enabled {
            // Disabled where it was enabled: nothing reports it open any more.
            // `contains_key` first: the common case takes only a read lock.
            if self.breakers.contains_key(&key) {
                self.breakers.remove(&key);
            }
            return None;
        }
        let config = breaker_config(cb);
        let fits = |b: &Breaker| {
            *b.breaker.config() == config
                && *b.alias == *upstream.alias
                && b.endpoints == upstream.server.endpoints
        };
        let now = self.now_ms();
        // The common case under a shard read lock; the guard must be gone
        // before `entry` takes the write lock of the same shard.
        if let Some(b) = self.breakers.get(&key)
            && fits(&b)
        {
            b.last_used.store(now, Ordering::Relaxed);
            return Some(Arc::clone(&b.breaker));
        }
        let entry = self.breakers.entry(key);
        if let Entry::Occupied(e) = &entry
            && fits(e.get())
        {
            e.get().last_used.store(now, Ordering::Relaxed);
            return Some(Arc::clone(&e.get().breaker));
        }
        let b = self.build(config, upstream, owner, now)?;
        let breaker = Arc::clone(&b.breaker);
        entry.insert(b);
        Some(breaker)
    }

    fn build(
        &self,
        config: BreakerConfig,
        upstream: &Upstream,
        owner: Uuid,
        now: u64,
    ) -> Option<Breaker> {
        let name = breaker_name(&upstream.alias, upstream.id, owner);
        // Every write path rejects such values; a row written around them
        // fails open rather than refusing every request.
        let breaker = match CircuitBreaker::new(config) {
            Ok(breaker) => breaker.with_name(name),
            Err(e) => {
                tracing::warn!(breaker = %name, error = %e, "invalid circuit breaker config, not applied");
                return None;
            }
        };
        let breaker = match &self.observer {
            Some(observer) => breaker.with_observer(Arc::clone(observer)),
            None => breaker,
        };
        Some(Breaker {
            breaker: Arc::new(breaker),
            alias: Arc::from(upstream.alias.as_str()),
            endpoints: upstream.server.endpoints.clone(),
            last_used: AtomicU64::new(now),
        })
    }

    /// Drop the breakers of upstream `id` and those it set the config of, so
    /// the next request starts closed.
    pub fn remove_for_upstream(&self, id: Uuid) {
        self.breakers
            .retain(|(upstream, owner), _| *upstream != id && *owner != id);
    }

    /// Remove every breaker no request has used for [`IDLE_GRACE`] past its
    /// `timeout_seconds` and no request holds now: those of upstreams deleted,
    /// disabled or re-owned on another replica, which would otherwise stay in
    /// the map and in the state metrics. The next request builds a closed
    /// one. Returns the number removed.
    pub fn evict_idle(&self) -> usize {
        let now = self.now_ms();
        let mut evicted = 0;
        self.breakers.retain(|_, b| {
            let idle =
                Duration::from_millis(now.saturating_sub(b.last_used.load(Ordering::Relaxed)));
            let stale = Arc::strong_count(&b.breaker) == 1
                && idle >= b.breaker.config().open_for().saturating_add(IDLE_GRACE);
            evicted += usize::from(stale);
            !stale
        });
        evicted
    }

    /// Breakers currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.breakers.len()
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The breaker's name in logs and metrics: the alias, which tenants may
/// share by shadowing, and the upstream id; the config owner too when it is
/// another upstream.
fn breaker_name(alias: &str, upstream: Uuid, owner: Uuid) -> Arc<str> {
    if owner == upstream {
        Arc::from(format!("{alias}/{upstream}"))
    } else {
        Arc::from(format!("{alias}/{upstream}/{owner}"))
    }
}

/// The toolkit breaker for `cb`: open after `failure_threshold` failures in
/// a row, stay open `timeout_seconds`, then let `success_threshold` probes
/// through, all of which must succeed to close.
fn breaker_config(cb: &CircuitBreakerConfig) -> BreakerConfig {
    BreakerConfig::default()
        .with_consecutive_failures(cb.failure_threshold)
        .with_open_for(Duration::from_secs(u64::from(cb.timeout_seconds)))
        .with_half_open_calls(cb.success_threshold)
}

/// Leave for one request to reach its upstream; empty when the upstream has
/// no breaker. Dropped without a record (the request failed before the
/// exchange, or the client went away), it counts as nothing and frees its
/// half-open slot.
#[domain_model]
#[derive(Debug)]
#[must_use = "record what the upstream answered"]
pub struct UpstreamPermit {
    permit: Option<Permit>,
    /// Whether target resolution and the exchange started with nearly the
    /// whole request timeout left (see [`Self::with_budget`]).
    full_budget: bool,
    /// Whether the caller picked one endpoint of several (see
    /// [`Self::pinned_to_one_endpoint`]).
    pinned: bool,
}

impl UpstreamPermit {
    fn new(permit: Option<Permit>) -> Self {
        Self {
            permit,
            full_budget: true,
            pinned: false,
        }
    }

    /// The caller picked the endpoint (`X-OAGW-Target-Host`) of an upstream
    /// with several. The breaker covers them all, so what one of them does
    /// is not counted: a caller pinning a dead endpoint would otherwise open
    /// the circuit for every tenant while the others serve.
    pub fn pinned_to_one_endpoint(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    /// The upstream has `left` of the shared request deadline, whose whole
    /// is `request_timeout`, for target resolution and the exchange. When the
    /// gateway's own steps took more than a tenth of it, the upstream did not
    /// get its budget and a deadline hit is not counted. Called again with
    /// less once a signing phase has run; the last call holds.
    pub fn with_budget(mut self, left: Duration, request_timeout: Duration) -> Self {
        self.full_budget = has_full_budget(left, request_timeout);
        self
    }

    /// The upstream answered with `status`.
    pub fn answered(self, status: StatusCode) {
        self.record(outcome_of_status(status));
    }

    /// Target resolution or the exchange failed with `err` on its own, before
    /// the request deadline.
    pub fn failed(self, err: &DomainError) {
        self.record(outcome_of_error(err));
    }

    /// The request deadline passed during target resolution or the exchange:
    /// a failure with the full budget, nothing otherwise.
    pub fn timed_out(self) {
        let outcome = if self.full_budget {
            Outcome::Failure
        } else {
            Outcome::Ignore
        };
        self.record(outcome);
    }

    /// The request ended for a reason that says nothing about the upstream:
    /// an SSRF refusal, or a failure while the client was still sending its
    /// body.
    pub fn ignored(self) {
        self.record(Outcome::Ignore);
    }

    fn record(self, outcome: Outcome) {
        if let Some(permit) = self.permit {
            permit.record(if self.pinned {
                Outcome::Ignore
            } else {
                outcome
            });
        }
    }
}

/// Whether `left` of `request_timeout` is all but the gateway's share.
fn has_full_budget(left: Duration, request_timeout: Duration) -> bool {
    left >= request_timeout.saturating_sub(request_timeout / GATEWAY_SHARE)
}

/// 502, 503 and 504 say the upstream (or what is in front of it) is not
/// serving; any other status is an answer.
pub fn outcome_of_status(status: StatusCode) -> Outcome {
    match status {
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT => {
            Outcome::Failure
        }
        _ => Outcome::Success,
    }
}

/// What an error of target resolution or the exchange says about the
/// upstream. No connection, a TLS failure, the engine's own connect or read
/// timeout and a broken exchange mean the upstream is unreachable or not
/// answering. Everything else is the gateway's own or the caller's doing.
/// A hit of the shared request deadline goes through
/// [`UpstreamPermit::timed_out`], not here.
pub fn outcome_of_error(err: &DomainError) -> Outcome {
    match err {
        // DNS failure, no route, connect error.
        DomainError::LinkUnavailable { .. }
        // Refused, TLS handshake, invalid certificate, peer closed, invalid
        // response.
        | DomainError::DownstreamError { .. }
        | DomainError::ConnectionTimeout { .. }
        // The engine's read or write timeout, a budget of its own.
        | DomainError::RequestTimeout { .. }
        | DomainError::ProtocolError { .. }
        | DomainError::StreamAborted { .. } => Outcome::Failure,
        DomainError::NotFound { .. }
        | DomainError::Conflict { .. }
        | DomainError::RegistryManaged { .. }
        | DomainError::Validation { .. }
        | DomainError::UpstreamDisabled { .. }
        | DomainError::Internal { .. }
        | DomainError::MissingTargetHost { .. }
        | DomainError::InvalidTargetHost { .. }
        | DomainError::UnknownTargetHost { .. }
        | DomainError::AuthenticationFailed { .. }
        | DomainError::PayloadTooLarge { .. }
        | DomainError::RateLimitExceeded { .. }
        | DomainError::SecretNotFound { .. }
        | DomainError::SecretRefNotAccessible { .. }
        | DomainError::CredentialStoreUnavailable { .. }
        | DomainError::IdpUnavailable { .. }
        | DomainError::IdpInvalidResponse { .. }
        | DomainError::GuardRejected { .. }
        | DomainError::CorsOriginNotAllowed { .. }
        | DomainError::CorsMethodNotAllowed { .. }
        | DomainError::CircuitBreakerOpen { .. }
        | DomainError::IdleTimeout { .. }
        | DomainError::PluginNotFound { .. }
        | DomainError::PluginInUse { .. }
        | DomainError::Forbidden { .. } => Outcome::Ignore,
    }
}

#[cfg(test)]
#[path = "circuit_breaker_tests.rs"]
mod tests;
