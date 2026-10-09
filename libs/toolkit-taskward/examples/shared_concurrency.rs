#![allow(clippy::unwrap_used, clippy::use_debug)]

//! Shared capacity: cap how many workers run at once, and keep the important
//! ones running when it is scarce.
//!
//! A gear runs several workers against one database that tolerates two
//! concurrent background queries. Every pass first takes a permit from the
//! worker's [`ConcurrencyLimit`] and gives it back when the pass ends.
//!
//! - `Fixed(semaphore)` - one pool. Workers sharing it never exceed its size.
//! - `Tiered { guaranteed, shared }` - try the shared pool first, fall back to
//!   a reserved pool of its own. A high-priority worker keeps running while
//!   low-priority workers saturate the shared pool, and borrows shared
//!   capacity when there is some.
//!
//! Part 1 runs four compaction workers on `Fixed(2)`. Part 2 adds a
//! high-priority sequencer and splits the same two slots two ways: all
//! shared, then one shared plus one reserved for the sequencer.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `shared_concurrency`

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{
    BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit, Directive, PacingConfig, TaskSet,
    WorkerAction, WorkerBuilder,
};

/// Always has more work: each pass is a 40ms query, then `Proceed`.
struct BusyQuery {
    passes: Arc<AtomicU32>,
    running: Arc<AtomicU32>,
    peak: Arc<AtomicU32>,
}

impl WorkerAction for BusyQuery {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        let now_running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now_running, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        self.running.fetch_sub(1, Ordering::SeqCst);
        self.passes.fetch_add(1, Ordering::SeqCst);
        Ok(Directive::proceed())
    }
}

#[derive(Default, Clone)]
struct Counters {
    running: Arc<AtomicU32>,
    peak: Arc<AtomicU32>,
}

impl Counters {
    fn worker(&self, passes: &Arc<AtomicU32>) -> BusyQuery {
        BusyQuery {
            passes: Arc::clone(passes),
            running: Arc::clone(&self.running),
            peak: Arc::clone(&self.peak),
        }
    }
}

fn spawn(
    tasks: &mut TaskSet,
    cancel: &CancellationToken,
    name: &str,
    limit: ConcurrencyLimit,
    action: BusyQuery,
) {
    let bulkhead = Bulkhead::new(
        name,
        BulkheadConfig {
            semaphore: limit,
            backoff: BackoffConfig::default(),
        },
    );
    // One wakeup to start; from then on each pass returns `Proceed`.
    let start = Arc::new(Notify::new());
    start.notify_one();
    let worker = WorkerBuilder::new(name, cancel.clone())
        .notifier(start)
        .pacing(PacingConfig {
            min_interval: Duration::from_millis(1),
            active_interval: Duration::from_millis(1),
            ramp_step: Duration::ZERO,
        })
        .bulkhead(bulkhead)
        .build(action);
    tasks.spawn(name, worker.run());
}

async fn run_for(tasks: TaskSet, d: Duration) {
    tokio::time::sleep(d).await;
    tasks.shutdown().await;
}

#[tokio::main]
async fn main() {
    println!("-- part 1: four compaction workers, Fixed(2) --");
    let cancel = CancellationToken::new();
    let mut tasks = TaskSet::new(cancel.clone());
    let db = Arc::new(Semaphore::new(2));
    let counters = Counters::default();
    let passes = Arc::new(AtomicU32::new(0));
    for i in 1..=4 {
        spawn(
            &mut tasks,
            &cancel,
            &format!("compaction-{i}"),
            ConcurrencyLimit::Fixed(Arc::clone(&db)),
            counters.worker(&passes),
        );
    }
    run_for(tasks, Duration::from_millis(500)).await;
    println!(
        "{} passes, at most {} queries at once",
        passes.load(Ordering::SeqCst),
        counters.peak.load(Ordering::SeqCst)
    );

    for reserved in [false, true] {
        let label = if reserved {
            "1 shared + 1 reserved: sequencer Tiered, compaction Fixed(shared)"
        } else {
            "2 shared: everyone Fixed(shared)"
        };
        println!("\n-- part 2: three compaction workers and a sequencer, {label} --");
        let cancel = CancellationToken::new();
        let mut tasks = TaskSet::new(cancel.clone());
        let shared = Arc::new(Semaphore::new(if reserved { 1 } else { 2 }));
        let counters = Counters::default();
        let compaction_passes = Arc::new(AtomicU32::new(0));
        let sequencer_passes = Arc::new(AtomicU32::new(0));

        for i in 1..=3 {
            spawn(
                &mut tasks,
                &cancel,
                &format!("compaction-{i}"),
                ConcurrencyLimit::Fixed(Arc::clone(&shared)),
                counters.worker(&compaction_passes),
            );
        }
        let sequencer_limit = if reserved {
            ConcurrencyLimit::Tiered {
                guaranteed: Arc::new(Semaphore::new(1)),
                shared: Arc::clone(&shared),
            }
        } else {
            ConcurrencyLimit::Fixed(Arc::clone(&shared))
        };
        spawn(
            &mut tasks,
            &cancel,
            "sequencer",
            sequencer_limit,
            counters.worker(&sequencer_passes),
        );

        run_for(tasks, Duration::from_millis(500)).await;
        println!(
            "sequencer {} passes, compaction {} passes, at most {} queries at once",
            sequencer_passes.load(Ordering::SeqCst),
            compaction_passes.load(Ordering::SeqCst),
            counters.peak.load(Ordering::SeqCst),
        );
    }
}
