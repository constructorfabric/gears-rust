use std::any::Any;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::taskward::{Directive, WorkerAction, WorkerListener};

// ---------------------------------------------------------------------------
// StatsListener — per-worker atomic counters
// ---------------------------------------------------------------------------

type MsgExtractor = Box<dyn Fn(&dyn Any) -> u64 + Send + Sync>;

/// Per-worker statistics collector.
///
/// Registered as a `WorkerListener` on each worker. Maintains atomic counters
/// that are drained periodically by [`StatsReporter`].
///
/// Uses an internal `Arc` so that cloning produces a handle to the **same**
/// underlying counters. This lets callers pass a clone to
/// [`WorkerBuilder::listener`] while retaining a reference for the registry.
#[derive(Clone)]
pub struct StatsListener {
    inner: Arc<StatsListenerInner>,
}

struct StatsListenerInner {
    executions: AtomicU64,
    noop_execs: AtomicU64,
    failures: AtomicU64,
    total_exec_us: AtomicU64,
    max_exec_us: AtomicU64,
    total_idle_us: AtomicU64,
    total_msgs: AtomicU64,
    last_event: Mutex<Instant>,
    msg_extractor: MsgExtractor,
}

impl StatsListener {
    /// Create a new stats listener.
    ///
    /// `msg_extractor` converts the typed payload to a message count. The
    /// closure receives `&dyn Any` so that `StatsListener` itself doesn't need
    /// a generic parameter — the closure is created with knowledge of the
    /// concrete payload type at registration time.
    pub fn new(msg_extractor: MsgExtractor) -> Self {
        Self {
            inner: Arc::new(StatsListenerInner {
                executions: AtomicU64::new(0),
                noop_execs: AtomicU64::new(0),
                failures: AtomicU64::new(0),
                total_exec_us: AtomicU64::new(0),
                max_exec_us: AtomicU64::new(0),
                total_idle_us: AtomicU64::new(0),
                total_msgs: AtomicU64::new(0),
                last_event: Mutex::new(Instant::now()),
                msg_extractor,
            }),
        }
    }

    /// Atomically drain all counters and return a snapshot.
    pub fn snapshot_and_reset(&self) -> StatsSnapshot {
        let inner = &self.inner;
        StatsSnapshot {
            executions: inner.executions.swap(0, Ordering::Relaxed),
            noop_execs: inner.noop_execs.swap(0, Ordering::Relaxed),
            failures: inner.failures.swap(0, Ordering::Relaxed),
            total_exec_us: inner.total_exec_us.swap(0, Ordering::Relaxed),
            max_exec_us: inner.max_exec_us.swap(0, Ordering::Relaxed),
            total_idle_us: inner.total_idle_us.swap(0, Ordering::Relaxed),
            total_msgs: inner.total_msgs.swap(0, Ordering::Relaxed),
        }
    }

    fn record_idle_since_last_event(&self) {
        let now = Instant::now();
        if let Ok(mut last) = self.inner.last_event.lock() {
            let idle = now.duration_since(*last);
            #[allow(clippy::cast_possible_truncation)]
            let idle_us = idle.as_micros() as u64;
            self.inner
                .total_idle_us
                .fetch_add(idle_us, Ordering::Relaxed);
            *last = now;
        }
    }

    fn touch_last_event(&self) {
        if let Ok(mut last) = self.inner.last_event.lock() {
            *last = Instant::now();
        }
    }

    /// Update the max counter if `val` exceeds the current stored value.
    fn fetch_max(counter: &AtomicU64, val: u64) {
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            if val <= current {
                break;
            }
            match counter.compare_exchange_weak(current, val, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }
}

impl<P: Send + Sync + 'static> WorkerListener<P> for StatsListener {
    fn on_execute_start(&self) {
        self.record_idle_since_last_event();
    }

    fn on_complete(&self, duration: Duration, directive: &Directive<P>) {
        self.inner.executions.fetch_add(1, Ordering::Relaxed);
        #[allow(clippy::cast_possible_truncation)]
        let us = duration.as_micros() as u64;
        self.inner.total_exec_us.fetch_add(us, Ordering::Relaxed);
        Self::fetch_max(&self.inner.max_exec_us, us);

        let msgs = (self.inner.msg_extractor)(directive.payload() as &dyn Any);
        self.inner.total_msgs.fetch_add(msgs, Ordering::Relaxed);
        if msgs == 0 {
            self.inner.noop_execs.fetch_add(1, Ordering::Relaxed);
        }

        self.touch_last_event();
    }

