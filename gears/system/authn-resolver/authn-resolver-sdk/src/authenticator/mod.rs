//! Bounded caching for inbound bearer-token validation.
//!
//! [`CachingAuthenticator`] shares one validation per token. Caller cancellation leaves it
//! running. Keys are SHA-256 digests. A cached [`SecurityContext`] may carry the token for
//! forwarding. Neither tokens nor digests are logged.
//!
//! - **Reuse.** A success needs a future [`cache_until`](crate::CacheControl::cache_until).
//!   Reuse ends at the earliest of that deadline, known credential expiry and
//!   [`CachingAuthenticatorConfig::max_ttl`] after validation. Revocation can go
//!   unnoticed until then. Failures trigger neither caching nor backoff.
//! - **Delivery.** A credential already expired at completion yields
//!   [`AuthNResolverError::Unauthorized`], even within backend clock-skew leeway.
//!   Current waiters may collect until credential expiry, even after the reuse window
//!   closes. If invalidation or expiry discards three successive deliveries, the call
//!   returns [`AuthNResolverError::ServiceUnavailable`]. Each validation has its own timeout.
//! - **Refresh.** Each validation requests [`RequestCacheControl::Refresh`]. A retained
//!   [`CacheTag`] permits only a provably newer plugin result. Expiry, invalidation and
//!   no-store results can leave a tag for the next validation. Failures preserve it.
//!   Clearing or evicting the tag makes the next refresh unconditional.
//! - **Capacity.** At [`CachingAuthenticatorConfig::max_entries`], evict tag-only records
//!   first, then the earliest-expiring result. At [`CachingAuthenticatorConfig::max_in_flight`],
//!   new validations immediately return [`CachingAuthenticatorError::Overloaded`].
//!   Cache hits and callers joining an existing validation can still proceed.
//! - **Invalidation.** [`CachingAuthenticator::invalidate`] discards a resident result
//!   and its pending deliveries. [`CachingAuthenticator::clear`] covers all tokens,
//!   forgets their tags and cancels validations.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use secrecy::SecretString;
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::Instant;
use toolkit_security::crypto::hash::Sha256;
use toolkit_security::{AuthNError, BearerAuthenticator, SecurityContext};

use crate::api::AuthNResolverClient;
use crate::bearer::map_error;
use crate::error::AuthNResolverError;
use crate::local_cache::{
    Deadline, MAX_EVALUATIONS, clone_error, lock_or_terminate, next_cache_id,
    spawn_in_caller_context,
};
use crate::models::{
    AuthenticationRequest, AuthenticationResult, CacheOrigin, CacheTag, RequestCacheControl,
};

/// Upper bound on [`CachingAuthenticatorConfig::max_entries`].
const MAX_ENTRIES: usize = 100_000;
/// Upper bound on [`CachingAuthenticatorConfig::max_in_flight`].
const MAX_IN_FLIGHT: usize = 4096;
/// Upper bound on [`CachingAuthenticatorConfig::max_ttl`] and
/// [`CachingAuthenticatorConfig::acquisition_timeout`].
const MAX_DURATION: Duration = Duration::from_hours(24);

/// Tuning for [`CachingAuthenticator`]. Checked when it is built.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CachingAuthenticatorConfig {
    /// Maximum number of retained records, `1..=100_000`: cached results
    /// plus tag-only refresh hints. Default 10 000.
    pub max_entries: usize,
    /// Maximum number of validations running at once, counting cancelled
    /// ones until their task has ended, `1..=4096`. Default 256.
    pub max_in_flight: usize,
    /// Longest reuse of one result, measured from the end of its validation;
    /// the producer's `cache_until` and the credential expiry can only
    /// shorten it. Non-zero, at most one day. Default 30s.
    pub max_ttl: Duration,
    /// Bound on one validation; on expiry it fails with `ServiceUnavailable`.
    /// Non-zero, at most one day. Default 30s.
    pub acquisition_timeout: Duration,
}

impl Default for CachingAuthenticatorConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_in_flight: 256,
            max_ttl: Duration::from_secs(30),
            acquisition_timeout: Duration::from_secs(30),
        }
    }
}

impl CachingAuthenticatorConfig {
    /// Set [`Self::max_entries`].
    #[must_use]
    pub const fn with_max_entries(mut self, entries: usize) -> Self {
        self.max_entries = entries;
        self
    }

