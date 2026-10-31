//! The watcher: which logged changes it applies, and when it empties the
//! cache instead (ADR-0020).
//!
//! The watch is driven by hand; the log is written straight into the
//! cluster. The cache holds `hold_u` (alias `u` holding upstream `u`) and
//! `hold_v` (alias `v` holding `v`), both in tenant `t`'s chain.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::{CacheEvent, CacheWatchEvent, ClusterCacheV1, ClusterError, RetryPolicy};
use futures_util::FutureExt;
use futures_util::future;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit::{Healthcheck, HealthcheckStatus};
use uuid::Uuid;

use super::{READ_TIMEOUT, WatchHealth, WatchState, WatchStatus, spawn};
use crate::domain::model::Upstream;
use crate::domain::ports::NoopMetrics;
use crate::infra::proxy_cache::change_log::{ChangeLog, LOG_KEY, MAX_CHANGES};
use crate::infra::proxy_cache::testing::{
    BACKEND_LOG_KEY, CacheMetrics, ClusterFixture, ScriptedCache, connection_lost, facade, put_log,
    resolver, until, upstream,
};
use crate::infra::proxy_cache::{AliasKey, Change, ChangeFeed, ProxyConfigCache, WriteHook};

struct Fixture {
    cluster: ClusterFixture,
    scripted: Arc<ScriptedCache>,
    log_store: ClusterCacheV1,
    cache: Arc<ProxyConfigCache>,
    metrics: Arc<CacheMetrics>,
    status: WatchStatus,
    /// This replica, and another.
    me: Uuid,
    other: Uuid,
    u: Upstream,
    v: Upstream,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Fixture {
    /// A watcher started over `log` already in the cluster (none if
    /// `None`), once it has read its baseline; the cache is then seeded.
    async fn start(log: Option<&ChangeLog>) -> Self {
        Self::start_with(log, |cache| cache).await
    }

    async fn start_with(
        log: Option<&ChangeLog>,
        script: impl FnOnce(ScriptedCache) -> ScriptedCache,
    ) -> Self {
        let hub = Arc::new(ClientHub::new());
        let (cluster, scripted) =
            ClusterFixture::scripted(&hub, |cache| script(cache.with_manual_watch()));
        let log_store = facade(&hub).await;
        if let Some(log) = log {
            put_log(&log_store, &log.encode()).await;
        }
        let metrics = Arc::new(CacheMetrics::default());
        let cache = Arc::new(ProxyConfigCache::new(
            std::time::Duration::from_secs(60),
            100,
            metrics.clone(),
        ));
        let (me, cancel) = (Uuid::new_v4(), CancellationToken::new());
        let status = WatchStatus::default();
        let task = spawn(
            log_store.clone(),
            Arc::clone(&cache),
            me,
            status.clone(),
            cancel.clone(),
        );
        let t = Uuid::new_v4();
        let f = Self {
            cluster,
            scripted,
            log_store,
            cache,
            metrics,
            status,
            me,
            other: Uuid::new_v4(),
            u: upstream(t, "u"),
            v: upstream(t, "v"),
            cancel,
            task,
        };
        f.settle(1).await;
        f.seed().await;
        f
    }

    /// Wait until the watcher has read the log `reads` times in all, and
    /// acted on the read: the read and what follows run in one poll.
    async fn settle(&self, reads: usize) {
        until("the watcher read the log", || self.scripted.gets() >= reads).await;
    }

    /// (Re-)cache `hold_u` and `hold_v`.
    async fn seed(&self) {
        for upstream in [&self.u, &self.v] {
            let value = vec![upstream.clone()];
            self.cache
                .aliases
                .get_or_load(self.key(upstream), move |_| {
                    future::ready(Ok(value)).boxed()
                })
                .await
                .unwrap();
        }
        assert_eq!(self.cached(), names(&["hold_u", "hold_v"]));
    }

    fn key(&self, upstream: &Upstream) -> AliasKey {
        AliasKey {
            alias: upstream.alias.clone(),
            chain: vec![upstream.tenant_id],
        }
    }

    /// The seeded entries still cached.
    fn cached(&self) -> BTreeSet<&'static str> {
        self.cache.aliases.assert_consistent();
        [("hold_u", &self.u), ("hold_v", &self.v)]
            .into_iter()
            .filter(|(_, upstream)| self.cache.aliases.contains(&self.key(upstream)))
            .map(|(name, _)| name)
            .collect()
    }

