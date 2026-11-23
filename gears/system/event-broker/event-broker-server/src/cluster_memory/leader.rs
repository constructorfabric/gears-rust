//! Cache-backed election adapted from the cluster defaults' `leader.rs`.

use std::convert::Infallible;
use std::fmt;
use std::future::{Future, pending, poll_fn};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cluster_sdk::cache::{CacheConsistency, CacheWatch, CacheWatchEvent, ClusterCacheBackend};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{
    ElectionConfig, LeaderElectionBackend, LeaderElectionFeatures, LeaderStatus, LeaderWatch,
    LeaderWatchEvent, LeaderWatchSender, ResignReceiver, ResignResponder,
};
use cluster_sdk::lease::LeaseToken;
use cluster_sdk::observability::{self, ClusterMetrics, ResourceId, spans, transition};
use rand::RngExt as _;
use tokio::sync::{Notify, oneshot};
use tokio::task::{JoinError, JoinSet};
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;
use toolkit::async_trait;
use toolkit_taskward::{Directive, PanicPolicy, WorkerAction, WorkerBuilder};
use tracing::Instrument as _;
use uuid::Uuid;

use super::lease::{Acquisition, CacheLeaseStore, Claim, MEMORY_PROVIDER};

const EVENT_BUFFER: usize = 16;
const MAX_WATCH_RESUBSCRIBES: usize = 3;
const WORKER_NAME: &str = "eb-memory-election";

type Activation = oneshot::Receiver<Result<(), ClusterError>>;

/// Coordinates one profile's watch and token election clients.
pub struct MemoryLeader {
    leases: Arc<CacheLeaseStore>,
    metrics: Arc<dyn ClusterMetrics>,
    abort: CancellationToken,
    shutdown: CancellationToken,
    workers: Mutex<JoinSet<()>>,
    #[cfg(test)]
    counts: Arc<super::client::WorkerCounts>,
    #[cfg(test)]
    terminal_gate: Mutex<Option<Arc<TerminalGate>>>,
}

impl fmt::Debug for MemoryLeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryLeader")
            .finish_non_exhaustive()
    }
}

