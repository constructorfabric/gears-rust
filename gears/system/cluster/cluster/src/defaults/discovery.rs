//! The cache-based default service-discovery backend over
//! `Arc<dyn ClusterCacheBackend>`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::defaults::{SVC_KEY_PREFIX, ShutdownRevoke, identity};
use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::cache::{CacheEvent, CacheWatch, CacheWatchEvent, ClusterCacheBackend};
use cluster_sdk::discovery::{
    DiscoveryFilter, InstanceState, ServiceCommandReceiver, ServiceDiscoveryBackend,
    ServiceDiscoveryFeatures, ServiceHandle, ServiceInstance, ServiceRegistration, ServiceRequest,
    ServiceWatch, ServiceWatchEvent, ServiceWatchSender, TopologyChange,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::observability::{self, ClusterMetrics, NoopMetrics, result, spans};

/// Records the metric side of a finished discovery op: a bounded-`result`
/// counter and the shared provider-error signals. Discovery has no latency
/// histogram in the contract, so no duration is recorded.
fn record_discovery<T>(
    metrics: &dyn ClusterMetrics,
    provider: &'static str,
    op: &'static str,
    name: &str,
    outcome: &Result<T, ClusterError>,
) {
    metrics.discovery_op(op, result::label(outcome));
    if let Err(err) = outcome {
        observability::emit_provider_error(
            metrics,
            provider,
            op,
            observability::ResourceId::Name(name),
            err,
        );
    }
}

/// Default registration TTL — an instance that stops heartbeating disappears
/// within this window.
const DEFAULT_TTL: Duration = Duration::from_secs(30);

/// Default heartbeat cadence. SD has no `max_missed_renewals` knob, so this is
/// the leader-election renewal formula `ttl / (max_missed_renewals + 1)` pinned
/// at the leader default of `max_missed_renewals = 2` — i.e. `ttl / 3`. Derived
/// from [`DEFAULT_TTL`] so the two stay in lockstep; keep in sync with the
/// renewal derivation in `leader.rs`.
#[allow(
    clippy::integer_division,
    reason = "exact 30/3 = 10s; deriving from DEFAULT_TTL keeps the cadence in lockstep with the TTL"
)]
const DEFAULT_RENEWAL: Duration = Duration::from_secs(DEFAULT_TTL.as_secs() / 3);

/// The in-flight command buffer for each [`ServiceHandle`].
const COMMAND_BUFFER: usize = 4;

/// The in-flight event buffer for each [`ServiceWatch`].
const WATCH_CAPACITY: usize = 256;