    /// The change an update of `upstream` makes.
    fn updated(upstream: &Upstream) -> Change {
        Change::Upstream {
            tenant_id: upstream.tenant_id,
            id: upstream.id,
            alias: upstream.alias.clone(),
        }
    }

    /// Store `log`, deliver its change event, and wait until the watcher has
    /// acted on it.
    async fn deliver(&self, log: &ChangeLog) {
        put_log(&self.log_store, &log.encode()).await;
        self.deliver_raw().await;
    }

    /// Deliver a change event for whatever the log holds, and wait.
    async fn deliver_raw(&self) {
        let reads = self.scripted.gets();
        self.scripted.send_changed().await;
        self.settle(reads + 1).await;
    }

    async fn stop(self) {
        self.cancel.cancel();
        self.task.await.expect("no panic");
        self.cluster.stop().await;
    }
}

fn names(names: &[&'static str]) -> BTreeSet<&'static str> {
    names.iter().copied().collect()
}

/// A change in a tenant no entry depends on.
fn unrelated() -> Change {
    Change::UpstreamDeleted {
        tenant_id: Uuid::new_v4(),
        id: Uuid::new_v4(),
    }
}

/// Another replica's changes since the last one applied are applied, in
/// one read or several; this replica's own are skipped, and a change is
/// not applied twice.
///
/// ADR-0020: CACHE-LOG-001.
#[tokio::test]
async fn other_replicas_changes_are_applied_and_own_skipped() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    log.append(f.me, [Fixture::updated(&f.u)]);
    log.append(f.other, [Fixture::updated(&f.v)]);
    f.deliver(&log).await;
    assert_eq!(f.cached(), names(&["hold_u"]), "own skipped, other applied");

    // The same log again, then one more change: nothing is applied twice.
    f.seed().await;
    f.deliver_raw().await;
    log.append(f.other, [unrelated()]);
    f.deliver(&log).await;
    assert_eq!(f.cached(), names(&["hold_u", "hold_v"]));

    log.append(f.other, [Fixture::updated(&f.u), Fixture::updated(&f.v)]);
    f.deliver(&log).await;
    assert!(f.cached().is_empty());
    f.stop().await;
}

/// Changes dropped from the log before they were read: the cache is
/// emptied, and the watcher follows the log from its last change.
///
/// ADR-0020: CACHE-LOG-002.
#[tokio::test]
async fn a_gap_empties_the_cache_and_rebases() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    log.append(f.other, (0..140).map(|_| unrelated()));
    f.deliver(&log).await;
    assert!(f.cached().is_empty(), "a gap");

    f.seed().await;
    log.append(f.other, [Fixture::updated(&f.v)]);
    f.deliver(&log).await;
    assert_eq!(f.cached(), names(&["hold_u"]), "rebased");
    f.stop().await;
}

/// A log with another ID: the cache is emptied, and the watcher follows
/// the new log.
///
/// ADR-0020: CACHE-LOG-002.
#[tokio::test]
async fn a_new_log_empties_the_cache_and_rebases() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    let mut new = ChangeLog::new();
    new.append(f.other, [unrelated(), unrelated()]);
    f.deliver(&new).await;
    assert!(f.cached().is_empty(), "a new log");

    f.seed().await;
    new.append(f.other, [Fixture::updated(&f.v)]);
    f.deliver(&new).await;
    assert_eq!(f.cached(), names(&["hold_u"]), "rebased");
    f.stop().await;
}

/// A value the watcher cannot read (garbage, or a newer version) empties
/// the cache, and the next readable log is not trusted either: it empties
/// the cache once more and becomes the baseline.
///
/// ADR-0020: CACHE-LOG-002.
#[tokio::test]
async fn an_unreadable_log_empties_the_cache_until_one_is_read() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    let mut newer: serde_json::Value = serde_json::from_slice(&log.encode()).unwrap();
    newer["v"] = 2.into();
    for value in [b"garbage".to_vec(), serde_json::to_vec(&newer).unwrap()] {
        f.seed().await;
        put_log(&f.log_store, &value).await;
        f.deliver_raw().await;
        assert!(f.cached().is_empty(), "unreadable");
    }

    f.seed().await;
    log.append(f.other, [unrelated()]);
    f.deliver(&log).await;
    assert!(f.cached().is_empty(), "no baseline");

    f.seed().await;
    log.append(f.other, [Fixture::updated(&f.v)]);
    f.deliver(&log).await;
    assert_eq!(f.cached(), names(&["hold_u"]), "rebased");
    f.stop().await;
}

