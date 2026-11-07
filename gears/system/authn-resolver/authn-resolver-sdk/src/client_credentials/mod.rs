//! Client-side S2S [`SecurityContext`](toolkit_security::SecurityContext) provider with caching.
//!
//! [`ClientCredentialsProvider`] caches `AuthenticationResult`s for fixed
//! client credentials, one slot per exact scope set. Create it with
//! [`ClientCredentialsProvider::builder`].
//!
//! - **Slots.** [`ClientCredentialsProvider::get`] uses the default scopes
//!   ([`ClientCredentialsProviderBuilder::with_default_scopes`], otherwise the
//!   `IdP` defaults); [`ClientCredentialsProvider::get_with_scopes`] uses an
//!   exact [`ScopeSet`]. A set equal to the default one shares its slot;
//!   subsets and supersets never do. Each slot behaves as described below on
//!   its own. [`ClientCredentialsProviderConfig::max_entries`] bounds the slots,
//!   default one included; see [`ClientCredentialsProvider::get_with_scopes`]
//!   for eviction.
//!
//! - **Reuse.** Requires [`CacheControl::cache_until`](crate::CacheControl).
//!   Ends at the earlier of that deadline and known credential expiry, minus
//!   [`ClientCredentialsProviderConfig::safety_margin`]. No-store results reach current waiters
//!   only; expired credentials are rejected. The reuse window and the margin
//!   bound only reuse: an acquisition's waiters receive its result until the
//!   credential's known expiry. A call whose result is discarded before it
//!   collects it acquires again, at most three acquisitions in total, then
//!   fails with `ServiceUnavailable` without recording a failure.
//! - **Single flight.** Callers share an acquisition bounded by
//!   [`ClientCredentialsProviderConfig::acquisition_timeout`]. Caller cancellation leaves it
//!   running; [`ClientCredentialsProvider::shutdown`] or final handle drop cancels it.
//! - **Early refresh.** The first `get` in a per-entry jittered window starts
//!   refresh and returns the usable entry. There is no periodic worker.
//!   Re-acquiring a cacheable result sends [`RequestCacheControl::Refresh`]
//!   with the plugin's tag to prevent stale plugin-cache reuse.
//! - **Failures.** Shared with the attempt's waiters; all trigger jittered
//!   exponential backoff. Only `ServiceUnavailable` retains a usable entry
//!   until its deadline; other errors discard it. Failures and recoveries are
//!   logged, as is the start of a run of reusable results too short-lived
//!   for the safety margin (each then costs one acquisition per call); any
//!   other success ends the run.
//! - **Invalidation.** [`ClientCredentialsProvider::clear`] (every slot) and
//!   [`ClientCredentialsProvider::invalidate`] (the slot that returned a
//!   result, while that result is its latest) discard the entry, cancel
//!   pending acquisition and request plugin refresh next, preserving backoff.
//!   Uncollected deliveries survive newer successes, but not clear, shutdown
//!   or a failure that discards the entry.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use rand::RngExt as _;
use secrecy::SecretString;
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::api::AuthNResolverClient;
use crate::error::AuthNResolverError;
use crate::local_cache::{
    Deadline, MAX_ENTRY_LIFETIME, MAX_EVALUATIONS, clone_error, error_kind, next_cache_id,
    spawn_in_caller_context,
};
use crate::models::{AuthenticationResult, CacheOrigin, CacheTag, RequestCacheControl};
use crate::scopes::ScopeSet;

pub use self::builder::ClientCredentialsProviderBuilder;
use self::registry::Registry;

/// Refresh in the last 20% of usable lifetime, up to another 10% earlier.
/// Draw jitter once per entry to spread independent providers' acquisitions.
const REFRESH_AHEAD_RATIO: f64 = 0.2;
const REFRESH_JITTER_RATIO: f64 = 0.1;

/// Upper bound on [`ClientCredentialsProviderConfig::max_entries`]; eviction scans every
/// dynamic slot under the registry lock.
const MAX_ENTRIES: usize = 1024;

/// Default [`ClientCredentialsProviderConfig::max_entries`]: the default slot and 16
/// other scope sets.
const DEFAULT_MAX_ENTRIES: usize = 17;

/// Slot id of the configured scopes; dynamic slots count up from 1.
const CONFIGURED_SLOT: u64 = 0;

