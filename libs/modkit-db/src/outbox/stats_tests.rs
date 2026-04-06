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
    let listener: &dyn WorkerListener<u64> = &StatsListener::new(counting_extractor(|v: &u64| *v));

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
