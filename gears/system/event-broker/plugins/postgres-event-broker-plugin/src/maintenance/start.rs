//! Starts every maintenance worker of one instance on its task tracker, so
//! `stop` ends them with the rest. This is the whole of what runs, and what
//! wakes each worker.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use toolkit_taskward::{PanicPolicy, TracingListener, WorkerBuilder, poker};

use super::counter::{self, Counter};
use super::detacher::{self, Detacher};
use super::extender::{self, Extender};
use super::reconcile::{self, Reconcile};
use super::rescan::Rescan;
use super::retention::{self, Retention};
use crate::shared::Shared;

/// Starts the rescan and the job workers over `pool`; they run until `stop`.
pub fn spawn(shared: &Arc<Shared>, pool: &PgPool, stop: &CancellationToken, tasks: &TaskTracker) {
    let settings = shared.settings.maintenance;
    let hub = &shared.maintenance;

    for i in 0..settings.extender {
        let worker = WorkerBuilder::new(format!("evbk-{}-{i}", extender::NAME), stop.clone())
            .notifier(hub.extend.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Extender::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

    for i in 0..settings.counter {
        let worker = WorkerBuilder::new(format!("evbk-{}-{i}", counter::NAME), stop.clone())
            .notifier(hub.count.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Counter::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

    for i in 0..settings.retention {
        let worker = WorkerBuilder::new(format!("evbk-{}-{i}", retention::NAME), stop.clone())
            .notifier(hub.retain.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Retention::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

    // Each on a connection of its own, outside the pool.
    for i in 0..settings.detacher {
        let worker = WorkerBuilder::new(format!("evbk-{}-{i}", detacher::NAME), stop.clone())
            .notifier(hub.detach.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Detacher::new(Arc::clone(shared)));
        tasks.spawn(worker.run());
    }

    // Offered only by the cross-check, which runs after startup.
    for i in 0..settings.reconcile {
        let worker = WorkerBuilder::new(format!("evbk-{}-{i}", reconcile::NAME), stop.clone())
            .notifier(hub.reconcile.wakeup())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Reconcile::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

    let (tick, _) = poker(settings.rescan_interval, stop.clone());
    let worker = WorkerBuilder::new("evbk-rescan", stop.clone())
        .notifier(tick)
        .notifier(started())
        .listener(TracingListener)
        .on_panic(PanicPolicy::CatchAndRetry)
        .build(Rescan::new(Arc::clone(shared), pool.clone()));
    tasks.spawn(worker.run());
}

/// Fires once, as soon as its worker waits: a worker starts idle, and this
/// gives it a first pass, which picks up what was due before the start.
fn started() -> Arc<Notify> {
    let notify = Arc::new(Notify::new());
    notify.notify_one();
    notify
}