    /// Set [`Self::max_in_flight`].
    #[must_use]
    pub const fn with_max_in_flight(mut self, validations: usize) -> Self {
        self.max_in_flight = validations;
        self
    }

    /// Set [`Self::max_ttl`].
    #[must_use]
    pub const fn with_max_ttl(mut self, ttl: Duration) -> Self {
        self.max_ttl = ttl;
        self
    }

    /// Set [`Self::acquisition_timeout`].
    #[must_use]
    pub const fn with_acquisition_timeout(mut self, timeout: Duration) -> Self {
        self.acquisition_timeout = timeout;
        self
    }

    /// Check the configuration.
    ///
    /// # Errors
    ///
    /// [`CachingAuthenticatorConfigError`] naming the first invalid setting.
    pub fn validate(&self) -> Result<(), CachingAuthenticatorConfigError> {
        if !(1..=MAX_ENTRIES).contains(&self.max_entries) {
            return Err(CachingAuthenticatorConfigError::MaxEntries);
        }
        if !(1..=MAX_IN_FLIGHT).contains(&self.max_in_flight) {
            return Err(CachingAuthenticatorConfigError::MaxInFlight);
        }
        if self.max_ttl.is_zero() || self.max_ttl > MAX_DURATION {
            return Err(CachingAuthenticatorConfigError::MaxTtl);
        }
        if self.acquisition_timeout.is_zero() || self.acquisition_timeout > MAX_DURATION {
            return Err(CachingAuthenticatorConfigError::AcquisitionTimeout);
        }
        Ok(())
    }
}

/// Why a [`CachingAuthenticatorConfig`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CachingAuthenticatorConfigError {
    /// `max_entries` is zero or exceeds 100 000.
    #[error("max_entries must be between 1 and 100000")]
    MaxEntries,
    /// `max_in_flight` is zero or exceeds 4096.
    #[error("max_in_flight must be between 1 and 4096")]
    MaxInFlight,
    /// `max_ttl` is zero or exceeds one day.
    #[error("max_ttl must be non-zero and at most one day")]
    MaxTtl,
    /// `acquisition_timeout` is zero or exceeds one day.
    #[error("acquisition_timeout must be non-zero and at most one day")]
    AcquisitionTimeout,
}

/// Why a [`CachingAuthenticator::authenticate`] call failed.
///
/// A closed set: each variant calls for a different caller action.
#[derive(Debug, thiserror::Error)]
pub enum CachingAuthenticatorError {
    /// The authenticator was shut down, or disabled by a panic while its
    /// state was locked. Permanent.
    #[error("caching authenticator is shut down")]
    ShutDown,
    /// The token needs a new validation, but `max_in_flight` validations are
    /// running. Nothing was sent to the resolver. Temporary: retry later.
    #[error("all {max_in_flight} token validations are in use")]
    Overloaded {
        /// The configured [`CachingAuthenticatorConfig::max_in_flight`].
        max_in_flight: usize,
    },
    /// The resolver error shared by this validation's callers. Includes `Unauthorized`
    /// for expired credentials and `ServiceUnavailable` after three discarded deliveries.
    #[error("token validation failed")]
    Resolver(#[source] AuthNResolverError),
}

type WallClock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;
type Outcome = Result<AuthenticationResult, AuthNResolverError>;
type AuthenticatorOutcome = Result<AuthenticationResult, CachingAuthenticatorError>;
type DeliverySlot = watch::Receiver<Option<Delivery>>;

/// SHA-256 digest of a token, so cache keys never duplicate the raw token.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TokenKey([u8; 32]);

impl TokenKey {
    fn of(token: &str) -> Self {
        Self(Sha256::digest(token.as_bytes()))
    }
}

impl fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenKey(..)")
    }
}

