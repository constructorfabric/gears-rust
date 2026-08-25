//! The `cluster_lock_released` NOTIFY channel (DESIGN.md §2.1, §5.3): wakes
//! blocked [`lock()`](cluster_sdk::DistributedLockBackend::lock) waiters
//! promptly instead of relying solely on the polling fallback in
//! `lock/mod.rs`'s acquisition loop.
//!
//! Unlike the cache's `cluster_cache_changes` channel (DESIGN.md §2.3), this
//! payload is the bare lock name — no `<event_type>:` prefix — per DESIGN.md
//! §2.1: "The Postgres NOTIFY channel `cluster_lock_released` carries the
//! lock name when a holder calls `release()` explicitly."

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use cluster_sdk::ClusterError;
use dashmap::DashMap;
use sqlx::postgres::PgListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::pg_error::map_sqlx_error;

pub const RELEASE_CHANNEL: &str = "cluster_lock_released";

/// `NOTIFY cluster_lock_released, '<name>'` for a batch of names in **one**
/// round-trip — the TTL sweep's form, and the orphan sweep's and the shutdown
/// drain's.
///
/// `pg_notify` rather than a literal `NOTIFY`, which avoids any quoting concern
/// for names containing `'`. `unnest` rather than a loop of single calls: a sweep
/// batch is up to `reaper::SWEEP_BATCH` names, and the whole point of chunking
/// the sweep was to stop per-row work from stretching how long one statement's
/// worth of table locks is held. Postgres de-duplicates identical `(channel, payload)` pairs
/// within a transaction, so a batch containing the same name twice — impossible
/// today, since `name` is the table's primary key — would still notify once.
pub async fn notify_released_many<'e, E>(executor: E, names: &[String]) -> Result<(), ClusterError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query("SELECT pg_notify($1, name) FROM unnest($2::text[]) AS t(name)")
        .bind(RELEASE_CHANNEL)
        .bind(names)
        .execute(executor)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

/// Registry of in-process waiters blocked in
/// [`lock()`](cluster_sdk::DistributedLockBackend::lock), keyed by the lock
/// name they're waiting to retry. `lock/mod.rs`'s acquisition loop also polls
/// on a short heartbeat independent of this registry, so a missed wake here
/// (a waiter registered just after `notify` already fired, or this task not
/// running yet) only costs latency, never correctness.
pub struct ReleaseWaiters {
    /// Per-name set of live waiters, each keyed by a process-unique id so a
    /// waiter can remove *its own* registration on drop (see [`ReleaseWait`])
    /// without disturbing the others. A plain `Vec<Sender>` (as an earlier
    /// version used) had no such handle: senders were only ever removed by
    /// `notify`, so `lock()`'s per-heartbeat `wait_for` call left a dead sender
    /// behind on every 250ms tick, and a name that is renewed-but-never-released
    /// (no `NOTIFY` ever fires) grew the `Vec` unbounded for the waiter's whole
    /// duration (PGR-M7).
    waiters: DashMap<String, HashMap<u64, oneshot::Sender<()>>>,
    /// Monotonic source of the per-waiter ids above.
    next_id: AtomicU64,
}

