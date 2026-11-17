//! Per-process lease renewal for every directory record the process registered
//! (`cpt-cf-adr-directory-persistence-and-replication`, `cpt-cf-component-oop-bootstrap`).
//!
//! Under the lease model a registration is only kept observable while its lease
//! is renewed. Several registrars in one process write directory records — the
//! `OoP` presence loop, grpc-hub's gRPC registrar, [`RestAdvertiser`], and
//! event-broker's role registration. The [`PresenceManager`] is the single
//! per-process owner of renewal for *all* of those records: whoever registers a
//! record hands it to the manager, which then renews it on one timer,
//! re-registers it if the directory forgets it, and serializes its own writes
//! so recovery and a concurrent registration cannot reach the directory out of
//! order (DESIGN's "one writer per instance record").
//!
//! The state each record reports (`Starting` / `Serving` / `Draining`) comes
//! from the process readiness signal (see [`ReportedStateSource`]: full
//! readiness in the `OoP` serve path, the dependency/drain checker in-process),
//! so readiness and drain are reflected in the directory without each registrar
//! tracking them itself.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use cf_system_sdks::directory::{
    DirectoryClient, DirectoryNotFound, RegisterInstanceInfo, ReportedState,
};

use crate::runtime::readiness::{DependencyChecker, ReadinessLifecycle, ReadinessState};

/// Where a [`PresenceManager`] reads the state to report for this process,
/// evaluated at each renewal: `Starting` while not ready, `Serving`
/// once ready, `Draining` while shutting down.
///
/// Using the *readiness* signal — not dependency resolution alone — keeps a gear
/// that is failing its readiness check from advertising itself as `Serving`.
/// There are exactly two process topologies, so this is a closed set rather than
/// an open trait.
pub enum ReportedStateSource {
    /// The `OoP` serve path's full readiness: startup completion, dependency
    /// resolution, healthchecks and drain (ADR-0005). `Ready`/`Degraded` are
    /// serving; anything else (including an `Unhealthy` healthcheck) is not.
    Readiness(Arc<ReadinessState>),
    /// The in-process host's dependency/drain signal — it has no aggregate
    /// readiness object, so this is the best available source.
    Dependencies(Arc<DependencyChecker>),
}

impl ReportedStateSource {
    /// The state to report right now.
    pub async fn reported_state(&self) -> ReportedState {
        match self {
            Self::Readiness(readiness) => match readiness.evaluate().await.state {
                ReadinessLifecycle::Draining => ReportedState::Draining,
                ReadinessLifecycle::Ready | ReadinessLifecycle::Degraded => ReportedState::Serving,
                ReadinessLifecycle::Starting => ReportedState::Starting,
            },
            Self::Dependencies(deps) => {
                if deps.is_draining() {
                    ReportedState::Draining
                } else if deps.all_resolved() {
                    ReportedState::Serving
                } else {
                    ReportedState::Starting
                }
            }
        }
    }
}

/// Lower bound on the renewal cadence, so a tiny or zero lease TTL cannot turn
/// the renewal loop into a hot loop.
const MIN_RENEWAL_INTERVAL: Duration = Duration::from_secs(1);

/// Union `incoming` with a half already tracked for the same record, mirroring
/// the directory's half-preserving merge (`GearInstance::with_metadata_of`): a
/// present half in `incoming` replaces the stored one, an absent half keeps it.
/// Keeps the renewal snapshot complete when grpc-hub and the REST advertiser
/// register different halves of one `(gear, instance_id)`.
fn merge_registration(
    stored: &RegisterInstanceInfo,
    mut incoming: RegisterInstanceInfo,
) -> RegisterInstanceInfo {
    if incoming.grpc_services.is_empty() {
        incoming.grpc_services.clone_from(&stored.grpc_services);
    }
    if incoming.rest_endpoint.is_none() {
        incoming.rest_endpoint.clone_from(&stored.rest_endpoint);
    }
    if incoming.openapi_spec.is_none() {
        incoming.openapi_spec.clone_from(&stored.openapi_spec);
    }
    if incoming.openapi_spec_hash.is_none() {
        incoming
            .openapi_spec_hash
            .clone_from(&stored.openapi_spec_hash);
    }
    if incoming.labels.is_empty() {
        incoming.labels.clone_from(&stored.labels);
    }
    if incoming.version.is_none() {
        incoming.version.clone_from(&stored.version);
    }
    if incoming.reported_state.is_none() {
        incoming.reported_state = stored.reported_state;
    }
    incoming
}