/// A service-discovery backend that derives set-membership discovery from cache
/// operations (DESIGN §3.11, ADR-001).
///
/// Each instance is stored under a per-instance key `"svc/{name}/{instance_id}"`
/// with a heartbeat TTL (`put`); a background task renews it on
/// [`DEFAULT_RENEWAL`] so a live instance stays discoverable and a crashed one
/// lapses within the TTL. The `svc/` namespace (ADR-001) keeps SD keys from
/// colliding with the leader (`election/`) and lock (`lock/`) keyspaces when an
/// omit-primitive profile shares one cache across all four defaults. Discovery
/// is served from a backend-maintained membership view fed by a
/// `watch_prefix("svc/{name}/")` subscription, and
/// [`watch`](ServiceDiscoveryBackend::watch) translates that prefix stream into
/// [`TopologyChange`] events. Metadata filtering is **client-side** —
/// [`features`](ServiceDiscoveryBackend::features) reports
/// `metadata_pushdown == false`.
///
/// # Consistency (ADR-009)
///
/// Unlike the leader-election and lock defaults, service discovery imposes **no
/// consistency guard**: set-membership tolerates transient staleness, so the
/// constructor is a single infallible [`new`](Self::new). A freshly started
/// membership view converges to full cross-process membership within one
/// heartbeat interval (every live instance re-`put`s on its heartbeat, surfacing
/// as a `watch_prefix` change), which is the bounded staleness this primitive
/// accepts.
///
/// # Prefix-watch dependency
///
/// Discovery and watch require the cache's native `watch_prefix`. A cache that
/// declares no prefix watch returns
/// [`ClusterError::Unsupported`] from `watch_prefix`, so
/// [`watch`](ServiceDiscoveryBackend::watch) surfaces that error and
/// [`discover`](ServiceDiscoveryBackend::discover) is **best-effort**: with no
/// maintainer to reap entries on TTL expiry, [`register`](ServiceDiscoveryBackend::register)
/// deliberately skips its local pre-insert (an unreapable pre-insert would read
/// as live forever), so degraded-mode `discover` returns an empty set rather
/// than a stale view. This is a known limitation pending the
/// `PollingPrefixWatch` polyfill (DECOMPOSITION §2.7), which is out of scope
/// here.
pub struct CacheBasedServiceDiscoveryBackend {
    cache: Arc<dyn ClusterCacheBackend>,
    shared: Arc<Shared>,
    ttl: Duration,
    renewal: Duration,
    /// Cancelled by [`ShutdownRevoke::revoke`] on graceful shutdown (DESIGN
    /// §3.13): signals every in-flight [`watch`](ServiceDiscoveryBackend::watch)
    /// translator to close its stream terminally, every [`HeartbeatTask`] to stop
    /// renewing and exit, and every store-maintainer task to stop maintaining its
    /// view and exit. Registered entries still lapse via TTL.
    shutdown: CancellationToken,
    /// Handles of the spawned [`WatchTranslator`], [`HeartbeatTask`], and
    /// store-maintainer tasks, so `revoke` can await their cancellation. Finished
    /// handles are pruned as new
    /// tasks are tracked.
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// The bounded `provider` label for emitted signals (default `"unknown"`
    /// until set via [`with_observability`](Self::with_observability)).
    provider: &'static str,
    /// The metrics sink (default [`NoopMetrics`]).
    metrics: Arc<dyn ClusterMetrics>,
}

impl CacheBasedServiceDiscoveryBackend {
    /// Creates a service-discovery backend over `cache` with the default
    /// heartbeat timing.
    #[must_use]
    pub fn new(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        Self {
            cache,
            shared: Arc::new(Shared {
                registry: Mutex::new(Registry::default()),
            }),
            ttl: DEFAULT_TTL,
            renewal: DEFAULT_RENEWAL,
            shutdown: CancellationToken::new(),
            tasks: Arc::new(Mutex::new(Vec::new())),
            provider: "unknown",
            metrics: Arc::new(NoopMetrics),
        }
    }

    /// Sets the `provider` label and metrics sink the backend emits through.
    ///
    /// Called by the wrapping plugin so emitted signals carry the deployment's
    /// provider name (ADR-004). Without it, signals use `provider = "unknown"`
    /// and a no-op sink.
    #[must_use]
    pub fn with_observability(
        mut self,
        provider: &'static str,
        metrics: Arc<dyn ClusterMetrics>,
    ) -> Self {
        self.provider = provider;
        self.metrics = metrics;
        self
    }

