#![allow(clippy::unwrap_used, clippy::use_debug)]

//! A gear's background work, wired into its lifecycle.
//!
//! Gears run background work through the toolkit's `RunnableCapability`:
//!
//! ```text
//! async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()>;
//! async fn stop(&self, deadline_token: CancellationToken) -> anyhow::Result<()>;
//! ```
//!
//! `start` gets a child of the runtime's root token. `stop` gets a fresh
//! token: not cancelled means "shut down gracefully", cancelled means "the
//! graceful period is over".
//!
//! This example mirrors that shape without depending on the toolkit:
//!
//! - `start` spawns every worker into one [`TaskSet`] on a child token and
//!   keeps the set.
//! - `stop` runs `TaskSet::shutdown()`, which joins every worker, against the
//!   deadline token. Each worker's own `stop_grace` already bounds a stuck
//!   pass; the deadline token is the gear-wide backstop.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `gear_background_worker`

use std::convert::Infallible;
use std::sync::Mutex;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_taskward::{
    Directive, PanicPolicy, TaskSet, TracingListener, WorkerAction, WorkerBuilder, poker,
};

struct RefreshExchangeRates;

impl WorkerAction for RefreshExchangeRates {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        println!("  refreshed exchange rates");
        Ok(Directive::idle())
    }
}

struct PurgeExpiredQuotes;

impl WorkerAction for PurgeExpiredQuotes {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        println!("  purged expired quotes");
        Ok(Directive::idle())
    }
}

#[derive(Default)]
struct PricingGear {
    tasks: Mutex<Option<TaskSet>>,
}

impl PricingGear {
    #[allow(
        clippy::unused_async,
        reason = "mirrors RunnableCapability::start, which is async"
    )]
    async fn start(&self, cancel: CancellationToken) {
        // A child token: the gear can stop its own work without cancelling
        // the runtime's.
        let cancel = cancel.child_token();
        let mut tasks = TaskSet::new(cancel.clone());

        let (every_200ms, _) = poker(Duration::from_millis(200), cancel.clone());
        tasks.spawn(
            "refresh-exchange-rates",
            WorkerBuilder::new("refresh-exchange-rates", cancel.clone())
                .notifier(every_200ms)
                // Stale rates break pricing: never lose this worker to a panic.
                .on_panic(PanicPolicy::CatchAndRetry)
                .listener(TracingListener)
                .build(RefreshExchangeRates)
                .run(),
        );

        let (every_350ms, _) = poker(Duration::from_millis(350), cancel.clone());
        tasks.spawn(
            "purge-expired-quotes",
            WorkerBuilder::new("purge-expired-quotes", cancel)
                .notifier(every_350ms)
                .listener(TracingListener)
                .build(PurgeExpiredQuotes)
                .run(),
        );

        *self.tasks.lock().unwrap() = Some(tasks);
    }

    async fn stop(&self, deadline_token: CancellationToken) {
        let Some(tasks) = self.tasks.lock().unwrap().take() else {
            return;
        };
        tokio::select! {
            () = tasks.shutdown() => println!("  all workers stopped"),
            () = deadline_token.cancelled() => {
                // Dropping the unfinished shutdown drops the TaskSet, which
                // cancels its token; workers end at their next await.
                println!("  graceful period over, abandoning remaining workers");
            }
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .init();

    let runtime_root = CancellationToken::new();
    let gear = PricingGear::default();

    println!("start");
    gear.start(runtime_root.clone()).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    println!("stop");
    gear.stop(CancellationToken::new()).await;
}