impl ReleaseWaiters {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            waiters: DashMap::new(),
            next_id: AtomicU64::new(0),
        })
    }

    /// Registers interest in `name`'s next release, returning a [`ReleaseWait`]
    /// future that resolves once [`notify`](Self::notify) is called for that
    /// name (or resolves immediately if this registry is dropped first — the
    /// caller's own heartbeat/timeout bound covers that case). Dropping the
    /// returned future without it resolving deregisters this waiter, so a
    /// caller that gives up (timeout, or re-acquired via the heartbeat) never
    /// leaves a stale sender behind.
    pub fn wait_for(self: &Arc<Self>, name: &str) -> ReleaseWait {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.waiters
            .entry(name.to_owned())
            .or_default()
            .insert(id, tx);
        ReleaseWait {
            // A `Weak`, not an `Arc`: the wait must not keep the registry alive
            // past `PostgresLock`'s own lifetime (dropping the registry closes
            // this waiter's sender, resolving the wait — see the unit tests).
            registry: Arc::downgrade(self),
            name: name.to_owned(),
            id,
            rx,
        }
    }

    /// Wakes every waiter registered under `name` (best-effort — a waiter that
    /// has already given up and dropped its receiver is silently skipped).
    pub fn notify(&self, name: &str) {
        if let Some((_, senders)) = self.waiters.remove(name) {
            for (_id, sender) in senders {
                // `Err` just means the waiter already gave up (dropped its
                // receiver) — nothing to do either way, so the outcome is
                // deliberately unused (named, not `_`, to satisfy
                // `clippy::let_underscore_must_use`).
                let _send_result = sender.send(());
            }
        }
    }

    /// Test-only: the number of live waiters currently registered under `name`.
    /// Lets an integration test synchronize on a blocked `lock()` caller having
    /// actually reached its [`wait_for`](Self::wait_for) registration before
    /// releasing, so the release provably races an already-registered NOTIFY
    /// waiter rather than an unscheduled task (PGR-E3). Gated behind
    /// `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_registered_count(&self, name: &str) -> usize {
        self.waiters.get(name).map_or(0, |entry| entry.len())
    }

    /// Deregisters waiter `id` under `name` (called from [`ReleaseWait::drop`]).
    fn deregister(&self, name: &str, id: u64) {
        let now_empty = {
            let Some(mut entry) = self.waiters.get_mut(name) else {
                return;
            };
            entry.remove(&id);
            entry.is_empty()
        };
        // Prune the now-empty per-name entry, but only if a concurrent
        // `wait_for` hasn't repopulated it in the gap after we dropped the
        // `get_mut` guard (dropping the guard first avoids a same-shard
        // re-entrant lock).
        if now_empty {
            self.waiters
                .remove_if(name, |_, waiters| waiters.is_empty());
        }
    }
}

/// A registered blocked-`lock()` waiter. Resolves when a `cluster_lock_released`
/// NOTIFY for its name wakes it; deregisters itself from [`ReleaseWaiters`] on
/// drop so an abandoned wait (timeout / heartbeat re-acquire) leaves nothing
/// behind (PGR-M7).
pub struct ReleaseWait {
    registry: Weak<ReleaseWaiters>,
    name: String,
    id: u64,
    rx: oneshot::Receiver<()>,
}

impl Future for ReleaseWait {
    /// `Ok(())` — woken by a `notify` for this name. `Err(())` — the sender was
    /// dropped (the registry went away). Both mean "stop waiting and re-attempt
    /// the acquire"; the caller re-checks `pg_try_advisory_lock` as the source
    /// of truth regardless, so the distinction only matters to the unit tests.
    type Output = Result<(), ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), ()>> {
        Pin::new(&mut self.rx)
            .poll(cx)
            .map(|result| result.map_err(|_| ()))
    }
}

impl Drop for ReleaseWait {
    fn drop(&mut self) {
        // If the registry is already gone there is nothing to deregister from.
        if let Some(registry) = self.registry.upgrade() {
            registry.deregister(&self.name, self.id);
        }
    }
}

/// Backoff policy for this task's own reconnect loop, used only when
/// [`PgListener`]'s internal (single-attempt) reconnect fails outright —
/// mirrors `cache/watch.rs::ListenRetryPolicy` exactly; kept as a separate
/// copy rather than a shared type since the two channels' dispatch payloads
/// differ enough (bare name vs. `<event>:<key>`) that unifying the two tasks
/// would add more indirection than it would save.
struct RetryPolicy {
    initial_backoff: Duration,
    max_backoff: Duration,
    max_retries: u32,
}

impl RetryPolicy {
    const DEFAULT: Self = Self {
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
        max_retries: 10,
    };

    fn backoff_for(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt);
        self.initial_backoff
            .checked_mul(factor)
            .map_or(self.max_backoff, |grown| grown.min(self.max_backoff))
    }
}

async fn connect_and_listen(connection_string: &str) -> Result<PgListener, ClusterError> {
    let mut listener = PgListener::connect(connection_string)
        .await
        .map_err(map_sqlx_error)?;
    listener
        .listen(RELEASE_CHANNEL)
        .await
        .map_err(map_sqlx_error)?;
    Ok(listener)
}