/// Validates bearer tokens through an [`AuthNResolverClient`], reusing a
/// result only within the bounded window its producer authorized.
///
/// Clones share the cache. Dropping the last clone cancels running
/// validations; [`Self::shutdown`] does the same and waits for them. Must be
/// used from within a Tokio runtime.
///
/// It also implements [`BearerAuthenticator`], as an explicit opt-in
/// alternative to the always-revalidating
/// [`AuthNResolverBearerAuthenticator`](crate::AuthNResolverBearerAuthenticator).
///
/// ```no_run
/// # use std::sync::Arc;
/// use authn_resolver_sdk::{
///     AuthNResolverClient, CachingAuthenticator, CachingAuthenticatorConfig,
///     CachingAuthenticatorError,
/// };
/// use toolkit_security::DynBearerAuthenticator;
/// # use toolkit_security::SecurityContext;
/// # /// Stand-in for a downstream call: `true` when it rejected the context.
/// # async fn call_downstream_rejected(_: &SecurityContext) -> bool { false }
///
/// # async fn example(
/// #     resolver: Arc<dyn AuthNResolverClient>,
/// #     token: &str,
/// # ) -> Result<(), Box<dyn std::error::Error>> {
/// let authenticator = CachingAuthenticator::builder(resolver)
///     .with_config(CachingAuthenticatorConfig::default()) // optional
///     .build()?;
/// let result = authenticator.authenticate(token).await?;
/// if call_downstream_rejected(&result.security_context).await {
///     let _dropped = authenticator.invalidate(&result);
/// }
/// match authenticator.authenticate(token).await {
///     Ok(_result) => {}
///     Err(CachingAuthenticatorError::Overloaded { .. }) => { /* retry later: 503 */ }
///     Err(other) => return Err(other.into()),
/// }
///
/// // Or hand it to transport middleware:
/// let bearer = DynBearerAuthenticator::new(authenticator.clone());
/// # let _ = bearer;
/// authenticator.shutdown().await;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct CachingAuthenticator {
    handle: Arc<Handle>,
}

/// Owned only by public handles; validation tasks hold [`Inner`], so dropping
/// the last public handle is observable here.
struct Handle {
    inner: Arc<Inner>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.inner.lock_state().stop();
    }
}

struct Inner {
    id: u64,
    client: Arc<dyn AuthNResolverClient>,
    config: CachingAuthenticatorConfig,
    wall_clock: WallClock,
    state: Mutex<State>,
    /// Validation tasks not yet ended, including cancelled ones. Bounded by
    /// `max_in_flight`; raised only under the state lock.
    running: watch::Sender<usize>,
}

#[derive(Default)]
struct State {
    /// What is retained per token; at most `max_entries` records.
    records: HashMap<TokenKey, Record>,
    /// Every record by [`Record::evict_key`]: eviction order, earliest first.
    eviction: BTreeMap<(Instant, u64), TokenKey>,
    /// Cached entries by publication id, for [`CachingAuthenticator::invalidate`].
    by_id: HashMap<u64, TokenKey>,
    flights: HashMap<TokenKey, Flight>,
    /// Flight ids (a cached entry keeps its flight's id), also used as
    /// eviction sequence numbers.
    next_id: u64,
    /// Bumped by `clear` and shutdown; a delivery (success or failure) is
    /// returned only while it is unchanged.
    epoch: u64,
    /// Whether the current run of `Overloaded` rejections was warned about;
    /// reset when a validation starts again.
    overloaded: bool,
    shut_down: bool,
}

/// A cached result or a tag retained for the next refresh.
struct Record {
    /// Plugin [`CacheTag`] of the latest result for this token.
    tag: Option<CacheTag>,
    cached: Option<Entry>,
    /// Position in [`State::eviction`]: the cached entry's deadline, or the
    /// time a hint was created, so hints go before live entries.
    evict_key: (Instant, u64),
}

struct Entry {
    id: u64,
    /// Shared with the publication's delivery; copied only outside the lock.
    result: Arc<AuthenticationResult>,
    deadline: Deadline,
    /// Shared with the publication's delivery; cleared only by `invalidate`.
    valid: Validity,
}

/// Whether one publication may still be handed out. Eviction leaves it set:
/// only an explicit invalidation of that publication fences its delivery.
type Validity = Arc<AtomicBool>;

struct Flight {
    id: u64,
    delivery: DeliverySlot,
    /// `None` between reservation and spawn.
    abort: Option<AbortHandle>,
}

#[derive(Clone)]
enum Delivery {
    Success {
        epoch: u64,
        /// `None` only without any known time bound.
        deadline: Option<Deadline>,
        valid: Validity,
        result: Arc<AuthenticationResult>,
    },
    Failure {
        /// [`State::epoch`] when published: fenced like a success. After
        /// `clear` its waiters validate again; after shutdown they get
        /// `ShutDown`.
        epoch: u64,
        error: Arc<AuthNResolverError>,
    },
}