    /// Records a spawned watch-translator or heartbeat task's handle, pruning any
    /// that have already finished so the set stays bounded across many
    /// short-lived watches and registrations.
    fn track(&self, handle: JoinHandle<()>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|handle| !handle.is_finished());
        tasks.push(handle);
    }

    fn prefix(name: &str) -> String {
        format!("{SVC_KEY_PREFIX}{name}/")
    }

    fn instance_key(name: &str, instance_id: &str) -> String {
        format!("{SVC_KEY_PREFIX}{name}/{instance_id}")
    }

    /// Ensures a membership-maintainer task is running for `name` so
    /// [`discover`](ServiceDiscoveryBackend::discover) is backed by a converging
    /// view.
    ///
    /// # Errors
    /// Propagates the cache's [`ClusterError::Unsupported`] when it has no native
    /// prefix watch.
    async fn ensure_maintainer(&self, name: &str) -> Result<MaintainerStatus, ClusterError> {
        {
            let mut registry = self.shared.lock();
            match registry.maintained.get(name) {
                // A maintainer is confirmed live, so a local pre-insert is safe.
                Some(MaintainerState::Active) => return Ok(MaintainerStatus::Live),
                // Another caller is mid-startup: its `watch_prefix` await is still
                // in flight and may yet fail and roll back. Don't double-spawn, and
                // don't report the view as maintained — a concurrent caller that
                // pre-inserts off an unconfirmed maintainer would leak an
                // unreapable entry if that startup then fails. The in-flight
                // caller's maintainer, once live, reconciles from backend truth and
                // picks up any prior `put`.
                Some(MaintainerState::Pending) => return Ok(MaintainerStatus::Pending),
                None => {}
            }
            registry
                .maintained
                .insert(name.to_owned(), MaintainerState::Pending);
            registry.instances.entry(name.to_owned()).or_default();
        }
        let prefix = Self::prefix(name);
        let watch = match self.cache.watch_prefix(&prefix).await {
            Ok(watch) => watch,
            Err(err) => {
                // Roll back the mark so a later call can retry once the cache
                // gains prefix-watch support.
                self.shared.lock().maintained.remove(name);
                return Err(err);
            }
        };
        let task = StoreMaintainer {
            cache: Arc::clone(&self.cache),
            shared: Arc::downgrade(&self.shared),
            name: name.to_owned(),
            prefix,
            shutdown: self.shutdown.clone(),
        };
        // Confirm the slot live before spawning so a concurrent caller can
        // pre-insert safely; the task clears the slot itself on exit.
        self.shared
            .lock()
            .maintained
            .insert(name.to_owned(), MaintainerState::Active);
        // Tracked like the watch-translator / heartbeat tasks so `revoke` tears it
        // down deterministically rather than leaving it to lapse via TTL.
        self.track(tokio::spawn(task.run(watch)));
        Ok(MaintainerStatus::Live)
    }
}

#[async_trait]
impl ServiceDiscoveryBackend for CacheBasedServiceDiscoveryBackend {
    fn features(&self) -> ServiceDiscoveryFeatures {
        // Metadata filtering is applied client-side via `DiscoveryFilter::matches`.
        ServiceDiscoveryFeatures::new(false)
    }

    async fn register(&self, reg: ServiceRegistration) -> Result<ServiceHandle, ClusterError> {
        let instance_id = reg.instance_id.clone().unwrap_or_else(identity::fresh_id);
        // Captured before the async block moves `reg`, so the metric recorder
        // below can name the service.
        let service_name = reg.name.clone();
        let span = tracing::info_span!(
            spans::DISCOVERY_REGISTER,
            provider = %self.provider,
            name = %reg.name,
            instance_id = %instance_id
        );
        let out = async {
            let key = Self::instance_key(&reg.name, &instance_id);
            let record = InstanceRecord {
                address: reg.address,
                metadata: reg.metadata,
                state: InstanceState::Enabled,
                registered_at: SystemTime::now(),
            };
            let encoded = record.encode();
            self.cache
                .put(PutRequest {
                    key: &key,
                    value: &encoded,
                    ttl: Ttl::Of(self.ttl),
                })
                .await?;
            // A cache without native prefix watch cannot maintain a cross-process
            // view, so discovery degrades to best-effort (DECOMPOSITION §2.7).
            if matches!(
                self.ensure_maintainer(&reg.name).await,
                Ok(MaintainerStatus::Live)
            ) {
                // Maintainer is running: pre-insert so this process's own `discover`
                // observes the registration immediately (the maintainer subscribed
                // only after the `put` above). While the maintainer stays alive it
                // reaps the entry on TTL expiry / deregistration via the prefix-watch
                // stream; if the maintainer task ends, a later `register`/`discover`
                // restarts one, which reconciles the view from a `scan_prefix` sweep
                // of backend truth (dropping any entries stranded by the old task).
                let instance = record.to_instance(instance_id.clone());
                let mut registry = self.shared.lock();
                registry
                    .instances
                    .entry(reg.name.clone())
                    .or_default()
                    .insert(instance_id.clone(), instance);
            }
            // No confirmed maintainer for this process to pre-insert behind: skip
            // it. Either degraded mode (the maintainer could not start) — with
            // nothing to reap the entry on TTL expiry it would otherwise read as
            // live forever, so `discover` stays best-effort empty until prefix
            // watch is available — or a concurrent caller is still bringing the
            // maintainer up, in which case that maintainer's initial reconcile
            // picks this registration up from backend truth.
            let (receiver, handle) = ServiceHandle::channel(instance_id.clone(), COMMAND_BUFFER);
            let task = HeartbeatTask {
                cache: Arc::clone(&self.cache),
                name: reg.name.clone(),
                instance_id,
                key,
                ttl: self.ttl,
                renewal: self.renewal,
                record,
                shutdown: self.shutdown.clone(),
                provider: self.provider,
                metrics: Arc::clone(&self.metrics),
            };
            self.track(tokio::spawn(task.run(receiver)));
            Ok(handle)
        }
        .instrument(span)
        .await;
        record_discovery(
            &*self.metrics,
            self.provider,
            "register",
            &service_name,
            &out,
        );
        out
    }