/// A lagged or re-established watch empties the cache and re-reads the
/// log; a deleted or expired log empties it.
///
/// ADR-0020: CACHE-FRESH-008.
#[tokio::test]
async fn lagged_reset_deleted_and_expired_empty_the_cache() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    for event in [
        CacheWatchEvent::Lagged { dropped: 3 },
        CacheWatchEvent::Reset,
    ] {
        f.seed().await;
        // Changed while the events were lost: the re-read is the baseline.
        log.append(f.other, [Fixture::updated(&f.u)]);
        put_log(&f.log_store, &log.encode()).await;
        let reads = f.scripted.gets();
        f.scripted.send(event.clone()).await;
        f.settle(reads + 1).await;
        assert!(f.cached().is_empty(), "{event:?}");

        f.seed().await;
        log.append(f.other, [Fixture::updated(&f.v)]);
        f.deliver(&log).await;
        assert_eq!(f.cached(), names(&["hold_u"]), "{event:?}: re-read");
    }

    let key = BACKEND_LOG_KEY.to_owned();
    for event in [
        CacheEvent::Deleted { key: key.clone() },
        CacheEvent::Expired { key },
    ] {
        f.seed().await;
        f.scripted.send(CacheWatchEvent::Event(event.clone())).await;
        until("the cache is emptied", || f.cached().is_empty()).await;
    }
    f.stop().await;
}

/// A watch that closes for good empties the cache, reports the watch down
/// (Degraded) and ends the task.
///
/// ADR-0020: CACHE-FRESH-008.
#[tokio::test]
async fn a_closed_watch_empties_the_cache_and_ends() {
    let f = Fixture::start(None).await;
    assert_eq!(f.status.get(), WatchState::Up);
    f.scripted
        .send(CacheWatchEvent::Closed(ClusterError::Shutdown))
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), f.task)
        .await
        .expect("the task ends")
        .expect("no panic");
    assert_eq!(f.cache.aliases.len(), 0, "emptied");
    assert_eq!(f.status.get(), WatchState::Down);
    let health = WatchHealth(f.status.clone()).check().await;
    assert_eq!(health.status, HealthcheckStatus::Degraded);
    assert_eq!(health.code.as_deref(), Some("proxy_cache_watch_down"));
    assert_eq!(f.metrics.count("flushes watch_closed"), 1);
    f.cluster.stop().await;
}

/// A first watch that fails is retried in the task, with backoff.
#[tokio::test(start_paused = true)]
async fn a_failing_first_watch_is_retried() {
    let f = Fixture::start_with(None, |cache| {
        cache.fail_watches(3);
        cache
    })
    .await;
    assert_eq!(f.scripted.watch_calls(), 4);

    let mut log = ChangeLog::new();
    log.append(f.other, [Fixture::updated(&f.v)]);
    f.deliver(&log).await;
    // No baseline yet (no log at the start): a new log empties the cache.
    assert!(f.cached().is_empty());
    f.stop().await;
}

/// The healthcheck of the watch `status` reports now: its status and code.
async fn health(status: &WatchStatus) -> (HealthcheckStatus, Option<String>) {
    let result = WatchHealth(status.clone()).check().await;
    (result.status, result.code)
}