/// [`connect_and_listen`], but abandoned the instant `cancel` fires so a
/// shutdown never stalls waiting on a hung `PgListener::connect` network I/O.
/// Returns `None` if cancellation won the race — the caller must stop rather
/// than treat it as a connect failure to retry.
async fn connect_and_listen_cancellable(
    connection_string: &str,
    cancel: &CancellationToken,
) -> Option<Result<PgListener, ClusterError>> {
    tokio::select! {
        () = cancel.cancelled() => None,
        result = connect_and_listen(connection_string) => Some(result),
    }
}

/// Logs the permanent loss of `cluster_lock_released` NOTIFY wakeups once
/// [`reconnect_with_backoff`] returns `None` for a genuine reason: the retry
/// budget was exhausted, not `cancel` firing mid-backoff (a graceful
/// shutdown, which needs no log). Called at both `reconnect_with_backoff`
/// call sites in [`spawn_release_listen_task`] right before they `return`.
///
/// A missed wake-up here only costs latency, never correctness —
/// `lock/mod.rs`'s heartbeat poll is the acquisition loop's correctness
/// backstop (see this module's doc comment) — so an error log satisfies
/// "observable" without any change to that fallback behavior.
fn log_release_listen_exhausted_if_not_cancelled(cancel: &CancellationToken) {
    if !cancel.is_cancelled() {
        tracing::error!(
            name: "cluster.lock.release_listen_lost",
            "cluster_lock_released LISTEN reconnect retry budget exhausted; \
             lock() callers now rely solely on the heartbeat poll fallback"
        );
    }
}

async fn reconnect_with_backoff(
    connection_string: &str,
    policy: &RetryPolicy,
    attempt: &mut u32,
    cancel: &CancellationToken,
) -> Option<PgListener> {
    while *attempt < policy.max_retries {
        let backoff = policy.backoff_for(*attempt);
        *attempt += 1;
        tokio::select! {
            () = cancel.cancelled() => return None,
            () = tokio::time::sleep(backoff) => {}
        }
        match connect_and_listen_cancellable(connection_string, cancel).await {
            // Cancelled mid-connect: stop, don't spin the backoff loop.
            None => return None,
            Some(Ok(listener)) => return Some(listener),
            // Connect failed: fall through to the next backoff attempt.
            Some(Err(_lost)) => {}
        }
    }
    None
}

