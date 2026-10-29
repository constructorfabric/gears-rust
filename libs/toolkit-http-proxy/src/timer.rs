//! Shared coarse timers.
//!
//! tokio's default timer driver keeps every timer in one wheel behind one
//! lock: each per-request timeout takes that lock to register and again to
//! deregister, and under load every connection contends on it. [`Timers`]
//! groups timeouts instead:
//!
//! - deadlines are rounded **up** to a [`TICK`], so a timeout never fires
//!   early and at most one tick late;
//! - all waiters for the same tick on the same shard share one bucket (a
//!   fired flag plus a [`Notify`]); shards are picked per thread, so
//!   registering a timeout takes a short, practically uncontended lock;
//! - one driver task per client sleeps on tokio's clock until the earliest
//!   pending tick and fires every due bucket, so tokio's timer lock is taken
//!   once per tick rather than once per timeout. Because it runs on tokio's
//!   clock, `tokio::time::pause` keeps working in tests.
//!
//! The driver starts with the first timer and stops when the last handle
//! (the client and every response body still holding one) is gone. It runs
//! on the runtime that registered that timer; if that runtime shuts down
//! (dropping the driver), the next timer starts a new driver on the current
//! runtime, which picks up every pending bucket.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use parking_lot::Mutex;
use pin_project_lite::pin_project;
use tokio::sync::Notify;
use tokio::sync::futures::OwnedNotified;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Timer resolution.
pub const TICK: Duration = Duration::from_millis(10);

const SHARDS: usize = 16;

#[derive(Default)]
struct Bucket {
    fired: AtomicBool,
    notify: Arc<Notify>,
}

struct Shared {
    epoch: Instant,
    shards: [Mutex<BTreeMap<u64, Arc<Bucket>>>; SHARDS],
    /// The tick the driver sleeps until (`u64::MAX` when idle).
    armed: AtomicU64,
    /// Wakes the driver when an earlier tick is registered.
    rearm: Arc<Notify>,
    driver_started: Arc<AtomicBool>,
    stop: CancellationToken,
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Handle to a client's timer service (cheap to clone).
#[derive(Clone)]
pub struct Timers {
    shared: Arc<Shared>,
}

fn shard_index() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static SHARD: Cell<usize> = Cell::new(NEXT.fetch_add(1, Ordering::Relaxed) % SHARDS);
    }
    SHARD.with(Cell::get)
}

impl Default for Timers {
    fn default() -> Self {
        Self::new()
    }
}

impl Timers {
    /// A timer service. Its driver task is spawned with the first timer, so
    /// this works outside a runtime too.
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                epoch: Instant::now(),
                shards: std::array::from_fn(|_| Mutex::new(BTreeMap::new())),
                armed: AtomicU64::new(u64::MAX),
                rearm: Arc::new(Notify::new()),
                driver_started: Arc::new(AtomicBool::new(false)),
                stop: CancellationToken::new(),
            }),
        }
    }

    /// The tick at or after `deadline`.
    fn tick_at(&self, deadline: Instant) -> u64 {
        let since = deadline.saturating_duration_since(self.shared.epoch);
        let tick = TICK.as_nanos();
        u64::try_from(since.as_nanos().div_ceil(tick)).unwrap_or(u64::MAX - 1)
    }

    /// Make sure a driver runs; spawns one on the current runtime if the
    /// previous one is gone (never started, or its runtime shut down).
    fn ensure_driver(&self) {
        if self.shared.driver_started.load(Ordering::Acquire)
            || self.shared.driver_started.swap(true, Ordering::AcqRel)
        {
            return;
        }
        tokio::spawn(drive(
            Arc::downgrade(&self.shared),
            Arc::clone(&self.shared.rearm),
            self.shared.stop.clone(),
            Running {
                started: Arc::clone(&self.shared.driver_started),
                shared: Arc::downgrade(&self.shared),
            },
        ));
    }

    /// A timer that fires at `deadline` (rounded up to the next tick).
    pub fn expiry(&self, deadline: Instant) -> Expiry {
        self.ensure_driver();
        let tick = self.tick_at(deadline);
        let bucket = Arc::clone(
            self.shared.shards[shard_index()]
                .lock()
                .entry(tick)
                .or_default(),
        );
        if tick < self.shared.armed.load(Ordering::Acquire) {
            self.shared.rearm.notify_one();
        }
        Expiry {
            bucket,
            notified: None,
            timers: self.clone(),
        }
    }

    /// `fut`, failing with [`Elapsed`] if it does not finish within `limit`.
    /// A `limit` too large to represent as a deadline never elapses.
    pub fn timeout<F: Future>(&self, limit: Duration, fut: F) -> Timeout<F> {
        Timeout {
            fut,
            deadline: Instant::now().checked_add(limit),
            timers: self.clone(),
            expiry: None,
        }
    }
}