    async fn discover(
        &self,
        name: &str,
        filter: DiscoveryFilter,
    ) -> Result<Vec<ServiceInstance>, ClusterError> {
        let span =
            tracing::info_span!(spans::DISCOVERY_DISCOVER, provider = %self.provider, name = %name);
        let out = async {
            // Best-effort: ensure the membership view is being maintained.
            let _maintainer = self.ensure_maintainer(name).await;
            let registry = self.shared.lock();
            let instances = registry.instances.get(name).map_or_else(Vec::new, |map| {
                map.values()
                    .filter(|instance| filter.matches(instance))
                    .cloned()
                    .collect()
            });
            Ok(instances)
        }
        .instrument(span)
        .await;
        record_discovery(&*self.metrics, self.provider, "discover", name, &out);
        out
    }

    async fn watch(&self, name: &str) -> Result<ServiceWatch, ClusterError> {
        let span =
            tracing::info_span!(spans::DISCOVERY_WATCH, provider = %self.provider, name = %name);
        let out = async {
            let prefix = Self::prefix(name);
            // Each watch gets its own prefix subscription; the cache's `Unsupported`
            // surfaces here directly.
            let cache_watch = self.cache.watch_prefix(&prefix).await?;
            let (sender, mut watch) = ServiceWatch::channel(WATCH_CAPACITY);
            // Stamp the watch so an `auto_restart`ed consumer emits the watch-reset
            // signals (`cluster_watch_resets_total` / `cluster.watch.reset`).
            watch.set_observability(self.provider, Arc::clone(&self.metrics));
            let translator = WatchTranslator {
                cache: Arc::clone(&self.cache),
                prefix,
                seen: HashSet::new(),
                shutdown: self.shutdown.clone(),
            };
            self.track(tokio::spawn(translator.run(cache_watch, sender)));
            Ok(watch)
        }
        .instrument(span)
        .await;
        record_discovery(&*self.metrics, self.provider, "watch", name, &out);
        out
    }
}

#[async_trait]
impl ShutdownRevoke for CacheBasedServiceDiscoveryBackend {
    /// Closes active service-discovery watches and stops heartbeat renewal on
    /// graceful shutdown (`cpt-cf-clst-fr-shutdown-revoke`): cancels the shared
    /// token — so every in-flight watch translator sends a terminal
    /// `ServiceWatchEvent::Closed(ClusterError::Shutdown)` and exits, every
    /// heartbeat task stops renewing and exits, and every store-maintainer task
    /// stops maintaining its view and exits — then awaits those tracked tasks, so
    /// an active watcher has observed the close before this returns. Registered
    /// entries still lapse via TTL (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).
    async fn revoke(&self) {
        self.shutdown.cancel();
        let handles = {
            let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *tasks)
        };
        for handle in handles {
            let _joined = handle.await;
        }
    }
}

/// The backend-shared membership view.
#[derive(Default)]
struct Registry {
    /// `service name → (instance id → instance)`.
    instances: HashMap<String, HashMap<String, ServiceInstance>>,
    /// Service names with a maintainer task being started or running.
    maintained: HashMap<String, MaintainerState>,
}