/// Tuning for [`ClientCredentialsProvider`]. Checked by [`ClientCredentialsProviderConfig::validate`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ClientCredentialsProviderConfig {
    /// Subtracted from the reuse deadline, so a cached context is not reused
    /// moments before the credential behind it expires. It does not bound a
    /// fresh acquisition's own waiters, who need only an unexpired
    /// credential. Default 30s.
    pub safety_margin: Duration,
    /// Bound on one acquisition attempt; on expiry it fails with
    /// `ServiceUnavailable`. Default 30s.
    pub acquisition_timeout: Duration,
    /// Backoff after the first consecutive failure. Default 1s.
    ///
    /// After the n-th consecutive failure, draw uniformly from
    /// `[max(d / 2, min_backoff), d]` with
    /// `d = min(min_backoff * 2^(n - 1), max_backoff)`.
    pub min_backoff: Duration,
    /// Cap of the exponential backoff. Default 60s.
    pub max_backoff: Duration,
    /// Maximum number of scope-set slots in total, the default one included:
    /// `1..=1024`, default 17. The default slot always exists and is never
    /// evicted; `1` allows only the default scopes. A slot counts whatever it
    /// holds (an entry, a failure in backoff, a running acquisition or
    /// nothing).
    pub max_entries: usize,
}

/// Why a [`ClientCredentialsProviderConfig`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClientCredentialsProviderConfigError {
    /// `acquisition_timeout` is zero or exceeds one year.
    #[error("acquisition_timeout must be non-zero and at most one year")]
    AcquisitionTimeout,
    /// `min_backoff` is zero, `max_backoff` is below it or exceeds one year.
    #[error(
        "min_backoff must be non-zero, not exceed max_backoff, and max_backoff must be at most one year"
    )]
    Backoff,
    /// `max_entries` is zero or exceeds 1024.
    #[error("max_entries must be between 1 and 1024")]
    MaxEntries,
}

