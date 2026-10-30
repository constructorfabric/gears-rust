//! Starts the maintenance of one instance on its task tracker, so `stop` ends
//! it with the rest: the worker, woken by offers, and the rescan, woken by a
//! timer.

use std::sync::Arc;

use sqlx::MySqlPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use toolkit_taskward::{
    BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit, PanicPolicy, TracingListener,
    WorkerBuilder, poker,
};

use super::rescan::Rescan;
use super::worker::Maintainer;
use crate::shared::Shared;

/// Starts the worker and the rescan over `pool`; they run until `stop`.
pub fn spawn(
    shared: &Arc<Shared>,
    pool: &MySqlPool,
    stop: &CancellationToken,
    tasks: &TaskTracker,
) {
    let settings = shared.settings.maintenance;
    let worker = WorkerBuilder::new("evbk-maintenance", stop.clone())
        .bulkhead(backoff("evbk-maintenance"))
        .notifier(shared.maintenance.queue.notifier())
        .notifier(started())
        .listener(TracingListener)
        .on_panic(PanicPolicy::CatchAndRetry)
        .build(Maintainer::new(Arc::clone(shared), pool.clone()));
    tasks.spawn(worker.run());

    let (tick, _) = poker(settings.rescan_interval, stop.clone());
    let worker = WorkerBuilder::new("evbk-rescan", stop.clone())
        .bulkhead(backoff("evbk-rescan"))
        .notifier(tick)
        .notifier(started())
        .listener(TracingListener)
        .on_panic(PanicPolicy::CatchAndRetry)
        .build(Rescan::new(Arc::clone(shared), pool.clone()));
    tasks.spawn(worker.run());
}

/// A failed pass holds its worker off for 100 ms, doubling up to 30 s, until a
/// pass succeeds: without it a pass that fails at once, e.g. on an
/// unreachable database, is retried at once.
fn backoff(name: impl Into<String>) -> Bulkhead {
    Bulkhead::new(
        name,
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig::default(),
        },
    )
}

/// Fires once, as soon as its worker waits: a worker starts idle, and this
/// gives it a first pass.
fn started() -> Arc<Notify> {
    let notify = Arc::new(Notify::new());
    notify.notify_one();
    notify
}