/// Lifecycle of a per-service maintainer slot in [`Registry::maintained`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum MaintainerState {
    /// A caller has claimed the slot and its `watch_prefix` await is in flight.
    /// The maintainer is not yet confirmed live: the await may fail and roll the
    /// slot back, so the view must not be treated as maintained.
    Pending,
    /// The maintainer task is spawned and running.
    Active,
}

/// What [`CacheBasedServiceDiscoveryBackend::ensure_maintainer`] tells its
/// caller about the membership view.
enum MaintainerStatus {
    /// A maintainer is confirmed running for this name, so a local pre-insert is
    /// safe — it will be reaped on TTL expiry / deregistration.
    Live,
    /// Another caller is still bringing the maintainer up. Not yet safe to
    /// pre-insert; the registration reconverges via the maintainer's reconcile.
    Pending,
}

struct Shared {
    registry: Mutex<Registry>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A decoded instance record (everything stored in the cache value except the
/// instance id, which is the key suffix).
struct InstanceRecord {
    address: String,
    metadata: HashMap<String, String>,
    state: InstanceState,
    registered_at: SystemTime,
}

impl InstanceRecord {
    fn to_instance(&self, instance_id: String) -> ServiceInstance {
        ServiceInstance {
            instance_id,
            address: self.address.clone(),
            metadata: self.metadata.clone(),
            state: self.state,
            registered_at: self.registered_at,
        }
    }

    /// Encodes the record into the opaque cache value with a private,
    /// dependency-free, length-prefixed layout (no serde on any SDK type).
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(match self.state {
            InstanceState::Enabled => 0,
            InstanceState::Disabled => 1,
        });
        let since = self
            .registered_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        buf.extend_from_slice(&since.as_secs().to_be_bytes());
        buf.extend_from_slice(&since.subsec_nanos().to_be_bytes());
        put_str(&mut buf, &self.address);
        let count = u32::try_from(self.metadata.len()).unwrap_or(u32::MAX);
        buf.extend_from_slice(&count.to_be_bytes());
        for (key, value) in &self.metadata {
            put_str(&mut buf, key);
            put_str(&mut buf, value);
        }
        buf
    }

    /// Decodes a record, returning `None` on a malformed value (a corrupt entry
    /// is skipped rather than crashing the maintainer).
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut pos = 0;
        let state = match take_u8(bytes, &mut pos)? {
            0 => InstanceState::Enabled,
            1 => InstanceState::Disabled,
            _ => return None,
        };
        let secs = take_u64(bytes, &mut pos)?;
        let nanos = take_u32(bytes, &mut pos)?;
        let registered_at = SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))?;
        let address = take_str(bytes, &mut pos)?;
        let count = take_u32(bytes, &mut pos)?;
        let mut metadata = HashMap::new();
        for _ in 0..count {
            let key = take_str(bytes, &mut pos)?;
            let value = take_str(bytes, &mut pos)?;
            metadata.insert(key, value);
        }
        Some(Self {
            address,
            metadata,
            state,
            registered_at,
        })
    }
}

fn put_str(buf: &mut Vec<u8>, value: &str) {
    let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(value.as_bytes());
}

fn take_u8(bytes: &[u8], pos: &mut usize) -> Option<u8> {
    let byte = *bytes.get(*pos)?;
    *pos += 1;
    Some(byte)
}

fn take_u32(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    let end = pos.checked_add(4)?;
    let slice = bytes.get(*pos..end)?;
    let mut arr = [0u8; 4];
    arr.copy_from_slice(slice);
    *pos = end;
    Some(u32::from_be_bytes(arr))
}

fn take_u64(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let end = pos.checked_add(8)?;
    let slice = bytes.get(*pos..end)?;
    let mut arr = [0u8; 8];
    arr.copy_from_slice(slice);
    *pos = end;
    Some(u64::from_be_bytes(arr))
}

fn take_str(bytes: &[u8], pos: &mut usize) -> Option<String> {
    let len = usize::try_from(take_u32(bytes, pos)?).ok()?;
    let end = pos.checked_add(len)?;
    let slice = bytes.get(*pos..end)?;
    let value = std::str::from_utf8(slice).ok()?.to_owned();
    *pos = end;
    Some(value)
}