/// Why a [`ClientCredentialsProvider`] call failed.
///
/// A closed set: each variant calls for a different caller action, so adding
/// one is deliberately a breaking change.
#[derive(Debug, thiserror::Error)]
pub enum ClientCredentialsProviderError {
    /// The provider was shut down ([`ClientCredentialsProvider::shutdown`]). Permanent:
    /// retrying this provider cannot succeed.
    #[error("S2S caching provider is shut down")]
    ShutDown,
    /// A scope set other than the default one needs a slot, but every
    /// other slot is in use or backing off. Nothing was exchanged and no slot
    /// changed. Temporary (retry later) unless `max_entries` is 1, which
    /// allows only the default scopes.
    #[error("all {max_entries} scope-set cache slots are in use or backing off")]
    CapacityExhausted {
        /// The configured [`ClientCredentialsProviderConfig::max_entries`]: total
        /// slots, the default one included.
        max_entries: usize,
    },
    /// The acquisition through the resolver failed, with the resolver error as
    /// its source. Shared by every waiter of the attempt and repeated until
    /// its backoff ends; any kind may succeed on a later attempt. Only
    /// `ServiceUnavailable` keeps serving a still-usable cached result. Also
    /// `ServiceUnavailable`, for this call only, when three acquisitions in a
    /// row were discarded before the call collected them.
    #[error("S2S credential acquisition failed")]
    Resolver(#[source] AuthNResolverError),
}

impl Default for ClientCredentialsProviderConfig {
    fn default() -> Self {
        Self {
            safety_margin: Duration::from_secs(30),
            acquisition_timeout: Duration::from_secs(30),
            min_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

impl ClientCredentialsProviderConfig {
    /// Set [`Self::safety_margin`].
    #[must_use]
    pub fn with_safety_margin(mut self, margin: Duration) -> Self {
        self.safety_margin = margin;
        self
    }

    /// Set [`Self::acquisition_timeout`].
    #[must_use]
    pub fn with_acquisition_timeout(mut self, timeout: Duration) -> Self {
        self.acquisition_timeout = timeout;
        self
    }

    /// Set [`Self::min_backoff`] and [`Self::max_backoff`].
    #[must_use]
    pub fn with_backoff(mut self, min: Duration, max: Duration) -> Self {
        self.min_backoff = min;
        self.max_backoff = max;
        self
    }

    /// Set [`Self::max_entries`].
    #[must_use]
    pub fn with_max_entries(mut self, entries: usize) -> Self {
        self.max_entries = entries;
        self
    }

    /// Check the configuration.
    ///
    /// # Errors
    ///
    /// [`ClientCredentialsProviderConfigError`] naming the first invalid setting.
    pub fn validate(&self) -> Result<(), ClientCredentialsProviderConfigError> {
        if self.acquisition_timeout.is_zero() || self.acquisition_timeout > MAX_ENTRY_LIFETIME {
            return Err(ClientCredentialsProviderConfigError::AcquisitionTimeout);
        }
        if self.min_backoff.is_zero()
            || self.max_backoff < self.min_backoff
            || self.max_backoff > MAX_ENTRY_LIFETIME
        {
            return Err(ClientCredentialsProviderConfigError::Backoff);
        }
        if !(1..=MAX_ENTRIES).contains(&self.max_entries) {
            return Err(ClientCredentialsProviderConfigError::MaxEntries);
        }
        Ok(())
    }
}

/// Acquisition flow, independent of cache management.
#[async_trait]
pub(crate) trait Acquire: Send + Sync + 'static {
    /// Acquire for exactly `scopes`; `None` means the `IdP` defaults.
    async fn acquire(
        &self,
        scopes: Option<&ScopeSet>,
        cache_control: RequestCacheControl,
    ) -> Result<AuthenticationResult, AuthNResolverError>;
}

type WallClock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;
/// A random duration in `0..=max`. Injected so tests can pin it.
type Jitter = Arc<dyn Fn(Duration) -> Duration + Send + Sync>;
/// Outcome of one acquisition through the resolver.
type Outcome = Result<AuthenticationResult, AuthNResolverError>;
/// Outcome of a public `get` call.
type ProviderOutcome = Result<AuthenticationResult, ClientCredentialsProviderError>;

/// Outcome of one acquisition as handed to its waiters.
#[derive(Clone)]
enum Delivery {
    Success {
        /// [`State::epoch`] when published; any later invalidation fences it.
        epoch: u64,
        /// Bounds captured when published; a backwards wall-clock jump cannot
        /// extend them. No-store without credential expiry has no time bound.
        deadline: Option<Deadline>,
        result: Arc<AuthenticationResult>,
    },
    /// Shared with every waiter of the attempt, whatever happened since.
    Failure(Arc<AuthNResolverError>),
}

type DeliverySlot = watch::Receiver<Option<Delivery>>;

/// Caches `SecurityContext`s for one set of client credentials, per exact
/// scope set, and refreshes them before they expire.
///
/// Clones share the cache. Dropping the last clone cancels in-flight
/// acquisitions; [`Self::shutdown`] does the same and waits for them. Must be
/// used from within a Tokio runtime.
///
/// On downstream rejection, pass the rejected result to [`Self::invalidate`],
/// then retry. It coalesces rejections of the same result into one refresh
/// and never touches another scope set or a newer result.
///
/// ```no_run
/// # use std::sync::Arc;
/// use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsProvider, ScopeSet};
/// # use secrecy::SecretString;
/// # use toolkit_security::SecurityContext;
/// # /// Stand-in for a downstream call: `true` when it rejected the context.
/// # async fn call_downstream_rejected(_: &SecurityContext) -> bool { false }
/// # async fn example(
/// #     authn: Arc<dyn AuthNResolverClient>,
/// #     client_secret: SecretString,
/// # ) -> Result<(), Box<dyn std::error::Error>> {
///
/// let provider = ClientCredentialsProvider::builder(authn, "my-service", client_secret)
///     .with_default_scopes(ScopeSet::new(["usage:write"])?)
///     .build()?;
/// let result = provider.get().await?;
/// if call_downstream_rejected(&result.security_context).await {
///     let _invalidated = provider.invalidate(&result);
/// }
/// let read_only = provider.get_with_scopes(&ScopeSet::new(["usage:read"])?).await?;
/// # let _ = read_only;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ClientCredentialsProvider {
    handle: Arc<Handle>,
}

/// Owned only by public handles. Acquisition tasks hold their slot's
/// `Shared`, never this, so dropping the last public handle is observable here.
struct Handle {
    /// The configured slot: pinned, never evicted.
    shared: Arc<Shared>,
    /// Dynamic slots. Lock order: registry, then a slot's state.
    registry: Mutex<Registry>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        let mut registry = self.registry.lock();
        registry.closed = true;
        self.shared.state.lock().stop();
        for slot in registry.slots.values() {
            slot.shared.state.lock().stop();
        }
    }
}

/// Settings and collaborators shared by every slot of one provider.
struct Core {
    id: u64,
    acquirer: Arc<dyn Acquire>,
    config: ClientCredentialsProviderConfig,
    wall_clock: WallClock,
    jitter: Jitter,
}

/// One single-entry cache: the configured scopes or one dynamic scope set.
struct Shared {
    core: Arc<Core>,
    slot: u64,
    /// Requested scopes; `None` means the `IdP` defaults.
    scopes: Option<ScopeSet>,
    state: Mutex<State>,
    /// Number of acquisition tasks not yet ended, including cancelled ones;
    /// `shutdown` waits for zero and eviction requires it.
    running: watch::Sender<usize>,
    /// Admitted `get_with_scopes` calls not yet finished. Raised only under
    /// the registry lock, so zero observed there stays zero until released.
    pinned: AtomicUsize,
}

#[derive(Default)]
struct State {
    entry: Option<Entry>,
    flight: Option<Flight>,
    failure: Option<Failure>,
    consecutive_failures: u32,
    next_flight_id: u64,
    next_generation: u64,
    /// Generation of the latest delivered result; `None` once cleared. Gates
    /// `invalidate`.
    current_generation: Option<u64>,
    /// Bumped by every invalidation (clear, stop, a failure that drops the
    /// entry). A delivered success is returned only while it is unchanged.
    epoch: u64,
    /// Plugin tag of the latest delivered result.
    last_plugin_tag: Option<CacheTag>,
    /// What the next acquisition asks of the plugin's own cache.
    next_request: RequestCacheControl,
    /// Whether the latest success was a reusable result too short-lived for
    /// `safety_margin`, so a run of them is warned about once.
    margin_suppressed: bool,
    shut_down: bool,
}

struct Entry {
    result: AuthenticationResult,
    refresh_at: Instant,
    usable_until: Instant,
    absolute_until: OffsetDateTime,
}

impl Entry {
    fn deadline(&self) -> Deadline {
        Deadline {
            monotonic: self.usable_until,
            absolute: self.absolute_until,
        }
    }
}

struct Flight {
    id: u64,
    delivery: DeliverySlot,
    /// `None` between reservation and spawn; see [`Shared::launch`].
    abort: Option<AbortHandle>,
}

/// A flight reserved under the state lock, to be spawned once it is released:
/// `tokio::spawn` may drop the task synchronously (no runtime, or a runtime
/// shutting down), and [`FlightGuard`]'s `Drop` takes that same lock.
struct Launch {
    guard: FlightGuard,
    cache_control: RequestCacheControl,
}

struct Failure {
    error: AuthNResolverError,
    retry_at: Instant,
}

enum Step {
    Ready(ProviderOutcome),
    Wait(DeliverySlot),
}

impl ClientCredentialsProvider {
    /// Start configuring a provider for `client_id` and `client_secret`,
    /// exchanged through `authn_client`.
    pub fn builder(
        authn_client: Arc<dyn AuthNResolverClient>,
        client_id: impl Into<String>,
        client_secret: SecretString,
    ) -> ClientCredentialsProviderBuilder {
        ClientCredentialsProviderBuilder::new(authn_client, client_id.into(), client_secret)
    }

