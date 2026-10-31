//! The watcher: applies other replicas' changes from the cluster change log
//! (ADR-0020).
//!
//! One task per replica. It knows which log it follows and the last change
//! it applied. On each change event it reads the log and applies the changes
//! after that one, skipping its own (the write already applied them). When it
//! cannot know what it missed (another log, a gap, a log it cannot read, a
//! lagged or re-established watch), it empties the cache and starts over from
//! the log as it is now.
//!
//! It reconnects a broken watch itself rather than through the SDK's
//! `auto_restart`, which hides the break until the reconnect succeeds: the
//! healthcheck reports the watch as connecting meanwhile ([`WatchStatus`]).
//! A log read that hangs past [`READ_TIMEOUT`] breaks the watch too.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{
    CacheEvent, CacheWatch, CacheWatchEvent, ClusterCacheV1, ClusterError, RetryPolicy,
};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit::{Healthcheck, HealthcheckResult};
use tracing::{info, warn};
use uuid::Uuid;

use super::change_log::{ChangeLog, LOG_KEY, Unreadable};
use super::{Change, ProxyConfigCache};
use crate::domain::ports::metric_labels::{cache_change_source, cache_flush};
use crate::domain::services::registry_state::jitter;

/// How long a read of the log may take before the watch counts as broken;
/// no other replica's change is applied meanwhile.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the log could not be read.
#[derive(Debug, thiserror::Error)]
enum ReadError {
    /// The cluster failed the read.
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    /// The value is no log this build reads.
    #[error(transparent)]
    Unreadable(#[from] Unreadable),
    /// The read did not answer within [`READ_TIMEOUT`].
    #[error("the read did not answer within {READ_TIMEOUT:?}")]
    TimedOut,
    /// `cancel` fired during the read.
    #[error("the watcher is stopping")]
    Cancelled,
}

impl ReadError {
    /// How the failed read ends following the watch, if it does: a hung
    /// read breaks it, `cancel` ends it.
    fn ends(&self) -> Option<Ended> {
        match self {
            Self::TimedOut => Some(Ended::Broken),
            Self::Cancelled => Some(Ended::Cancelled),
            Self::Cluster(_) | Self::Unreadable(_) => None,
        }
    }
}

/// Why the watcher empties the cache: its `cause` label in
/// `oagw_proxy_cache_flushes_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlushCause {
    /// The watch started, was re-established, or fell behind.
    WatchReset,
    /// Changes were dropped from the log before they were read.
    LogGap,
    /// Another log, or the log was deleted, expired or gone.
    LogReset,
    /// The log could not be read.
    LogUnreadable,
    /// The watch closed for good.
    WatchClosed,
}

impl FlushCause {
    /// The metric label.
    fn label(self) -> &'static str {
        match self {
            Self::WatchReset => cache_flush::WATCH_RESET,
            Self::LogGap => cache_flush::LOG_GAP,
            Self::LogReset => cache_flush::LOG_RESET,
            Self::LogUnreadable => cache_flush::LOG_UNREADABLE,
            Self::WatchClosed => cache_flush::WATCH_CLOSED,
        }
    }
}

/// Where the change log watch stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchState {
    /// Not watching yet, or reconnecting a broken watch: other replicas'
    /// changes are not heard.
    Connecting,
    /// Watching.
    Up,
    /// Closed for good, or the feed stopped: other replicas' changes show
    /// only when entries expire.
    Down,
}

/// What the change feed reports to the healthcheck: the [`WatchState`] the
/// watcher sets, and the version of a newer log the publisher's appends
/// skip. It outlives each `start`.
#[derive(Clone, Default)]
pub(crate) struct WatchStatus(Arc<parking_lot::Mutex<FeedReport>>);

/// The content of a [`WatchStatus`].
#[derive(Debug, Clone, Copy)]
struct FeedReport {
    watch: WatchState,
    newer_log: Option<u64>,
}