/// Identifies a tracked record within the process.
type RecordKey = (String, String);

/// Mutable state behind the manager's lock.
#[derive(Default)]
struct Inner {
    /// Every `(gear, instance_id)` the process registered, mapped to the info
    /// last used to register it so a `NOT_FOUND` heartbeat can re-register.
    /// Held behind an `Arc` so recovery can grab the record handle under the
    /// lock and deep-copy the (up to ~1 MiB `OpenAPI`) info outside it.
    records: HashMap<RecordKey, Arc<RegisterInstanceInfo>>,
    /// Lease TTL the directory last granted, if any. `None` means fall back to
    /// the configured heartbeat interval (an old directory that returns no TTL).
    lease_ttl: Option<Duration>,
    /// Set once [`deregister_all`](PresenceManager::deregister_all) has run.
    /// A `register` after shutdown is rejected rather than silently re-tracked,
    /// so it cannot resurrect a record the manager has already drained.
    closed: bool,
}

/// Owns lease renewal for every directory record registered by this process.
///
/// Create one per process, register each record through
/// [`register`](Self::register), and drive renewal with [`run`](Self::run).
/// During shutdown call [`begin_drain`](Self::begin_drain) (after readiness has
/// flipped to draining) to push an immediate `Draining` heartbeat, then
/// [`deregister_all`](Self::deregister_all) once in-flight requests have
/// drained.
pub struct PresenceManager {
    directory: Arc<dyn DirectoryClient>,
    /// Source of the reported state: the process readiness signal,
    /// evaluated at each renewal.
    readiness: ReportedStateSource,
    /// Heartbeat interval used when the directory returns no lease TTL.
    fallback_interval: Duration,
    inner: parking_lot::Mutex<Inner>,
    /// Serializes this manager's directory *writes* (`register` and
    /// `reregister_after_not_found`). Both read the record and send it to the
    /// directory across an await; without serialization a recovery
    /// re-registration and a concurrent registration of the same record can
    /// reach the directory in the opposite order to the cache, leaving the
    /// directory holding stale metadata. Held across the `register_with_lease`
    /// await, so it must be an async mutex. Heartbeats do not take it (they
    /// carry no metadata half to clobber).
    write_lock: tokio::sync::Mutex<()>,
    /// Wakes the renewal loop for an out-of-band renewal (e.g. the immediate
    /// `Draining` heartbeat at the start of drain).
    wake: Notify,
}