    /// Build shared cache state with a separately supplied acquisition flow,
    /// configured scopes, clock and jitter source. Private construction
    /// seam behind [`ClientCredentialsProviderBuilder::build`] and the unit tests.
    ///
    /// # Errors
    /// Returns the first invalid provider configuration setting.
    fn from_acquirer(
        acquirer: Arc<dyn Acquire>,
        configured: Option<ScopeSet>,
        config: ClientCredentialsProviderConfig,
        wall_clock: WallClock,
        jitter: Jitter,
    ) -> Result<Self, ClientCredentialsProviderConfigError> {
        config.validate()?;
        let core = Arc::new(Core {
            id: next_cache_id(),
            acquirer,
            config,
            wall_clock,
            jitter,
        });
        // The configured slot may reuse a plugin-cached result on first use.
        let shared = Shared::new(core, CONFIGURED_SLOT, configured, State::default());
        Ok(Self {
            handle: Arc::new(Handle {
                shared: Arc::new(shared),
                registry: Mutex::new(Registry::new()),
            }),
        })
    }

    /// Return a usable result for the default scopes, acquiring one if
    /// needed.
    ///
    /// # Errors
    ///
    /// [`ClientCredentialsProviderError::Resolver`] for acquisition failures, shared by
    /// waiters and repeated during backoff; [`ClientCredentialsProviderError::ShutDown`]
    /// after [`Self::shutdown`].
    ///
    /// # Panics
    ///
    /// When an acquisition has to start outside a Tokio runtime.
    pub async fn get(&self) -> Result<AuthenticationResult, ClientCredentialsProviderError> {
        self.handle.shared.get().await
    }

    /// Return a usable result for exactly `scopes`, acquiring one if needed.
    ///
    /// A set equal to the default scopes uses the [`Self::get`] slot, even
    /// when [`ClientCredentialsProviderConfig::max_entries`] is 1. Others use a
    /// dynamic slot with the same caching behaviour. A new dynamic slot's
    /// first acquisition asks the plugin to refresh, so a result rejected
    /// before the slot was evicted cannot come back from the plugin cache;
    /// this costs one plugin-cache bypass per admission.
    ///
    /// When [`ClientCredentialsProviderConfig::max_entries`] slots exist, admitting a
    /// new set evicts one dynamic slot that has no call in progress, no
    /// acquisition task (even a cancelled one still ending) and no pending
    /// backoff: first one without a usable entry, otherwise the least
    /// recently used. A slot's backoff state ends with it.
    ///
    /// # Errors
    ///
    /// As [`Self::get`]. For a set other than the default one,
    /// [`ClientCredentialsProviderError::CapacityExhausted`] when no slot can
    /// be evicted (always when `max_entries` is 1); then no slot is changed
    /// and nothing waits for capacity.
    ///
    /// # Panics
    ///
    /// When an acquisition has to start outside a Tokio runtime.
    pub async fn get_with_scopes(
        &self,
        scopes: &ScopeSet,
    ) -> Result<AuthenticationResult, ClientCredentialsProviderError> {
        let lease = self.handle.admit(scopes)?;
        lease.shared.get().await
    }

