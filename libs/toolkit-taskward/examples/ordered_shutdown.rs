#![allow(clippy::unwrap_used, clippy::use_debug)]

//! Shutdown: stop in order, lose nothing that was accepted, and never hang.
//!
//! A gear ingests readings and a writer persists them in batches. At shutdown
//! the writer must flush whatever ingest already accepted, and a writer that
//! hangs must not block the process forever.
//!
//! - [`TaskSet`] owns named tasks under one token. `shutdown()` cancels the
//!   token, then joins every task in spawn order and logs any that panicked.
//!   Dropping a `TaskSet` without `shutdown()` still cancels its token.
//! - Ordering comes from giving each stage its own `TaskSet`: stop the
//!   producers, flush, then stop the consumers.
//! - `stop_grace` bounds a pass that is still running at cancellation. Past
//!   the grace its future is dropped and the worker exits with a warning.
//!   [`stop_deadline`] is the same deadline as a future, for code that has to
//!   race its own work against it.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `ordered_shutdown`

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, TaskSet, WorkerAction, WorkerBuilder, poker, stop_deadline};

type Buffer = Arc<Mutex<VecDeque<u32>>>;

/// Accepts one reading per tick.
struct Ingest {
    buffer: Buffer,
    accepted: Arc<AtomicU32>,
    wake_writer: Arc<Notify>,
}

impl WorkerAction for Ingest {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        let reading = self.accepted.fetch_add(1, Ordering::SeqCst);
        self.buffer.lock().unwrap().push_back(reading);
        self.wake_writer.notify_one();
        Ok(Directive::idle())
    }
}

/// Persists everything buffered, 30ms per batch.
struct Writer {
    buffer: Buffer,
    written: Arc<AtomicU32>,
    flushed: Arc<Notify>,
}

impl WorkerAction for Writer {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        let batch: Vec<u32> = self.buffer.lock().unwrap().drain(..).collect();
        if !batch.is_empty() {
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.written
                .fetch_add(u32::try_from(batch.len()).unwrap(), Ordering::SeqCst);
        }
        if self.buffer.lock().unwrap().is_empty() {
            self.flushed.notify_one();
        }
        Ok(Directive::idle())
    }
}

/// Ignores its cancellation token and never finishes.
struct Stuck;

impl WorkerAction for Stuck {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Ok(Directive::idle())
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .init();

    let buffer: Buffer = Arc::default();
    let accepted = Arc::new(AtomicU32::new(0));
    let written = Arc::new(AtomicU32::new(0));
    let wake_writer = Arc::new(Notify::new());
    let flushed = Arc::new(Notify::new());

    // Writers first: they outlive ingest.
    let writer_cancel = CancellationToken::new();
    let mut writers = TaskSet::new(writer_cancel.clone());
    let writer = WorkerBuilder::new("writer", writer_cancel.clone())
        .notifier(Arc::clone(&wake_writer))
        .stop_grace(Duration::from_millis(200))
        .build(Writer {
            buffer: Arc::clone(&buffer),
            written: Arc::clone(&written),
            flushed: Arc::clone(&flushed),
        });
    writers.spawn("writer", writer.run());

    let ingest_cancel = CancellationToken::new();
    let mut ingest = TaskSet::new(ingest_cancel.clone());
    let (tick, _poker) = poker(Duration::from_millis(5), ingest_cancel.clone());
    let worker = WorkerBuilder::new("ingest", ingest_cancel.clone())
        .notifier(tick)
        .build(Ingest {
            buffer: Arc::clone(&buffer),
            accepted: Arc::clone(&accepted),
            wake_writer: Arc::clone(&wake_writer),
        });
    ingest.spawn("ingest", worker.run());

    tokio::time::sleep(Duration::from_millis(300)).await;

    println!("1. stop ingest - nothing new is accepted from here on");
    ingest.shutdown().await;
    println!("2. flush - wait until everything accepted is written");
    // `flushed` may hold a permit from an earlier pass, so re-check the
    // condition rather than trusting a single wakeup.
    while written.load(Ordering::SeqCst) < accepted.load(Ordering::SeqCst) {
        wake_writer.notify_one();
        flushed.notified().await;
    }
    println!("3. stop the writer");
    writers.shutdown().await;
    println!(
        "   accepted {}, written {}",
        accepted.load(Ordering::SeqCst),
        written.load(Ordering::SeqCst)
    );

    println!("\n-- a worker that ignores cancellation, stop_grace 150ms --");
    let cancel = CancellationToken::new();
    let mut tasks = TaskSet::new(cancel.clone());
    let start = Arc::new(Notify::new());
    start.notify_one();
    let worker = WorkerBuilder::new("stuck-export", cancel.clone())
        .notifier(start)
        .stop_grace(Duration::from_millis(150))
        .build(Stuck);
    tasks.spawn("stuck-export", worker.run());
    tokio::time::sleep(Duration::from_millis(20)).await;

    let started = Instant::now();
    tasks.shutdown().await;
    println!("   shutdown took {:.0?}, not an hour", started.elapsed());

    // The deadline the worker loop used, available to your own code too.
    let cancel = CancellationToken::new();
    cancel.cancel();
    let started = Instant::now();
    stop_deadline(&cancel, Duration::from_millis(50)).await;
    println!(
        "   stop_deadline(cancelled, 50ms) resolved after {:.0?}",
        started.elapsed()
    );
}