/// Opens the dedicated LISTEN connection for `cluster_lock_released` and spawns
/// its fan-out task (DESIGN.md §5.3). Mirrors `cache/watch.rs::spawn_listen_task`'s
/// reconnect/backoff shape; unlike the cache channel there is no `Reset` concept
/// here to broadcast on reconnect — a lost wake-up during a gap only costs a
/// waiter its heartbeat-interval latency (`lock/mod.rs`), never correctness, so
/// silently resuming is sufficient.
///
/// **The initial `LISTEN` is awaited before returning**, so by the time
/// `build_and_start` resolves this channel is subscribed. Spawning it
/// fire-and-forget instead left a startup window in which a `release()` — and the
/// reaper sweep's cross-instance notifications — produced a `NOTIFY` with nobody
/// yet listening: every blocked `lock()` caller in that window silently fell back
/// to the 250 ms heartbeat poll, which is exactly the degradation the channel
/// exists to avoid (and `PG-LOCK-003` measures). The cache watch listener already
/// had this guarantee (DESIGN.md §3.2 step 5); this one now matches it.
///
/// Unlike the cache listener, a failed initial connect is **not** a startup
/// error: the task falls into the same cancellation-aware backoff retry the
/// mid-stream path uses, since the heartbeat fallback keeps `lock()` correct in
/// the meantime. So this is a readiness guarantee on the happy path, not a new
/// way for `build_and_start` to fail.
///
/// Each notification does exactly one thing: wake local `lock()` waiters for that
/// name, which is a synchronous, allocation-free registry hit. The reader loop
/// therefore never awaits I/O — a hard requirement rather than a nicety, since a
/// `LISTEN` session that stops draining pins the tail of Postgres's
/// **cluster-wide** notify queue, shared by every database on the server
/// (DESIGN.md §11).
///
/// It used to do a second thing: nudge the reaper to reconcile locks whose rows
/// another instance's sweep had deleted. That existed because only the owning
/// instance could release its own advisory locks, so a sweep it did not run left
/// a name wedged fleet-wide. With the lease row as the arbiter there is nothing
/// to reconcile — whoever deletes the row frees the name for everyone.
pub(super) async fn spawn_release_listen_task(
    connection_string: String,
    waiters: Arc<ReleaseWaiters>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    // Awaited here, outside the spawned task, so the caller cannot resolve before
    // the channel is subscribed. `None` (cancelled mid-connect) and `Err` are both
    // handled inside the task, which keeps this function infallible.
    let connected = connect_and_listen_cancellable(&connection_string, &cancel).await;

    tokio::spawn(async move {
        let policy = RetryPolicy::DEFAULT;
        let mut attempt: u32 = 0;

        let mut listener = match connected {
            // Cancelled before we ever connected: shutdown, stop here.
            None => return,
            Some(Ok(listener)) => listener,
            Some(Err(_lost)) => {
                // A Postgres blip at startup must not permanently disable NOTIFY
                // wakeups (they would otherwise silently degrade to
                // `lock/mod.rs`'s heartbeat-only fallback forever), so fall into
                // the same backoff retry the mid-stream reconnect path uses.
                if let Some(reconnected) =
                    reconnect_with_backoff(&connection_string, &policy, &mut attempt, &cancel).await
                {
                    reconnected
                } else {
                    log_release_listen_exhausted_if_not_cancelled(&cancel);
                    return;
                }
            }
        };
        attempt = 0;

        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                received = listener.try_recv() => match received {
                    Ok(Some(notification)) => {
                        attempt = 0;
                        // A registry hit and nothing else — no I/O, no spawn.
                        // This loop must never stop draining (§11).
                        waiters.notify(notification.payload());
                    }
                    Ok(None) => {
                        // Transparent reconnect already happened inside
                        // `try_recv`; nothing else to do.
                        attempt = 0;
                    }
                    Err(_lost) => {
                        if let Some(reconnected) =
                            reconnect_with_backoff(&connection_string, &policy, &mut attempt, &cancel).await
                        {
                            listener = reconnected;
                        } else {
                            log_release_listen_exhausted_if_not_cancelled(&cancel);
                            return;
                        }
                    }
                },
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PGR-M7: dropping an abandoned `ReleaseWait` (the timeout/heartbeat
    /// give-up path) removes its own registration, pruning the now-empty
    /// per-name entry.
    #[tokio::test]
    async fn abandoned_wait_deregisters_itself_on_drop() {
        let waiters = ReleaseWaiters::new();
        {
            let _wait = waiters.wait_for("x");
            assert_eq!(
                waiters.waiters.get("x").map(|entry| entry.len()),
                Some(1),
                "wait_for must register exactly one waiter"
            );
        }
        assert!(
            waiters.waiters.get("x").is_none(),
            "dropping an abandoned wait must deregister it and prune the empty entry"
        );
    }

    /// PGR-M7 regression: `lock()`'s per-heartbeat `wait_for` must not leave a
    /// dead sender behind on every tick. The pre-fix `Vec<Sender>` grew by one
    /// per call for a never-notified name; the drop-guard keeps it at zero.
    #[tokio::test]
    async fn repeated_wait_for_does_not_accumulate_dead_senders() {
        let waiters = ReleaseWaiters::new();
        for _ in 0..100 {
            let _wait = waiters.wait_for("y");
        }
        assert!(
            waiters.waiters.get("y").is_none(),
            "abandoned waits must not accumulate stale senders"
        );
    }

    /// A live waiter must survive another waiter's drop under the same name.
    #[tokio::test]
    async fn dropping_one_waiter_leaves_a_sibling_registered() {
        let waiters = ReleaseWaiters::new();
        let live = waiters.wait_for("z");
        {
            let _abandoned = waiters.wait_for("z");
            assert_eq!(waiters.waiters.get("z").map(|entry| entry.len()), Some(2));
        }
        assert_eq!(
            waiters.waiters.get("z").map(|entry| entry.len()),
            Some(1),
            "dropping one waiter must not deregister its sibling"
        );
        waiters.notify("z");
        assert!(
            live.await.is_ok(),
            "the surviving waiter must still be woken"
        );
    }
}
