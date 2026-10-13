#![allow(clippy::unwrap_used, clippy::use_debug)]

//! Observability: one listener per concern, attached at build time.
//!
//! A hand-rolled loop gets whatever logging its author remembered to write.
//! A taskward worker reports its lifecycle to [`WorkerListener`]s: start,
//! each pass, its outcome and duration, errors with their backoff, idle and
//! sleep, stop. All hooks are optional no-ops.
//!
//! - [`TracingListener`] logs these events through `tracing`: debug for
//!   start/stop and early failures, warn once failures keep repeating.
//! - A custom listener turns them into metrics. Here a pass reports how many
//!   rows it handled as the directive's payload (`Directive<usize>`), and the
//!   listener reads it back through `directive.payload()`.
//!
//! Every worker's events are emitted inside a `worker` span carrying its name.
//!
//! Logs at debug level unless `RUST_LOG` says otherwise.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `observability`

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_taskward::{
    BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit, Directive, PacingConfig,
    TracingListener, WorkerAction, WorkerBuilder, WorkerListener, poker,
};
use tracing_subscriber::EnvFilter;

/// What a metrics exporter would read on scrape.
#[derive(Default)]
struct WorkerMetrics {
    passes: AtomicU32,
    errors: AtomicU32,
    rows: AtomicUsize,
    slowest_pass_micros: AtomicU64,
}

struct MetricsListener(Arc<WorkerMetrics>);

impl WorkerListener<usize> for MetricsListener {
    fn on_complete(&self, duration: Duration, directive: &Directive<usize>) {
        self.0.passes.fetch_add(1, Ordering::Relaxed);
        self.0
            .rows
            .fetch_add(*directive.payload(), Ordering::Relaxed);
        let micros = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        self.0
            .slowest_pass_micros
            .fetch_max(micros, Ordering::Relaxed);
    }

    fn on_error(&self, _duration: Duration, _error: &str, _failures: u32, _backoff: Duration) {
        self.0.errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// Archives rows in batches of up to 100; the third pass fails.
struct ArchiveRows {
    backlog: usize,
    pass: u32,
}

impl WorkerAction for ArchiveRows {
    type Payload = usize;
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive<usize>, String> {
        self.pass += 1;
        if self.pass == 3 {
            return Err("archive bucket: connection reset".to_owned());
        }
        let batch = self.backlog.min(100);
        self.backlog -= batch;
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(if self.backlog > 0 {
            Directive::Proceed(batch)
        } else {
            Directive::Idle(batch)
        })
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "debug".into()))
        .with_target(false)
        .without_time()
        .init();

    let metrics = Arc::new(WorkerMetrics::default());
    let cancel = CancellationToken::new();
    let (tick, _poker) = poker(Duration::from_millis(20), cancel.clone());

    let worker = WorkerBuilder::new("row-archiver", cancel.clone())
        .notifier(tick)
        // Back-to-back batches 5ms apart while there is a backlog.
        .pacing(PacingConfig {
            min_interval: Duration::from_millis(5),
            active_interval: Duration::from_millis(5),
            ramp_step: Duration::ZERO,
        })
        .bulkhead(Bulkhead::new(
            "row-archiver",
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Unlimited,
                backoff: BackoffConfig::new(
                    Duration::from_millis(20),
                    Duration::from_millis(200),
                    2.0,
                ),
            },
        ))
        .listener(TracingListener)
        .listener(MetricsListener(Arc::clone(&metrics)))
        .build(ArchiveRows {
            backlog: 450,
            pass: 0,
        });
    let handle = tokio::spawn(worker.run());

    tokio::time::sleep(Duration::from_millis(400)).await;
    cancel.cancel();
    handle.await.unwrap();

    println!(
        "metrics: passes={} errors={} rows={} slowest_pass={:?}",
        metrics.passes.load(Ordering::Relaxed),
        metrics.errors.load(Ordering::Relaxed),
        metrics.rows.load(Ordering::Relaxed),
        Duration::from_micros(metrics.slowest_pass_micros.load(Ordering::Relaxed)),
    );
}