/// A watch that breaks (a retryable close, or the backend dropping it) is
/// reported connecting (Degraded) while the watcher reconnects with
/// backoff; after the reconnect the cache is emptied and health recovers.
///
/// ADR-0020: CACHE-FAIL-003.
#[tokio::test(start_paused = true)]
async fn a_broken_watch_is_degraded_until_it_reconnects_and_empties_the_cache() {
    let f = Fixture::start(None).await;
    let connecting = (
        HealthcheckStatus::Degraded,
        Some("proxy_cache_watch_connecting".to_owned()),
    );
    assert_eq!(health(&f.status).await, (HealthcheckStatus::Healthy, None));

    f.scripted.fail_watches(2);
    f.scripted
        .send(CacheWatchEvent::Closed(connection_lost()))
        .await;
    until("the watch is reported connecting", || {
        f.status.get() == WatchState::Connecting
    })
    .await;
    assert_eq!(f.scripted.watch_calls(), 1, "the backoff comes first");
    assert_eq!(health(&f.status).await, connecting);
    until("the failing re-watches are retried", || {
        f.scripted.watch_calls() == 3
    })
    .await;
    assert_eq!(health(&f.status).await, connecting, "still reconnecting");
    // The re-watch succeeds, but its log read is held: the cache is not
    // emptied yet, so the watch is not up yet either.
    f.scripted.hold();
    until("the re-watch succeeds", || f.scripted.watch_calls() == 4).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert_eq!(
        health(&f.status).await,
        connecting,
        "not up before the cache is emptied"
    );
    assert_eq!(f.cached(), names(&["hold_u", "hold_v"]));
    f.scripted.release();
    until("the watch is up again", || f.status.get() == WatchState::Up).await;
    assert_eq!(f.scripted.watch_calls(), 4);
    assert!(f.cached().is_empty(), "emptied after the reconnect");
    assert_eq!(health(&f.status).await, (HealthcheckStatus::Healthy, None));

    // A watch the backend drops reconnects the same way.
    f.seed().await;
    f.scripted.end_watch();
    until("the dropped watch is replaced", || {
        f.scripted.watch_calls() == 5
    })
    .await;
    until("the watch is up again", || f.status.get() == WatchState::Up).await;
    assert!(f.cached().is_empty(), "emptied after the reconnect");

    // The new watch is followed.
    f.seed().await;
    let mut log = ChangeLog::new();
    log.append(f.other, [unrelated()]);
    f.deliver(&log).await;
    f.seed().await;
    log.append(f.other, [Fixture::updated(&f.v)]);
    f.deliver(&log).await;
    assert_eq!(f.cached(), names(&["hold_u"]));
    assert!(!f.task.is_finished());
    f.stop().await;
}

/// Each point where the watcher empties the cache counts its cause.
#[tokio::test]
async fn each_flush_counts_its_cause() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;
    let count = |cause: &str| f.metrics.count(&format!("flushes {cause}"));
    assert_eq!(count("watch_reset"), 1, "subscribed");

    for event in [
        CacheWatchEvent::Lagged { dropped: 1 },
        CacheWatchEvent::Reset,
    ] {
        let reads = f.scripted.gets();
        f.scripted.send(event).await;
        f.settle(reads + 1).await;
    }
    assert_eq!(count("watch_reset"), 3, "lagged, reset");

    log.append(f.other, (0..140).map(|_| unrelated()));
    f.deliver(&log).await;
    assert_eq!(count("log_gap"), 1);

    let mut new = ChangeLog::new();
    new.append(f.other, [unrelated()]);
    f.deliver(&new).await;
    assert_eq!(count("log_reset"), 1, "a new log");
    f.log_store.delete(LOG_KEY).await.unwrap();
    f.deliver_raw().await;
    assert_eq!(count("log_reset"), 2, "the log is gone");
    let key = BACKEND_LOG_KEY.to_owned();
    for (n, event) in [
        CacheEvent::Deleted { key: key.clone() },
        CacheEvent::Expired { key },
    ]
    .into_iter()
    .enumerate()
    {
        f.scripted.send(CacheWatchEvent::Event(event)).await;
        until("the flush is counted", || {
            count("log_reset") == 3 + n as u64
        })
        .await;
    }

    put_log(&f.log_store, b"garbage").await;
    f.deliver_raw().await;
    assert_eq!(count("log_unreadable"), 1);

    assert_eq!(count("watch_closed"), 0);
    f.scripted
        .send(CacheWatchEvent::Closed(ClusterError::Shutdown))
        .await;
    until("the close is counted", || count("watch_closed") == 1).await;
    let flushes: u64 = f
        .metrics
        .counts()
        .iter()
        .filter(|(name, _)| name.starts_with("flushes "))
        .map(|(_, n)| n)
        .sum();
    assert_eq!(flushes, 3 + 1 + 4 + 1 + 1, "nothing else counted");
    f.task.await.expect("no panic");
    f.cluster.stop().await;
}

