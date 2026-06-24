//! The CAS-based default leader-election backend over `Arc<dyn ClusterCacheBackend>`.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use rand::RngExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::defaults::{ELECTION_KEY_PREFIX, ShutdownRevoke, guard, identity};
use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::cache::{CacheWatch, CacheWatchEvent, ClusterCacheBackend};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{
    ElectionConfig, LeaderElectionBackend, LeaderElectionFeatures, LeaderStatus, LeaderWatch,
    LeaderWatchEvent, LeaderWatchSender, ResignReceiver, ResignResponder,
};
use cluster_sdk::observability::{self, ClusterMetrics, NoopMetrics, logs, spans, transition};

/// The in-flight event buffer for each [`LeaderWatch`].
const EVENT_BUFFER: usize = 16;

/// A leader-election backend that derives single-leader behavior from cache
/// compare-and-swap operations (DESIGN §3.11, ADR-001).
///
/// Candidacy is a `put_if_absent(election_key, node_id, ttl)`; the claim is
/// renewed on [`ElectionConfig::renewal_interval`] via a version
/// `compare_and_swap`, so a renewal that races a foreign takeover is detected
/// as a [`ClusterError::CasConflict`] and surfaces as
/// [`LeaderStatus::Lost`] followed by auto-reenrollment. A `watch` on the
/// election key reconciles status reactively: it issues no renewal
/// `compare_and_swap` (only the renewal timer does), though it may
/// opportunistically re-`claim` a vacant key. A renewal's own change event
/// therefore reconciles to a no-op status check, so it cannot re-trigger a
/// renewal.
///
/// # Consistency safety (ADR-009)
///
/// The at-most-one-leader guarantee holds only over a **linearizable** cache.
/// Construct with [`new`](Self::new) (default-safe, rejects an
/// eventually-consistent cache) or, to intentionally accept the split-brain
/// risk, [`new_allow_weak_consistency`](Self::new_allow_weak_consistency).
/// [`features`](LeaderElectionBackend::features) derives `linearizable` from the
/// underlying cache's consistency.
pub struct CasBasedLeaderElectionBackend {
    cache: Arc<dyn ClusterCacheBackend>,
    /// Cancelled by [`ShutdownRevoke::revoke`] to signal every in-flight
    /// election task to surface a terminal shutdown (DESIGN §3.13).
    shutdown: CancellationToken,
    /// Handles of the spawned election tasks, so `revoke` can await their
    /// shutdown emit. Finished handles are pruned as new elections start.
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// The bounded `provider` label for emitted signals (default `"unknown"`
    /// until set via [`with_observability`](Self::with_observability)).
    provider: &'static str,
    /// The metrics sink (default [`NoopMetrics`]).
    metrics: Arc<dyn ClusterMetrics>,
}

impl CasBasedLeaderElectionBackend {
    const NAME: &'static str = "CasBasedLeaderElectionBackend";

