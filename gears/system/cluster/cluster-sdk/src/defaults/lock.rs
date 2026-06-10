// Created: 2026-06-04 by Constructor Tech
//! The CAS-based default distributed-lock backend over `Arc<dyn ClusterCacheBackend>`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::cache::{CacheWatchEvent, ClusterCacheBackend};
use crate::defaults::{ShutdownRevoke, guard, identity};
use crate::error::{ClusterError, ProviderErrorKind};
use crate::lock::{
    DistributedLockBackend, LockCommandReceiver, LockFeatures, LockGuard, LockRequest,
};
use crate::observability::{self, ClusterMetrics, NoopMetrics, result, spans};

/// Records the metric side of a finished lock op (duration + bounded-`result`
/// counter) and the shared provider-error signals. Used by both the backend
/// (`try_lock`/`lock`) and the per-guard task (`extend`/`release`).
fn record_lock<T>(
    metrics: &dyn ClusterMetrics,
    provider: &'static str,
    op: &'static str,
    lock: &str,
    started: std::time::Instant,
    outcome: &Result<T, ClusterError>,
) {
    metrics.lock_op_duration(op, started.elapsed().as_secs_f64());
    metrics.lock_op(op, result::label(outcome));
    if let Err(err) = outcome {
        observability::emit_provider_error(
            metrics,
            provider,
            op,
            observability::ResourceId::Lock(lock),
            err,
        );
    }
}

/// The in-flight command buffer for each [`LockGuard`].
const COMMAND_BUFFER: usize = 4;

/// The maximum number of consecutive *immediately*-ended watch re-subscribes a
/// blocking [`lock`](CasBasedDistributedLockBackend::lock) tolerates before
/// treating the backend watch as structurally unusable. Bounds a busy-spin
/// against a backend that hands back a watch yielding `None` at once.
const MAX_CONSECUTIVE_WATCH_RESETS: u32 = 8;

/// Minimal backoff applied before re-subscribing to a watch that ended
/// immediately, so a busy-spin cannot burn CPU before the cap (or the caller's
/// timeout) fires. It doubles as the "immediate" threshold: a watch that lived
/// at least this long before ending is treated as a legitimate stream rotation
/// rather than a busy-spin, so the acquisition keeps waiting.
///
/// This boundary is a **heuristic**, not a guarantee: a backend that legitimately
/// rotates its watch faster than this is classified as a busy-spin (the backoff
/// still prevents CPU spin, but the unusable-watch cap may eventually fire).
const WATCH_RESUBSCRIBE_BACKOFF: Duration = Duration::from_millis(50);

/// A distributed-lock backend that derives TTL-bounded mutual exclusion from
/// cache compare-and-swap operations (DESIGN §3.11, ADR-001).
///
/// Acquisition is a `put_if_absent(lock_key, holder_id, ttl)`. A blocking
/// [`lock`](DistributedLockBackend::lock) subscribes to a `watch` on the key and
/// retries on each release/expiry event until it acquires or the timeout
/// elapses. Release is **conditional**: the held entry is deleted only if this
/// holder still owns it, so a foreign holder (which re-acquired after this
/// holder's TTL lapsed) is not released while this holder's own lease is
/// unexpired (the conditional delete has a documented non-atomic window — see
/// [`LockGuard::release`]). A crashed holder is reaped by the
/// cache TTL — there is no auto-renewal and there are **no fencing tokens**
/// (the no-remote-in-critical-section rule eliminates the stale-writer scenario,
/// ADR-002); a long critical section extends its lease via
/// [`LockGuard::extend`].
///
/// # Consistency safety (ADR-009)
///
/// Correctness-grade exclusion holds only over a **linearizable** cache.
/// Construct with [`new`](Self::new) (default-safe) or
/// [`new_allow_weak_consistency`](Self::new_allow_weak_consistency) to accept
/// the split-brain risk. [`features`](DistributedLockBackend::features) derives
/// `linearizable` from the underlying cache's consistency.
// @cpt-dod:cpt-cf-clst-dod-sdk-default-backends-lock:p1
pub struct CasBasedDistributedLockBackend {
    cache: Arc<dyn ClusterCacheBackend>,
    /// Cancelled by [`ShutdownRevoke::revoke`] to signal an in-flight blocking
    /// [`lock`](Self::lock) waiter to return [`ClusterError::Shutdown`] promptly
    /// on graceful shutdown (DESIGN §3.13). The waiter runs in the caller's
    /// future (not a spawned task), so there is no task set to await.
    shutdown: CancellationToken,
    /// The bounded `provider` label for emitted signals (default `"unknown"`
    /// until set via [`with_observability`](Self::with_observability)).
    provider: &'static str,
    /// The metrics sink (default [`NoopMetrics`]).
    metrics: Arc<dyn ClusterMetrics>,
}

