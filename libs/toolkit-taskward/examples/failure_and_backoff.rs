#![allow(clippy::unwrap_used, clippy::use_debug)]

//! Failure handling: errors back off, panics follow an explicit policy.
//!
//! A gear pushes usage reports to a billing API that is down for a while.
//! A hand-rolled loop either hammers the API at full rate, or exits on the
//! first error and silently stops reporting.
//!
//! A taskward worker never exits on an error. The [`Bulkhead`] records each
//! consecutive failure and raises a backoff floor - here 50ms, doubling, capped
//! at 400ms - below which the worker does not run again. The first success
//! resets it.
//!
//! A panic is a separate decision, set by [`PanicPolicy`]:
//!
//! - `Propagate` (default) - the panic ends the worker's task, as a panic
//!   should when it means a broken invariant. A [`toolkit_taskward::TaskSet`]
//!   reports it at shutdown.
//! - `CatchAndRetry` - the panic is counted as one more failure and the worker
//!   keeps running. Use it for a worker the gear cannot afford to lose.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `failure_and_backoff`

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{
    BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit, Directive, PanicPolicy,
    WorkerAction, WorkerBuilder, WorkerListener, poker,
};

/// Prints each failure with the backoff it earns, and the recovery.
struct PrintFailures {
    started: Instant,
    failing: AtomicBool,
}

impl PrintFailures {
    fn new(started: Instant) -> Self {
        Self {
            started,
            failing: AtomicBool::new(false),
        }
    }
}

impl WorkerListener for PrintFailures {
    fn on_complete(&self, _duration: Duration, _directive: &Directive) {
        if self.failing.swap(false, Ordering::Relaxed) {
            println!(
                "{:>5.0?}  recovered - backoff reset",
                self.started.elapsed()
            );
        }
    }

    fn on_error(&self, _duration: Duration, error: &str, failures: u32, backoff: Duration) {
        self.failing.store(true, Ordering::Relaxed);
        println!(
            "{:>5.0?}  failed ({error}) - {failures} in a row, next try in >= {backoff:.0?}",
            self.started.elapsed(),
        );
    }
}

/// Fails `outage` times, then succeeds.
struct ReportUsage {
    outage: u32,
    attempts: u32,
}

impl WorkerAction for ReportUsage {
    type Payload = ();
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        self.attempts += 1;
        if self.attempts <= self.outage {
            return Err("billing API: 503".to_owned());
        }
        Ok(Directive::idle())
    }
}

/// Panics on its second pass.
struct CrashOnce {
    attempts: u32,
}

impl WorkerAction for CrashOnce {
    type Payload = ();
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        self.attempts += 1;
        assert!(self.attempts != 2, "index out of bounds in report builder");
        Ok(Directive::idle())
    }
}

fn backoff_bulkhead(name: &str) -> Bulkhead {
    let mut backoff =
        BackoffConfig::new(Duration::from_millis(50), Duration::from_millis(400), 2.0);
    // No jitter, so the printed numbers are exact. Keep the default (0.3) in
    // a real gear: it stops many instances from retrying in lockstep.
    backoff.jitter = 0.0;
    Bulkhead::new(
        name,
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff,
        },
    )
}

#[tokio::main]
async fn main() {
    // The default panic hook would print a backtrace hint for the demo panics.
    std::panic::set_hook(Box::new(|info| {
        println!(
            "       panic: {}",
            info.payload_as_str().unwrap_or("<non-string>")
        );
    }));

    println!("-- billing API down for 5 attempts, retry tick every 10ms --");
    let started = Instant::now();
    let cancel = CancellationToken::new();
    // The tick is the retry trigger; the bulkhead decides when a retry is allowed.
    let (tick, _poker) = poker(Duration::from_millis(10), cancel.clone());
    let worker = WorkerBuilder::new("usage-reporter", cancel.clone())
        .notifier(tick)
        .bulkhead(backoff_bulkhead("usage-reporter"))
        .listener(PrintFailures::new(started))
        .build(ReportUsage {
            outage: 5,
            attempts: 0,
        });
    let handle = tokio::spawn(worker.run());
    tokio::time::sleep(Duration::from_millis(1250)).await;
    cancel.cancel();
    handle.await.unwrap();

    for (policy, label) in [
        (PanicPolicy::CatchAndRetry, "CatchAndRetry"),
        (PanicPolicy::Propagate, "Propagate"),
    ] {
        println!("\n-- a pass panics, PanicPolicy::{label} --");
        let started = Instant::now();
        let cancel = CancellationToken::new();
        let (tick, _poker) = poker(Duration::from_millis(10), cancel.clone());
        let worker = WorkerBuilder::new("report-builder", cancel.clone())
            .notifier(tick)
            .bulkhead(backoff_bulkhead("report-builder"))
            .on_panic(policy)
            .listener(PrintFailures::new(started))
            .build(CrashOnce { attempts: 0 });
        let handle = tokio::spawn(worker.run());
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        match handle.await {
            Ok(()) => println!("       worker still alive until shutdown"),
            Err(e) if e.is_panic() => println!("       worker task died with the panic"),
            Err(e) => println!("       worker task ended: {e}"),
        }
    }
}