/// Another replica's changes count as remote, one per change applied; own
/// changes and changes already applied do not count.
#[tokio::test]
async fn applied_changes_count_as_remote() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    log.append(f.me, [unrelated()]);
    log.append(f.other, [unrelated(), unrelated()]);
    f.deliver(&log).await;
    f.deliver_raw().await;
    assert_eq!(f.metrics.count("changes remote"), 2);
    assert_eq!(f.metrics.count("changes local"), 0);
    f.stop().await;
}

// ── Gap boundary ────────────────────────────────────────────────────────

/// The changes after `first`, as many as make `total` changes in all.
fn starting_with(first: Change, total: usize) -> impl Iterator<Item = Change> {
    std::iter::once(first).chain((1..total).map(|_| unrelated()))
}

/// Exactly as many changes since the last one applied as the log keeps:
/// none was dropped, so all are applied, the oldest included, and the
/// cache is not emptied.
///
/// ADR-0020: CACHE-LOG-002.
#[tokio::test]
async fn a_full_log_of_new_changes_is_applied_without_a_gap() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    log.append(f.other, starting_with(Fixture::updated(&f.u), MAX_CHANGES));
    assert_eq!(log.oldest(), Some(2), "the first change since the baseline");
    f.deliver(&log).await;
    assert_eq!(
        f.cached(),
        names(&["hold_v"]),
        "applied, the oldest included"
    );
    assert_eq!(f.metrics.count("flushes log_gap"), 0);
    assert_eq!(f.metrics.count("changes remote"), MAX_CHANGES as u64);
    f.stop().await;
}

/// One change more than the log keeps: the first change since the last one
/// applied was dropped, so the cache is emptied and nothing is applied.
///
/// ADR-0020: CACHE-LOG-002.
#[tokio::test]
async fn one_change_past_a_full_log_is_a_gap() {
    let mut log = ChangeLog::new();
    log.append(Uuid::new_v4(), [unrelated()]);
    let f = Fixture::start(Some(&log)).await;

    log.append(
        f.other,
        starting_with(Fixture::updated(&f.u), MAX_CHANGES + 1),
    );
    assert_eq!(
        log.oldest(),
        Some(3),
        "the first change since the baseline is gone"
    );
    f.deliver(&log).await;
    assert!(f.cached().is_empty(), "a gap");
    assert_eq!(f.metrics.count("flushes log_gap"), 1);
    assert_eq!(f.metrics.count("changes remote"), 0);
    f.stop().await;
}

// ── Hung reads, flapping watches, stop ──────────────────────────────────

/// A log read that does not answer within [`READ_TIMEOUT`] breaks the
/// watch: the cache is emptied, the watch is reported connecting, and once
/// reads answer it reconnects, resyncs and is up again.
///
/// ADR-0020: CACHE-FAIL-003.
#[tokio::test(start_paused = true)]
async fn a_hung_log_read_breaks_the_watch_and_it_reconnects() {
    let f = Fixture::start(None).await;
    let flushes = |cause: &str| f.metrics.count(&format!("flushes {cause}"));
    assert_eq!(f.scripted.watch_calls(), 1);

    f.scripted.hold();
    let sent = Instant::now();
    f.scripted.send_changed().await;
    until("the hung read breaks the watch", || {
        f.status.get() == WatchState::Connecting
    })
    .await;
    assert!(sent.elapsed() >= READ_TIMEOUT, "{:?}", sent.elapsed());
    assert!(
        f.cached().is_empty(),
        "emptied: changes may have been missed"
    );
    assert_eq!(flushes("log_unreadable"), 1);
    assert_eq!(
        health(&f.status).await,
        (
            HealthcheckStatus::Degraded,
            Some("proxy_cache_watch_connecting".to_owned())
        )
    );

    f.seed().await;
    f.scripted.release();
    until("the watch is up again", || f.status.get() == WatchState::Up).await;
    assert_eq!(f.scripted.watch_calls(), 2, "reconnected");
    assert!(f.cached().is_empty(), "emptied by the resync");
    assert!(!f.task.is_finished());
    f.stop().await;
}