    /// Creates a default-safe backend over `cache`.
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidConfig`] when `cache` declares
    /// [`CacheConsistency::EventuallyConsistent`](cluster_sdk::cache::CacheConsistency),
    /// because the at-most-one-leader guarantee requires linearizable CAS.
    pub fn new(cache: Arc<dyn ClusterCacheBackend>) -> Result<Self, ClusterError> {
        guard::reject_weak_consistency(cache.consistency(), Self::NAME)?;
        Ok(Self::with_cache(cache))
    }

    /// Creates a backend over `cache`, bypassing the consistency guard.
    ///
    /// Always succeeds and emits a `tracing::warn!` acknowledging the
    /// split-brain risk. Use only when the cache is intentionally
    /// eventually consistent and the consumer accepts that two leaders may be
    /// elected under partition (ADR-009).
    #[must_use]
    pub fn new_allow_weak_consistency(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        guard::warn_weak_consistency(cache.consistency(), Self::NAME);
        Self::with_cache(cache)
    }

    fn with_cache(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        Self {
            cache,
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

    /// Records a spawned election task's handle, pruning any that have already
    /// finished so the set stays bounded across many short-lived elections.
    fn track(&self, handle: JoinHandle<()>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|handle| !handle.is_finished());
        tasks.push(handle);
    }

    /// The cache key a named election claims. Prefixed so an election does not
    /// collide with a same-named lock when both defaults share one cache.
    fn election_key(name: &str) -> String {
        format!("{ELECTION_KEY_PREFIX}{name}")
    }

    async fn join(&self, name: &str, config: ElectionConfig) -> Result<LeaderWatch, ClusterError> {
        let span =
            tracing::info_span!(spans::LEADER_ELECT, provider = %self.provider, election = %name);
        let out = async {
            // Refuse to enrol a new election once graceful shutdown has begun:
            // `revoke()` drains and awaits the tracked tasks, and a task spawned
            // after that drain would escape the revocation-completion guarantee.
            if self.shutdown.is_cancelled() {
                return Err(ClusterError::Shutdown);
            }
            let key = Self::election_key(name);
            let node_id = identity::fresh_id();
            // Subscribe before the first claim so a transition between the claim and
            // the watch establishment cannot be missed.
            let cache_watch = self.cache.watch(&key).await?;
            let initial = match self
                .cache
                .put_if_absent(PutRequest {
                    key: &key,
                    value: node_id.as_bytes(),
                    ttl: Ttl::Of(config.ttl()),
                })
                .await?
            {
                Some(_) => LeaderStatus::Leader,
                None => LeaderStatus::Follower,
            };
            let (sender, resign_rx, mut watch) =
                LeaderWatch::channel(EVENT_BUFFER, LeaderStatus::Follower);
            // Stamp the watch so an `auto_restart`ed consumer emits the watch-reset
            // signals (`cluster_watch_resets_total` / `cluster.watch.reset`).
            watch.set_observability(self.provider, Arc::clone(&self.metrics));
            let task = ElectionTask {
                cache: Arc::clone(&self.cache),
                name: name.to_owned(),
                key,
                node_id,
                config,
                sender,
                am_leader: matches!(initial, LeaderStatus::Leader),
                missed: 0,
                shutdown: self.shutdown.clone(),
                provider: self.provider,
                metrics: Arc::clone(&self.metrics),
            };
            self.track(tokio::spawn(task.run(
                initial,
                Some(cache_watch),
                resign_rx,
            )));
            Ok(watch)
        }
        .instrument(span)
        .await;
        if let Err(err) = &out {
            observability::emit_provider_error(
                &*self.metrics,
                self.provider,
                "elect",
                observability::ResourceId::Election(name),
                err,
            );
        }
        out
    }
}

#[async_trait]
impl ShutdownRevoke for CasBasedLeaderElectionBackend {
    /// Revokes leadership confidence on graceful shutdown
    /// (`cpt-cf-clst-fr-shutdown-revoke`): cancels the shared token — so every
    /// in-flight election task latches `Status(Lost)` then `Closed(Shutdown)` —
    /// and awaits those tasks, so a current leader has observed loss before this
    /// returns. No remote release is performed; claims lapse via TTL
    /// (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).
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

#[async_trait]
impl LeaderElectionBackend for CasBasedLeaderElectionBackend {
    fn features(&self) -> LeaderElectionFeatures {
        LeaderElectionFeatures::new(
            self.cache.consistency() == cluster_sdk::cache::CacheConsistency::Linearizable,
        )
    }

    async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
        self.join(name, ElectionConfig::default()).await
    }

    async fn elect_with_config(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError> {
        self.join(name, config).await
    }
}

/// The background task that owns the renewal loop and self-terminates on
/// channel closure (the consumer dropping its [`LeaderWatch`]).
struct ElectionTask {
    cache: Arc<dyn ClusterCacheBackend>,
    /// The election name (the span/log `election` attribute), distinct from the
    /// prefixed cache [`key`](Self::key).
    name: String,
    key: String,
    node_id: String,
    config: ElectionConfig,
    sender: LeaderWatchSender,
    am_leader: bool,
    missed: u8,
    /// Cancelled by [`ShutdownRevoke::revoke`] on graceful cluster shutdown.
    shutdown: CancellationToken,
    /// The bounded `provider` label for emitted signals.
    provider: &'static str,
    /// The metrics sink.
    metrics: Arc<dyn ClusterMetrics>,
}

impl ElectionTask {
    async fn run(
        mut self,
        initial: LeaderStatus,
        mut cache_watch: Option<CacheWatch>,
        mut resign_rx: ResignReceiver,
    ) {
        // Emit the resolved initial status. If the consumer is already gone,
        // best-effort release and stop.
        if !self.emit_initial(initial).await {
            let _release = self.release_if_holder().await;
            return;
        }
        let interval = self.config.renewal_interval();
        // A recomputed absolute deadline rather than a fixed-period `interval`, so
        // a follower's reclaim tick can carry per-tick jitter (a leader's renewal
        // stays on the exact cadence — see `next_renewal_delay`).
        let mut next_tick = tokio::time::Instant::now() + self.next_renewal_delay(interval);
        // Cloned to a local so the `cancelled()` future does not borrow `self`,
        // which the other arms' bodies mutate.
        let shutdown = self.shutdown.clone();
        loop {
            let tick = tokio::time::sleep_until(next_tick);
            tokio::pin!(tick);
            tokio::select! {
                // Graceful cluster shutdown: revoke leadership confidence and end
                // the watch terminally, without remote release (TTL reaps the
                // claim). A current leader observes `Status(Lost)` first.
                () = shutdown.cancelled() => {
                    self.sender.revoke_for_shutdown(self.am_leader);
                    return;
                }
                () = &mut tick => {
                    if !self.renew_tick().await {
                        break;
                    }
                    next_tick = tokio::time::Instant::now() + self.next_renewal_delay(interval);
                }
                event = recv_optional(&mut cache_watch) => {
                    match event {
                        Some(ev) => {
                            if !self.on_watch_event(ev).await {
                                break;
                            }
                        }
                        // The cache watch ended; keep tracking via the renewal
                        // timer alone.
                        None => cache_watch = None,
                    }
                }
                resign = resign_rx.recv() => {
                    match resign {
                        Some(responder) => {
                            self.handle_resign(responder).await;
                            return;
                        }
                        // Consumer dropped the watch without resigning.
                        None => break,
                    }
                }
            }
        }
        // Teardown (consumer gone / cache watch closed / fatal): best-effort
        // release so a successor is elected promptly; the claim otherwise lapses
        // via TTL.
        let _release = self.release_if_holder().await;
    }

    /// Releases the claim on an explicit consumer resign and reports the outcome
    /// to the resigner. A `resigned` transition is recorded only when this
    /// participant was the leader. Spans the release as `cluster.leader.resign`.
    async fn handle_resign(&mut self, responder: ResignResponder) {
        let was_leader = self.am_leader;
        let result = self
            .release_if_holder()
            .instrument(tracing::info_span!(
                spans::LEADER_RESIGN,
                provider = %self.provider,
                election = %self.name
            ))
            .await;
        if was_leader {
            self.record_transition(transition::RESIGNED);
        }
        responder.respond(result);
    }

    /// Emits the leadership-transition signals: the
    /// `cluster_leader_transitions_total` metric and the
    /// `cluster.leader.transition` INFO log, labelled by the bounded
    /// [`transition`](crate::observability::transition) kind.
    fn record_transition(&self, transition: &'static str) {
        self.metrics.leader_transition(transition);
        tracing::event!(
            name: logs::LEADER_TRANSITION,
            tracing::Level::INFO,
            provider = %self.provider,
            election = %self.name,
            transition,
            "cluster leadership transition"
        );
    }

    /// The wait before the next renewal/reclaim tick.
    ///
    /// A leader renews on the exact `interval`, kept comfortably inside the TTL.
    /// A follower adds up to half an interval of random jitter so that when many
    /// participants reclaim on the same cadence (cluster startup, or all
    /// followers after a leader drops) their `put_if_absent` attempts spread
    /// across the window instead of stampeding the election key on the same tick
    /// (cf. the k8s elector's equal-jitter backoff). The `put_if_absent` is
    /// atomic regardless, so this only relieves contention.
    fn next_renewal_delay(&self, interval: Duration) -> Duration {
        if self.am_leader {
            interval
        } else {
            interval + reclaim_jitter(interval / 2)
        }
    }

    /// Renews the lease on the timer tick. Only the timer renews — watch events
    /// never write — so a renewal's own change event cannot re-trigger one.
    /// Spanned as `cluster.leader.renew`.
    async fn renew_tick(&mut self) -> bool {
        let span = tracing::info_span!(spans::LEADER_RENEW, provider = %self.provider, election = %self.name);
        async {
            if !self.am_leader {
                // Opportunistically (re)claim a vacant key in case a free event was
                // missed (e.g. after `Lagged`).
                return self.claim().await;
            }
            let entry = match self.cache.get(&self.key).await {
                Ok(Some(entry)) => entry,
                Ok(None) => return self.lose_then_reclaim().await,
                Err(err) if err.is_retryable() => return self.on_transient().await,
                Err(err) => return self.close(err).await,
            };
            if entry.value.as_slice() != self.node_id.as_bytes() {
                return self.transition_lost_then(LeaderStatus::Follower).await;
            }
            match self
                .cache
                .compare_and_swap(
                    &self.key,
                    entry.version,
                    self.node_id.as_bytes(),
                    Ttl::Of(self.config.ttl()),
                )
                .await
            {
                Ok(_) => {
                    self.missed = 0;
                    true
                }
                Err(ClusterError::CasConflict { .. }) => self.lose_then_reclaim().await,
                Err(err) if err.is_retryable() => self.on_transient().await,
                Err(err) => self.close(err).await,
            }
        }
        .instrument(span)
        .await
    }

    /// Emits the resolved initial status to the consumer, recording an
    /// `acquired` transition when the initial claim won leadership. Returns
    /// `false` if the consumer is already gone (the caller releases and stops).
    async fn emit_initial(&mut self, initial: LeaderStatus) -> bool {
        if self.sender.send_status(initial).await.is_err() {
            return false;
        }
        if matches!(initial, LeaderStatus::Leader) {
            // The initial claim won leadership outright (e.g. sole candidate).
            self.record_transition(transition::ACQUIRED);
        }
        true
    }

    /// Reconciles cached state into a status transition (the reactive path for
    /// watch events). Issues no renewal `compare_and_swap`, but may `claim` a
    /// vacant key — a `put_if_absent` write — when it reconciles an observed
    /// vacancy.
    async fn reconcile(&mut self) -> bool {
        let entry = match self.cache.get(&self.key).await {
            Ok(entry) => entry,
            // Transient read failures are retried by the renewal timer.
            Err(err) if err.is_retryable() => return true,
            Err(err) => return self.close(err).await,
        };
        match entry {
            Some(entry) if entry.value.as_slice() == self.node_id.as_bytes() => {
                self.ensure_leader().await
            }
            Some(_) => {
                if self.am_leader {
                    self.transition_lost_then(LeaderStatus::Follower).await
                } else {
                    true
                }
            }
            None => {
                if self.am_leader {
                    // Our own claim vanished (a TTL lapse observed via the watch)
                    // while we still believed we were leader. Surface the loss
                    // before reclaiming, mirroring the renewal timer's
                    // `Ok(None) => lose_then_reclaim()` path — otherwise
                    // `claim()`'s re-win would flow through `ensure_leader()`,
                    // whose `am_leader` short-circuit would silently swallow the
                    // lost-then-reacquired transition.
                    self.lose_then_reclaim().await
                } else {
                    self.claim().await
                }
            }
        }
    }

    /// Attempts to claim a vacant key, resolving to leader or follower.
    async fn claim(&mut self) -> bool {
        match self
            .cache
            .put_if_absent(PutRequest {
                key: &self.key,
                value: self.node_id.as_bytes(),
                ttl: Ttl::Of(self.config.ttl()),
            })
            .await
        {
            Ok(Some(_)) => self.ensure_leader().await,
            Ok(None) => {
                if self.am_leader {
                    self.transition_lost_then(LeaderStatus::Follower).await
                } else {
                    true
                }
            }
            Err(err) if err.is_retryable() => true,
            Err(err) => self.close(err).await,
        }
    }

    /// Marks this participant leader, emitting `Status(Leader)` on a transition.
    async fn ensure_leader(&mut self) -> bool {
        self.missed = 0;
        if self.am_leader {
            return true;
        }
        self.am_leader = true;
        self.record_transition(transition::ACQUIRED);
        self.sender.send_status(LeaderStatus::Leader).await.is_ok()
    }

    /// Emits the transient `Status(Lost)` then the resolved `next` status.
    async fn transition_lost_then(&mut self, next: LeaderStatus) -> bool {
        self.am_leader = matches!(next, LeaderStatus::Leader);
        self.missed = 0;
        self.record_transition(transition::LOST);
        if self.sender.send_status(LeaderStatus::Lost).await.is_err() {
            return false;
        }
        self.sender.send_status(next).await.is_ok()
    }

    /// Emits `Status(Lost)` then auto-reenrolls, resolving to leader or follower.
    async fn lose_then_reclaim(&mut self) -> bool {
        self.am_leader = false;
        self.missed = 0;
        self.record_transition(transition::LOST);
        if self.sender.send_status(LeaderStatus::Lost).await.is_err() {
            return false;
        }
        match self
            .cache
            .put_if_absent(PutRequest {
                key: &self.key,
                value: self.node_id.as_bytes(),
                ttl: Ttl::Of(self.config.ttl()),
            })
            .await
        {
            Ok(Some(_)) => {
                self.am_leader = true;
                self.record_transition(transition::ACQUIRED);
                self.sender.send_status(LeaderStatus::Leader).await.is_ok()
            }
            Ok(None) => self
                .sender
                .send_status(LeaderStatus::Follower)
                .await
                .is_ok(),
            // A transient failure to reclaim resolves to follower for now; the
            // renewal timer retries the claim on the next tick.
            Err(err) if err.is_retryable() => self
                .sender
                .send_status(LeaderStatus::Follower)
                .await
                .is_ok(),
            Err(err) => self.close(err).await,
        }
    }

    /// Records a missed renewal; once the budget is exceeded, treats the claim
    /// as lost and auto-reenrolls.
    async fn on_transient(&mut self) -> bool {
        if !self.am_leader {
            return true;
        }
        self.missed = self.missed.saturating_add(1);
        if self.missed > self.config.max_missed_renewals() {
            self.lose_then_reclaim().await
        } else {
            true
        }
    }

    async fn on_watch_event(&mut self, event: CacheWatchEvent) -> bool {
        match event {
            CacheWatchEvent::Event(_) => self.reconcile().await,
            CacheWatchEvent::Lagged { dropped } => {
                if self
                    .sender
                    .send(LeaderWatchEvent::Lagged { dropped })
                    .await
                    .is_err()
                {
                    return false;
                }
                self.reconcile().await
            }
            CacheWatchEvent::Reset => {
                if self.sender.send(LeaderWatchEvent::Reset).await.is_err() {
                    return false;
                }
                self.reconcile().await
            }
            CacheWatchEvent::Closed(err) => {
                let _closed = self.sender.send(LeaderWatchEvent::Closed(err)).await;
                false
            }
            _ => true,
        }
    }

    /// Emits a terminal `Closed(err)` and signals the loop to stop. A genuine
    /// backend error also raises the shared provider-error signals.
    async fn close(&mut self, err: ClusterError) -> bool {
        observability::emit_provider_error(
            &*self.metrics,
            self.provider,
            "leader",
            observability::ResourceId::Election(&self.name),
            &err,
        );
        let _closed = self.sender.send(LeaderWatchEvent::Closed(err)).await;
        false
    }

    /// Releases this participant's claim atomically: deletes the election key
    /// only if it still carries *our* node id, so a foreign holder is never
    /// released.
    ///
    /// The delete is **value-guarded** (`compare_and_delete` on `node_id`): if
    /// this node's TTL lapses and a successor claims the key between teardown and
    /// the delete, the key now holds the successor's id, so the delete is a
    /// no-op — no spurious leadership flap. This mirrors the k8s elector's
    /// `holderIdentity`-guarded release, and being a single atomic op it also
    /// avoids the read-to-delete race a `get`-then-`delete` would carry. A
    /// version guard would not suffice, since a re-created key resets its version.
    async fn release_if_holder(&self) -> Result<(), ClusterError> {
        self.cache
            .compare_and_delete(&self.key, self.node_id.as_bytes())
            .await
            .map(|_| ())
    }
}

/// A uniform jitter in `0..max` drawn from the thread RNG (zero when `max` is
/// zero). Spreads follower reclaim attempts so simultaneous contenders
/// desynchronize. Uses the same `rand` source as the watch auto-restart backoff
/// jitter — `ThreadRng` is entropy-seeded, so concurrent participants diverge
/// without any explicit seeding.
fn reclaim_jitter(max: Duration) -> Duration {
    let max_nanos = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(rand::rng().random_range(0..max_nanos))
}

/// Awaits the next cache watch event, or pends forever once the watch has
/// ended (so the `select!` arm becomes inert rather than busy-looping).
async fn recv_optional(watch: &mut Option<CacheWatch>) -> Option<CacheWatchEvent> {
    match watch.as_mut() {
        Some(watch) => watch.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
#[path = "leader_tests.rs"]
mod leader_tests;
