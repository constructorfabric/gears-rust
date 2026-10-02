//! Native distributed lock over one `Lease` per lock name (DESIGN.md §5).
//!
//! [`K8sLock`] implements [`DistributedLockBackend`] over
//! `coordination.k8s.io/v1.Lease`. A held lock is a Lease carrying the
//! store-owned-lease **holder token** (`<owner>#<fence>`, §5.1, §5.8.1);
//! acquisition is create-or-guarded-claim (§5.2); a blocking
//! [`lock()`](DistributedLockBackend::lock) establishes a watch on the one Lease
//! *before* its first attempt and shares it with any same-process waiter via
//! [`waiters`] (§5.3); renew and release are token-fenced guarded writes (§5.4); and
//! release **clears** the holder rather than deleting the object, with a background
//! [`reaper`] pruning long-empty objects (§5.5).
//!
//! ## Two halves, one lease
//!
//! `try_lock` / `lock` hand back a [`LockGuard`];
//! [`acquire`](DistributedLockBackend::acquire) /
//! [`acquire_waiting`](DistributedLockBackend::acquire_waiting) hand back the
//! [`LeaseToken`] the guard cannot carry, so a gear serving lock RPCs over the wire
//! -- where a `LockGuard` cannot cross the process boundary -- can `renew` and
//! `release` against the token from a task that never saw the acquire (the
//! store-owned-leases half of [`DistributedLockBackend`], invariant I7). Both halves
//! are built over one acquire primitive ([`LockRuntime::try_acquire`]) minting one
//! `holderIdentity` value, and both `renew` and `release` fence on that value alone
//! ([`renew_holder`] / [`release_holder`]), so the guard path and the token path can
//! never fence on different things.
//!
//! Per §3.3 a held lock runs no renewal loop — renewal is consumer-driven through
//! the [`LockGuard`]. Servicing that guard's command channel is one parked task per
//! held lock (the same shape the postgres plugin uses), which costs no connection
//! and no polling.
//!
//! The pure pieces carry the L1 coverage: the [`HolderToken`] round-trip, the
//! blocking-wait 3-outcome classifier ([`classify_wait`]), the [`waiters`] registry,
//! and the [`reaper`] eligibility predicate. Real-server behaviour is Phase 6.

mod reaper;
mod waiters;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use futures_util::StreamExt;
use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::Api;
use kube::api::ObjectMeta;
use kube::runtime::watcher;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use cluster_sdk::lock::{
    DistributedLockBackend, LockCommandReceiver, LockFeatures, LockGuard, LockRequest,
};
use cluster_sdk::observability::{self, ResourceId, result, spans};
use cluster_sdk::{ClusterError, ClusterMetrics, LeaseToken};
use tracing::Instrument as _;

use crate::client::ResolvedClient;
use crate::config::K8sLockConfig;
use crate::guarded::{self, CallSite, Created, Replaced};
use crate::k8s_error;
use crate::lease::{
    HOLDER_WRITE_MAX_ATTEMPTS, fresh_fence, holder_of, holder_string, holder_write_exhausted,
    lease_duration_seconds, ttl_ms,
};
use crate::naming::{
    self, ANNOTATION_NAME, ANNOTATION_TTL_MS, LABEL_MANAGED_BY, LABEL_PRIMITIVE, MANAGED_BY_VALUE,
    Seg,
};
use crate::observed::Observed;

use self::waiters::LockWaiters;

/// The in-flight command buffer for each [`LockGuard`] (§5.4).
const GUARD_COMMAND_BUFFER: usize = 4;

/// The `(holderIdentity, renewTime)` pair `Observed` tracks for expiry (§2.8).
type Record = (Option<String>, Option<String>);

/// A per-acquisition lock holder token: `<owner>#<fence>` (§5.1, §5.8.1).
///
/// The `owner` half separates two holders; on the guard path it is the pod
/// `identity`, so it also answers "which replica holds this?" in `kubectl` without a
/// lookup, while a brokered [`acquire`](DistributedLockBackend::acquire) supplies the
/// caller's own owner. The `fence` half — a fresh random `u64` per acquisition,
/// [`fresh_fence`](crate::lease::fresh_fence) — separates two acquisitions of one name
/// across a lapse, so a re-acquired name draws a fence the previous holder's token
/// will not match. Together they make `renew`/`release` safe against a successor and
/// force two in-process acquisitions to arbitrate through the API server exactly as
/// two processes would (§5.1). Composed into `holderIdentity` by
/// [`holder_string`](crate::lease::holder_string), which is a pure function of a
/// [`LeaseToken`]'s identity fields — that is what lets a remote caller holding only
/// the token fence identically from a replica that never saw the acquire (I7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderToken {
    owner: String,
    fence: u64,
}

impl HolderToken {
    /// A fresh token for `owner` with a random fence (§5.1, §5.8.1).
    #[must_use]
    pub fn generate(owner: &str) -> Self {
        Self {
            owner: owner.to_owned(),
            fence: fresh_fence(),
        }
    }

    /// Reconstructs the holder a [`LeaseToken`] presented over the wire stands for,
    /// so the token-path `renew`/`release` fence on the same `holderIdentity` the
    /// acquiring instance wrote (I7).
    #[must_use]
    fn from_token(token: &LeaseToken) -> Self {
        Self {
            owner: token.owner.clone(),
            fence: token.fence,
        }
    }

    /// The wire form written to `holderIdentity`: `<owner>#<fence>`.
    #[must_use]
    pub fn to_holder_string(&self) -> String {
        holder_string(&self.owner, self.fence)
    }

    /// Mints the [`LeaseToken`] a won acquisition of `name` hands back, with no
    /// deadline armed (the lock's token path renews on the caller's cadence, not a
    /// backend renewal task).
    #[must_use]
    fn into_lease_token(self, name: &str) -> LeaseToken {
        LeaseToken::new(name, self.owner, self.fence)
    }

    /// Parses a `holderIdentity`, splitting on the **last** `#` (§5.1). Returns
    /// `None` for a holder with no `#` or a non-numeric fence — a foreign/legacy
    /// holder this plugin did not write.
    ///
    /// The inverse of [`to_holder_string`](Self::to_holder_string). Every fencing
    /// path compares the raw `holderIdentity` string rather than a parsed token, so
    /// this is retained as the token codec's other half — exercised by the unit tests
    /// and used by `kubectl`-side diagnostics — rather than consumed on a hot path.
    #[allow(dead_code)]
    #[must_use]
    pub fn parse(holder: &str) -> Option<Self> {
        let (owner, fence) = holder.rsplit_once('#')?;
        let fence = fence.parse::<u64>().ok()?;
        Some(Self {
            owner: owner.to_owned(),
            fence,
        })
    }
}