impl Default for FeedReport {
    fn default() -> Self {
        Self {
            watch: WatchState::Connecting,
            newer_log: None,
        }
    }
}

impl WatchStatus {
    /// Where the watch stands now.
    pub(crate) fn get(&self) -> WatchState {
        self.0.lock().watch
    }

    /// Record where the watch stands.
    pub(crate) fn set(&self, state: WatchState) {
        self.0.lock().watch = state;
    }

    /// The version of the newer log the last append skipped, until an
    /// append lands.
    pub(crate) fn newer_log(&self) -> Option<u64> {
        self.0.lock().newer_log
    }

    /// Record that appends skip a log of `version` (newer than this
    /// build's), or, with `None`, that an append landed.
    pub(crate) fn set_newer_log(&self, version: Option<u64>) {
        self.0.lock().newer_log = version;
    }
}

/// The change log watch in the readiness report: Degraded while the watch
/// is not up or appends skip a newer log; never Unhealthy, since the cache
/// stays correct within the TTL.
pub(crate) struct WatchHealth(
    /// The watch state reported.
    pub(crate) WatchStatus,
);

#[async_trait]
impl Healthcheck for WatchHealth {
    fn name(&self) -> &'static str {
        "oagw-proxy-cache-watch"
    }

    async fn check(&self) -> HealthcheckResult {
        match self.0.get() {
            WatchState::Up => match self.0.newer_log() {
                None => HealthcheckResult::healthy(),
                Some(version) => HealthcheckResult::degraded(format!(
                    "the proxy configuration change log has version {version}, newer than this \
                     build's, and this replica's changes are not appended to it; other replicas \
                     see them when their cache entries expire. If no replica runs a build that \
                     writes that version, delete the cluster key oagw/proxy-config-changes \
                     (ADR-0020 Rolling upgrades)"
                ))
                .with_code("proxy_cache_newer_log"),
            },
            WatchState::Connecting => HealthcheckResult::degraded(
                "the proxy configuration change log watch is connecting; \
                 other replicas' changes show when cache entries expire",
            )
            .with_code("proxy_cache_watch_connecting"),
            WatchState::Down => HealthcheckResult::degraded(
                "the proxy configuration change log watch is down; \
                 other replicas' changes show when cache entries expire",
            )
            .with_code("proxy_cache_watch_down"),
        }
    }
}

/// Start the task applying the changes other processes than `origin` log in
/// `cluster` to `cache`, reporting the watch to `status`. It ends when
/// `cancel` fires or the watch closes for good.
pub(crate) fn spawn(
    cluster: ClusterCacheV1,
    cache: Arc<ProxyConfigCache>,
    origin: Uuid,
    status: WatchStatus,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    let watcher = Watcher {
        cluster,
        cache,
        origin,
        status,
        baseline: None,
    };
    tokio::spawn(watcher.run(cancel))
}

/// The task's state.
struct Watcher {
    cluster: ClusterCacheV1,
    cache: Arc<ProxyConfigCache>,
    origin: Uuid,
    status: WatchStatus,
    /// The log followed and the sequence number of the last change applied;
    /// `None` when unknown, and the next log read starts over.
    baseline: Option<(Uuid, u64)>,
}

/// How following one watch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// `cancel` fired.
    Cancelled,
    /// The watch broke in a way a new one may fix.
    Broken,
    /// The watch closed for good.
    Closed,
}

/// The wait before the next watch attempt: the cluster's reconnect backoff,
/// doubling up to its maximum. It carries over from one watch to the next,
/// so a watch that keeps breaking does not empty the cache every second.
struct Backoff {
    /// The first and the longest wait.
    policy: RetryPolicy,
    /// The next wait, before jitter.
    next: Duration,
}

impl Backoff {
    fn new() -> Self {
        let policy = RetryPolicy::default();
        Self {
            next: policy.initial_backoff,
            policy,
        }
    }