/// The background task that keeps the shared membership view fresh from a
/// `watch_prefix` stream. Self-terminates when the backend is dropped (its
/// `Weak<Shared>` no longer upgrades), the cache watch ends, or graceful
/// shutdown cancels [`shutdown`](Self::shutdown).
struct StoreMaintainer {
    cache: Arc<dyn ClusterCacheBackend>,
    shared: Weak<Shared>,
    name: String,
    prefix: String,
    /// Shared backend shutdown token; cancelled by [`ShutdownRevoke::revoke`] so
    /// this tracked task exits deterministically on graceful shutdown.
    shutdown: CancellationToken,
}

impl StoreMaintainer {
    async fn run(self, mut watch: CacheWatch) {
        // Initial reconcile from backend truth. A prior maintainer for this name
        // may have died leaving stale entries behind (it clears `maintained` but
        // not `instances`), and this subscription only became live after any
        // in-flight `register` put. Rebuilding from a `scan_prefix` sweep drops
        // departed instances and picks up ones registered before the watch.
        if let Some(shared) = self.shared.upgrade() {
            self.reconcile(&shared).await;
        }
        loop {
            let event = tokio::select! {
                // Cancelled on graceful shutdown — stop maintaining the view and
                // let the registered entries lapse via TTL.
                () = self.shutdown.cancelled() => break,
                event = watch.recv() => event,
            };
            let Some(event) = event else {
                break;
            };
            let Some(shared) = self.shared.upgrade() else {
                break;
            };
            match event {
                CacheWatchEvent::Event(cache_event) => self.apply(&shared, &cache_event).await,
                CacheWatchEvent::Closed(_) => break,
                // Lagged / Reset: events were dropped or the subscription was
                // re-established, so departures may have been missed. A heartbeat
                // stream re-adds joins but never removes a departed instance, so
                // reconcile from backend truth to drop the stragglers.
                CacheWatchEvent::Lagged { .. } | CacheWatchEvent::Reset => {
                    self.reconcile(&shared).await;
                }
                _ => {}
            }
        }
        if let Some(shared) = self.shared.upgrade() {
            shared.lock().maintained.remove(&self.name);
        }
    }

    /// Rebuilds this service's slice of the shared view from a `scan_prefix` +
    /// `get` sweep of backend truth, replacing whatever was cached. Best-effort:
    /// if the backend lacks `scan_prefix` the view is left untouched (joins still
    /// reconverge from the heartbeat stream). Never holds the registry lock
    /// across an `.await` — the sweep completes before the single locked insert.
    async fn reconcile(&self, shared: &Shared) {
        let Ok(keys) = self.cache.scan_prefix(&self.prefix).await else {
            return;
        };
        let mut fresh = HashMap::new();
        for key in keys {
            let Some(instance_id) = key.strip_prefix(self.prefix.as_str()).map(str::to_owned)
            else {
                continue;
            };
            if let Ok(Some(entry)) = self.cache.get(&key).await
                && let Some(record) = InstanceRecord::decode(&entry.value)
            {
                fresh.insert(instance_id.clone(), record.to_instance(instance_id));
            }
        }
        shared.lock().instances.insert(self.name.clone(), fresh);
    }

    async fn apply(&self, shared: &Shared, event: &CacheEvent) {
        let key = event.key();
        let Some(instance_id) = key.strip_prefix(self.prefix.as_str()).map(str::to_owned) else {
            return;
        };
        match event {
            CacheEvent::Changed { .. } => {
                let instance = match self.cache.get(key).await {
                    Ok(Some(entry)) => InstanceRecord::decode(&entry.value)
                        .map(|record| record.to_instance(instance_id.clone())),
                    _ => None,
                };
                if let Some(instance) = instance {
                    let mut registry = shared.lock();
                    registry
                        .instances
                        .entry(self.name.clone())
                        .or_default()
                        .insert(instance_id, instance);
                }
            }
            CacheEvent::Deleted { .. } | CacheEvent::Expired { .. } => {
                let mut registry = shared.lock();
                if let Some(map) = registry.instances.get_mut(&self.name) {
                    map.remove(&instance_id);
                }
            }
            _ => {}
        }
    }
}