/// Why a blocking `lock()` attempt stops waiting, decided from the loop's terminal
/// conditions (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitDecision {
    /// The plugin is shutting down — the caller must not retry (`ClusterError::Shutdown`).
    Shutdown,
    /// The caller's budget elapsed with the lock genuinely held (`LockTimeout`).
    Timeout,
    /// Neither: keep waiting for a release or the holder's expiry.
    Keep,
}

/// Classifies a blocked `lock()`'s next step from `(shutdown cancelled, deadline
/// passed)` (§5.3), a pure function so the three-way distinction — the point of the
/// section — is unit-tested as one.
///
/// Shutdown is checked first: a plugin going down must return `Shutdown`, not a
/// `LockTimeout` the caller might retry. A backend (`Provider`) error is not one of
/// these outcomes — it propagates immediately from the attempt and never reaches
/// this decision.
#[must_use]
pub fn classify_wait(cancelled: bool, deadline_passed: bool) -> WaitDecision {
    if cancelled {
        WaitDecision::Shutdown
    } else if deadline_passed {
        WaitDecision::Timeout
    } else {
        WaitDecision::Keep
    }
}

/// The mapped Kubernetes object name for a lock (§2.2), distinct at the type level
/// from the unmapped coordination name. The two are adjacent parameters on the
/// acquire path and are used for different purposes — one is the API path segment
/// (and the watch field selector), the other the coordination name stamped into the
/// `name` annotation — so transposing them, which would write a Lease under the wrong
/// name while annotating it with the other, must not compile.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ObjectName(String);

impl ObjectName {
    /// The mapped name as a string slice, for the API path, the `observed` map key,
    /// and the watch field selector.
    fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the newtype into its owned string (for a task that outlives the
    /// borrow, e.g. the guard task's stored name).
    fn into_string(self) -> String {
        self.0
    }
}

/// Shared runtime for every lock this backend serves.
struct LockRuntime {
    client: kube::Client,
    namespace: String,
    identity: String,
    lease_prefix: String,
    /// The ADR-004 metrics sink; emits `cluster_lock_ops_total` /
    /// `cluster_lock_op_duration_seconds` / `cluster_provider_errors_total` (§8).
    metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label attached to every emitted signal.
    provider: &'static str,
    request_timeout: Duration,
    reaper_enabled: bool,
    reaper_interval: Duration,
    lock_object_retention: Duration,
    lock_name_cardinality_warn: u64,
    /// Per-name incumbent observations, refreshed on each acquire attempt so a
    /// lapsed foreign holder can be stolen only after a full TTL of observation
    /// (§2.8, §5.2) — never on first sight.
    observed: DashMap<String, Observed<Record>>,
    /// In-process release-waiter registry shared by blocking `lock()` calls (§5.3).
    waiters: Arc<LockWaiters>,
}

impl LockRuntime {
    fn api(&self) -> Api<Lease> {
        Api::namespaced(self.client.clone(), &self.namespace)
    }

    fn lease_name(&self, coordination_name: &str) -> ObjectName {
        ObjectName(naming::lease_name(
            &self.lease_prefix,
            Seg::Lock,
            coordination_name,
        ))
    }

    async fn read(&self, name: &str) -> Result<Option<Lease>, ClusterError> {
        let api = self.api();
        self.timed("get lock lease", guarded::read(&api, name))
            .await
    }

    /// Records the ADR-004 metric side of a finished lock op — the duration
    /// histogram, the bounded-`result` counter, and (for a `Provider` error) the
    /// shared provider-error signals — mirroring the postgres native lock's
    /// `record_lock` so both natives emit the identical signal set (§8). Called by
    /// `try_lock`/`lock` and by the per-guard task's `renew`/`release`.
    fn record_lock<T>(
        &self,
        op: &'static str,
        lock: &str,
        started: std::time::Instant,
        outcome: &Result<T, ClusterError>,
    ) {
        self.metrics
            .lock_op_duration(op, started.elapsed().as_secs_f64());
        self.metrics.lock_op(op, result::label(outcome));
        if let Err(err) = outcome {
            observability::emit_provider_error(
                &*self.metrics,
                self.provider,
                op,
                ResourceId::Lock(lock),
                err,
            );
        }
    }