    /// Wait the next backoff, jittered, then double it; `false` when
    /// `cancel` fired first.
    async fn wait(&mut self, cancel: &CancellationToken) -> bool {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return false,
            () = tokio::time::sleep(jitter(self.next)) => {}
        }
        self.next = (self.next * 2).min(self.policy.max_backoff);
        true
    }

    /// A watch was up for `up`: if that is at least the longest backoff,
    /// the next wait is the first one again.
    fn after_watch_up_for(&mut self, up: Duration) {
        if up >= self.policy.max_backoff {
            self.next = self.policy.initial_backoff;
        }
    }
}

impl Watcher {
    async fn run(mut self, cancel: CancellationToken) {
        let mut backoff = Backoff::new();
        let mut broken = false;
        loop {
            self.status.set(WatchState::Connecting);
            let Some(mut watch) = self.subscribe(&cancel, broken, &mut backoff).await else {
                return;
            };
            let detail = if broken {
                "the change log watch was re-established"
            } else {
                "the change log watch started"
            };
            let ended = match self.resync(detail, &cancel).await {
                Some(ended) => ended,
                None => {
                    self.status.set(WatchState::Up);
                    let up = Instant::now();
                    let ended = self.follow(&mut watch, &cancel).await;
                    backoff.after_watch_up_for(up.elapsed());
                    ended
                }
            };
            match ended {
                Ended::Cancelled => return,
                Ended::Broken => broken = true,
                Ended::Closed => {
                    self.status.set(WatchState::Down);
                    return;
                }
            }
        }
    }