    fn on_error(
        &self,
        _duration: Duration,
        _error: &str,
        _consecutive_failures: u32,
        _backoff: Duration,
    ) {
        self.inner.failures.fetch_add(1, Ordering::Relaxed);
        self.touch_last_event();
    }
}

// ---------------------------------------------------------------------------
// StatsSnapshot — immutable point-in-time counters
// ---------------------------------------------------------------------------

/// Point-in-time snapshot of a single worker's counters.
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub executions: u64,
    pub noop_execs: u64,
    pub failures: u64,
    pub total_exec_us: u64,
    pub max_exec_us: u64,
    pub total_idle_us: u64,
    pub total_msgs: u64,
}

impl StatsSnapshot {
    /// Average execution time in microseconds (0 if no executions).
    pub fn avg_exec_us(&self) -> u64 {
        if self.executions == 0 {
            0
        } else {
            #[allow(clippy::integer_division)]
            {
                self.total_exec_us / self.executions
            }
        }
    }

    /// Average messages per execution (0 if no executions).
    pub fn avg_msgs(&self) -> u64 {
        if self.executions == 0 {
            0
        } else {
            #[allow(clippy::integer_division)]
            {
                self.total_msgs / self.executions
            }
        }
    }

    /// Returns true if no activity was recorded.
    pub fn is_empty(&self) -> bool {
        self.executions == 0 && self.failures == 0
    }

    /// Merge another snapshot into this one (sum counters, max of max).
    fn merge(&mut self, other: &Self) {
        self.executions += other.executions;
        self.noop_execs += other.noop_execs;
        self.failures += other.failures;
        self.total_exec_us += other.total_exec_us;
        self.max_exec_us = self.max_exec_us.max(other.max_exec_us);
        self.total_idle_us += other.total_idle_us;
        self.total_msgs += other.total_msgs;
    }
}

/// Aggregated snapshot for a category of workers.
#[derive(Debug, Clone)]
pub struct CategorySnapshot {
    pub workers: usize,
    pub snapshot: StatsSnapshot,
}

// ---------------------------------------------------------------------------
// StatsRegistry — collection of named listeners
// ---------------------------------------------------------------------------

/// Registry of per-worker stats listeners.
///
/// Built during `OutboxBuilder::start()`, then shared via `Arc` with the
/// `StatsReporter`. Immutable after construction.
pub struct StatsRegistry {
    listeners: Vec<(String, StatsListener)>,
}

impl StatsRegistry {
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }

    /// Register a listener under a category (e.g. `"processor"`, `"sequencer"`).
    pub fn register(&mut self, category: String, listener: StatsListener) {
        self.listeners.push((category, listener));
    }

    /// Drain all listeners and return aggregated `(category, snapshot)` pairs.
    ///
    /// Workers sharing the same category are merged into a single
    /// [`CategorySnapshot`] with summed counters and a `workers` count.
    pub fn snapshot_all(&self) -> Vec<(String, CategorySnapshot)> {
        // Drain all listeners, preserving insertion order for categories.
        let mut order: Vec<String> = Vec::new();
        let mut map: std::collections::HashMap<String, CategorySnapshot> =
            std::collections::HashMap::new();

        for (category, listener) in &self.listeners {
            let snap = listener.snapshot_and_reset();
            if let Some(cat) = map.get_mut(category) {
                cat.workers += 1;
                cat.snapshot.merge(&snap);
            } else {
                order.push(category.clone());
                map.insert(
                    category.clone(),
                    CategorySnapshot {
                        workers: 1,
                        snapshot: snap,
                    },
                );
            }
        }

        order
            .into_iter()
            .filter_map(|cat| map.remove(&cat).map(|cs| (cat, cs)))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// StatsReporter — periodic drain-and-log worker
// ---------------------------------------------------------------------------

/// Background worker that periodically drains all [`StatsListener`] instances
/// and logs a summary table via `tracing::info!`.
pub struct StatsReporter {
    registry: Arc<StatsRegistry>,
    last_drain: Instant,
}

impl StatsReporter {
    pub fn new(registry: Arc<StatsRegistry>) -> Self {
        Self {
            registry,
            last_drain: Instant::now(),
        }
    }

    fn drain_and_format(&mut self) -> Option<String> {
        let period = self.last_drain.elapsed();
        self.last_drain = Instant::now();

        let categories = self.registry.snapshot_all();

        // Suppress if no activity
        if categories.iter().all(|(_, cs)| cs.snapshot.is_empty()) {
            return None;
        }

        let period_secs = period.as_secs_f64();
        let mut lines = Vec::with_capacity(categories.len() + 1);
        lines.push(format!("Outbox Stats (period: {period_secs:.1}s)"));

        for (cat, cs) in &categories {
            let s = &cs.snapshot;
            if s.is_empty() {
                continue;
            }
            lines.push(format!(
                // "msgs" = messages: rows_claimed (sequencer), msgs_delivered (processor),
                // rows_deleted (vacuum). Consistent with the outbox domain vocabulary.
                "  {cat:<18} workers={:<4} execs={:<6} noop={:<6} fails={:<4} exec={:<8} avg={:<8} max={:<8} idle={:<8} msgs={:<8} avg_batch={}",
                cs.workers,
                s.executions,
                s.noop_execs,
                s.failures,
                format_us(s.total_exec_us),
                format_us(s.avg_exec_us()),
                format_us(s.max_exec_us),
                format_us(s.total_idle_us),
                s.total_msgs,
                s.avg_msgs(),
            ));
        }

        Some(lines.join("\n"))
    }
}

impl WorkerAction for StatsReporter {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        if let Some(report) = self.drain_and_format() {
            tracing::info!("{report}");
        }
        Ok(Directive::idle())
    }
}