    async fn timed<T, F>(&self, ctx: &'static str, fut: F) -> Result<T, ClusterError>
    where
        F: std::future::Future<Output = Result<T, ClusterError>>,
    {
        match tokio::time::timeout(self.request_timeout, fut).await {
            Ok(result) => result,
            Err(_) => Err(k8s_error::timeout(ctx)),
        }
    }

    /// A fresh claim Lease for `coordination_name`, holder set to `token` (create
    /// path, no `resourceVersion`).
    fn new_claim(
        &self,
        coordination_name: &str,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<Lease, ClusterError> {
        let mut lease = Lease {
            metadata: ObjectMeta {
                name: Some(self.lease_name(coordination_name).into_string()),
                namespace: Some(self.namespace.clone()),
                labels: Some(BTreeMap::from([
                    (LABEL_MANAGED_BY.to_owned(), MANAGED_BY_VALUE.to_owned()),
                    (
                        LABEL_PRIMITIVE.to_owned(),
                        Seg::Lock.primitive_label().to_owned(),
                    ),
                ])),
                annotations: Some(BTreeMap::from([(
                    ANNOTATION_NAME.to_owned(),
                    coordination_name.to_owned(),
                )])),
                ..ObjectMeta::default()
            },
            spec: Some(LeaseSpec::default()),
        };
        set_holder(&mut lease, token, ttl, true)?;
        Ok(lease)
    }

    /// One acquire attempt for `name` under `token` (§5.2). `Ok(Some(token))` on a
    /// won claim — the [`LeaseToken`] the acquisition is authority over — `Ok(None)`
    /// on contention, `Err` on a backend fault.
    ///
    /// The single acquire primitive both halves of the lock are built over: the guard
    /// path drives it from a spawned, cancel-proof task
    /// ([`acquire_guarded`](K8sLock::acquire_guarded)) and wraps the won claim in a
    /// [`LockGuard`]; the token path ([`acquire`](DistributedLockBackend::acquire))
    /// runs it inline and hands the [`LeaseToken`] straight back. One lease, one
    /// `holderIdentity` value, so the two never fence on different things. On
    /// [`LockRuntime`] rather than [`K8sLock`] so the cancel-proof task owns only an
    /// `Arc<LockRuntime>`.
    async fn try_acquire(
        &self,
        object: &ObjectName,
        coordination_name: &str,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<Option<LeaseToken>, ClusterError> {
        let won = self.claim(object, coordination_name, token, ttl).await?;
        Ok(won.then(|| token.clone().into_lease_token(coordination_name)))
    }

    /// The acquire decision behind [`try_acquire`](Self::try_acquire): `Ok(true)` when
    /// this attempt took the lease, `Ok(false)` on contention (§5.2).
    async fn claim(
        &self,
        object: &ObjectName,
        coordination_name: &str,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<bool, ClusterError> {
        let existing = self.read(object.as_str()).await?;
        let Some(lease) = existing else {
            return self.create_claim(coordination_name, token, ttl).await;
        };

        let holder = holder_of(&lease);

        if holder.is_none() {
            // Free (cleared) object: claim it with a guarded replace.
            self.observed.remove(object.as_str());
            return self
                .guarded_claim(object, coordination_name, lease, token, ttl)
                .await;
        }

        // Held by someone: steal only once our own observation has aged past a full
        // TTL (§2.8). A single sighting is never enough. The relevant TTL is the
        // *current holder's* claim duration — read from the observed Lease's ttl-ms
        // annotation — not our own requested `ttl`: whether their claim has lapsed
        // depends on how long *they* held it for, and a long-TTL acquirer must not be
        // forced to wait out its own TTL to reclaim a short-lived lapsed claim.
        let holder_ttl = observed_ttl(&lease).unwrap_or(ttl);
        let record: Record = claim_record(&lease);
        let now = std::time::Instant::now();
        let expired = {
            let mut observed = self
                .observed
                .entry(object.as_str().to_owned())
                .or_insert_with(|| Observed::new(record.clone(), now));
            observed.observe(record, now);
            observed.is_expired(now, holder_ttl)
        };
        if expired {
            self.observed.remove(object.as_str());
            self.guarded_claim(object, coordination_name, lease, token, ttl)
                .await
        } else {
            Ok(false)
        }
    }

    /// Creates the Lease as ours; a `409 AlreadyExists` is contention this tick.
    /// `Ok(true)` on a won create.
    async fn create_claim(
        &self,
        coordination_name: &str,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<bool, ClusterError> {
        let lease = self.new_claim(coordination_name, token, ttl)?;
        let api = self.api();
        let created = self
            .timed(
                "create lock lease",
                guarded::create(&api, &lease, CallSite::LockAcquire),
            )
            .await?;
        Ok(matches!(created, Created::Created(_)))
    }

    /// Guarded replace claiming a free/lapsed Lease; a `409` is contention (§5.2).
    /// `Ok(true)` on a won claim.
    async fn guarded_claim(
        &self,
        object: &ObjectName,
        coordination_name: &str,
        mut lease: Lease,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<bool, ClusterError> {
        // Adopt-path parity with `new_claim`: an object this plugin did not create
        // (a foreign/legacy Lease occupying the same computed name) must still carry
        // the managed-by/primitive labels and the `name` annotation once we claim it,
        // or the reaper's label-selector list never sees it (a released object then
        // leaks) and the `name` annotation reports the object name instead of the
        // coordination name in `LockExpired`/`LockTimeout` (§2.2, §5.5).
        set_identity(&mut lease, coordination_name);
        set_holder(&mut lease, token, ttl, true)?;
        let api = self.api();
        let replaced = self
            .timed(
                "claim lock lease",
                guarded::replace(&api, object.as_str(), &lease, CallSite::LockAcquire),
            )
            .await?;
        Ok(matches!(replaced, Replaced::Applied(_)))
    }
}

/// Token-fenced renew (§5.4, invariant I7): GET the Lease, verify its
/// `holderIdentity` is still `<owner>#<fence>`, then guarded-replace `renewTime`/TTL
/// with the fresh `resourceVersion` from that read. Predicated **entirely on stored
/// state** — no process-local deadline — so a renew issued from a replica that never
/// saw the acquire fences identically (§5.8.1). A holder that no longer matches —
/// lapsed and reaped, stolen, or never ours — is [`ClusterError::LockExpired`].
///
/// A `409` re-reads and re-verifies: a successor now shows a different holder
/// (→ `LockExpired`), a benign metadata update still shows us (→ retry with the fresh
/// `resourceVersion`). Exhausting the bounded retries while our holder persists is
/// pathological churn on an object essentially only the holder writes; it resolves to
/// the retryable [`holder_write_exhausted`] error rather than `LockExpired`, because
/// every re-read confirmed the claim is still ours — reporting a lost lock would be a
/// false loss (see [`holder_write_exhausted`]).
///
/// Shared by the guard task's `Renew` and by
/// [`DistributedLockBackend::renew`](DistributedLockBackend::renew), so both fence on
/// the one value.
async fn renew_holder(
    runtime: &LockRuntime,
    object_name: &str,
    name: &str,
    token: &HolderToken,
    new_ttl: Duration,
) -> Result<(), ClusterError> {
    let holder = token.to_holder_string();
    for _attempt in 0..HOLDER_WRITE_MAX_ATTEMPTS {
        let Some(mut lease) = runtime.read(object_name).await? else {
            // The object is gone (reaped after a lapse): our claim is gone with it.
            return Err(ClusterError::LockExpired {
                name: name.to_owned(),
            });
        };
        if holder_of(&lease).as_deref() != Some(holder.as_str()) {
            return Err(ClusterError::LockExpired {
                name: name.to_owned(),
            });
        }
        // Renewal: preserve `acquireTime` (this holder's original acquisition),
        // refresh only `renewTime`/TTL (§2.10).
        set_holder(&mut lease, token, new_ttl, false)?;
        let api = runtime.api();
        let replaced = runtime
            .timed(
                "renew lock lease",
                guarded::replace(&api, object_name, &lease, CallSite::LockRenew),
            )
            .await?;
        match replaced {
            Replaced::Applied(_) => return Ok(()),
            Replaced::Conflict => {}
        }
    }
    Err(holder_write_exhausted(name))
}

/// Token-fenced release (§5.4, §5.5): GET the Lease, verify our holder, then clear
/// `holderIdentity` with a guarded replace — freeing the lock without deleting the
/// object. Shared by the guard task's `Release`, the cancel-safety cleanup path, and
/// [`DistributedLockBackend::release`](DistributedLockBackend::release).
///
/// **Idempotent by absence** (the release-if-still-holder contract): a missing object
/// or a non-matching holder (a successor took over, or the holder is already cleared)
/// is `Ok(())` — the postcondition, "this holder no longer holds the name", already
/// holds. A `409` re-reads: a successor resolves to `Ok`, a benign update retries the
/// clear against the fresh `resourceVersion` so a genuine release is not silently
/// dropped. Exhausting the retries while our holder is *still present and uncleared*
/// is the one non-`Ok` outcome — the retryable [`holder_write_exhausted`], not a false
/// `Ok`: returning success there would report the lock released while it stays held
/// until its TTL lapses, wedging the name for a waiter.
async fn release_holder(
    runtime: &LockRuntime,
    object_name: &str,
    name: &str,
    token: &HolderToken,
) -> Result<(), ClusterError> {
    let holder = token.to_holder_string();
    for _attempt in 0..HOLDER_WRITE_MAX_ATTEMPTS {
        let Some(mut lease) = runtime.read(object_name).await? else {
            return Ok(()); // nothing to release
        };
        if holder_of(&lease).as_deref() != Some(holder.as_str()) {
            return Ok(()); // a successor holds it, or it is already cleared
        }
        if let Some(spec) = lease.spec.as_mut() {
            spec.holder_identity = None;
            spec.renew_time = Some(now_micro());
        }
        let api = runtime.api();
        let replaced = runtime
            .timed(
                "release lock lease",
                guarded::replace(&api, object_name, &lease, CallSite::Release),
            )
            .await?;
        match replaced {
            Replaced::Applied(_) => return Ok(()),
            Replaced::Conflict => {}
        }
    }
    Err(holder_write_exhausted(name))
}

/// Pushes `handle` onto the shared task list, pruning finished handles first. A free
/// function so both [`K8sLock::track`] and the cancel-proof acquire task (which owns
/// only a clone of the list) can reach it.
fn push_task(tasks: &Mutex<Vec<JoinHandle<()>>>, handle: JoinHandle<()>) {
    let mut tasks = tasks.lock().unwrap_or_else(PoisonError::into_inner);
    tasks.retain(|h| !h.is_finished());
    tasks.push(handle);
}

/// Stamps `lease` with the identifying labels the reaper's label-selector lists on
/// (§5.5) and the coordination-name annotation read back for error messages
/// (§2.2). Idempotent; applied on both the create path (via `new_claim`) and the
/// claim/adopt path (via `guarded_claim`) so a Lease this plugin did not create still
/// becomes visible and correctly named once we claim it. Free-standing for the same
/// reason as `set_holder`.
fn set_identity(lease: &mut Lease, coordination_name: &str) {
    let labels = lease.metadata.labels.get_or_insert_with(BTreeMap::new);
    labels.insert(LABEL_MANAGED_BY.to_owned(), MANAGED_BY_VALUE.to_owned());
    labels.insert(
        LABEL_PRIMITIVE.to_owned(),
        Seg::Lock.primitive_label().to_owned(),
    );
    lease
        .metadata
        .annotations
        .get_or_insert_with(BTreeMap::new)
        .insert(ANNOTATION_NAME.to_owned(), coordination_name.to_owned());
}

/// Stamps `lease` with `token` as holder, a fresh `renewTime`, the rounded-up
/// `leaseDurationSeconds`, and the exact `ttl-ms` annotation (§2.9). Free-standing
/// because it needs nothing from the runtime — the holder is the token, not the
/// backend's identity.
///
/// `claiming` distinguishes acquisition (create/adopt) from renewal. On a claim it
/// stamps `acquireTime`; on a renewal it leaves `acquireTime` as the instant this
/// holder first acquired the lock. `Lease.spec.acquireTime` is defined as the time
/// the *current* holder took the lease, so rewriting it on every renewal would erase
/// "held since" for foreign readers (`kubectl`, other controllers) and make a holder
/// change indistinguishable from a renewal (§2.10).
fn set_holder(
    lease: &mut Lease,
    token: &HolderToken,
    ttl: Duration,
    claiming: bool,
) -> Result<(), ClusterError> {
    let ttl_millis = ttl_ms(ttl)?;
    lease
        .metadata
        .annotations
        .get_or_insert_with(BTreeMap::new)
        .insert(ANNOTATION_TTL_MS.to_owned(), ttl_millis.to_string());
    let spec = lease.spec.get_or_insert_with(LeaseSpec::default);
    spec.holder_identity = Some(token.to_holder_string());
    spec.lease_duration_seconds = Some(lease_duration_seconds(ttl)?);
    spec.renew_time = Some(now_micro());
    if claiming {
        spec.acquire_time = Some(now_micro());
    }
    Ok(())
}

/// The native Kubernetes distributed-lock backend (§5).
pub struct K8sLock {
    runtime: Arc<LockRuntime>,
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl K8sLock {
    /// Builds a lock backend from a resolved client and the lock config (§3.5).
    ///
    /// # Errors
    ///
    /// [`ClusterError::InvalidConfig`] when `lease_prefix` is not a legal RFC 1123
    /// label (§2.2).
    pub fn new(
        resolved: &ResolvedClient,
        config: &K8sLockConfig,
        metrics: Arc<dyn ClusterMetrics>,
    ) -> Result<Self, ClusterError> {
        naming::validate_lease_prefix(&config.lease_prefix)?;
        let runtime = LockRuntime {
            client: resolved.client.clone(),
            namespace: resolved.namespace.clone(),
            identity: resolved.identity.clone(),
            lease_prefix: config.lease_prefix.clone(),
            metrics,
            provider: crate::provider::PROVIDER_NAME,
            request_timeout: Duration::from_millis(config.request_timeout_ms),
            reaper_enabled: config.reaper,
            reaper_interval: Duration::from_millis(config.reaper_interval_ms),
            lock_object_retention: Duration::from_millis(config.lock_object_retention_ms),
            lock_name_cardinality_warn: config.lock_name_cardinality_warn_threshold,
            observed: DashMap::new(),
            waiters: Arc::new(LockWaiters::new()),
        };
        let backend = Self {
            runtime: Arc::new(runtime),
            shutdown: CancellationToken::new(),
            tasks: Arc::new(Mutex::new(Vec::new())),
        };
        backend.spawn_reaper();
        Ok(backend)
    }

    /// Cancels the shutdown token and awaits the guard/reaper tasks (§11).
    pub async fn stop(&self) {
        self.shutdown.cancel();
        let handles = {
            let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *tasks)
        };
        for handle in handles {
            let _joined = handle.await;
        }
    }

    fn track(&self, handle: JoinHandle<()>) {
        push_task(&self.tasks, handle);
    }

    /// Cancels the guard/reaper tasks synchronously, without awaiting them — the
    /// teardown the handle's `Drop` uses when `stop()` was never called and cannot
    /// `.await` (§11). A held lock's Lease is left to lapse on its own deadline.
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Spawns the stale lock-object reaper if enabled (§5.5).
    fn spawn_reaper(&self) {
        if !self.runtime.reaper_enabled {
            return;
        }
        let handle = tokio::spawn(reaper::run_reaper(
            Arc::clone(&self.runtime),
            self.shutdown.clone(),
        ));
        self.track(handle);
    }

    /// Acquires `name` and hands back a [`LockGuard`], or `Ok(None)` on contention.
    ///
    /// **Cancel-safety seam.** The acquire's guarded write and the guard hand-off run
    /// on a spawned, cancel-proof task rather than inline: if the *caller's* future is
    /// dropped while the claim write is in flight (a `tokio::time::timeout` or a
    /// `select!` arm around `lock()`), the write may still land server-side, and doing
    /// this inline would abandon a held Lease under a token nobody holds — no guard
    /// task, no renew, no release — until its TTL lapsed (§5.4). Here the task owns the
    /// acquire, so a dropped caller cannot interrupt it, and hands the guard over a
    /// `oneshot`; if that hand-off fails because the caller is gone, it releases the
    /// claim (a token-fenced guarded replace) instead of leaving it to lapse. The guard
    /// task is spawned only on a successful hand-off, so a lost caller leaks nothing.
    async fn acquire_guarded(
        &self,
        name: &str,
        object: &ObjectName,
        token: HolderToken,
        ttl: Duration,
    ) -> Result<Option<LockGuard>, ClusterError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let runtime = Arc::clone(&self.runtime);
        let shutdown = self.shutdown.clone();
        let tasks = Arc::clone(&self.tasks);
        let name = name.to_owned();
        let object = object.clone();
        let handle = tokio::spawn(async move {
            // Anchor the guard's initial deadline to *before* the acquire's read+write
            // round trip, so it can never believe its claim outlasts the granted TTL
            // measured from when the write landed (§2.8) — capturing it earlier is a
            // safe over-estimate of the elapsed time, only ever making the deadline
            // sooner. The deadline is the guard path's liveness proxy (SC-LOCK-005): a
            // consumer-driven `renew`/`release` past it reports the lock lost rather
            // than re-extending a lease the fleet may already treat as reclaimable.
            let issued_at = Instant::now();
            match runtime.try_acquire(&object, &name, &token, ttl).await {
                Ok(Some(_lease_token)) => {
                    let (commands, guard) = LockGuard::channel(name.clone(), GUARD_COMMAND_BUFFER);
                    let task = GuardTask {
                        runtime: Arc::clone(&runtime),
                        object_name: object.into_string(),
                        name: name.clone(),
                        token,
                        deadline: issued_at + ttl,
                        shutdown: shutdown.clone(),
                    };
                    match tx.send(Ok(Some(guard))) {
                        // Handed off: spawn the guard task to service its commands.
                        Ok(()) => push_task(&tasks, tokio::spawn(task.run(commands))),
                        // The caller's future was dropped before the hand-off. The guard
                        // task was never spawned, so nothing leaks; release the claim now
                        // rather than holding the lock for a full TTL (§5.4).
                        Err(_dropped_guard) => {
                            if let Err(err) =
                                release_holder(&runtime, &task.object_name, &task.name, &task.token)
                                    .await
                            {
                                tracing::warn!(
                                    error = %err, lock = %name,
                                    "cluster.provider.lock_orphan_release_failed: could not \
                                     release a lock claimed for a caller whose future was \
                                     dropped; it will lapse on its TTL"
                                );
                            }
                        }
                    }
                }
                Ok(None) => {
                    let _gone = tx.send(Ok(None));
                }
                Err(err) => {
                    let _gone = tx.send(Err(err));
                }
            }
        });
        self.track(handle);
        // The acquire task always sends exactly one message unless the process is torn
        // down mid-acquire; a closed channel then reads as `Shutdown`.
        rx.await.unwrap_or(Err(ClusterError::Shutdown))
    }
}

#[async_trait]
impl DistributedLockBackend for K8sLock {
    /// Unconditionally linearizable (§3.7): a Lease guarded replace is Raft-arbitrated.
    fn features(&self) -> LockFeatures {
        LockFeatures::new(true)
    }

    fn provider_name(&self) -> &'static str {
        crate::provider::PROVIDER_NAME
    }

    /// NOT cancel-safe: dropping this future may still leave a Lease claimed
    /// server-side, so the claim + guard hand-off runs on a cancel-proof task
    /// ([`acquire_guarded`](K8sLock::acquire_guarded)) that releases the claim if the
    /// caller is gone. A `select!`/`timeout` around this call is therefore safe (§5.4).
    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        let span = tracing::info_span!(spans::LOCK_TRY_LOCK, provider = %self.runtime.provider, lock = %name);
        let started = std::time::Instant::now();
        let out = async {
            if self.shutdown.is_cancelled() {
                return Err(ClusterError::Shutdown);
            }
            let object_name = self.runtime.lease_name(name);
            // The guard path holds its own lease: `owner` is the pod identity, which
            // keeps the `kubectl` diagnostic (which replica holds this?) and is fenced
            // by the fresh per-acquisition fence, not the owner (§5.1).
            let token = HolderToken::generate(&self.runtime.identity);
            // Note: a contended `try_lock` deliberately leaves its `Observed` record in
            // place. Reclamation of a lapsed holder is driven by that record ageing
            // across repeated attempts (SC-LOCK-003 reclaims by polling `try_lock`), and
            // a stable lapsed holder keeps `seen_at` fixed by design — so an abandoned
            // record cannot be told apart from an actively-polled one, and evicting here
            // would break reclamation. The map is bounded by lock-name cardinality,
            // which the reaper already warns about (§5.5).
            match self.acquire_guarded(name, &object_name, token, ttl).await? {
                Some(guard) => Ok(guard),
                None => Err(ClusterError::LockContended {
                    name: name.to_owned(),
                }),
            }
        }
        .instrument(span)
        .await;
        self.runtime.record_lock("try_lock", name, started, &out);
        out
    }

    /// NOT cancel-safe: see [`try_lock`](Self::try_lock). Each acquire attempt hands
    /// its claim off through the cancel-proof [`acquire_guarded`](K8sLock::acquire_guarded)
    /// seam, so a dropped `lock()` future never abandons a held Lease (§5.4).
    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        let span =
            tracing::info_span!(spans::LOCK_LOCK, provider = %self.runtime.provider, lock = %name);
        let started = std::time::Instant::now();
        let out = self
            .wait_acquire_guard(name, ttl, timeout)
            .instrument(span)
            .await;
        self.runtime.record_lock("lock", name, started, &out);
        out
    }

    /// The [`LeaseToken`] counterpart of [`try_lock`](Self::try_lock): one acquire
    /// attempt for `owner`, handing the token back rather than a guard (§5.8.1). This
    /// is the store-owned-leases half a gear serving lock RPCs over the wire needs —
    /// the caller renews and releases against the token from wherever it likes, and
    /// any replica fences on the same `holderIdentity` (I7).
    ///
    /// Run inline rather than through the cancel-proof
    /// [`acquire_guarded`](K8sLock::acquire_guarded) seam: this hands the token to a
    /// caller, so a dropped future leaves the lease to lapse on its TTL like any
    /// unheld lease (§5.8.2), never the orphan-and-release the guard path guards
    /// against. Instrumented as `try_lock`, the operation it shares a lease with.
    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let span = tracing::info_span!(spans::LOCK_TRY_LOCK, provider = %self.runtime.provider, lock = %name);
        let started = std::time::Instant::now();
        let out = async {
            if self.shutdown.is_cancelled() {
                return Err(ClusterError::Shutdown);
            }
            let object_name = self.runtime.lease_name(name);
            let token = HolderToken::generate(owner);
            match self
                .runtime
                .try_acquire(&object_name, name, &token, ttl)
                .await?
            {
                Some(lease_token) => Ok(lease_token),
                None => Err(ClusterError::LockContended {
                    name: name.to_owned(),
                }),
            }
        }
        .instrument(span)
        .await;
        self.runtime.record_lock("try_lock", name, started, &out);
        out
    }

    /// The [`LeaseToken`] counterpart of [`lock`](Self::lock): the blocking acquire,
    /// handing the token back rather than a guard. Shares the wait loop with `lock`
    /// (so budget, watch-driven wake, and shutdown behave identically) and,
    /// like [`acquire`](Self::acquire), runs each attempt inline — a dropped
    /// `acquire_waiting` leaves the lease to lapse. Instrumented as `lock`.
    async fn acquire_waiting(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let span =
            tracing::info_span!(spans::LOCK_LOCK, provider = %self.runtime.provider, lock = %name);
        let started = std::time::Instant::now();
        let out = self
            .wait_acquire_token(name, owner, ttl, timeout)
            .instrument(span)
            .await;
        self.runtime.record_lock("lock", name, started, &out);
        out
    }

    /// Token-fenced renew (§5.4, I7): reconstruct the `holderIdentity` the token
    /// stands for and extend the lease to `ttl` if a replica still finds it there.
    /// Reuses [`renew_holder`] — the exact path the guard task's `Renew` takes — so a
    /// lease acquired through `acquire` and one acquired through `try_lock` renew
    /// identically. Cross-checking that the transport caller is `token.owner` is the
    /// serving gear's authorization decision, not this predicate's.
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let span = tracing::info_span!(
            spans::LOCK_RENEW, provider = %self.runtime.provider, lock = %token.name
        );
        let started = std::time::Instant::now();
        let holder = HolderToken::from_token(token);
        let object_name = self.runtime.lease_name(&token.name);
        let out = renew_holder(
            &self.runtime,
            object_name.as_str(),
            &token.name,
            &holder,
            ttl,
        )
        .instrument(span)
        .await;
        self.runtime
            .record_lock("renew", &token.name, started, &out);
        out
    }

