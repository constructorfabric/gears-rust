#![allow(clippy::unwrap_used, clippy::use_debug)]

//! Periodic sweep - the loop most gears write by hand.
//!
//! A gear keeps sessions in memory and has to drop the expired ones every few
//! seconds. The hand-rolled version is a `tokio::spawn` around
//! `select! { cancel, sleep(interval) }`. That works, but every gear re-decides
//! what happens on error, on panic, when a sweep has more to do than one batch,
//! and how long shutdown may wait.
//!
//! Here the timer is a [`poker`], the sweep is a [`WorkerAction`], and the
//! directive the sweep returns says what happens next:
//!
//! - `Idle` - nothing left: wait for the next tick.
//! - `Proceed` - the batch was full, more is due: run again shortly, without
//!   waiting for the tick. [`PacingConfig`] sets how shortly.
//!
//! Note that a worker starts idle: its first pass runs on the first wakeup,
//! not at spawn. A worker with no notifier never runs.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-taskward --example `periodic_sweep`

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, PacingConfig, WorkerAction, WorkerBuilder, poker};

type Sessions = Arc<Mutex<BTreeMap<u32, Instant>>>;

struct ExpiredSessionSweeper {
    sessions: Sessions,
    batch: usize,
    started: Instant,
}

impl WorkerAction for ExpiredSessionSweeper {
    type Payload = ();
    // The sweep cannot fail; see `failure_and_backoff` for one that can.
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().unwrap();
        let expired: Vec<u32> = sessions
            .iter()
            .filter(|(_, expires_at)| **expires_at <= now)
            .map(|(id, _)| *id)
            .take(self.batch)
            .collect();
        for id in &expired {
            sessions.remove(id);
        }
        let more_due = sessions.values().any(|expires_at| *expires_at <= now);

        println!(
            "{:>5.0?}  swept {expired:?}, {} left{}",
            self.started.elapsed(),
            sessions.len(),
            if more_due {
                ", more due -> Proceed"
            } else {
                " -> Idle"
            },
        );
        Ok(if more_due {
            Directive::proceed()
        } else {
            Directive::idle()
        })
    }
}

#[tokio::main]
async fn main() {
    let started = Instant::now();

    // Sessions 1-5 expire at once, 6-7 a bit later, 8 outlives the demo.
    let sessions: Sessions = Arc::new(Mutex::new(BTreeMap::from([
        (1, started),
        (2, started),
        (3, started),
        (4, started),
        (5, started),
        (6, started + Duration::from_millis(400)),
        (7, started + Duration::from_millis(450)),
        (8, started + Duration::from_secs(3600)),
    ])));

    // The gear's lifecycle token; cancelling it stops both the poker and the
    // worker.
    let cancel = CancellationToken::new();
    let (tick, _poker) = poker(Duration::from_millis(250), cancel.clone());

    let worker = WorkerBuilder::new("session-sweeper", cancel.clone())
        .notifier(tick)
        // Between back-to-back passes: start at 50ms, ramp down to 20ms.
        .pacing(PacingConfig {
            min_interval: Duration::from_millis(20),
            active_interval: Duration::from_millis(50),
            ramp_step: Duration::from_millis(15),
        })
        .build(ExpiredSessionSweeper {
            sessions: Arc::clone(&sessions),
            batch: 2,
            started,
        });
    let handle = tokio::spawn(worker.run());

    tokio::time::sleep(Duration::from_millis(900)).await;
    cancel.cancel();
    handle.await.unwrap();

    println!(
        "stopped; remaining sessions: {:?}",
        sessions.lock().unwrap().keys().collect::<Vec<_>>()
    );
}