    /// Discard every slot's entry and pending deliveries; cancel any
    /// acquisition and let its waiters retry with plugin refresh. Preserves
    /// backoff.
    ///
    /// Repeated calls cancel even a refresh in progress. For concurrent
    /// rejections of one result, use [`Self::invalidate`].
    pub fn clear(&self) {
        // Holding the registry lock: no slot is admitted halfway through.
        let registry = self.handle.registry.lock();
        self.handle.shared.state.lock().clear();
        for slot in registry.slots.values() {
            slot.shared.state.lock().clear();
        }
    }

    /// [`Self::clear`] for the one slot that returned `result`, but only
    /// while `result` is the latest result it delivered. Returns whether it
    /// cleared.
    ///
    /// A no-op for a result this provider did not return (straight from the
    /// resolver, or from another provider or cache), one already replaced or
    /// invalidated, or one from an evicted slot.
    #[must_use]
    pub fn invalidate(&self, result: &AuthenticationResult) -> bool {
        result
            .origin()
            .is_some_and(|origin| self.invalidate_origin(origin))
    }

    fn invalidate_origin(&self, tag: CacheOrigin) -> bool {
        let handle = &self.handle;
        if tag.cache() != handle.shared.core.id {
            return false;
        }
        if tag.slot() == CONFIGURED_SLOT {
            return handle.shared.clear_if_current(tag);
        }
        let registry = handle.registry.lock();
        registry
            .slots
            .values()
            .find(|slot| slot.shared.slot == tag.slot())
            .is_some_and(|slot| slot.shared.clear_if_current(tag))
    }

    /// Stop the provider: close admission, drop every slot's cache, cancel
    /// in-flight acquisitions and wait until no acquisition task is left.
    /// Later [`Self::get`] and [`Self::get_with_scopes`] calls fail. Safe to
    /// call concurrently or again after a cancelled call.
    pub async fn shutdown(&self) {
        let handle = &self.handle;
        let slots: Vec<Arc<Shared>> = {
            let mut registry = handle.registry.lock();
            registry.closed = true;
            std::iter::once(Arc::clone(&handle.shared))
                .chain(registry.slots.values().map(|slot| Arc::clone(&slot.shared)))
                .collect()
        };
        // Admission is closed, so this list is final.
        let mut running: Vec<_> = slots.iter().map(|slot| slot.running.subscribe()).collect();
        for slot in &slots {
            slot.state.lock().stop();
        }
        // Each sender lives in a slot kept alive by `slots`.
        for receiver in &mut running {
            drop(receiver.wait_for(|count| *count == 0).await);
        }
    }
}

impl fmt::Debug for ClientCredentialsProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let core = &self.handle.shared.core;
        f.debug_struct("ClientCredentialsProvider")
            .field("id", &core.id)
            .field("config", &core.config)
            .finish_non_exhaustive()
    }
}

impl State {
    fn clear(&mut self) {
        self.invalidate();
        self.next_request = RequestCacheControl::Refresh {
            replacing: self.last_plugin_tag.clone(),
        };
        // Even a pending refresh may carry the rejected token. Cancel it so
        // waiters join an acquisition started after this invalidation.
        self.cancel_flight();
    }

    fn stop(&mut self) {
        self.shut_down = true;
        self.invalidate();
        self.cancel_flight();
    }

    /// Drop the entry and fence every success delivered so far.
    fn invalidate(&mut self) {
        self.entry = None;
        self.current_generation = None;
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// A flight not spawned yet has no handle; [`Shared::launch`] aborts it
    /// when it finds the flight gone.
    fn cancel_flight(&mut self) {
        if let Some(abort) = self.flight.take().and_then(|flight| flight.abort) {
            abort.abort();
        }
    }

    fn in_backoff(&self, now: Instant) -> bool {
        self.failure
            .as_ref()
            .is_some_and(|failure| now < failure.retry_at)
    }
}

/// Settles the flight and running count on task exit, including panic or
/// cancellation, so waiters cannot be stranded.
struct FlightGuard {
    shared: Arc<Shared>,
    id: u64,
    tx: watch::Sender<Option<Delivery>>,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        // Completed and cancelled flights are already removed; only panic needs settling.
        self.shared.complete(
            self.id,
            Err(AuthNResolverError::Internal(
                "S2S credential acquisition ended unexpectedly".to_owned(),
            )),
            &self.tx,
        );
        self.shared.running.send_modify(|count| *count -= 1);
    }
}

impl Shared {
    fn new(core: Arc<Core>, slot: u64, scopes: Option<ScopeSet>, state: State) -> Self {
        Self {
            core,
            slot,
            scopes,
            state: Mutex::new(state),
            running: watch::Sender::new(0),
            pinned: AtomicUsize::new(0),
        }
    }