    /// Token-fenced release (§5.4): reconstruct the `holderIdentity` and clear it if
    /// this holder still holds it, idempotent by absence. Reuses [`release_holder`],
    /// the guard task's own `Release` path.
    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let span = tracing::info_span!(
            spans::LOCK_RELEASE, provider = %self.runtime.provider, lock = %token.name
        );
        let started = std::time::Instant::now();
        let holder = HolderToken::from_token(token);
        let object_name = self.runtime.lease_name(&token.name);
        let out = release_holder(&self.runtime, object_name.as_str(), &token.name, &holder)
            .instrument(span)
            .await;
        self.runtime
            .record_lock("release", &token.name, started, &out);
        out
    }
}

/// Which half of the lock a blocking [`wait_acquire`](K8sLock::wait_acquire) is
/// serving — the only difference between [`lock`](K8sLock::lock) and
/// [`acquire_waiting`](K8sLock::acquire_waiting), which otherwise share the whole
/// wait loop (§5.3).
enum AcquireKind {
    /// [`lock`](K8sLock::lock): hold our own lease (owner = pod identity) and hand
    /// back a [`LockGuard`] via the cancel-proof
    /// [`acquire_guarded`](K8sLock::acquire_guarded) seam.
    Guard,
    /// [`acquire_waiting`](K8sLock::acquire_waiting): acquire for `owner` inline and
    /// hand back the [`LeaseToken`].
    Token { owner: String },
}

