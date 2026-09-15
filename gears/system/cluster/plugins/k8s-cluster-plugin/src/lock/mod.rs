//! Native distributed lock over one `Lease` per lock name (DESIGN.md §5).
//!
//! [`K8sLock`] implements [`DistributedLockBackend`] over
//! `coordination.k8s.io/v1.Lease`. A held lock is a Lease carrying our per-acquisition
//! **holder token** (`<identity>#<uuid>`, §5.1); acquisition is create-or-guarded-
//! claim (§5.2); a blocking [`lock()`](DistributedLockBackend::lock) establishes a
//! watch on the one Lease *before* its first attempt and shares it with any
//! same-process waiter via [`waiters`] (§5.3); renew and release are token-fenced
//! guarded writes (§5.4); and release **clears** the holder rather than deleting the
//! object, with a background [`reaper`] pruning long-empty objects (§5.5).
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
use uuid::Uuid;

use cluster_sdk::lock::{
    DistributedLockBackend, LockCommandReceiver, LockFeatures, LockGuard, LockRequest,
};
use cluster_sdk::observability::{self, ResourceId, result, spans};
use cluster_sdk::{ClusterError, ClusterMetrics};
use tracing::Instrument as _;

use crate::client::ResolvedClient;
use crate::config::K8sLockConfig;
use crate::guarded::{self, CallSite, Created, Replaced};
use crate::k8s_error;
use crate::lease::{holder_of, lease_duration_seconds, ttl_ms};
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

/// A per-acquisition lock holder token: `<identity>#<uuid-v4>` (§5.1).
///
/// The identity prefix answers "which replica holds this?" in `kubectl` without a
/// lookup; the fresh UUID makes two acquisitions unconfusable, which is what makes
/// renew/release safe against a successor and forces two in-process acquisitions to
/// arbitrate through the API server exactly as two processes would (§5.1). Only this
/// plugin parses the `#`, and it splits on the **last** one so an identity may
/// itself contain `#`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderToken {
    identity: String,
    uuid: String,
}

impl HolderToken {
    /// A fresh token for `identity` with a new v4 UUID (§5.1).
    #[must_use]
    pub fn generate(identity: &str) -> Self {
        Self {
            identity: identity.to_owned(),
            uuid: Uuid::new_v4().to_string(),
        }
    }

    /// The wire form written to `holderIdentity`: `<identity>#<uuid>`.
    #[must_use]
    pub fn to_holder_string(&self) -> String {
        format!("{}#{}", self.identity, self.uuid)
    }