enum Step {
    Ready(AuthenticatorOutcome),
    /// A cached result, copied by the caller once the state lock is released.
    Hit(Arc<AuthenticationResult>),
    Wait(DeliverySlot),
}

/// A flight reserved under the state lock, spawned once it is released:
/// `tokio::spawn` may drop the task synchronously, and [`FlightGuard`]'s
/// `Drop` takes that lock.
struct Launch {
    guard: FlightGuard,
    request: AuthenticationRequest,
}

/// Settles the flight and the running count on task exit, including panic
/// and cancellation, so waiters are never stranded.
struct FlightGuard {
    inner: Arc<Inner>,
    key: TokenKey,
    id: u64,
    tx: watch::Sender<Option<Delivery>>,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        // Completed and cancelled flights are already gone; only panic remains.
        self.inner.complete(
            self.key,
            self.id,
            Err(AuthNResolverError::Internal(
                "token validation ended unexpectedly".to_owned(),
            )),
            &self.tx,
        );
        self.inner.running.send_modify(|count| *count -= 1);
    }
}

/// Configures a [`CachingAuthenticator`]. Created by
/// [`CachingAuthenticator::builder`]; nothing is spawned or validated until
/// the first [`CachingAuthenticator::authenticate`].
#[must_use]
pub struct CachingAuthenticatorBuilder {
    client: Arc<dyn AuthNResolverClient>,
    config: CachingAuthenticatorConfig,
}

impl CachingAuthenticatorBuilder {
    /// Replace [`CachingAuthenticatorConfig::default`].
    pub fn with_config(mut self, config: CachingAuthenticatorConfig) -> Self {
        self.config = config;
        self
    }

    /// Create the authenticator.
    ///
    /// # Errors
    ///
    /// [`CachingAuthenticatorConfigError`] if the configuration is invalid.
    pub fn build(self) -> Result<CachingAuthenticator, CachingAuthenticatorConfigError> {
        CachingAuthenticator::from_parts(
            self.client,
            self.config,
            Arc::new(OffsetDateTime::now_utc),
        )
    }
}

impl fmt::Debug for CachingAuthenticatorBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachingAuthenticatorBuilder")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl CachingAuthenticator {
    /// Start configuring an authenticator over `client`.
    pub fn builder(client: Arc<dyn AuthNResolverClient>) -> CachingAuthenticatorBuilder {
        CachingAuthenticatorBuilder {
            client,
            config: CachingAuthenticatorConfig::default(),
        }
    }

    fn from_parts(
        client: Arc<dyn AuthNResolverClient>,
        config: CachingAuthenticatorConfig,
        wall_clock: WallClock,
    ) -> Result<Self, CachingAuthenticatorConfigError> {
        config.validate()?;
        let inner = Inner {
            id: next_cache_id(),
            client,
            config,
            wall_clock,
            state: Mutex::new(State::default()),
            running: watch::Sender::new(0),
        };
        Ok(Self {
            handle: Arc::new(Handle {
                inner: Arc::new(inner),
            }),
        })
    }

    /// Validate `token` (without the `Bearer ` prefix), reusing a cached
    /// result when its producer allowed it.
    ///
    /// # Errors
    ///
    /// [`CachingAuthenticatorError::Resolver`] when validation fails or the
    /// credential is known to have expired;
    /// [`CachingAuthenticatorError::Overloaded`] when a new validation is
    /// needed but none can start; [`CachingAuthenticatorError::ShutDown`]
    /// after [`Self::shutdown`].
    ///
    /// # Panics
    ///
    /// When a validation has to start outside a Tokio runtime, or when the
    /// platform crypto backend is unavailable to hash the token.
    pub async fn authenticate(
        &self,
        token: &str,
    ) -> Result<AuthenticationResult, CachingAuthenticatorError> {
        let inner = &self.handle.inner;
        let key = TokenKey::of(token);
        for round in 1..=MAX_EVALUATIONS {
            let slot = match inner.next_step(key, token) {
                Step::Ready(outcome) => return outcome,
                Step::Hit(result) => return Ok(AuthenticationResult::clone(&result)),
                Step::Wait(slot) => slot,
            };
            if let Some(outcome) = inner.await_delivery(slot).await {
                return outcome;
            }
            tracing::debug!(
                authenticator = inner.id,
                round,
                "token validation result discarded before collection"
            );
        }
        // A shutdown also discards the last flight; it stays permanent.
        if inner.lock_state().shut_down {
            return Err(CachingAuthenticatorError::ShutDown);
        }
        Err(CachingAuthenticatorError::Resolver(
            AuthNResolverError::ServiceUnavailable(
                "token validation results kept being discarded before collection".to_owned(),
            ),
        ))
    }

    /// Drop the matching resident entry and discard its uncollected deliveries.
    /// Returns `true` even if the deadline passed but the entry is still resident.
    /// Returns `false` for foreign, never-cached, replaced or removed results.
    #[must_use]
    pub fn invalidate(&self, result: &AuthenticationResult) -> bool {
        let inner = &self.handle.inner;
        let Some(origin) = result.origin().filter(|origin| origin.cache() == inner.id) else {
            return false;
        };
        let mut state = inner.lock_state();
        let Some(key) = state.by_id.get(&origin.slot()).copied() else {
            return false;
        };
        let Some(entry) = state.demote(key, Instant::now()) else {
            return false;
        };
        entry.valid.store(false, Ordering::Release);
        true
    }

    /// Discard all entries, tags and pending deliveries, and cancel validations.
    /// Affected callers retry with an unconditional refresh.
    pub fn clear(&self) {
        self.handle.inner.lock_state().clear();
    }

    /// Stop the authenticator: drop the cache, cancel validations and wait
    /// until no validation task is left. Later calls fail with
    /// [`CachingAuthenticatorError::ShutDown`]. Safe to repeat.
    pub async fn shutdown(&self) {
        let inner = &self.handle.inner;
        let mut running = inner.running.subscribe();
        inner.lock_state().stop();
        drop(running.wait_for(|count| *count == 0).await);
    }
}