    /// Return a usable result from this slot, acquiring one if needed.
    async fn get(self: &Arc<Self>) -> ProviderOutcome {
        for round in 1..=MAX_EVALUATIONS {
            let slot = match self.next_step() {
                Step::Ready(outcome) => return outcome,
                Step::Wait(slot) => slot,
            };
            if let Some(outcome) = self.await_delivery(slot).await {
                return outcome;
            }
            tracing::debug!(
                provider = self.core.id,
                slot = self.slot,
                round,
                "S2S credential discarded before collection"
            );
        }
        // A shutdown also discards the last flight; it stays permanent.
        if self.state.lock().shut_down {
            return Err(ClientCredentialsProviderError::ShutDown);
        }
        // Per call only: no failure is recorded and no backoff starts.
        Err(ClientCredentialsProviderError::Resolver(
            AuthNResolverError::ServiceUnavailable(
                "S2S credentials kept being discarded before collection".to_owned(),
            ),
        ))
    }

    /// Clear this slot if `tag` names the latest result it delivered.
    fn clear_if_current(&self, tag: CacheOrigin) -> bool {
        let mut state = self.state.lock();
        let matches = state
            .current_generation
            .is_some_and(|generation| tag == CacheOrigin::new(self.core.id, self.slot, generation));
        if matches {
            state.clear();
        }
        matches
    }

    /// Decide what `get` does next, spawning a flight it reserved only after
    /// the state lock is released.
    fn next_step(self: &Arc<Self>) -> Step {
        let (step, launch) = self.poll();
        if let Some(launch) = launch {
            self.launch(launch);
        }
        step
    }

    fn poll(self: &Arc<Self>) -> (Step, Option<Launch>) {
        let mut state = self.state.lock();
        if state.shut_down {
            return (
                Step::Ready(Err(ClientCredentialsProviderError::ShutDown)),
                None,
            );
        }
        let now = Instant::now();
        if let Some(entry) = &state.entry {
            if entry.deadline().usable(now, (self.core.wall_clock)()) {
                let result = entry.result.clone();
                let refresh =
                    now >= entry.refresh_at && state.flight.is_none() && !state.in_backoff(now);
                // Background early refresh; this caller keeps the entry.
                let launch = refresh.then(|| self.reserve_flight(&mut state).1);
                return (Step::Ready(Ok(result)), launch);
            }
            state.entry = None;
        }
        if let Some(flight) = &state.flight {
            return (Step::Wait(flight.delivery.clone()), None);
        }
        if let Some(failure) = &state.failure
            && now < failure.retry_at
        {
            let error = ClientCredentialsProviderError::Resolver(clone_error(&failure.error));
            return (Step::Ready(Err(error)), None);
        }
        let (slot, launch) = self.reserve_flight(&mut state);
        (Step::Wait(slot), Some(launch))
    }

    /// Wait for the flight behind `slot`; `None` means re-evaluate the state
    /// (the flight was cancelled, or its result was invalidated before this
    /// waiter resumed).
    async fn await_delivery(&self, mut slot: DeliverySlot) -> Option<ProviderOutcome> {
        let delivery = slot.wait_for(Option::is_some).await.ok()?.clone()?;
        self.accept(&delivery)
    }

    /// Whether a delivered outcome may still be returned now.
    fn accept(&self, delivery: &Delivery) -> Option<ProviderOutcome> {
        let state = self.state.lock();
        if state.shut_down {
            return Some(Err(ClientCredentialsProviderError::ShutDown));
        }
        match delivery {
            Delivery::Failure(error) => Some(Err(ClientCredentialsProviderError::Resolver(
                clone_error(error),
            ))),
            Delivery::Success {
                epoch,
                deadline,
                result,
            } => {
                let usable = deadline.is_none_or(|deadline| {
                    deadline.usable(Instant::now(), (self.core.wall_clock)())
                });
                (*epoch == state.epoch && usable && self.unexpired(result))
                    .then(|| Ok(AuthenticationResult::clone(result)))
            }
        }
    }

    fn unexpired(&self, result: &AuthenticationResult) -> bool {
        result
            .security_context
            .expires_at()
            .is_none_or(|expires_at| expires_at > (self.core.wall_clock)())
    }

    /// Record a new flight. The caller must hand the returned [`Launch`] to
    /// [`Self::launch`] after releasing the state lock.
    fn reserve_flight(self: &Arc<Self>, state: &mut State) -> (DeliverySlot, Launch) {
        let id = state.next_flight_id;
        state.next_flight_id = state.next_flight_id.wrapping_add(1);
        let (tx, rx) = watch::channel(None);
        self.running.send_modify(|count| *count += 1);
        state.flight = Some(Flight {
            id,
            delivery: rx.clone(),
            abort: None,
        });
        // Construct before spawn so even a never-polled task drops the guard.
        let guard = FlightGuard {
            shared: Arc::clone(self),
            id,
            tx,
        };
        let launch = Launch {
            guard,
            cache_control: state.next_request.clone(),
        };
        (rx, launch)
    }