/// The per-`watch` task translating a cache prefix stream into
/// [`ServiceWatchEvent`]s. Owns the `ServiceWatchSender`, so it self-terminates
/// when the consumer drops the [`ServiceWatch`] (the send fails).
struct WatchTranslator {
    cache: Arc<dyn ClusterCacheBackend>,
    prefix: String,
    seen: HashSet<String>,
    /// Cancelled by [`ShutdownRevoke::revoke`] on graceful cluster shutdown.
    shutdown: CancellationToken,
}

impl WatchTranslator {
    async fn run(mut self, mut cache_watch: CacheWatch, sender: ServiceWatchSender) {
        // Cloned to a local so the `cancelled()` future does not borrow `self`,
        // which `translate` mutates in the other arm.
        let shutdown = self.shutdown.clone();
        loop {
            let event = tokio::select! {
                // Graceful cluster shutdown: end the watch terminally with a
                // best-effort `Closed(Shutdown)` and exit. The send is
                // non-blocking (`try_send`): `revoke` awaits this task, so a
                // blocking send against a backed-up consumer would stall
                // `ClusterHandle::stop`. A full or dropped consumer simply does
                // not receive the event (it observes end-of-stream instead).
                () = shutdown.cancelled() => {
                    let _closed = sender.try_send(ServiceWatchEvent::Closed(ClusterError::Shutdown));
                    break;
                }
                event = cache_watch.recv() => event,
            };
            let Some(event) = event else {
                break;
            };
            // Map the cache event to the topology event to emit (if any) and
            // whether it terminates the watch.
            let (to_send, terminal) = match event {
                CacheWatchEvent::Event(cache_event) => match self.translate(&cache_event).await {
                    Ok(outcome) => (outcome, false),
                    // A backend read failure while translating a change is
                    // terminal for this raw watch: surface Closed so the consumer
                    // (or the auto-restart combinator) reconnects and re-reads,
                    // rather than silently dropping a join/update and diverging
                    // from the backend.
                    Err(err) => (Some(ServiceWatchEvent::Closed(err)), true),
                },
                CacheWatchEvent::Lagged { dropped } => {
                    (Some(ServiceWatchEvent::Lagged { dropped }), false)
                }
                CacheWatchEvent::Reset => (Some(ServiceWatchEvent::Reset), false),
                CacheWatchEvent::Closed(err) => (Some(ServiceWatchEvent::Closed(err)), true),
                _ => (None, false),
            };
            if let Some(service_event) = to_send {
                // The outgoing send must stay cancellation-aware: `revoke` cancels
                // the token and then awaits this task, so a blocking `send` against
                // a backpressured-but-still-live consumer would otherwise hang
                // `ClusterHandle::stop`. On shutdown emit a best-effort terminal
                // `Closed(Shutdown)` (non-blocking) and exit instead of blocking.
                tokio::select! {
                    () = shutdown.cancelled() => {
                        let _closed =
                            sender.try_send(ServiceWatchEvent::Closed(ClusterError::Shutdown));
                        break;
                    }
                    result = sender.send(service_event) => {
                        if result.is_err() {
                            // Consumer dropped the watch.
                            break;
                        }
                    }
                }
            }
            if terminal {
                break;
            }
        }
    }