impl Drop for StatsReporter {
    fn drop(&mut self) {
        if let Some(report) = self.drain_and_format() {
            tracing::info!("(final) {report}");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format microseconds into a human-readable duration string.
fn format_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us}\u{b5}s")
    } else if us < 1_000_000 {
        #[allow(clippy::cast_precision_loss)]
        let ms = us as f64 / 1_000.0;
        format!("{ms:.1}ms")
    } else {
        #[allow(clippy::cast_precision_loss)]
        let s = us as f64 / 1_000_000.0;
        format!("{s:.1}s")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn noop_extractor() -> MsgExtractor {
        Box::new(|_| 0)
    }

    fn counting_extractor<T: Send + Sync + 'static>(f: fn(&T) -> u64) -> MsgExtractor {
        Box::new(move |any| any.downcast_ref::<T>().map_or(0, f))
    }

    #[test]
    fn snapshot_and_reset_drains_counters() {
        let listener = StatsListener::new(noop_extractor());

        listener.inner.executions.store(10, Ordering::Relaxed);
        listener.inner.failures.store(2, Ordering::Relaxed);
        listener.inner.total_exec_us.store(5000, Ordering::Relaxed);
        listener.inner.max_exec_us.store(800, Ordering::Relaxed);
        listener.inner.total_idle_us.store(3000, Ordering::Relaxed);
        listener.inner.total_msgs.store(100, Ordering::Relaxed);

        let snap = listener.snapshot_and_reset();
        assert_eq!(snap.executions, 10);
        assert_eq!(snap.failures, 2);
        assert_eq!(snap.total_exec_us, 5000);
        assert_eq!(snap.max_exec_us, 800);
        assert_eq!(snap.total_idle_us, 3000);
        assert_eq!(snap.total_msgs, 100);

        // Counters should be zeroed
        assert_eq!(listener.inner.executions.load(Ordering::Relaxed), 0);
        assert_eq!(listener.inner.failures.load(Ordering::Relaxed), 0);
        assert_eq!(listener.inner.total_exec_us.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn snapshot_computed_fields() {
        let snap = StatsSnapshot {
            executions: 10,
            noop_execs: 0,
            failures: 0,
            total_exec_us: 5000,
            max_exec_us: 800,
            total_idle_us: 3000,
            total_msgs: 100,
        };
        assert_eq!(snap.avg_exec_us(), 500);
        assert_eq!(snap.avg_msgs(), 10);
        assert!(!snap.is_empty());
    }

    #[test]
    fn snapshot_empty() {
        let snap = StatsSnapshot::default();
        assert!(snap.is_empty());
        assert_eq!(snap.avg_exec_us(), 0);
        assert_eq!(snap.avg_msgs(), 0);
    }

    #[test]
    fn on_complete_increments_counters() {
        let listener: &dyn WorkerListener<u64> =
            &StatsListener::new(counting_extractor(|v: &u64| *v));

        let directive = Directive::Proceed(42_u64);
        listener.on_complete(Duration::from_micros(150), &directive);

        // Downcast back to check internals
        // Can't easily — but we test via snapshot below
    }

    #[test]
    fn on_complete_updates_via_snapshot() {
        let listener = StatsListener::new(counting_extractor(|v: &u64| *v));

        let l: &dyn WorkerListener<u64> = &listener;
        l.on_complete(Duration::from_micros(150), &Directive::Proceed(42_u64));
        l.on_complete(Duration::from_micros(250), &Directive::Idle(10_u64));

        let snap = listener.snapshot_and_reset();
        assert_eq!(snap.executions, 2);
        assert_eq!(snap.total_exec_us, 400);
        assert_eq!(snap.max_exec_us, 250);
        assert_eq!(snap.total_msgs, 52);
    }

    #[test]
    fn on_error_increments_failures() {
        let listener = StatsListener::new(noop_extractor());
        let l: &dyn WorkerListener<()> = &listener;

        l.on_error(Duration::from_millis(1), "boom", 1, Duration::from_secs(1));
        l.on_error(Duration::from_millis(2), "boom2", 2, Duration::from_secs(2));

        let snap = listener.snapshot_and_reset();
        assert_eq!(snap.failures, 2);
        assert_eq!(snap.executions, 0);
    }

    #[test]
    fn fetch_max_updates_correctly() {
        let counter = AtomicU64::new(100);
        StatsListener::fetch_max(&counter, 50); // no-op
        assert_eq!(counter.load(Ordering::Relaxed), 100);

        StatsListener::fetch_max(&counter, 200); // update
        assert_eq!(counter.load(Ordering::Relaxed), 200);

        StatsListener::fetch_max(&counter, 200); // equal, no-op
        assert_eq!(counter.load(Ordering::Relaxed), 200);
    }

    #[test]
    fn registry_snapshot_all_aggregates_by_category() {
        let mut registry = StatsRegistry::new();

        let l1 = StatsListener::new(noop_extractor());
        l1.inner.executions.store(5, Ordering::Relaxed);
        l1.inner.max_exec_us.store(100, Ordering::Relaxed);
        registry.register("processor".to_owned(), l1);

        let l2 = StatsListener::new(noop_extractor());
        l2.inner.executions.store(3, Ordering::Relaxed);
        l2.inner.max_exec_us.store(200, Ordering::Relaxed);
        registry.register("processor".to_owned(), l2);

        let l3 = StatsListener::new(noop_extractor());
        l3.inner.executions.store(7, Ordering::Relaxed);
        registry.register("sequencer".to_owned(), l3);

        let categories = registry.snapshot_all();
        assert_eq!(categories.len(), 2);

        // processor: 2 workers, summed executions, max of max
        assert_eq!(categories[0].0, "processor");
        assert_eq!(categories[0].1.workers, 2);
        assert_eq!(categories[0].1.snapshot.executions, 8);
        assert_eq!(categories[0].1.snapshot.max_exec_us, 200);

        // sequencer: 1 worker
        assert_eq!(categories[1].0, "sequencer");
        assert_eq!(categories[1].1.workers, 1);
        assert_eq!(categories[1].1.snapshot.executions, 7);
    }

    #[test]
    fn reporter_suppresses_empty() {
        let registry = Arc::new(StatsRegistry::new());
        let mut reporter = StatsReporter::new(registry);
        assert!(reporter.drain_and_format().is_none());
    }

    #[test]
    fn reporter_formats_output() {
        let mut registry = StatsRegistry::new();
        let l = StatsListener::new(noop_extractor());
        l.inner.executions.store(10, Ordering::Relaxed);
        l.inner.total_exec_us.store(5000, Ordering::Relaxed);
        l.inner.max_exec_us.store(800, Ordering::Relaxed);
        l.inner.total_msgs.store(100, Ordering::Relaxed);
        registry.register("sequencer".to_owned(), l);
        let registry = Arc::new(registry);

        let mut reporter = StatsReporter::new(registry);
        let output = reporter.drain_and_format();
        assert!(output.is_some());
        let text = output.unwrap();
        assert!(text.contains("Outbox Stats"));
        assert!(text.contains("sequencer"));
        assert!(text.contains("workers=1"));
        assert!(text.contains("execs=10"));
    }

    #[test]
    fn format_us_ranges() {
        assert_eq!(format_us(500), "500\u{b5}s");
        assert_eq!(format_us(1500), "1.5ms");
        assert_eq!(format_us(1_500_000), "1.5s");
    }
}