    /// Spawn a reserved flight. Must not be called with the state lock held.
    fn launch(self: &Arc<Self>, launch: Launch) {
        let Launch {
            guard,
            cache_control,
        } = launch;
        let id = guard.id;
        let acquisition = async move {
            let guard = guard;
            let shared = &guard.shared;
            let outcome = tokio::time::timeout(
                shared.core.config.acquisition_timeout,
                shared
                    .core
                    .acquirer
                    .acquire(shared.scopes.as_ref(), cache_control),
            )
            .await
            .unwrap_or_else(|_| {
                Err(AuthNResolverError::ServiceUnavailable(
                    "S2S credential acquisition timed out".to_owned(),
                ))
            });
            shared.complete(guard.id, outcome, &guard.tx);
        };
        let task = spawn_in_caller_context(acquisition);
        let mut state = self.state.lock();
        match &mut state.flight {
            Some(flight) if flight.id == id => flight.abort = Some(task.abort_handle()),
            // Invalidated or completed since reservation; abort was unavailable
            // before spawn. Aborting a finished task is harmless.
            _ => task.abort(),
        }
    }

    /// Record the outcome of flight `id` and hand it to its waiters, unless
    /// the flight is no longer current (cancelled, or already completed).
    fn complete(&self, id: u64, outcome: Outcome, tx: &watch::Sender<Option<Delivery>>) {
        let (transition, margin_warning) = {
            let mut state = self.state.lock();
            if state.flight.as_ref().map(|flight| flight.id) != Some(id) {
                return;
            }
            state.flight = None;
            let (delivery, transition, margin_warning) =
                match outcome.and_then(|result| self.admit(result)) {
                    Ok(admitted) => self.publish(&mut state, admitted),
                    Err(error) => {
                        let (delivery, transition) = self.fail(&mut state, error);
                        (delivery, transition, false)
                    }
                };
            tx.send_replace(Some(delivery));
            (transition, margin_warning)
        };
        // Outside the lock: a subscriber must not run inside the critical section.
        transition.log(self.core.id, self.slot);
        if margin_warning {
            log_margin_suppressed(self.core.id, self.slot, self.core.config.safety_margin);
        }
    }

    /// Check a freshly acquired result before it is published.
    fn admit(&self, mut result: AuthenticationResult) -> Result<Admitted, AuthNResolverError> {
        if !self.unexpired(&result) {
            return Err(AuthNResolverError::TokenAcquisitionFailed(
                "acquired credential is already expired".to_owned(),
            ));
        }
        let cacheable = !result.cache_control.is_no_store();
        if result
            .cache_control
            .cache_until
            .is_some_and(|until| until <= (self.core.wall_clock)())
        {
            // The plugin's reuse window has closed but the credential is still
            // valid: deliver it to this attempt only, like a no-store result.
            result.cache_control.cache_until = None;
        }
        Ok(Admitted { result, cacheable })
    }