impl Shared {
    fn earliest(&self) -> Option<u64> {
        self.shards
            .iter()
            .filter_map(|shard| shard.lock().keys().next().copied())
            .min()
    }

    fn fire_through(&self, tick: u64) {
        for shard in &self.shards {
            let due = {
                let mut map = shard.lock();
                let later = map.split_off(&(tick + 1));
                std::mem::replace(&mut *map, later)
            };
            for bucket in due.into_values() {
                bucket.fired.store(true, Ordering::Release);
                bucket.notify.notify_waiters();
            }
        }
    }
}

/// Marks the driver as running; dropped with the driver task (also when its
/// runtime shuts down before it ever ran). Then every pending timer is woken
/// so its owner, on whatever runtime it lives, starts a new driver.
struct Running {
    started: Arc<AtomicBool>,
    shared: Weak<Shared>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.started.store(false, Ordering::Release);
        if let Some(shared) = self.shared.upgrade() {
            for shard in &shared.shards {
                for bucket in shard.lock().values() {
                    bucket.notify.notify_waiters();
                }
            }
        }
    }
}

/// The driver holds the shared state only weakly between ticks, so the
/// last handle going away ends it (through `stop`).
async fn drive(
    shared: Weak<Shared>,
    rearm: Arc<Notify>,
    stop: CancellationToken,
    _running: Running,
) {
    loop {
        let Some(s) = shared.upgrade() else {
            return;
        };
        let next = s.earliest();
        s.armed.store(next.unwrap_or(u64::MAX), Ordering::Release);
        // A registration between `earliest` and publishing `armed` saw the
        // old value and may not have notified: look again.
        if s.earliest() != next {
            continue;
        }
        let rearmed = rearm.notified();
        // A tick too far out to represent as an `Instant` never comes.
        let wake_at = next.and_then(|tick| {
            let nanos = u64::try_from(TICK.as_nanos()).unwrap_or(u64::MAX);
            s.epoch
                .checked_add(Duration::from_nanos(tick.saturating_mul(nanos)))
        });
        let sleep = async {
            match wake_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        drop(s);
        tokio::select! {
            () = stop.cancelled() => return,
            () = rearmed => {}
            () = sleep => {
                if let (Some(s), Some(tick)) = (shared.upgrade(), next) {
                    s.fire_through(tick);
                }
            }
        }
    }
}

/// A pending coarse timer.
pub struct Expiry {
    bucket: Arc<Bucket>,
    notified: Option<Pin<Box<OwnedNotified>>>,
    timers: Timers,
}

impl Expiry {
    /// Ready once the deadline passed; until then `cx` is woken when it does.
    pub fn poll_elapsed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.bucket.fired.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        let notified = self
            .notified
            .get_or_insert_with(|| Box::pin(Arc::clone(&self.bucket.notify).notified_owned()));
        // Register before the second look, so a firing in between is seen
        // either way.
        notified.as_mut().enable();
        if self.bucket.fired.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        if notified.as_mut().poll(cx).is_ready() {
            if self.bucket.fired.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            // Woken because the driver went away: wait on a fresh
            // registration with a driver running on this runtime.
            self.notified = None;
            self.timers.ensure_driver();
            return self.poll_elapsed(cx);
        }
        // A driver must be running for this timer to fire.
        self.timers.ensure_driver();
        Poll::Pending
    }
}

/// The future passed to [`Timers::timeout`] did not finish in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