/// What a won [`wait_acquire`](K8sLock::wait_acquire) produced, matching its
/// [`AcquireKind`].
enum Won {
    Guard(LockGuard),
    Token(LeaseToken),
}

impl K8sLock {
    /// [`lock`](Self::lock)'s typed entry into the shared blocking loop.
    async fn wait_acquire_guard(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        match self
            .wait_acquire(name, &AcquireKind::Guard, ttl, timeout)
            .await?
        {
            Won::Guard(guard) => Ok(guard),
            // The mode determines the variant, so the token arm is unreachable.
            Won::Token(_) => unreachable!("guard-mode wait_acquire yields a guard"),
        }
    }

    /// [`acquire_waiting`](Self::acquire_waiting)'s typed entry into the shared
    /// blocking loop.
    async fn wait_acquire_token(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let kind = AcquireKind::Token {
            owner: owner.to_owned(),
        };
        match self.wait_acquire(name, &kind, ttl, timeout).await? {
            Won::Token(token) => Ok(token),
            // The mode determines the variant, so the guard arm is unreachable.
            Won::Guard(_) => unreachable!("token-mode wait_acquire yields a token"),
        }
    }

    /// The uninstrumented blocking-acquire loop that [`lock`](Self::lock) and
    /// [`acquire_waiting`](Self::acquire_waiting) span and measure (§5.3). One loop
    /// for both halves so the wait, the watch-driven wake, the budget, and the
    /// shutdown behaviour are identical — only the per-attempt acquire and the value
    /// it hands back differ, per `kind`.
    ///
    /// The guard half is NOT cancel-safe in the caller's frame but never orphans a
    /// Lease: each attempt's claim + guard hand-off runs on the cancel-proof
    /// [`acquire_guarded`](Self::acquire_guarded) seam. The token half runs each
    /// attempt inline and leaves a dropped acquisition's lease to lapse (§5.8.2).
    async fn wait_acquire(
        &self,
        name: &str,
        kind: &AcquireKind,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<Won, ClusterError> {
        if self.shutdown.is_cancelled() {
            return Err(ClusterError::Shutdown);
        }
        let object_name = self.runtime.lease_name(name);
        let deadline = Instant::now() + timeout;

        // Subscribe to the shared waiter registry *before* the first attempt so a
        // release landing between "we saw it held" and "we subscribed" cannot be
        // missed (§5.3). The first subscriber spawns the shared watch and hands its
        // handle to the registry, which aborts it when the last waiter leaves.
        let (notify, first) = self.runtime.waiters.subscribe(name);
        if first {
            let handle = self.spawn_lock_watch(name, object_name.as_str().to_owned());
            self.runtime.waiters.attach_watch(name, handle);
        }
        let _guard = WaiterGuard {
            waiters: Arc::clone(&self.runtime.waiters),
            name: name.to_owned(),
        };

        // Arm the release notification once, before the first attempt, and keep it
        // armed across the loop: `wake()` uses `notify_waiters` (which stores no
        // permit), so a release landing between an attempt and the await is lost
        // unless the `Notified` is already registered. `enable()` at the top of each
        // iteration registers it before the attempt reads the lock's state.
        let notified = notify.notified();
        tokio::pin!(notified);

        loop {
            // Re-check shutdown *before* the acquire: the `select!` below can wake on
            // `shutdown.cancelled()`, and without this a re-entered loop would issue
            // another attempt first — which could return `Ok` during shutdown (the
            // contract requires `Shutdown`) and, on the guard path, spawn a `GuardTask`
            // on an already-cancelled token, leaving the consumer a guard whose
            // commands have no receiver and a Lease held until its TTL lapses (§5.4).
            if self.shutdown.is_cancelled() {
                return Err(ClusterError::Shutdown);
            }
            notified.as_mut().enable();
            let won = match kind {
                AcquireKind::Guard => {
                    let token = HolderToken::generate(&self.runtime.identity);
                    self.acquire_guarded(name, &object_name, token, ttl)
                        .await?
                        .map(Won::Guard)
                }
                AcquireKind::Token { owner } => {
                    let token = HolderToken::generate(owner);
                    self.runtime
                        .try_acquire(&object_name, name, &token, ttl)
                        .await?
                        .map(Won::Token)
                }
            };
            if let Some(won) = won {
                return Ok(won);
            }
            let now = Instant::now();
            match classify_wait(self.shutdown.is_cancelled(), now >= deadline) {
                WaitDecision::Shutdown => return Err(ClusterError::Shutdown),
                WaitDecision::Timeout => {
                    return Err(ClusterError::LockTimeout {
                        name: name.to_owned(),
                        waited: timeout,
                    });
                }
                WaitDecision::Keep => {}
            }
            // Wait for a release/change, our budget, or shutdown — whichever first.
            let remaining = deadline.saturating_duration_since(now);
            tokio::select! {
                () = &mut notified => notified.set(notify.notified()),
                () = tokio::time::sleep(remaining) => {}
                () = self.shutdown.cancelled() => {}
            }
        }
    }
}

impl K8sLock {
    /// Spawns the shared watch feeding [`LockWaiters::wake`] for `name` (§5.3).
    fn spawn_lock_watch(&self, name: &str, object_name: String) -> JoinHandle<()> {
        let runtime = Arc::clone(&self.runtime);
        let name = name.to_owned();
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            let api = runtime.api();
            let wc = watcher::Config::default().fields(&format!("metadata.name={object_name}"));
            let stream = watcher(api, wc);
            tokio::pin!(stream);
            // Wake blocked waiters only on a *meaningful* transition — the holder
            // cleared, changed, or the object deleted/relisted — never on the current
            // holder's own consumer-driven renewals (§5.3). Waking on every event makes
            // every waiter run a full `try_acquire` (a GET plus a possible replace)
            // against a lock it cannot take, so API load would otherwise scale with
            // waiters × renewals. Suppressing a same-holder event is safe because
            // reclamation of a lapsed holder is driven by `Observed` ageing across the
            // waiter's own budget-driven wake-ups (`wait_acquire`'s timer arm), not by
            // this notify.
            let mut last_holder: Option<Option<String>> = None; // outer None: nothing seen yet
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    event = stream.next() => {
                        match event {
                            Some(Ok(event)) => {
                                let (wake, new_holder) = match &event {
                                    watcher::Event::Apply(lease)
                                    | watcher::Event::InitApply(lease) => {
                                        let holder = holder_of(lease);
                                        (last_holder.as_ref() != Some(&holder), Some(holder))
                                    }
                                    // A deletion frees the name; a relist may have
                                    // dropped a release event — wake unconditionally on
                                    // both rather than reasoning about what was missed.
                                    watcher::Event::Delete(_) | watcher::Event::Init => {
                                        (true, None)
                                    }
                                    // Initial-list boundary: nothing to wake for.
                                    watcher::Event::InitDone => (false, None),
                                };
                                if let Some(holder) = new_holder {
                                    last_holder = Some(holder);
                                }
                                if wake {
                                    runtime.waiters.wake(&name);
                                }
                            }
                            // `kube` retries internally, so an error item is transient
                            // (a blocked `lock()` still wakes via its budget or the next
                            // event); log it so a persistent failure is diagnosable.
                            Some(Err(err)) => tracing::warn!(
                                error = %err, lock = %name,
                                "cluster.provider.lock_watch_error: the lock-release watcher \
                                 stream returned an error; kube retries internally"
                            ),
                            None => return,
                        }
                        if !runtime.waiters.has_waiters(&name) {
                            return;
                        }
                    }
                }
            }
        })
    }
}