    /// Also returns whether a run of results too short-lived for the margin
    /// starts here, to be warned about once.
    fn publish(&self, state: &mut State, admitted: Admitted) -> (Delivery, Transition, bool) {
        let Admitted { result, cacheable } = admitted;
        let now = Instant::now();
        let transition = match state.consecutive_failures {
            0 => Transition::None,
            failures => Transition::Recovered { failures },
        };
        state.failure = None;
        state.consecutive_failures = 0;
        state.last_plugin_tag.clone_from(&result.cache_control.tag);
        // Refresh previously cacheable results even if their reuse window
        // closed; no-store results allow ordinary exchange.
        state.next_request = if cacheable {
            RequestCacheControl::Refresh {
                replacing: result.cache_control.tag.clone(),
            }
        } else {
            RequestCacheControl::AllowCached
        };
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1);
        state.current_generation = Some(generation);
        let result = result.with_origin(CacheOrigin::new(self.core.id, self.slot, generation));
        state.entry = self.entry_for(&result, now);
        // A reusable result that the margin leaves no lifetime for: every call
        // will acquire. Warn when such a run starts, not on every acquisition;
        // any other success ends the run.
        let suppressed = result.cache_control.cache_until.is_some() && state.entry.is_none();
        let margin_warning = suppressed && !state.margin_suppressed;
        state.margin_suppressed = suppressed;
        // This attempt's waiters are not reusing the result: only the
        // credential's expiry bounds their collection. The reuse window and
        // the safety margin bound the entry, so a result too short for the
        // margin still reaches them.
        let deadline = self.delivery_deadline(&result, now);
        let delivery = Delivery::Success {
            epoch: state.epoch,
            deadline,
            result: Arc::new(result),
        };
        (delivery, transition, margin_warning)
    }

    fn fail(&self, state: &mut State, error: AuthNResolverError) -> (Delivery, Transition) {
        let now = Instant::now();
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let serving_previous = matches!(error, AuthNResolverError::ServiceUnavailable(_))
            && state
                .entry
                .as_ref()
                .is_some_and(|entry| entry.deadline().usable(now, (self.core.wall_clock)()));
        if !serving_previous {
            state.invalidate();
        }
        let retry_in = self.backoff(state.consecutive_failures);
        state.failure = Some(Failure {
            error: clone_error(&error),
            retry_at: now + retry_in,
        });
        let transition = Transition::Failed {
            kind: error_kind(&error),
            consecutive_failures: state.consecutive_failures,
            retry_in,
            serving_previous,
        };
        (Delivery::Failure(Arc::new(error)), transition)
    }

    /// The cache entry for `result`, or `None` when it must not be reused.
    fn entry_for(&self, result: &AuthenticationResult, now: Instant) -> Option<Entry> {
        let mut deadline = result.cache_control.cache_until?;
        if let Some(expires_at) = result.security_context.expires_at() {
            deadline = deadline.min(expires_at);
        }
        let remaining = Duration::try_from(deadline - (self.core.wall_clock)()).ok()?;
        let lifetime = remaining
            .checked_sub(self.core.config.safety_margin)?
            .min(MAX_ENTRY_LIFETIME);
        if lifetime.is_zero() {
            return None;
        }
        // The built-in ahead + jitter ratios are below 1: a fresh entry
        // does not ask for refresh immediately.
        let jitter = self.jitter(lifetime.mul_f64(REFRESH_JITTER_RATIO));
        let refresh_after = lifetime
            .mul_f64(1.0 - REFRESH_AHEAD_RATIO)
            .saturating_sub(jitter);
        Some(Entry {
            result: result.clone(),
            refresh_at: now + refresh_after,
            usable_until: now + lifetime,
            absolute_until: deadline - self.core.config.safety_margin,
        })
    }

    /// The known credential expiry as a [`Deadline`]; `None` when unknown.
    fn delivery_deadline(&self, result: &AuthenticationResult, now: Instant) -> Option<Deadline> {
        let absolute = result.security_context.expires_at()?;
        let remaining = Duration::try_from(absolute - (self.core.wall_clock)()).unwrap_or_default();
        Some(Deadline {
            monotonic: now + remaining.min(MAX_ENTRY_LIFETIME),
            absolute,
        })
    }

    /// See [`ClientCredentialsProviderConfig::min_backoff`] for the distribution.
    fn backoff(&self, failures: u32) -> Duration {
        let max = self.core.config.max_backoff;
        // In nanoseconds, so only the duration itself, never an intermediate
        // factor, decides when the cap is reached.
        let ceiling = 1u128
            .checked_shl(failures.saturating_sub(1))
            .and_then(|factor| self.core.config.min_backoff.as_nanos().checked_mul(factor))
            .filter(|nanos| *nanos < max.as_nanos())
            .and_then(|nanos| u64::try_from(nanos).ok())
            .map_or(max, Duration::from_nanos);
        let floor = (ceiling / 2).max(self.core.config.min_backoff).min(ceiling);
        ceiling.saturating_sub(self.jitter(ceiling.saturating_sub(floor)))
    }

    fn jitter(&self, max: Duration) -> Duration {
        (self.core.jitter)(max).min(max)
    }
}

/// A result that passed [`Shared::admit`].
struct Admitted {
    result: AuthenticationResult,
    /// Whether the plugin marked it reusable, even if that window has closed.
    cacheable: bool,
}

/// What an acquisition changed, logged once the state lock is released.
enum Transition {
    None,
    Failed {
        kind: &'static str,
        consecutive_failures: u32,
        retry_in: Duration,
        serving_previous: bool,
    },
    Recovered {
        failures: u32,
    },
}

impl Transition {
    fn log(self, provider: u64, slot: u64) {
        match self {
            Self::None => {}
            Self::Failed {
                kind,
                consecutive_failures,
                retry_in,
                serving_previous,
            } => tracing::warn!(
                provider,
                slot,
                error_kind = kind,
                consecutive_failures,
                ?retry_in,
                serving_previous,
                "S2S credential acquisition failed"
            ),
            Self::Recovered { failures } => tracing::info!(
                provider,
                slot,
                after_failures = failures,
                "S2S credential acquisition recovered"
            ),
        }
    }
}

/// A reusable result is too short-lived for `safety_margin`: it is not cached
/// and every call acquires until another kind of success arrives.
fn log_margin_suppressed(provider: u64, slot: u64, safety_margin: Duration) {
    tracing::warn!(
        provider,
        slot,
        ?safety_margin,
        "S2S credential lifetime is within safety_margin: not cached, every call acquires"
    );
}

fn random_jitter(max: Duration) -> Duration {
    let max_nanos = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(rand::rng().random_range(0..=max_nanos))
}

mod builder;

mod registry;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "mod_tests.rs"]
mod tests;