impl fmt::Debug for CachingAuthenticator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = &self.handle.inner;
        f.debug_struct("CachingAuthenticator")
            .field("id", &inner.id)
            .field("config", &inner.config)
            .finish_non_exhaustive()
    }
}

impl BearerAuthenticator for CachingAuthenticator {
    /// [`CachingAuthenticator::authenticate`], mapped like
    /// [`AuthNResolverBearerAuthenticator`](crate::AuthNResolverBearerAuthenticator);
    /// `Overloaded` and `ShutDown` map to [`AuthNError::Unavailable`].
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        Self::authenticate(self, token)
            .await
            .map(|result| result.security_context)
            .map_err(|error| match error {
                CachingAuthenticatorError::Resolver(error) => map_error(&error),
                CachingAuthenticatorError::Overloaded { .. }
                | CachingAuthenticatorError::ShutDown => AuthNError::Unavailable,
            })
    }
}

impl State {
    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    fn remove_record(&mut self, key: TokenKey) -> Option<Record> {
        let record = self.records.remove(&key)?;
        self.eviction.remove(&record.evict_key);
        if let Some(entry) = &record.cached {
            self.by_id.remove(&entry.id);
        }
        Some(record)
    }

    /// Store `record`, evicting the records that come first in eviction
    /// order while `max_entries` are held. Evicted tags are forgotten.
    fn insert_record(&mut self, key: TokenKey, record: Record, max_entries: usize) {
        self.remove_record(key);
        while self.records.len() >= max_entries {
            let Some((_, victim)) = self.eviction.pop_first() else {
                break;
            };
            self.remove_record(victim);
        }
        self.eviction.insert(record.evict_key, key);
        if let Some(entry) = &record.cached {
            self.by_id.insert(entry.id, key);
        }
        self.records.insert(key, record);
    }

    /// Turn the cached entry for `key` into a refresh hint (or forget the
    /// record when it has no tag). Returns the dropped entry.
    fn demote(&mut self, key: TokenKey, now: Instant) -> Option<Entry> {
        let mut record = self.remove_record(key)?;
        let entry = record.cached.take();
        if record.tag.is_some() {
            record.evict_key = (now, self.next_id());
            self.eviction.insert(record.evict_key, key);
            self.records.insert(key, record);
        }
        entry
    }

    /// Refresh unconditionally when the tag is unknown. An evicted record may have been rejected.
    fn request_for(&self, key: TokenKey) -> RequestCacheControl {
        RequestCacheControl::Refresh {
            replacing: self.records.get(&key).and_then(|record| record.tag.clone()),
        }
    }