impl PresenceManager {
    /// Create a manager that renews through `directory`, reporting the state
    /// from `readiness`, and heartbeating at `fallback_interval` until a
    /// registration teaches it the directory's lease TTL.
    #[must_use]
    pub fn new(
        directory: Arc<dyn DirectoryClient>,
        readiness: ReportedStateSource,
        fallback_interval: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            directory,
            readiness,
            fallback_interval,
            inner: parking_lot::Mutex::new(Inner::default()),
            write_lock: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
        })
    }

    /// The renewal cadence: a third of the granted lease TTL, or the
    /// configured fallback when the directory returned none, clamped so it can
    /// never hot-loop.
    fn renewal_interval(&self) -> Duration {
        let base = self
            .inner
            .lock()
            .lease_ttl
            .map_or(self.fallback_interval, |ttl| ttl / 3);
        base.max(MIN_RENEWAL_INTERVAL)
    }

    /// Register (or re-register) `info` and track it for renewal.
    ///
    /// The declared state is overwritten with the process's current reported
    /// state so every registrar agrees on one record's liveness. The lease TTL
    /// the directory grants is remembered to pace renewals.
    ///
    /// Several registrars write the same `(gear, instance_id)` — grpc-hub's
    /// gRPC half and the REST advertiser's REST half — and the directory unions
    /// their halves ([`GearInstance::with_metadata_of`](crate::runtime)). `info`
    /// is merged with whatever half is already tracked so the renewal snapshot
    /// reflects that same union; otherwise a `NOT_FOUND` re-register would drop
    /// whichever half the last registrar did not carry.
    ///
    /// Serialized with recovery and shutdown through [`write_lock`](Self::write_lock);
    /// a registration after [`deregister_all`](Self::deregister_all) is rejected
    /// so it cannot resurrect a record the manager has already drained.
    ///
    /// # Errors
    /// Propagates a registration failure (e.g. a gRPC service-name conflict);
    /// the record is still tracked so renewal retries it. Also errors if the
    /// manager has already been drained by `deregister_all`.
    pub async fn register(&self, mut info: RegisterInstanceInfo) -> anyhow::Result<()> {
        info.reported_state = Some(self.readiness.reported_state().await);
        let key = (info.gear.clone(), info.instance_id.clone());
        // Cover the merge *and* the directory write (see `write_lock`). Held
        // across `register_with_lease` so a drain cannot run between tracking
        // the record and the directory write landing.
        let _write = self.write_lock.lock().await;
        // Merge with any tracked half and track before the await, so a record
        // whose first registration fails is still renewed (which re-registers
        // it) rather than lost, and so recovery carries every registrar's half.
        let merged = {
            let mut inner = self.inner.lock();
            // Reject once drained: re-tracking here would resurrect a record
            // `deregister_all` already removed, leaving a zombie nothing renews.
            if inner.closed {
                anyhow::bail!("presence manager is draining; registration rejected");
            }
            let merged = Arc::new(match inner.records.get(&key) {
                Some(existing) => merge_registration(existing, info),
                None => info,
            });
            inner.records.insert(key, Arc::clone(&merged));
            merged
        };

        let ack = self
            .directory
            .register_with_lease((*merged).clone())
            .await?;
        if let Some(ttl) = ack.lease_ttl {
            let shortened = {
                let mut inner = self.inner.lock();
                // A newly learned (or shorter) TTL means the renewal interval
                // just dropped. The loop may already be sleeping on the longer
                // fallback (or a previous, longer lease), so its next renewal
                // would land after this lease expires.
                let shortened = inner.lease_ttl.is_none_or(|prev| ttl < prev);
                inner.lease_ttl = Some(ttl);
                shortened
            };
            if shortened {
                // Wake the loop so it re-reads the interval and renews before
                // the deadline it was previously scheduled for.
                self.wake.notify_one();
            }
        }
        Ok(())
    }

    /// Stop renewing a record without deregistering it, e.g. when the registrar
    /// that owns it deregisters it itself. Prevents the renewal loop from
    /// re-registering a record that was intentionally removed.
    pub fn untrack(&self, gear: &str, instance_id: &str) {
        self.inner
            .lock()
            .records
            .remove(&(gear.to_owned(), instance_id.to_owned()));
    }

    /// Request an immediate renewal pass. Used at the start of drain so the
    /// directory (and the edge) sees `Draining` without waiting for the next
    /// tick. Readiness must already be flipped to draining before calling this.
    pub fn begin_drain(&self) {
        self.wake.notify_one();
    }

    /// Run the renewal loop until `cancel` fires. Each pass heartbeats every
    /// tracked record with the current reported state; a record the directory
    /// has forgotten (`NOT_FOUND`) is re-registered, so recovery after a
    /// directory restart or an eviction takes one heartbeat.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        loop {
            let interval = self.renewal_interval();
            tokio::select! {
                () = cancel.cancelled() => break,
                () = self.wake.notified() => {}
                () = tokio::time::sleep(interval) => {}
            }
            self.renew_all().await;
        }
        tracing::debug!("presence manager renewal loop stopped");
    }

    /// Heartbeat every tracked record once, re-registering any the directory no
    /// longer knows.
    async fn renew_all(&self) {
        let reported = self.readiness.reported_state().await;
        // Snapshot the keys only; the steady-state path just heartbeats, and
        // the cold `NOT_FOUND` path re-reads the current record (below), so the
        // (potentially ~1 MiB) info is never cloned here.
        let keys: Vec<RecordKey> = self.inner.lock().records.keys().cloned().collect();

        for (gear, instance_id) in keys {
            match self
                .directory
                .heartbeat(&gear, &instance_id, Some(reported))
                .await
            {
                Ok(()) => {}
                Err(err) if err.is::<DirectoryNotFound>() => {
                    self.reregister_after_not_found(&gear, &instance_id, reported)
                        .await;
                }
                Err(err) => tracing::warn!(
                    gear = %gear,
                    instance = %instance_id,
                    error = %err,
                    "heartbeat failed; will retry next pass"
                ),
            }
        }
    }

    /// Re-register a record the directory answered `NOT_FOUND` for (a restart or
    /// an eviction), so it becomes routable again.
    ///
    /// Re-registers the record **currently tracked**, read under `write_lock`
    /// rather than the snapshot the renewal pass started with: a registrar may
    /// have merged in newer metadata while the heartbeat was in flight, and
    /// recovery must carry that forward, not roll back. `write_lock` serializes
    /// this with [`register`](Self::register) (see its doc).
    ///
    /// A concurrent [`untrack`](Self::untrack) / [`deregister_all`](Self::deregister_all)
    /// must win: if the record was removed, re-registering would resurrect one
    /// its owner deleted on purpose. The record is read before the await (absent
    /// ⇒ skip) and its membership re-checked after it (removed while in flight ⇒
    /// the re-registration is undone).
    async fn reregister_after_not_found(
        &self,
        gear: &str,
        instance_id: &str,
        reported: ReportedState,
    ) {
        let key = (gear.to_owned(), instance_id.to_owned());
        let _write = self.write_lock.lock().await;
        let Some(tracked) = self.inner.lock().records.get(&key).map(Arc::clone) else {
            return;
        };
        let mut info = (*tracked).clone();
        info.reported_state = Some(reported);
        match self.directory.register_with_lease(info).await {
            Ok(ack) => {
                if !self.inner.lock().records.contains_key(&key) {
                    // Untracked while we were re-registering: undo it so the
                    // removal still wins.
                    if let Err(e) = self.directory.deregister_instance(gear, instance_id).await {
                        tracing::warn!(
                            gear = %gear,
                            instance = %instance_id,
                            error = %e,
                            "could not undo a re-registration raced by untrack"
                        );
                    }
                    return;
                }
                if let Some(ttl) = ack.lease_ttl {
                    self.inner.lock().lease_ttl = Some(ttl);
                }
                tracing::info!(
                    gear = %gear,
                    instance = %instance_id,
                    "re-registered with the directory after a NOT_FOUND heartbeat"
                );
            }
            Err(e) => tracing::warn!(
                gear = %gear,
                instance = %instance_id,
                error = %e,
                "re-registration after NOT_FOUND failed; will retry next pass"
            ),
        }
    }

    /// Deregister every tracked record and stop tracking them. Called at the end
    /// of drain, after in-flight requests have finished, so the manager stays the
    /// single writer of each record it owns.
    ///
    /// Holds [`write_lock`](Self::write_lock) across the whole drain and marks
    /// the manager `closed`, so a [`register`](Self::register) whose directory
    /// write is still in flight cannot land after the deregister (the lock
    /// serializes them), and a `register` that arrives afterwards is rejected
    /// rather than re-tracked (the flag). Either way no record survives the drain.
    pub async fn deregister_all(&self) {
        let _write = self.write_lock.lock().await;
        let records: Vec<RecordKey> = {
            let mut inner = self.inner.lock();
            inner.closed = true;
            inner.records.drain().map(|(k, _)| k).collect()
        };
        for (gear, instance_id) in records {
            if let Err(err) = self
                .directory
                .deregister_instance(&gear, &instance_id)
                .await
            {
                tracing::warn!(
                    gear = %gear,
                    instance = %instance_id,
                    error = %err,
                    "deregistration during shutdown failed"
                );
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "presence_manager_tests.rs"]
mod tests;