impl CasBasedDistributedLockBackend {
    const NAME: &'static str = "CasBasedDistributedLockBackend";

    /// Creates a default-safe backend over `cache`.
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidConfig`] when `cache` declares
    /// [`CacheConsistency::EventuallyConsistent`](crate::cache::CacheConsistency),
    /// because correctness-grade exclusion requires linearizable CAS.
    pub fn new(cache: Arc<dyn ClusterCacheBackend>) -> Result<Self, ClusterError> {
        guard::reject_weak_consistency(cache.consistency(), Self::NAME)?;
        Ok(Self::with_cache(cache))
    }

    /// Creates a backend over `cache`, bypassing the consistency guard.
    ///
    /// Always succeeds and emits a `tracing::warn!` acknowledging the
    /// split-brain risk (two holders may transiently acquire the same lock under
    /// partition). Use only when the cache is intentionally eventually
    /// consistent and the consumer accepts that risk (ADR-009).
    #[must_use]
    pub fn new_allow_weak_consistency(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        guard::warn_weak_consistency(cache.consistency(), Self::NAME);
        Self::with_cache(cache)
    }

    fn with_cache(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        Self {
            cache,
            shutdown: CancellationToken::new(),
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

    /// The cache key a named lock claims. Prefixed so a lock does not collide
    /// with a same-named election when both defaults share one cache.
    fn lock_key(name: &str) -> String {
        format!("lock/{name}")
    }

    /// Spawns the guard's command task and returns the consumer-facing guard.
    fn spawn_guard(&self, name: &str, key: String, holder: String) -> LockGuard {
        let (receiver, guard) = LockGuard::channel(name.to_owned(), COMMAND_BUFFER);
        let task = GuardTask {
            cache: Arc::clone(&self.cache),
            name: name.to_owned(),
            key,
            holder,
            provider: self.provider,
            metrics: Arc::clone(&self.metrics),
        };
        tokio::spawn(task.run(receiver));
        guard
    }
}

#[async_trait]
impl DistributedLockBackend for CasBasedDistributedLockBackend {
    fn features(&self) -> LockFeatures {
        LockFeatures::new(self.cache.consistency() == crate::cache::CacheConsistency::Linearizable)
    }

    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        let span =
            tracing::info_span!(spans::LOCK_TRY_LOCK, provider = %self.provider, lock = %name);
        let op_started = std::time::Instant::now();
        let out = async {
            let key = Self::lock_key(name);
            let holder = identity::fresh_id();
            // @cpt-begin:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-acquire
            // @cpt-begin:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-reap
            // @cpt-begin:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-attach
            // @cpt-begin:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-crash
            // @cpt-begin:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-auto
            // @cpt-begin:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-return
            match self
                .cache
                .put_if_absent(&key, holder.as_bytes(), Some(ttl))
                .await?
            {
                Some(_) => Ok(self.spawn_guard(name, key, holder)),
                None => Err(ClusterError::LockContended {
                    name: name.to_owned(),
                }),
            }
            // @cpt-end:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-return
            // @cpt-end:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-auto
            // @cpt-end:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-crash
            // @cpt-end:cpt-cf-clst-algo-distributed-lock-ttl-safety:p1:inst-ts-attach
            // @cpt-end:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-reap
            // @cpt-end:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-acquire
        }
        .instrument(span)
        .await;
        record_lock(
            &*self.metrics,
            self.provider,
            "try_lock",
            name,
            op_started,
            &out,
        );
        out
    }

    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        let span = tracing::info_span!(spans::LOCK_LOCK, provider = %self.provider, lock = %name);
        let op_started = std::time::Instant::now();
        let out = async {
            let key = Self::lock_key(name);
            let holder = identity::fresh_id();
            let started = tokio::time::Instant::now();
            // Subscribe before the first attempt so a release between a failed claim
            // and the wait cannot be missed.
            // @cpt-begin:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-watch
            let mut watch = self.cache.watch(&key).await?;
            // @cpt-end:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-watch
            // Distinguish a busy-spin from a legitimate stream rotation. A watch that
            // ends (`recv` → `None`) *immediately* on every re-subscribe would spin
            // this loop hot (claim → watch ends → re-subscribe → claim …); a watch
            // that lived for a meaningful interval before ending is a normal
            // end-of-stream and the acquisition should keep waiting, bounded only by
            // `timeout`. Only consecutive *immediate* re-ends count toward the cap.
            let mut consecutive_immediate_resets: u32 = 0;
            // Cloned to a local so the `cancelled()` future in the wait `select!`
            // below does not borrow `self`.
            let shutdown = self.shutdown.clone();
            loop {
                // Graceful cluster shutdown observed before the next claim attempt:
                // abandon the wait promptly with a terminal `Shutdown` rather than
                // racing another claim against a backend that is tearing down.
                if shutdown.is_cancelled() {
                    return Err(ClusterError::Shutdown);
                }
                match self
                    .cache
                    .put_if_absent(&key, holder.as_bytes(), Some(ttl))
                    .await
                {
                    Ok(Some(_)) => return Ok(self.spawn_guard(name, key, holder)),
                    Ok(None) => {}
                    Err(err) => return Err(err),
                }
                // Treat an exhausted *or zero* budget as a timeout: a zero remaining
                // would otherwise let an always-ready (e.g. closed) watch spin the
                // loop at no time cost until the cap, reporting an unusable-watch
                // error where the caller's deadline is the real binding constraint.
                let Some(remaining) = timeout
                    .checked_sub(started.elapsed())
                    .filter(|r| !r.is_zero())
                else {
                    return Err(ClusterError::LockTimeout {
                        name: name.to_owned(),
                        waited: started.elapsed(),
                    });
                };
                let recv_started = tokio::time::Instant::now();
                let waited = tokio::select! {
                    // Graceful cluster shutdown: abandon the wait promptly with a
                    // terminal `Shutdown` (`cpt-cf-clst-fr-shutdown-revoke`). Held
                    // locks lapse via TTL; this only resolves an in-flight wait.
                    () = shutdown.cancelled() => return Err(ClusterError::Shutdown),
                    waited = tokio::time::timeout(remaining, watch.recv()) => waited,
                };
                match waited {
                    Err(_elapsed) => {
                        return Err(ClusterError::LockTimeout {
                            name: name.to_owned(),
                            waited: started.elapsed(),
                        });
                    }
                    Ok(Some(CacheWatchEvent::Closed(err))) => return Err(err),
                    // Any event (release / expiry / lag / reset) → retry the claim.
                    Ok(Some(_)) => consecutive_immediate_resets = 0,
                    // End-of-stream (sender dropped without a terminal `Closed`).
                    // Re-subscribe to keep waiting within the remaining timeout.
                    Ok(None) if recv_started.elapsed() >= WATCH_RESUBSCRIBE_BACKOFF => {
                        // The watch lived a meaningful interval: a legitimate
                        // rotation, not a busy-spin. Keep waiting.
                        consecutive_immediate_resets = 0;
                        watch = self.cache.watch(&key).await?;
                    }
                    Ok(None) => {
                        // Ended immediately: a busy-spin symptom. Cap consecutive
                        // immediate re-ends so a structurally unusable watch surfaces
                        // instead of spinning, and back off so it cannot burn CPU
                        // before the cap (or the timeout) fires.
                        consecutive_immediate_resets += 1;
                        if consecutive_immediate_resets >= MAX_CONSECUTIVE_WATCH_RESETS {
                            tracing::warn!(
                                lock = name,
                                immediate_resubscribes = MAX_CONSECUTIVE_WATCH_RESETS,
                                "distributed-lock backend watch ended immediately on every \
                             re-subscribe; treating it as structurally unusable for blocking \
                             acquisition and aborting the wait"
                            );
                            return Err(ClusterError::Provider {
                                kind: ProviderErrorKind::Other,
                                message: format!(
                                    "distributed-lock backend watch for `{name}` ended immediately \
                                 {MAX_CONSECUTIVE_WATCH_RESETS} times in a row; the watch is \
                                 unusable for blocking acquisition"
                                ),
                            });
                        }
                        // Clamp to the remaining wait so a tight `timeout` is not
                        // overshot by a full backoff interval.
                        tokio::time::sleep(WATCH_RESUBSCRIBE_BACKOFF.min(remaining)).await;
                        watch = self.cache.watch(&key).await?;
                    }
                }
            }
        }
        .instrument(span)
        .await;
        record_lock(
            &*self.metrics,
            self.provider,
            "lock",
            name,
            op_started,
            &out,
        );
        out
    }
}

#[async_trait]
impl ShutdownRevoke for CasBasedDistributedLockBackend {
    /// Revokes in-flight blocking acquisition on graceful shutdown
    /// (`cpt-cf-clst-fr-shutdown-revoke`): cancels the shared token so every
    /// waiting [`lock`](Self::lock) call returns [`ClusterError::Shutdown`]
    /// promptly. No task set is awaited — a waiter runs in the caller's own
    /// future, not a spawned task — and no remote release is performed; held
    /// locks lapse via TTL (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).
    async fn revoke(&self) {
        self.shutdown.cancel();
    }
}

/// The background task that completes a held lock's `extend`/`release` commands
/// and self-terminates on channel closure (the consumer dropping its guard).
struct GuardTask {
    cache: Arc<dyn ClusterCacheBackend>,
    name: String,
    key: String,
    holder: String,
    provider: &'static str,
    metrics: Arc<dyn ClusterMetrics>,
}

impl GuardTask {
    async fn run(self, mut receiver: LockCommandReceiver) {
        while let Some(request) = receiver.recv().await {
            match request {
                LockRequest::Extend {
                    additional_ttl,
                    responder,
                } => {
                    let span = tracing::info_span!(
                        spans::LOCK_EXTEND,
                        provider = %self.provider,
                        lock = %self.name
                    );
                    let op_started = std::time::Instant::now();
                    let out = self.extend(additional_ttl).instrument(span).await;
                    record_lock(
                        &*self.metrics,
                        self.provider,
                        "extend",
                        &self.name,
                        op_started,
                        &out,
                    );
                    responder.respond(out);
                }
                LockRequest::Release { responder } => {
                    let span = tracing::info_span!(
                        spans::LOCK_RELEASE,
                        provider = %self.provider,
                        lock = %self.name
                    );
                    let op_started = std::time::Instant::now();
                    let out = self.release().instrument(span).await;
                    record_lock(
                        &*self.metrics,
                        self.provider,
                        "release",
                        &self.name,
                        op_started,
                        &out,
                    );
                    responder.respond(out);
                    // Release consumes the guard — the task is done.
                    return;
                }
            }
        }
        // The consumer dropped the guard without releasing: no I/O, the entry
        // lapses via TTL (the safety net).
    }

    /// Refreshes the lease only while this holder still owns it. The lease is
    /// **reset** to `additional_ttl` from now — it is *not* added to the time
    /// already left (the cache exposes no remaining-TTL read, so this CAS-based
    /// default refreshes rather than strictly adds). The caller must therefore
    /// pass the full desired remaining duration; an `additional_ttl` smaller
    /// than the time currently left would shorten the lease.
    ///
    /// Non-atomic window (accepted tradeoff, ADR-002): this is a `get`-then-CAS,
    /// and the CAS matches on *version* only. If the lease lapses between the
    /// `get` and the CAS and a new holder re-acquires via `put_if_absent` (a
    /// fresh entry at version `1`), a holder whose last-seen version was also `1`
    /// can have its CAS match and overwrite the foreign entry — a lock steal,
    /// strictly worse than the release window's delete. Bounded by the same
    /// critical-section-shorter-than-TTL rule as [`release`](Self::release); see
    /// [`LockGuard::extend`](crate::lock::LockGuard::extend) for the consumer note.
    async fn extend(&self, additional_ttl: Duration) -> Result<(), ClusterError> {
        match self.cache.get(&self.key).await {
            Ok(Some(entry)) if entry.value.as_slice() == self.holder.as_bytes() => self
                .cache
                .compare_and_swap(
                    &self.key,
                    entry.version,
                    self.holder.as_bytes(),
                    Some(additional_ttl),
                )
                .await
                .map(|_entry| ())
                .map_err(|err| match err {
                    // A concurrent change won the race — we no longer hold it.
                    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-expired
                    // @cpt-begin:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-expired-return
                    ClusterError::CasConflict { .. } => ClusterError::LockExpired {
                        name: self.name.clone(),
                    },
                    // @cpt-end:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-expired-return
                    // @cpt-end:cpt-cf-clst-flow-distributed-lock-wait:p1:inst-wt-expired
                    other => other,
                }),
            // Not ours (TTL lapsed, possibly re-acquired) or already gone.
            Ok(_) => Err(ClusterError::LockExpired {
                name: self.name.clone(),
            }),
            Err(err) => Err(err),
        }
    }

    /// Deletes the entry only if this holder still owns it, so a foreign holder
    /// is never released (`cpt-cf-clst-algo-sdk-default-backends-cas-lock`).
    ///
    /// **Non-atomic window (accepted tradeoff, ADR-002):** the cache has no
    /// conditional / CAS delete, so this is a `get`-then-`delete`. The window is
    /// safe **only while this holder's own TTL is still unexpired**: if the lease
    /// lapsed between the `get` (which saw our value) and the `delete`, a new
    /// holder could have re-acquired in that gap and the unconditional `delete`
    /// would remove the *foreign* holder's entry, breaking mutual exclusion. The
    /// consumer contract that keeps this safe is the critical-section rule — the
    /// critical section (and thus the time to reach this release) must be shorter
    /// than the lock TTL (DESIGN §2.2/§3.3). Use [`LockGuard::extend`] to refresh
    /// the lease before it lapses for a long critical section.
    async fn release(&self) -> Result<(), ClusterError> {
        // @cpt-begin:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-release
        // @cpt-begin:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-compare
        match self.cache.get(&self.key).await {
            // @cpt-begin:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-release
            Ok(Some(entry)) if entry.value.as_slice() == self.holder.as_bytes() => {
                self.cache.delete(&self.key).await.map(|_existed| ())
            }
            // @cpt-end:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-release
            // A foreign holder's entry is left intact; from this holder's view
            // the lease is already gone, so release is a success.
            // @cpt-begin:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-foreign
            // @cpt-begin:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-skip
            Ok(_) => Ok(()),
            // @cpt-end:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-skip
            // @cpt-end:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-foreign
            Err(err) => Err(err),
        }
        // @cpt-end:cpt-cf-clst-algo-distributed-lock-release-if-holder:p1:inst-rh-compare
        // @cpt-end:cpt-cf-clst-algo-sdk-default-backends-cas-lock:p1:inst-cl-release
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod lock_tests;