impl MemoryLeader {
    pub(crate) fn new(
        cache: Arc<dyn ClusterCacheBackend>,
        metrics: Arc<dyn ClusterMetrics>,
        abort: CancellationToken,
    ) -> Result<Self, ClusterError> {
        if cache.consistency() != CacheConsistency::Linearizable {
            return Err(ClusterError::InvalidConfig {
                reason: "event-broker memory election requires a linearizable cache".to_owned(),
            });
        }
        Ok(Self {
            leases: Arc::new(CacheLeaseStore::for_elections(cache, metrics.clone())),
            metrics,
            abort,
            shutdown: CancellationToken::new(),
            workers: Mutex::new(JoinSet::new()),
            #[cfg(test)]
            counts: Arc::default(),
            #[cfg(test)]
            terminal_gate: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(super) fn with_test_clock(mut self, retention: Duration, nonce: u64) -> Self {
        Arc::get_mut(&mut self.leases)
            .expect("the test clock is set before sharing the lease store")
            .set_test_clock(retention, nonce);
        self
    }

    #[cfg(test)]
    pub(crate) fn test_worker_counts(&self) -> (usize, usize, usize) {
        self.counts.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn set_test_terminal_gate(&self, gate: Arc<TerminalGate>) {
        *self
            .terminal_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(gate);
    }

    fn workers(&self) -> MutexGuard<'_, JoinSet<()>> {
        self.workers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn key(name: &str) -> String {
        format!("election/{name}")
    }

    fn require_open(&self) -> Result<(), ClusterError> {
        if self.shutdown.is_cancelled() || self.abort.is_cancelled() {
            return Err(ClusterError::Shutdown);
        }
        Ok(())
    }

    pub(super) fn close(&self) {
        // The registry lock serializes close with registration.
        let _workers = self.workers();
        self.shutdown.cancel();
    }

    pub(super) async fn join_workers(&self) -> Result<(), JoinError> {
        let mut outcome = Ok(());
        while let Some(result) = poll_fn(|cx| self.workers().poll_join_next(cx)).await {
            #[cfg(test)]
            self.counts.joined();
            if let Err(error) = result {
                tracing::warn!(%error, "election worker failed");
                outcome = Err(error);
            }
        }
        outcome
    }

    async fn enrol(&self, name: &str, config: ElectionConfig) -> Result<LeaderWatch, ClusterError> {
        self.observe(ElectionOperation::Elect, name, async {
            let (mut watch, started) = self.register(name, config)?;
            started.await.map_err(|_| ClusterError::Shutdown)??;
            self.require_open()?;
            watch.set_observability(MEMORY_PROVIDER, self.metrics.clone());
            Ok(watch)
        })
        .await
    }

    fn register(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<(LeaderWatch, Activation), ClusterError> {
        let mut workers = self.workers();
        self.require_open()?;
        while let Some(result) = workers.try_join_next() {
            #[cfg(test)]
            self.counts.joined();
            if let Err(error) = result {
                tracing::warn!(%error, "election worker failed");
            }
        }

        let (sender, resign, watch) = LeaderWatch::channel(EVENT_BUFFER, LeaderStatus::Follower);
        let (ready, started) = oneshot::channel();
        let candidate = Candidate {
            leases: self.leases.clone(),
            name: name.to_owned(),
            key: Self::key(name),
            owner: Uuid::new_v4().to_string(),
            config,
            shutdown: self.shutdown.clone(),
            abort: self.abort.clone(),
            metrics: self.metrics.clone(),
            sender,
            resign,
            ready: Some(ready),
            held: None,
            incumbent_lapse: None,
            missed: 0,
            resubscribes: 0,
            terminal: false,
            #[cfg(test)]
            terminal_gate: self
                .terminal_gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        };
        let wake = Arc::new(Notify::new());
        wake.notify_one();
        let worker = WorkerBuilder::new(WORKER_NAME, self.abort.child_token())
            .notifier(wake)
            .on_panic(PanicPolicy::Propagate)
            .build(candidate);
        #[cfg(test)]
        let tracking = self.counts.track();
        workers.spawn(async move {
            #[cfg(test)]
            let _tracking = tracking;
            worker.run().await;
        });
        Ok((watch, started))
    }

    async fn observe<T>(
        &self,
        operation: ElectionOperation,
        name: &str,
        work: impl Future<Output = Result<T, ClusterError>>,
    ) -> Result<T, ClusterError> {
        operation.observe(&*self.metrics, name, work).await
    }
}

#[async_trait]
impl LeaderElectionBackend for MemoryLeader {
    fn features(&self) -> LeaderElectionFeatures {
        LeaderElectionFeatures::new(true)
    }

    async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
        self.enrol(name, ElectionConfig::default()).await
    }

    async fn elect_with_config(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError> {
        self.enrol(name, config).await
    }

    async fn join(
        &self,
        name: &str,
        owner: &str,
        config: ElectionConfig,
    ) -> Result<Option<LeaseToken>, ClusterError> {
        self.observe(ElectionOperation::Elect, name, async {
            self.require_open()?;
            let key = Self::key(name);
            let outcome = self.leases.try_acquire(&key, &Claim {
                name,
                owner,
                ttl: config.ttl(),
            }).await?;
            match outcome {
                Acquisition::Acquired(claim) => {
                    if let Err(error) = self.require_open() {
                        if let Err(discard) = self.leases.discard_record(&key, &claim).await {
                            tracing::warn!(%discard, "could not release an unpublished election claim");
                        }
                        return Err(error);
                    }
                    Ok(Some(claim.token))
                }
                Acquisition::Contended { .. } => {
                    self.require_open()?;
                    Ok(None)
                }
            }
        }).await
    }

    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        self.observe(ElectionOperation::Renew, &token.name, async {
            self.require_open()?;
            self.leases
                .renew_record(&Self::key(&token.name), token, ttl)
                .await?;
            Ok(())
        })
        .await
    }

    async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        self.observe(ElectionOperation::Resign, &token.name, async {
            self.require_open()?;
            self.leases
                .release_record(&Self::key(&token.name), token)
                .await
        })
        .await
    }
}

enum ElectionOperation {
    Elect,
    Renew,
    Resign,
}

impl ElectionOperation {
    async fn observe<T>(
        self,
        metrics: &dyn ClusterMetrics,
        name: &str,
        work: impl Future<Output = Result<T, ClusterError>>,
    ) -> Result<T, ClusterError> {
        let (label, span) = match self {
            Self::Elect => (
                "elect",
                tracing::info_span!(
                    spans::LEADER_ELECT,
                    provider = MEMORY_PROVIDER,
                    election = name
                ),
            ),
            Self::Renew => (
                "renew",
                tracing::info_span!(
                    spans::LEADER_RENEW,
                    provider = MEMORY_PROVIDER,
                    election = name
                ),
            ),
            Self::Resign => (
                "resign",
                tracing::info_span!(
                    spans::LEADER_RESIGN,
                    provider = MEMORY_PROVIDER,
                    election = name
                ),
            ),
        };
        let outcome = work.instrument(span).await;
        if let Err(error) = &outcome {
            observability::emit_provider_error(
                metrics,
                MEMORY_PROVIDER,
                label,
                ResourceId::Election(name),
                error,
            );
        }
        outcome
    }
}

struct HeldLease {
    token: LeaseToken,
    wall_deadline: Duration,
    confirmed: Instant,
}

struct Candidate {
    leases: Arc<CacheLeaseStore>,
    name: String,
    key: String,
    owner: String,
    config: ElectionConfig,
    shutdown: CancellationToken,
    abort: CancellationToken,
    metrics: Arc<dyn ClusterMetrics>,
    sender: LeaderWatchSender,
    resign: ResignReceiver,
    ready: Option<oneshot::Sender<Result<(), ClusterError>>>,
    held: Option<HeldLease>,
    incumbent_lapse: Option<Duration>,
    missed: u16,
    resubscribes: usize,
    terminal: bool,
    #[cfg(test)]
    terminal_gate: Option<Arc<TerminalGate>>,
}

impl Candidate {
    async fn activate(&mut self) -> Result<CacheWatch, ClusterError> {
        // Subscribe before claiming so a winner's release cannot fall between
        // the claim and the subscription.
        let feed = self.leases.cache.watch(&self.key).await?;
        self.claim().await?;
        Ok(feed)
    }

    async fn serve(&mut self) {
        if self.shutdown.is_cancelled() {
            return;
        }
        let initial = self.activate().await;
        if self.shutdown.is_cancelled() {
            return;
        }
        let Some(ready) = self.ready.take() else {
            return;
        };
        let mut feed = match initial {
            Ok(feed) => feed.into(),
            Err(error) => {
                let _sent = ready.send(Err(error));
                return;
            }
        };
        let initial_status = if self.held.is_some() {
            self.record_transition(transition::ACQUIRED);
            LeaderStatus::Leader
        } else {
            LeaderStatus::Follower
        };
        self.sender.try_send_status(initial_status);
        if ready.send(Ok(())).is_err() {
            self.release_on_exit().await;
            return;
        }

        let tick = sleep(self.delay());
        tokio::pin!(tick);
        loop {
            // A busy cache feed must leave time for renewal and shutdown.
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => return,
                () = &mut tick => {
                    self.sender.flush_lagged();
                    if !self.apply_tick().await {
                        break;
                    }
                    tick.as_mut().reset(self.next_wake());
                }
                command = self.resign.recv() => {
                    if let Some(responder) = command {
                        self.handle_resign(responder).await;
                        return;
                    }
                    break;
                }
                event = Self::receive(&mut feed) => {
                    let was_leader = self.held.is_some();
                    if !self.handle_event(event, &mut feed).await {
                        break;
                    }
                    // Ownership changes reset the wakeup. Followers can
                    // wake earlier at a newly observed lapse.
                    if was_leader != self.held.is_some() {
                        tick.as_mut().reset(self.next_wake());
                    } else if !was_leader {
                        let wake = tick.deadline().min(self.next_wake());
                        tick.as_mut().reset(wake);
                    }
                }
            }
        }
        self.release_on_exit().await;
    }

    async fn receive(feed: &mut Option<CacheWatch>) -> Option<CacheWatchEvent> {
        match feed {
            Some(feed) => feed.recv().await,
            None => pending().await,
        }
    }

    fn delay(&self) -> Duration {
        let interval = self.config.renewal_interval();
        if let Some(held) = &self.held {
            let remaining = held
                .wall_deadline
                .saturating_sub(self.leases.wall_now())
                .min(self.config.ttl().saturating_sub(held.confirmed.elapsed()));
            return interval.min(remaining);
        }
        let base = self
            .incumbent_lapse
            .map_or(interval, |lapse| interval.min(lapse));
        let half = base / 2;
        let fraction = rand::rng().random::<f64>();
        base.saturating_add(half.mul_f64(fraction))
    }

    fn next_wake(&self) -> Instant {
        // Tokio caps sleeps for TTLs beyond its timer range.
        sleep(self.delay()).deadline()
    }

    async fn claim(&mut self) -> Result<(), ClusterError> {
        match self
            .leases
            .try_acquire(
                &self.key,
                &Claim {
                    name: &self.name,
                    owner: &self.owner,
                    ttl: self.config.ttl(),
                },
            )
            .await?
        {
            Acquisition::Acquired(claim) => {
                self.held = Some(HeldLease {
                    token: claim.token,
                    wall_deadline: claim.deadline,
                    confirmed: Instant::now(),
                });
                self.incumbent_lapse = None;
            }
            Acquisition::Contended { lapse_in } => self.incumbent_lapse = lapse_in,
        }
        Ok(())
    }

    async fn apply_tick(&mut self) -> bool {
        let Some(held) = &self.held else {
            return self.reclaim(false).await;
        };
        if held.confirmed.elapsed() >= self.config.ttl()
            || self.leases.wall_now() >= held.wall_deadline
        {
            return self.lose().await;
        }
        let deadline = ElectionOperation::Renew
            .observe(
                &*self.metrics,
                &self.name,
                self.leases
                    .renew_record(&self.key, &held.token, self.config.ttl()),
            )
            .await;
        match deadline {
            Ok(wall_deadline) => {
                if let Some(held) = &mut self.held {
                    held.wall_deadline = wall_deadline;
                    held.confirmed = Instant::now();
                }
                self.missed = 0;
                true
            }
            Err(ClusterError::LockExpired { .. }) => self.lose().await,
            Err(error) if error.is_retryable() => {
                self.missed += 1;
                if self.missed > u16::from(self.config.max_missed_renewals()) {
                    self.lose().await
                } else {
                    true
                }
            }
            Err(error) => self.close_with(error),
        }
    }

    async fn reconcile(&mut self) -> bool {
        let record = match self.leases.read(&self.key).await {
            Ok(record) => record,
            Err(error) if error.is_retryable() => return true,
            Err(error) => return self.close_with(error),
        };
        let now = self.leases.wall_now();
        if let Some(record) = record.filter(|record| record.deadline() > now) {
            if let Some(held) = &self.held {
                if record.base.matches(&held.token) {
                    return true;
                }
                return self.lose().await;
            }
            self.incumbent_lapse = Some(record.deadline().saturating_sub(now));
            true
        } else if self.held.is_some() {
            self.lose().await
        } else {
            self.reclaim(false).await
        }
    }

    async fn lose(&mut self) -> bool {
        self.held = None;
        self.missed = 0;
        self.record_transition(transition::LOST);
        if !self.sender.try_send_status(LeaderStatus::Lost) {
            return false;
        }
        self.reclaim(true).await
    }

    async fn reclaim(&mut self, after_loss: bool) -> bool {
        match self.claim().await {
            Ok(()) => {}
            Err(error) if error.is_retryable() => {}
            Err(error) => return self.close_with(error),
        }
        if self.held.is_some() {
            self.record_transition(transition::ACQUIRED);
            self.sender.try_send_status(LeaderStatus::Leader)
        } else if after_loss {
            self.sender.try_send_status(LeaderStatus::Follower)
        } else {
            true
        }
    }

    async fn handle_event(
        &mut self,
        event: Option<CacheWatchEvent>,
        feed: &mut Option<CacheWatch>,
    ) -> bool {
        match event {
            Some(CacheWatchEvent::Lagged { dropped }) => {
                if !self.sender.try_send(LeaderWatchEvent::Lagged { dropped }) {
                    return false;
                }
            }
            Some(CacheWatchEvent::Reset) => {
                if !self.sender.try_send(LeaderWatchEvent::Reset) {
                    return false;
                }
            }
            Some(CacheWatchEvent::Closed(error)) if error.is_retryable() => {
                *feed = self.resubscribe().await;
                return true;
            }
            Some(CacheWatchEvent::Closed(error)) => return self.close_with(error),
            Some(_) => {}
            None => {
                *feed = None;
                return true;
            }
        }
        self.resubscribes = 0;
        self.reconcile().await
    }

    async fn resubscribe(&mut self) -> Option<CacheWatch> {
        if self.resubscribes == MAX_WATCH_RESUBSCRIBES {
            return None;
        }
        self.resubscribes += 1;
        match self.leases.cache.watch(&self.key).await {
            Ok(watch) => {
                if self.sender.try_send(LeaderWatchEvent::Reset) {
                    Some(watch)
                } else {
                    None
                }
            }
            Err(error) => {
                observability::emit_provider_error(
                    &*self.metrics,
                    MEMORY_PROVIDER,
                    "leader",
                    ResourceId::Election(&self.name),
                    &error,
                );
                None
            }
        }
    }

    fn close_with(&mut self, error: ClusterError) -> bool {
        observability::emit_provider_error(
            &*self.metrics,
            MEMORY_PROVIDER,
            "leader",
            ResourceId::Election(&self.name),
            &error,
        );
        self.sender.try_close(error);
        self.terminal = true;
        false
    }

    fn record_transition(&self, label: &'static str) {
        self.metrics.leader_transition(label);
        tracing::info!(
            event = observability::logs::LEADER_TRANSITION,
            provider = MEMORY_PROVIDER,
            election = self.name,
            transition = label,
            "leadership changed"
        );
    }

    async fn release_on_exit(&mut self) {
        if !self.shutdown.is_cancelled() {
            let _released = self.release().await;
        }
    }

    async fn handle_resign(&mut self, responder: ResignResponder) {
        let was_leader = self.held.is_some();
        let result = self.release().await;
        if was_leader && result.is_ok() {
            self.record_transition(transition::RESIGNED);
        }
        responder.respond(result);
    }

    async fn release(&mut self) -> Result<(), ClusterError> {
        if let Some(held) = &self.held {
            ElectionOperation::Resign
                .observe(
                    &*self.metrics,
                    &self.name,
                    self.leases.release_record(&self.key, &held.token),
                )
                .await?;
            self.held = None;
        }
        Ok(())
    }

    fn revoke(&mut self) {
        if !self.terminal {
            let was_leader = self.held.take().is_some();
            self.sender.revoke_for_shutdown(was_leader);
            if was_leader {
                self.record_transition(transition::LOST);
            }
            self.terminal = true;
        }
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        if self.abort.is_cancelled() || self.shutdown.is_cancelled() {
            self.revoke();
        }
    }
}

impl WorkerAction for Candidate {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        let abort = self.abort.clone();
        tokio::select! {
            biased;
            () = abort.cancelled() => {}
            () = self.serve() => {}
        }
        if self.abort.is_cancelled() || self.shutdown.is_cancelled() {
            self.revoke();
            #[cfg(test)]
            if let Some(gate) = &self.terminal_gate {
                gate.sent.notify_one();
                tokio::select! {
                    () = self.abort.cancelled() => {}
                    () = gate.resume.notified() => {}
                }
            }
        }
        cancel.cancel();
        Ok(Directive::idle())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct TerminalGate {
    pub(crate) sent: Notify,
    pub(crate) resume: Notify,
}