pin_project! {
    /// See [`Timers::timeout`].
    pub struct Timeout<F> {
        #[pin]
        fut: F,
        // `None`: the limit is beyond what an `Instant` can hold.
        deadline: Option<Instant>,
        timers: Timers,
        expiry: Option<Expiry>,
    }
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, Elapsed>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if let Poll::Ready(out) = this.fut.poll(cx) {
            return Poll::Ready(Ok(out));
        }
        let Some(deadline) = *this.deadline else {
            return Poll::Pending;
        };
        let expiry = this
            .expiry
            .get_or_insert_with(|| this.timers.expiry(deadline));
        ready!(expiry.poll_elapsed(cx));
        Poll::Ready(Err(Elapsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn never_fires_early_and_at_most_a_tick_late() {
        let timers = Timers::new();
        let started = Instant::now();
        let mut e = timers.expiry(started + Duration::from_millis(25));
        std::future::poll_fn(|cx| e.poll_elapsed(cx)).await;
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited <= Duration::from_millis(25) + TICK, "{waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_for_one_tick_share_a_bucket() {
        let timers = Timers::new();
        let at = Instant::now() + Duration::from_millis(50);
        let (a, b) = (timers.expiry(at), timers.expiry(at));
        assert!(Arc::ptr_eq(&a.bucket, &b.bucket));
        for mut e in [a, b] {
            std::future::poll_fn(|cx| e.poll_elapsed(cx)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_earlier_deadline_rearms_the_driver() {
        let timers = Timers::new();
        let started = Instant::now();
        let _late = timers.expiry(started + Duration::from_secs(60));
        tokio::task::yield_now().await;
        let mut early = timers.expiry(started + Duration::from_millis(30));
        std::future::poll_fn(|cx| early.poll_elapsed(cx)).await;
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_passes_ready_futures_and_fails_slow_ones() {
        let timers = Timers::new();
        assert_eq!(
            timers.timeout(Duration::from_millis(10), async { 7 }).await,
            Ok(7)
        );
        let slow = timers.timeout(
            Duration::from_millis(100),
            tokio::time::sleep(Duration::from_secs(1)),
        );
        assert_eq!(slow.await, Err(Elapsed));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unrepresentable_timeout_never_elapses() {
        let timers = Timers::new();
        assert_eq!(timers.timeout(Duration::MAX, async { 7 }).await, Ok(7));
        let slow = timers.timeout(Duration::MAX, tokio::time::sleep(Duration::from_secs(3600)));
        assert_eq!(slow.await, Ok(()), "only the inner future ends it");
    }

    #[tokio::test(start_paused = true)]
    async fn the_driver_stops_with_the_last_handle() {
        let timers = Timers::new();
        drop(timers.expiry(Instant::now() + Duration::from_millis(20)));
        let stop = timers.shared.stop.clone();
        let other = timers.clone();
        drop(timers);
        assert!(!stop.is_cancelled(), "a handle is still alive");
        drop(other);
        assert!(stop.is_cancelled());
    }

    #[test]
    fn a_new_runtime_restarts_the_driver() {
        let timers = Timers::new();
        let fire = |timers: &Timers| {
            let timers = timers.clone();
            async move {
                let mut e = timers.expiry(Instant::now() + Duration::from_millis(10));
                tokio::time::timeout(
                    Duration::from_secs(2),
                    std::future::poll_fn(|cx| e.poll_elapsed(cx)),
                )
                .await
                .expect("the timer fires");
            }
        };
        let first = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        first.block_on(fire(&timers));
        // Shutting the runtime down drops its driver.
        drop(first);
        let second = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        second.block_on(fire(&timers));
    }

    #[test]
    fn pending_timers_survive_their_drivers_runtime() {
        let timers = Timers::new();
        // The driver starts on runtime A.
        let a = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .unwrap();
        a.block_on(async {
            drop(timers.expiry(Instant::now() + Duration::from_secs(3600)));
            tokio::task::yield_now().await;
        });
        // A timer is pending on runtime B when A shuts down.
        let b = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        b.block_on(async move {
            let mut e = timers.expiry(Instant::now() + Duration::from_millis(300));
            let shutdown = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                drop(a);
            });
            tokio::time::timeout(
                Duration::from_secs(3),
                std::future::poll_fn(|cx| e.poll_elapsed(cx)),
            )
            .await
            .expect("fires after the driver's runtime went away");
            shutdown.join().unwrap();
        });
    }
}