    /// Act on `watch`'s events until it breaks, closes or `cancel` fires.
    async fn follow(&mut self, watch: &mut CacheWatch, cancel: &CancellationToken) -> Ended {
        loop {
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ended::Cancelled,
                event = watch.recv() => event,
            };
            let ended = match event {
                Some(CacheWatchEvent::Event(CacheEvent::Changed { .. })) => {
                    self.changed(cancel).await
                }
                Some(CacheWatchEvent::Event(CacheEvent::Deleted { .. })) => {
                    self.flush(FlushCause::LogReset, "the change log was deleted");
                    self.baseline = None;
                    None
                }
                Some(CacheWatchEvent::Event(CacheEvent::Expired { .. })) => {
                    self.flush(FlushCause::LogReset, "the change log expired");
                    self.baseline = None;
                    None
                }
                Some(CacheWatchEvent::Lagged { .. }) => {
                    self.resync("the change log watch fell behind", cancel)
                        .await
                }
                Some(CacheWatchEvent::Reset) => {
                    self.resync("the change log watch was re-established", cancel)
                        .await
                }
                Some(CacheWatchEvent::Closed(e)) if e.is_retryable() => {
                    warn!(
                        error = %e,
                        "The proxy configuration change log watch broke; reconnecting"
                    );
                    Some(Ended::Broken)
                }
                Some(CacheWatchEvent::Closed(e)) => {
                    warn!(
                        error = %e,
                        "The proxy configuration change log watch closed; \
                         other replicas' changes show when cache entries expire"
                    );
                    self.flush(FlushCause::WatchClosed, "the change log watch closed");
                    Some(Ended::Closed)
                }
                // The backend dropped the watch, as a remote one does on a
                // transient fault.
                None => {
                    warn!("The proxy configuration change log watch ended; reconnecting");
                    Some(Ended::Broken)
                }
                // An event this build does not know: what it means for the log
                // is unknown.
                Some(_) => {
                    self.resync("an unknown change log watch event", cancel)
                        .await
                }
            };
            if let Some(ended) = ended {
                return ended;
            }
        }
    }

    /// Watch the log, retrying after `backoff` until it works or `cancel`
    /// fires. After a `broken` watch the first attempt waits too, so a
    /// watch that keeps breaking does not spin.
    async fn subscribe(
        &self,
        cancel: &CancellationToken,
        broken: bool,
        backoff: &mut Backoff,
    ) -> Option<CacheWatch> {
        let mut wait = broken;
        loop {
            if wait && !backoff.wait(cancel).await {
                return None;
            }
            wait = true;
            let watched = tokio::select! {
                biased;
                () = cancel.cancelled() => return None,
                watched = self.cluster.watch(LOG_KEY) => watched,
            };
            match watched {
                Ok(watch) => return Some(watch),
                Err(e) => {
                    warn!(error = %e, "Could not watch the proxy configuration change log; retrying")
                }
            }
        }
    }

    /// Follow the log as it is now, and empty the cache, because of what
    /// `detail` says. The log is read first: every change up to the
    /// baseline was written before the cache was emptied, so no load after
    /// that misses it. `Some` when the read ended the watch: the cache is
    /// emptied all the same, with no baseline.
    async fn resync(&mut self, detail: &str, cancel: &CancellationToken) -> Option<Ended> {
        let (baseline, ended) = match self.read(cancel).await {
            Ok(log) => (log.map(|log| (log.id(), log.seq())), None),
            Err(e) => {
                if e.ends() != Some(Ended::Cancelled) {
                    warn!(error = %e, "Could not read the proxy configuration change log");
                }
                (None, e.ends())
            }
        };
        self.baseline = baseline;
        self.flush(FlushCause::WatchReset, detail);
        ended
    }

    /// Apply the changes logged since the last one applied. `Some` when the
    /// read ended the watch.
    async fn changed(&mut self, cancel: &CancellationToken) -> Option<Ended> {
        let log = match self.read(cancel).await {
            Ok(Some(log)) => log,
            Ok(None) => {
                self.flush(FlushCause::LogReset, "the change log is gone");
                self.baseline = None;
                return None;
            }
            Err(ReadError::Cancelled) => return Some(Ended::Cancelled),
            Err(e) => {
                warn!(error = %e, "Could not read the proxy configuration change log");
                self.flush(
                    FlushCause::LogUnreadable,
                    "the change log could not be read",
                );
                self.baseline = None;
                return e.ends();
            }
        };
        let last = match self.baseline {
            Some((id, seq)) if id == log.id() => seq,
            _ => {
                self.flush(FlushCause::LogReset, "a new change log");
                self.rebase(&log);
                return None;
            }
        };
        if log.seq() <= last {
            return None;
        }
        if log.oldest().is_none_or(|oldest| oldest > last + 1) {
            self.flush(
                FlushCause::LogGap,
                "changes dropped from the change log were missed",
            );
            self.rebase(&log);
            return None;
        }
        for logged in log.changes_after(last) {
            if logged.origin != self.origin {
                self.cache.apply(&logged.change);
                self.cache
                    .metrics()
                    .record_proxy_cache_change(cache_change_source::REMOTE);
            }
        }
        self.rebase(&log);
        None
    }

    /// The log in the cluster, if there is one.
    async fn read(&self, cancel: &CancellationToken) -> Result<Option<ChangeLog>, ReadError> {
        let read = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ReadError::Cancelled),
            read = tokio::time::timeout(READ_TIMEOUT, self.cluster.get(LOG_KEY)) => read,
        };
        let Some(entry) = read.map_err(|_| ReadError::TimedOut)?? else {
            return Ok(None);
        };
        Ok(Some(ChangeLog::decode(&entry.value)?))
    }

    /// Follow `log` from its last change on.
    fn rebase(&mut self, log: &ChangeLog) {
        self.baseline = Some((log.id(), log.seq()));
    }

    /// Empty the cache: changes may have been missed, for `cause`, as
    /// `detail` says.
    fn flush(&self, cause: FlushCause, detail: &str) {
        info!(
            cause = cause.label(),
            detail, "Emptying the proxy configuration cache"
        );
        self.cache.apply(&Change::All);
        self.cache.metrics().record_proxy_cache_flush(cause.label());
    }
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod tests;
