//! Starts every maintenance worker of one instance on its task tracker, so
//! `stop` ends them with the rest. This is the whole of what runs, and what
//! wakes each worker.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use toolkit_taskward::{
    BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit, PanicPolicy, TracingListener,
    WorkerBuilder, poker,
};

use super::detacher::{self, Detacher};
use super::extender::{self, Extender};
use super::reconcile::{self, Reconcile};
use super::rescan::Rescan;
use super::retention::{self, Retention};
use super::sealer::{self, Sealer};
use crate::shared::Shared;

/// Starts the rescan and the job workers over `pool`; they run until `stop`.
pub fn spawn(shared: &Arc<Shared>, pool: &PgPool, stop: &CancellationToken, tasks: &TaskTracker) {
    let settings = shared.settings.maintenance;
    let hub = &shared.maintenance;

    for i in 0..settings.extender {
        let name = format!("evbk-{}-{i}", extender::NAME);
        let worker = WorkerBuilder::new(name.clone(), stop.clone())
            .bulkhead(backoff(name))
            .notifier(hub.extend.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Extender::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

    for i in 0..settings.sealer {
        let name = format!("evbk-{}-{i}", sealer::NAME);
        let worker = WorkerBuilder::new(name.clone(), stop.clone())
            .bulkhead(backoff(name))
            .notifier(hub.seal.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Sealer::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

    for i in 0..settings.retention {
        let name = format!("evbk-{}-{i}", retention::NAME);
        let worker = WorkerBuilder::new(name.clone(), stop.clone())
            .bulkhead(backoff(name))
            .notifier(hub.retain.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Retention::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

    // Each on a connection of its own, outside the pool.
    for i in 0..settings.detacher {
        let name = format!("evbk-{}-{i}", detacher::NAME);
        let worker = WorkerBuilder::new(name.clone(), stop.clone())
            .bulkhead(backoff(name))
            .notifier(hub.detach.wakeup())
            .notifier(started())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Detacher::new(Arc::clone(shared)));
        tasks.spawn(worker.run());
    }

    // Offered only by the cross-check, which runs after startup.
    for i in 0..settings.reconcile {
        let name = format!("evbk-{}-{i}", reconcile::NAME);
        let worker = WorkerBuilder::new(name.clone(), stop.clone())
            .bulkhead(backoff(name))
            .notifier(hub.reconcile.wakeup())
            .listener(TracingListener)
            .on_panic(PanicPolicy::CatchAndRetry)
            .build(Reconcile::new(Arc::clone(shared), pool.clone()));
        tasks.spawn(worker.run());
    }

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
/// pass succeeds (`docs/DESIGN.md` §4.9). Without it a pass that fails at once,
/// e.g. on an unreachable database, is retried at once, as its job is
/// offered again.
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
/// gives it a first pass, which picks up what was due before the start.
fn started() -> Arc<Notify> {
    let notify = Arc::new(Notify::new());
    notify.notify_one();
    notify
}