    /// Parses a `holderIdentity`, splitting on the **last** `#` (§5.1). Returns
    /// `None` for a holder with no `#` — a foreign/legacy holder this plugin did not
    /// write.
    ///
    /// The exact inverse of [`to_holder_string`](Self::to_holder_string). The acquire
    /// path compares the raw `holderIdentity` string rather than a parsed token, so
    /// this is retained as the token codec's other half — exercised by the unit tests
    /// and used by `kubectl`-side diagnostics — rather than consumed on a hot path.
    #[allow(dead_code)]
    #[must_use]
    pub fn parse(holder: &str) -> Option<Self> {
        let (identity, uuid) = holder.rsplit_once('#')?;
        if uuid.is_empty() {
            return None;
        }
        Some(Self {
            identity: identity.to_owned(),
            uuid: uuid.to_owned(),
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
/// (and the watch field selector), the other the `name` annotation `lock_name_of`
/// reads back — so transposing them, which would write a Lease under the wrong name
/// while annotating it with the other, must not compile.
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

    /// One acquire attempt for `name` under `token` (§5.2). `Ok(Some((lease,
    /// issued_at)))` on a won claim — the written object plus the instant its write was
    /// issued, which the guard dates its deadline from (§2.8) — `Ok(None)` on
    /// contention, `Err` on a backend fault.
    ///
    /// On [`LockRuntime`] rather than [`K8sLock`] so the whole acquire can be driven
    /// from a spawned, cancel-proof task (§5.4) that owns only an `Arc<LockRuntime>`.
    async fn try_acquire(
        &self,
        object: &ObjectName,
        coordination_name: &str,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<Option<(Lease, Instant)>, ClusterError> {
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
            Ok(None)
        }
    }

    /// Creates the Lease as ours; a `409 AlreadyExists` is contention this tick. On a
    /// won claim returns the written object paired with the instant the create was
    /// *issued* (captured before it), which dates the guard's deadline (§2.8).
    async fn create_claim(
        &self,
        coordination_name: &str,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<Option<(Lease, Instant)>, ClusterError> {
        let lease = self.new_claim(coordination_name, token, ttl)?;
        let api = self.api();
        let issued_at = Instant::now();
        let created = self
            .timed(
                "create lock lease",
                guarded::create(&api, &lease, CallSite::LockAcquire),
            )
            .await?;
        Ok(match created {
            Created::Created(applied) => Some((*applied, issued_at)),
            Created::Exists => None,
        })
    }

    /// Guarded replace claiming a free/lapsed Lease; a `409` is contention (§5.2).
    async fn guarded_claim(
        &self,
        object: &ObjectName,
        coordination_name: &str,
        mut lease: Lease,
        token: &HolderToken,
        ttl: Duration,
    ) -> Result<Option<(Lease, Instant)>, ClusterError> {
        // Adopt-path parity with `new_claim`: an object this plugin did not create
        // (a foreign/legacy Lease occupying the same computed name) must still carry
        // the managed-by/primitive labels and the `name` annotation once we claim it,
        // or the reaper's label-selector list never sees it (a released object then
        // leaks) and `lock_name_of` reports the object name instead of the
        // coordination name in `LockExpired`/`LockTimeout` (§2.2, §5.5).
        set_identity(&mut lease, coordination_name);
        set_holder(&mut lease, token, ttl, true)?;
        let api = self.api();
        let issued_at = Instant::now();
        let replaced = self
            .timed(
                "claim lock lease",
                guarded::replace(&api, object.as_str(), &lease, CallSite::LockAcquire),
            )
            .await?;
        Ok(match replaced {
            Replaced::Applied(applied) => Some((*applied, issued_at)),
            Replaced::Conflict => None,
        })
    }
}

/// Clears the holder on `held` with a token-fenced (`resourceVersion`-guarded)
/// replace, freeing the lock without deleting the object (§5.4, §5.5). Shared by the
/// guard task's `release` and the cancel-safety cleanup path, which releases a claim
/// whose caller's future was dropped before the guard could be handed over. A `409`
/// (a successor already took over) satisfies the same postcondition, so it is `Ok`.
async fn release_claim(
    runtime: &LockRuntime,
    object_name: &str,
    held: &Lease,
) -> Result<(), ClusterError> {
    let mut lease = held.clone();
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
        Replaced::Applied(_) | Replaced::Conflict => Ok(()),
    }
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
/// (§5.5) and the `name` annotation `lock_name_of` reads back for error messages
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
            match runtime.try_acquire(&object, &name, &token, ttl).await {
                Ok(Some((lease, issued_at))) => {
                    let (commands, guard) = LockGuard::channel(name.clone(), GUARD_COMMAND_BUFFER);
                    let task = GuardTask {
                        runtime: Arc::clone(&runtime),
                        object_name: object.into_string(),
                        token,
                        held: lease,
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
                                release_claim(&runtime, &task.object_name, &task.held).await
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
        let out = self.lock_inner(name, ttl, timeout).instrument(span).await;
        self.runtime.record_lock("lock", name, started, &out);
        out
    }
}

impl K8sLock {
    /// The uninstrumented blocking-acquire loop that [`lock`](Self::lock) spans and
    /// measures (§5.3).
    ///
    /// NOT cancel-safe: each attempt's claim + guard hand-off runs on the cancel-proof
    /// [`acquire_guarded`](K8sLock::acquire_guarded) seam, so dropping this future
    /// between the write and the hand-off cannot orphan a held Lease (§5.4).
    async fn lock_inner(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
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
        // iteration registers it before `try_acquire` reads the lock's state.
        let notified = notify.notified();
        tokio::pin!(notified);

        loop {
            // Re-check shutdown *before* the acquire: the `select!` below can wake on
            // `shutdown.cancelled()`, and without this a re-entered loop would issue
            // another `try_acquire` first — which could return `Ok(guard)` during
            // shutdown (the contract requires `Shutdown`) and spawn a `GuardTask` on
            // an already-cancelled token, leaving the consumer a guard whose commands
            // have no receiver and a Lease held until its TTL lapses (§5.4).
            if self.shutdown.is_cancelled() {
                return Err(ClusterError::Shutdown);
            }
            notified.as_mut().enable();
            let token = HolderToken::generate(&self.runtime.identity);
            if let Some(guard) = self.acquire_guarded(name, &object_name, token, ttl).await? {
                return Ok(guard);
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
            // waiter's own budget-driven wake-ups (`lock_inner`'s timer arm), not by
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
struct GuardTask {
    runtime: Arc<LockRuntime>,
    object_name: String,
    token: HolderToken,
    /// The last-written Lease (its `resourceVersion` for the next guarded write).
    held: Lease,
    /// The monotonic deadline our claim is valid until (§2.8).
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
                            let name = lock_name_of(&self.held);
                            let span = tracing::info_span!(
                                spans::LOCK_RENEW, provider = %self.runtime.provider, lock = %name
                            );
                            let started = std::time::Instant::now();
                            let out = self.renew(new_ttl).instrument(span).await;
                            self.runtime.record_lock("renew", &name, started, &out);
                            responder.respond(out);
                        }
                        Some(LockRequest::Release { responder }) => {
                            let name = lock_name_of(&self.held);
                            let span = tracing::info_span!(
                                spans::LOCK_RELEASE, provider = %self.runtime.provider, lock = %name
                            );
                            let started = std::time::Instant::now();
                            let out = self.release().instrument(span).await;
                            self.runtime.record_lock("release", &name, started, &out);
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

    /// Token-fenced renew (§5.4): reset `renewTime`/TTL if we still hold and our
    /// deadline has not passed, else [`ClusterError::LockExpired`].
    async fn renew(&mut self, new_ttl: Duration) -> Result<(), ClusterError> {
        if self.deadline <= Instant::now() {
            return Err(ClusterError::LockExpired {
                name: lock_name_of(&self.held),
            });
        }
        let mut lease = self.held.clone();
        // Renewal: preserve `acquireTime` (this holder's original acquisition), refresh
        // only `renewTime`/TTL (§2.10).
        set_holder(&mut lease, &self.token, new_ttl, false)?;
        let api = self.runtime.api();
        // Anchor the renewed deadline to when the write was *issued*, not its response,
        // so we never believe our claim outlasts the earliest moment a competing
        // acquirer's `Observed` window permits a steal (§2.8). `deadline` also gates
        // `renew`/`release`'s "do not write, a successor may already hold it" checks,
        // so an over-long deadline could let a lapsed holder write.
        let issued_at = Instant::now();
        let replaced = self
            .runtime
            .timed(
                "renew lock lease",
                guarded::replace(&api, &self.object_name, &lease, CallSite::LockRenew),
            )
            .await?;
        match replaced {
            Replaced::Applied(applied) => {
                self.held = *applied;
                self.deadline = issued_at + new_ttl;
                Ok(())
            }
            // A 409 means our lease moved on — a successor stole it after our lapse.
            Replaced::Conflict => Err(ClusterError::LockExpired {
                name: lock_name_of(&self.held),
            }),
        }
    }

    /// Token-fenced release (§5.4, §5.5): clear the holder if we still hold within
    /// our deadline; a lapsed/foreign claim is left untouched. Never deletes.
    async fn release(&mut self) -> Result<(), ClusterError> {
        // Our deadline passed: the fleet may treat the lock as free, so do not write
        // — a successor may already hold it (§5.4).
        if self.deadline <= Instant::now() {
            return Ok(());
        }
        release_claim(&self.runtime, &self.object_name, &self.held).await
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

/// The unmapped coordination name from a Lease's annotation, for error messages
/// (falls back to the object name when the annotation is absent).
fn lock_name_of(lease: &Lease) -> String {
    lease
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANNOTATION_NAME))
        .cloned()
        .or_else(|| lease.metadata.name.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{HolderToken, WaitDecision, classify_wait};

    #[test]
    fn holder_token_round_trips_through_the_wire_form() {
        let token = HolderToken::generate("broker-7");
        let wire = token.to_holder_string();
        assert!(wire.starts_with("broker-7#"));
        assert_eq!(HolderToken::parse(&wire), Some(token));
    }

    #[test]
    fn holder_token_splits_on_the_last_hash() {
        // An identity that itself contains '#' round-trips, because parse splits on
        // the final '#' (the UUID never contains one).
        let token = HolderToken {
            identity: "team#broker-7".to_owned(),
            uuid: "abc-123".to_owned(),
        };
        let parsed = HolderToken::parse(&token.to_holder_string()).unwrap();
        assert_eq!(parsed.identity, "team#broker-7");
        assert_eq!(parsed.uuid, "abc-123");
    }

    #[test]
    fn a_holder_without_a_hash_is_not_our_token() {
        assert_eq!(HolderToken::parse("plain-identity"), None);
        // A trailing '#' with no uuid is also rejected.
        assert_eq!(HolderToken::parse("id#"), None);
    }

    #[test]
    fn two_tokens_for_one_identity_are_distinct() {
        // The fence: two acquisitions never share a token (§5.1).
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