/// A resync whose log read hangs (here after the watch fell behind) still
/// empties the cache, then breaks the watch; the reconnect resyncs again.
///
/// ADR-0020: CACHE-FAIL-003.
#[tokio::test(start_paused = true)]
async fn a_hung_resync_read_empties_the_cache_and_breaks_the_watch() {
    let f = Fixture::start(None).await;
    let flushes = |cause: &str| f.metrics.count(&format!("flushes {cause}"));
    assert_eq!(flushes("watch_reset"), 1, "subscribed");

    f.scripted.hold();
    f.scripted
        .send(CacheWatchEvent::Lagged { dropped: 1 })
        .await;
    until("the hung read breaks the watch", || {
        f.status.get() == WatchState::Connecting
    })
    .await;
    assert!(f.cached().is_empty(), "emptied: changes were missed");
    assert_eq!(flushes("watch_reset"), 2);

    f.seed().await;
    f.scripted.release();
    until("the watch is up again", || f.status.get() == WatchState::Up).await;
    assert_eq!(f.scripted.watch_calls(), 2, "reconnected");
    assert!(f.cached().is_empty(), "emptied by the resync");
    assert_eq!(flushes("watch_reset"), 3);
    f.stop().await;
}

/// `cancel` ends the task while a log read hangs, well before the read
/// would time out.
#[tokio::test(start_paused = true)]
async fn cancel_ends_the_task_during_a_hung_log_read() {
    let f = Fixture::start(None).await;
    f.scripted.hold();
    f.scripted.send_changed().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(f.status.get(), WatchState::Up, "the read is in flight");

    f.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), f.task)
        .await
        .expect("cancel ends the read")
        .expect("no panic");
    f.cluster.stop().await;
}

/// A watch that keeps breaking right after it comes up reconnects after a
/// growing backoff, not the first one each time (each reconnect empties
/// the cache). A watch that stayed up for the longest backoff reconnects
/// after the first one again.
///
/// ADR-0020: CACHE-FAIL-003.
#[tokio::test(start_paused = true)]
async fn a_flapping_watch_backs_off_across_reconnects() {
    let f = Fixture::start(None).await;
    let policy = RetryPolicy::default();
    let reconnect = |calls: usize| {
        let started = Instant::now();
        let f = &f;
        async move {
            f.scripted.end_watch();
            until("the watch is up again", || {
                f.scripted.watch_calls() == calls && f.status.get() == WatchState::Up
            })
            .await;
            started.elapsed()
        }
    };
    let within = |waited: Duration, backoff: Duration| {
        waited >= backoff.mul_f64(0.8) && waited <= backoff.mul_f64(1.2) + Duration::from_millis(50)
    };

    let mut backoff = policy.initial_backoff;
    for calls in 2..=5 {
        let waited = reconnect(calls).await;
        assert!(
            within(waited, backoff),
            "reconnect {calls}: {waited:?}, not {backoff:?}"
        );
        backoff *= 2;
    }

    tokio::time::sleep(policy.max_backoff).await;
    let waited = reconnect(6).await;
    assert!(
        within(waited, policy.initial_backoff),
        "after a stable watch: {waited:?}"
    );
    f.stop().await;
}

/// `stop` with its deadline already passed returns at once although the
/// watcher's log read is held: the watcher is cut off, drops its watch and
/// the held read, and the watch is reported down. That the publisher is
/// aborted mid-append is pinned by the write hook's
/// `stop_at_its_deadline_aborts_a_publisher_still_appending`.
#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn stop_at_its_deadline_aborts_the_tasks_and_reports_the_watch_down() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, scripted) = ClusterFixture::scripted(&hub, ScriptedCache::with_manual_watch);
    let cache = Arc::new(ProxyConfigCache::new(
        Duration::from_secs(60),
        100,
        Arc::new(NoopMetrics),
    ));
    let hook = WriteHook::new(Arc::clone(&cache));
    let status = WatchStatus::default();
    let feed = ChangeFeed::start(
        resolver(&hub),
        &hook,
        Uuid::new_v4(),
        &status,
        CancellationToken::new(),
    )
    .await
    .expect("the oagw profile");
    until("the watch is up", || status.get() == WatchState::Up).await;
    scripted.hold();
    scripted.send_changed().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let reads = scripted.gets();

    let deadline = CancellationToken::new();
    deadline.cancel();
    tokio::time::timeout(Duration::from_secs(1), feed.stop(deadline))
        .await
        .expect("stop returns at its deadline");
    assert!(logs_contain("cut off at the shutdown deadline"));
    assert_eq!(status.get(), WatchState::Down);
    until("the watcher has dropped its watch", || {
        scripted.watch_closed()
    })
    .await;
    scripted.release();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(scripted.gets(), reads, "the held read was dropped");
    cluster.stop().await;
}