    /// Translates one cache event into a topology event, reading the current
    /// value for a change.
    ///
    /// Returns `Ok(None)` when the event is not actionable (key outside the
    /// prefix, entry already gone, or an unseen instance leaving). Returns `Err`
    /// only on a backend read failure, which the caller surfaces as a terminal
    /// `Closed` rather than dropping the event.
    async fn translate(
        &mut self,
        event: &CacheEvent,
    ) -> Result<Option<ServiceWatchEvent>, ClusterError> {
        let key = event.key();
        let Some(instance_id) = key.strip_prefix(self.prefix.as_str()).map(str::to_owned) else {
            return Ok(None);
        };
        match event {
            CacheEvent::Changed { .. } => {
                // The entry may have vanished (delete/expiry) between the change
                // notification and this read — that is a non-actionable miss,
                // distinct from a backend error which must propagate.
                let Some(entry) = self.cache.get(key).await? else {
                    return Ok(None);
                };
                let Some(record) = InstanceRecord::decode(&entry.value) else {
                    return Ok(None);
                };
                let instance = record.to_instance(instance_id.clone());
                // First sighting of an id is a join; a later change is an update.
                let change = if self.seen.insert(instance_id) {
                    TopologyChange::Joined(instance)
                } else {
                    TopologyChange::Updated(instance)
                };
                Ok(Some(ServiceWatchEvent::Change(change)))
            }
            CacheEvent::Deleted { .. } | CacheEvent::Expired { .. } => {
                if self.seen.remove(&instance_id) {
                    Ok(Some(ServiceWatchEvent::Change(TopologyChange::Left {
                        instance_id,
                    })))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

/// The background task that renews an instance's heartbeat and completes its
/// handle commands. Self-terminates when the consumer drops the handle.
struct HeartbeatTask {
    cache: Arc<dyn ClusterCacheBackend>,
    /// The service name and instance id (the deregister span's `name` /
    /// `instance_id` attributes), kept alongside the prefixed cache
    /// [`key`](Self::key).
    name: String,
    instance_id: String,
    key: String,
    ttl: Duration,
    renewal: Duration,
    record: InstanceRecord,
    /// Cancelled by [`ShutdownRevoke::revoke`] so the renewal loop stops on
    /// graceful cluster shutdown instead of running until the handle drops. The
    /// entry itself still lapses via its TTL (`cpt-cf-clst-fr-shutdown-ttl-cleanup`);
    /// only the renewal loop is halted.
    shutdown: CancellationToken,
    /// The bounded `provider` label for emitted signals.
    provider: &'static str,
    /// The metrics sink.
    metrics: Arc<dyn ClusterMetrics>,
}

impl HeartbeatTask {
    async fn run(mut self, mut receiver: ServiceCommandReceiver) {
        let mut renewal =
            tokio::time::interval_at(tokio::time::Instant::now() + self.renewal, self.renewal);
        // Cloned to a local so the `cancelled()` future does not borrow `self`,
        // which the command arm mutates (mirrors `WatchTranslator::run`).
        let shutdown = self.shutdown.clone();
        loop {
            tokio::select! {
                // Graceful cluster shutdown: stop renewing and exit. The entry is
                // left to lapse via its TTL rather than deleted, matching the
                // established teardown contract.
                () = shutdown.cancelled() => return,
                _ = renewal.tick() => {
                    // Best-effort heartbeat renewal; a transient failure is
                    // retried on the next tick (or the instance lapses via TTL).
                    let encoded = self.record.encode();
                    let _renewed = self.cache.put(PutRequest {
                        key: &self.key,
                        value: &encoded,
                        ttl: Ttl::Of(self.ttl),
                    }).await;
                }
                command = receiver.recv() => {
                    match command {
                        Some(ServiceRequest::UpdateMetadata { metadata, responder }) => {
                            self.record.metadata = metadata;
                            responder.respond(self.put_current().await);
                        }
                        Some(ServiceRequest::SetState { state, responder }) => {
                            self.record.state = state;
                            responder.respond(self.put_current().await);
                        }
                        Some(ServiceRequest::Deregister { responder }) => {
                            let span = tracing::info_span!(
                                spans::DISCOVERY_DEREGISTER,
                                provider = %self.provider,
                                name = %self.name,
                                instance_id = %self.instance_id
                            );
                            let out: Result<(), ClusterError> = self
                                .cache
                                .delete(&self.key)
                                .instrument(span)
                                .await
                                .map(|_existed| ());
                            record_discovery(
                                &*self.metrics,
                                self.provider,
                                "deregister",
                                &self.name,
                                &out,
                            );
                            responder.respond(out);
                            return;
                        }
                        // Consumer dropped the handle without deregistering: no
                        // I/O, the instance lapses via the heartbeat TTL.
                        None => return,
                    }
                }
            }
        }
    }

    async fn put_current(&self) -> Result<(), ClusterError> {
        let encoded = self.record.encode();
        self.cache
            .put(PutRequest {
                key: &self.key,
                value: &encoded,
                ttl: Ttl::Of(self.ttl),
            })
            .await
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod discovery_tests;