/// Releases a blocking `lock()`'s waiter subscription on every exit path. The shared
/// watch is owned by [`LockWaiters`] and aborted there on the last unsubscribe, so
/// this guard only needs to deregister.
struct WaiterGuard {
    waiters: Arc<LockWaiters>,
    name: String,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        // `unsubscribe` aborts the shared watch when this is the last waiter.
        let _last = self.waiters.unsubscribe(&self.name);
    }
}

/// The task servicing one held lock's [`LockGuard`] commands (§5.4). Parked on
/// `recv` — no renewal loop, no connection (§3.3) — until a release or the consumer
/// drops the guard.
///
/// Both operations write through the shared holderIdentity fence
/// ([`renew_holder`] / [`release_holder`]) — the same path the token-facing
/// [`DistributedLockBackend::renew`](DistributedLockBackend::renew) /
/// [`release`](DistributedLockBackend::release) take, so a guard-held lease and a
/// token-held lease are the same lease fenced the same way — but the guard **also**
/// gates on a process-local `deadline` first. The guard is a *live in-process*
/// holder, so it has a meaningful local deadline the token (which crosses process
/// boundaries) does not: k8s never server-side-expires a Lease, so without this gate
/// a consumer whose granted TTL lapsed could `renew` and re-extend a claim the fleet
/// is already entitled to reclaim (SC-LOCK-005, §2.8). Past the deadline, `renew`
/// reports `LockExpired` and `release` is a silent no-op, exactly as the token path
/// cannot (it has no live deadline to consult).
struct GuardTask {
    runtime: Arc<LockRuntime>,
    object_name: String,
    /// The unmapped coordination name, for spans and `LockExpired`/error messages.
    name: String,
    token: HolderToken,
    /// The monotonic deadline this claim is valid until (§2.8), re-anchored on every
    /// successful renew. The guard path's liveness proxy for k8s's observational
    /// expiry, since the store carries no hard-expiry a guarded write can test.
    deadline: Instant,
    shutdown: CancellationToken,
}