    fn clear(&mut self) {
        self.records.clear();
        self.eviction.clear();
        self.by_id.clear();
        self.epoch = self.epoch.wrapping_add(1);
        // A task not spawned yet has no handle; `launch` aborts it on finding
        // its flight gone.
        for flight in self.flights.drain().map(|(_, flight)| flight) {
            if let Some(abort) = flight.abort {
                abort.abort();
            }
        }
    }

    fn stop(&mut self) {
        self.shut_down = true;
        self.clear();
    }
}

impl Inner {
    /// The state lock. A panic under it shuts the authenticator down for
    /// good: see [`lock_or_terminate`].
    fn lock_state(&self) -> MutexGuard<'_, State> {
        lock_or_terminate(&self.state, State::stop)
    }

    fn next_step(self: &Arc<Self>, key: TokenKey, token: &str) -> Step {
        let (step, launch) = self.poll(key, token);
        if let Some(launch) = launch {
            self.launch(launch);
        }
        step
    }

    fn poll(self: &Arc<Self>, key: TokenKey, token: &str) -> (Step, Option<Launch>) {
        let mut state = self.lock_state();
        if state.shut_down {
            return (Step::Ready(Err(CachingAuthenticatorError::ShutDown)), None);
        }
        let now = Instant::now();
        if let Some(entry) = state
            .records
            .get(&key)
            .and_then(|record| record.cached.as_ref())
        {
            if entry.deadline.usable(now, (self.wall_clock)()) {
                return (Step::Hit(Arc::clone(&entry.result)), None);
            }
            // Expired locally (deadline or `max_ttl`): keep only the tag.
            state.demote(key, now);
        }
        if let Some(flight) = state.flights.get(&key) {
            return (Step::Wait(flight.delivery.clone()), None);
        }
        let max_in_flight = self.config.max_in_flight;
        if *self.running.borrow() >= max_in_flight {
            let episode_starts = !std::mem::replace(&mut state.overloaded, true);
            drop(state);
            if episode_starts {
                tracing::warn!(
                    authenticator = self.id,
                    max_in_flight,
                    "token validations saturated: new tokens are rejected until one ends"
                );
            }
            tracing::debug!(
                authenticator = self.id,
                max_in_flight,
                "token validation rejected: all validations in use"
            );
            let error = CachingAuthenticatorError::Overloaded { max_in_flight };
            return (Step::Ready(Err(error)), None);
        }
        state.overloaded = false;
        let (slot, launch) = self.reserve_flight(&mut state, key, token);
        (Step::Wait(slot), Some(launch))
    }

    fn reserve_flight(
        self: &Arc<Self>,
        state: &mut State,
        key: TokenKey,
        token: &str,
    ) -> (DeliverySlot, Launch) {
        let id = state.next_id();
        let request = AuthenticationRequest::new(SecretString::from(token))
            .with_cache_control(state.request_for(key));
        let (tx, rx) = watch::channel(None);
        self.running.send_modify(|count| *count += 1);
        state.flights.insert(
            key,
            Flight {
                id,
                delivery: rx.clone(),
                abort: None,
            },
        );
        // Construct before spawn so even a never-polled task drops the guard.
        let guard = FlightGuard {
            inner: Arc::clone(self),
            key,
            id,
            tx,
        };
        let launch = Launch { guard, request };
        (rx, launch)
    }

    /// Spawn a reserved flight. Must not be called with the state lock held.
    fn launch(&self, launch: Launch) {
        let Launch { guard, request } = launch;
        let (key, id) = (guard.key, guard.id);
        let validation = async move {
            let guard = guard;
            let inner = &guard.inner;
            let outcome = tokio::time::timeout(
                inner.config.acquisition_timeout,
                inner.client.authenticate(&request),
            )
            .await
            .unwrap_or_else(|_| {
                // Once per flight, however many callers wait for it.
                tracing::warn!(
                    authenticator = inner.id,
                    timeout = ?inner.config.acquisition_timeout,
                    "token validation timed out"
                );
                Err(AuthNResolverError::ServiceUnavailable(
                    "token validation timed out".to_owned(),
                ))
            });
            // The token is needed no longer than the validation.
            drop(request);
            inner.complete(guard.key, guard.id, outcome, &guard.tx);
        };
        let task = spawn_in_caller_context(validation);
        let mut state = self.lock_state();
        match state.flights.get_mut(&key) {
            Some(flight) if flight.id == id => flight.abort = Some(task.abort_handle()),
            // Cancelled or completed since reservation. Aborting a finished
            // task is harmless.
            _ => task.abort(),
        }
    }

    /// Wait for the flight behind `slot`; `None` means evaluate again (the
    /// flight was cancelled, or its result was fenced or expired before this
    /// waiter collected it).
    async fn await_delivery(&self, mut slot: DeliverySlot) -> Option<AuthenticatorOutcome> {
        let delivery = slot.wait_for(Option::is_some).await.ok()?.clone()?;
        self.accept(&delivery)
    }

    fn accept(&self, delivery: &Delivery) -> Option<AuthenticatorOutcome> {
        // Decide under the lock, so the decision is one instant against
        // `clear` and clock reads; copy the result or error after releasing it.
        let state = self.lock_state();
        if state.shut_down {
            return Some(Err(CachingAuthenticatorError::ShutDown));
        }
        let eligible = match delivery {
            Delivery::Failure { epoch, .. } => *epoch == state.epoch,
            Delivery::Success {
                epoch,
                deadline,
                valid,
                ..
            } => {
                *epoch == state.epoch
                    && valid.load(Ordering::Acquire)
                    && deadline
                        .is_none_or(|deadline| deadline.usable(Instant::now(), (self.wall_clock)()))
            }
        };
        drop(state);
        eligible.then(|| match delivery {
            Delivery::Failure { error, .. } => {
                Err(CachingAuthenticatorError::Resolver(clone_error(error)))
            }
            Delivery::Success { result, .. } => Ok(AuthenticationResult::clone(result)),
        })
    }

    /// Record the outcome of flight `id` and hand it to its waiters, unless
    /// the flight is no longer current (cancelled, or already completed).
    fn complete(
        &self,
        key: TokenKey,
        id: u64,
        outcome: Outcome,
        tx: &watch::Sender<Option<Delivery>>,
    ) {
        let mut state = self.lock_state();
        if state.flights.get(&key).map(|flight| flight.id) != Some(id) {
            return;
        }
        state.flights.remove(&key);
        let delivery = match outcome {
            Ok(result) => self.publish(&mut state, key, id, result),
            Err(error) => Delivery::Failure {
                epoch: state.epoch,
                error: Arc::new(error),
            },
        };
        tx.send_replace(Some(delivery));
    }

    fn publish(
        &self,
        state: &mut State,
        key: TokenKey,
        id: u64,
        mut result: AuthenticationResult,
    ) -> Delivery {
        let wall = (self.wall_clock)();
        let now = Instant::now();
        let expires_at = result.security_context.expires_at();
        if expires_at.is_some_and(|expires_at| expires_at <= wall) {
            return Delivery::Failure {
                epoch: state.epoch,
                error: Arc::new(AuthNResolverError::Unauthorized(
                    "credential expired".to_owned(),
                )),
            };
        }
        if result
            .cache_control
            .cache_until
            .is_some_and(|until| until <= wall)
        {
            // The producer's reuse window has closed: deliver as no-store.
            result.cache_control.cache_until = None;
        }
        let result = Arc::new(result.with_origin(CacheOrigin::new(self.id, id, 0)));
        let valid: Validity = Arc::new(AtomicBool::new(true));
        let tag = result.cache_control.tag.clone();
        let max_entries = self.config.max_entries;
        if let Some(cache_until) = result.cache_control.cache_until {
            let max_ttl = wall + self.config.max_ttl;
            let absolute = expires_at
                .map_or(cache_until, |e| e.min(cache_until))
                .min(max_ttl);
            let deadline = Deadline::at(absolute, wall, now);
            let entry = Entry {
                id,
                result: Arc::clone(&result),
                deadline,
                valid: Arc::clone(&valid),
            };
            let record = Record {
                tag,
                cached: Some(entry),
                evict_key: (deadline.monotonic, id),
            };
            state.insert_record(key, record, max_entries);
        } else if tag.is_some() {
            // Not reusable here; remember a tag so the next validation asks
            // for a result newer than this one.
            let record = Record {
                tag,
                cached: None,
                evict_key: (now, id),
            };
            state.insert_record(key, record, max_entries);
        } else {
            state.remove_record(key);
        }
        // Current waiters may collect until credential expiry, even after the reuse deadline.
        let deadline = expires_at.map(|absolute| Deadline::at(absolute, wall, now));
        Delivery::Success {
            epoch: state.epoch,
            deadline,
            valid,
            result,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "tags_tests.rs"]
mod tags_tests;