impl GuardTask {
    async fn run(mut self, mut commands: LockCommandReceiver) {
        loop {
            tokio::select! {
                // Shutdown leaves the Lease exactly as it is — it lapses on its own
                // deadline, so a restart under a held lock revokes nothing (§11).
                () = self.shutdown.cancelled() => return,
                request = commands.recv() => {
                    match request {
                        Some(LockRequest::Renew { new_ttl, responder }) => {
                            let span = tracing::info_span!(
                                spans::LOCK_RENEW, provider = %self.runtime.provider, lock = %self.name
                            );
                            let started = std::time::Instant::now();
                            let out = self.renew(new_ttl).instrument(span).await;
                            self.runtime.record_lock("renew", &self.name, started, &out);
                            responder.respond(out);
                        }
                        Some(LockRequest::Release { responder }) => {
                            let span = tracing::info_span!(
                                spans::LOCK_RELEASE, provider = %self.runtime.provider, lock = %self.name
                            );
                            let started = std::time::Instant::now();
                            let out = self.release().instrument(span).await;
                            self.runtime.record_lock("release", &self.name, started, &out);
                            responder.respond(out);
                            return;
                        }
                        // Guard dropped without releasing: exit, leave the Lease to
                        // lapse via TTL (§5.2).
                        None => return,
                    }
                }
            }
        }
    }

    /// Consumer-driven renew: refuse past the local deadline (§2.8, SC-LOCK-005),
    /// else extend through the shared holderIdentity fence and re-anchor the deadline.
    ///
    /// The deadline check is primary and comes first: k8s never server-side-expires a
    /// Lease, so an expired-but-unstolen claim still bears our `holderIdentity` and
    /// [`renew_holder`] alone would happily re-extend it — the very thing SC-LOCK-005
    /// forbids. The new deadline is anchored to *before* the write is issued (§2.8), so
    /// the guard never believes its claim outlasts the earliest instant a competing
    /// acquirer's observation window permits a steal.
    async fn renew(&mut self, new_ttl: Duration) -> Result<(), ClusterError> {
        if self.deadline <= Instant::now() {
            return Err(ClusterError::LockExpired {
                name: self.name.clone(),
            });
        }
        let issued_at = Instant::now();
        renew_holder(
            &self.runtime,
            &self.object_name,
            &self.name,
            &self.token,
            new_ttl,
        )
        .await?;
        self.deadline = issued_at + new_ttl;
        Ok(())
    }

    /// Consumer-driven release: past the local deadline the fleet may already treat
    /// the lock as free, so issue no write (a successor may hold it) and report the
    /// idempotent `Ok`; otherwise clear through the shared holderIdentity fence.
    async fn release(&self) -> Result<(), ClusterError> {
        if self.deadline <= Instant::now() {
            return Ok(());
        }
        release_holder(&self.runtime, &self.object_name, &self.name, &self.token).await
    }
}

/// The current wall-clock as a `MicroTime` (output for readers, never input to
/// expiry — §2.8).
fn now_micro() -> MicroTime {
    MicroTime(k8s_openapi::jiff::Timestamp::now())
}

/// The current holder's claim duration, read from the Lease's ttl-ms annotation
/// (the exact millisecond TTL, §2.9). `None` when the annotation is absent or
/// unparseable — a foreign/legacy holder — so the caller falls back to its own TTL.
fn observed_ttl(lease: &Lease) -> Option<Duration> {
    lease
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANNOTATION_TTL_MS))
        .and_then(|ms| ms.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// The `(holderIdentity, renewTime)` record for `Observed` equality (§2.8).
fn claim_record(lease: &Lease) -> Record {
    let holder = lease.spec.as_ref().and_then(|s| s.holder_identity.clone());
    let renew = lease
        .spec
        .as_ref()
        .and_then(|spec| spec.renew_time.as_ref())
        .map(|t| t.0.to_string());
    (holder, renew)
}

#[cfg(test)]
mod tests {
    use super::{HolderToken, WaitDecision, classify_wait};
    use cluster_sdk::LeaseToken;

    #[test]
    fn holder_token_round_trips_through_the_wire_form() {
        let token = HolderToken::generate("broker-7");
        let wire = token.to_holder_string();
        assert!(wire.starts_with("broker-7#"));
        assert_eq!(HolderToken::parse(&wire), Some(token));
    }

    #[test]
    fn holder_token_splits_on_the_last_hash() {
        // An owner that itself contains '#' round-trips, because parse splits on the
        // final '#' (the fence, a decimal u64, never contains one).
        let token = HolderToken {
            owner: "team#broker-7".to_owned(),
            fence: 42,
        };
        let parsed = HolderToken::parse(&token.to_holder_string()).unwrap();
        assert_eq!(parsed.owner, "team#broker-7");
        assert_eq!(parsed.fence, 42);
    }

    #[test]
    fn a_holder_that_is_not_owner_hash_fence_is_not_our_token() {
        // No '#' at all.
        assert_eq!(HolderToken::parse("plain-identity"), None);
        // A trailing '#' with no fence is rejected.
        assert_eq!(HolderToken::parse("id#"), None);
        // A non-numeric fence (e.g. a legacy `<identity>#<uuid>` holder, or a foreign
        // controller's holderIdentity) does not parse as one of ours.
        assert_eq!(HolderToken::parse("id#not-a-number"), None);
    }

    #[test]
    fn a_lease_token_reconstructs_the_holder_it_was_acquired_under() {
        // The token half's invariant I7: `from_token` must rebuild exactly the
        // `holderIdentity` `into_lease_token` was minted from, so a remote renew/
        // release fences on the same value the acquiring instance wrote.
        let acquired = HolderToken::generate("broker-7");
        let wire = acquired.to_holder_string();
        let lease_token: LeaseToken = acquired.into_lease_token("res");
        assert_eq!(lease_token.name, "res");
        assert_eq!(
            HolderToken::from_token(&lease_token).to_holder_string(),
            wire
        );
    }

    #[test]
    fn two_tokens_for_one_owner_are_distinct() {
        // The fence: two acquisitions never share a token (§5.1, §5.8.1).
        assert_ne!(HolderToken::generate("me"), HolderToken::generate("me"));
    }

    #[test]
    fn wait_prefers_shutdown_then_timeout_then_keep() {
        // Shutdown wins even past the deadline — a going-down plugin must not hand
        // back a retryable timeout.
        assert_eq!(classify_wait(true, true), WaitDecision::Shutdown);
        assert_eq!(classify_wait(true, false), WaitDecision::Shutdown);
        // Not shutting down, budget elapsed → timeout.
        assert_eq!(classify_wait(false, true), WaitDecision::Timeout);
        // Still within budget → keep waiting.
        assert_eq!(classify_wait(false, false), WaitDecision::Keep);
    }
}
