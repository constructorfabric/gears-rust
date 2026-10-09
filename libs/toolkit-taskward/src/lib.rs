//! Runtime for a gear's long-lived background workers.
//!
//! A worker is a loop a gear owns for its whole lifetime: sweep expired rows,
//! drain a queue, refresh a cache, push reports downstream. `taskward` runs
//! that loop and owns the parts every such loop needs and most hand-rolled
//! `tokio::spawn` loops get partly wrong:
//!
//! - **when to run** - on a timer, on an event, or right away while there is
//!   a backlog, without polling faster than needed;
//! - **what a failure does** - errors back off and never end the worker;
//!   a panic either ends it or is retried, by explicit choice;
//! - **how it stops** - on cancellation, within a bounded grace, joined and
//!   accounted for;
//! - **what it reports** - lifecycle events to listeners for logs and metrics.
//!
//! The gear writes only the pass: a [`WorkerAction`] that does one unit of
//! work and returns a [`Directive`] saying what comes next. The transactional
//! outbox in `toolkit-db` runs all of its workers on this crate.
//!
//! # When to use it
//!
//! Use a taskward worker when a gear has background work that
//!
//! - repeats for the gear's lifetime, periodically or when woken;
//! - must survive errors in the work it does, and back off while they last;
//! - must stop promptly and predictably when the gear stops;
//! - should be observable without logging written into every loop.
//!
//! # When not to
//!
//! | Need | Use instead |
//! |---|---|
//! | A one-off future, fire and forget | `tokio::spawn` |
//! | Work that must happen if, and only if, a transaction commits | `toolkit_db::outbox` |
//! | Cron or wall-clock schedules ("daily at 02:00") | not provided; a timer [`poker`] gives intervals only |
//! | Concurrency inside a single request | `tokio::task::JoinSet`, `futures` |
//!
//! # Concepts
//!
//! | Item | Role |
//! |---|---|
//! | [`WorkerAction`], [`Directive`] | One pass of work, and what happens next: `Proceed` (more to do), `Idle` (wait for a wakeup), `Sleep(d)` (wait up to `d`) |
//! | [`WorkerBuilder`], [`WorkerTask`] | Assemble a worker from an action and its policies; `run()` is the loop |
//! | [`poker`], [`Signal`], `Arc<Notify>` | Wakeup sources: a timer, a set of subjects, a bare event |
//! | [`PacingConfig`] | Delay between back-to-back passes while the action returns `Proceed` |
//! | [`Bulkhead`], [`ConcurrencyLimit`], [`BackoffConfig`] | A permit per pass from a shared pool, and an error backoff floor |
//! | [`PanicPolicy`] | Whether a panicking pass ends the worker or counts as one more failure |
//! | [`TaskSet`], [`stop_deadline`], [`DEFAULT_STOP_GRACE`] | Named tasks under one token, joined on shutdown; the bound on a pass still running at cancellation |
//! | [`WorkerListener`], [`TracingListener`] | Lifecycle hooks for metrics and logs |
//!
//! A worker starts idle: its first pass runs on its first wakeup, not at
//! spawn. A worker with no notifier never runs. After an error it also goes
//! idle, so the retry needs a wakeup too - usually a timer.
//!
//! # Example
//!
//! A cleanup that runs every 30 seconds, drains a backlog in batches, and
//! stops with the gear:
//!
//! ```
//! use std::convert::Infallible;
//! use std::time::Duration;
//!
//! use tokio_util::sync::CancellationToken;
//! use toolkit_taskward::{Directive, TaskSet, WorkerAction, WorkerBuilder, poker};
//!
//! struct PurgeExpired;
//!
//! impl WorkerAction for PurgeExpired {
//!     type Payload = ();
//!     type Error = Infallible;
//!
//!     async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Infallible> {
//!         let purged = 0; // delete up to one batch of expired rows
//!         let batch_was_full = purged == 500;
//!         Ok(if batch_was_full { Directive::proceed() } else { Directive::idle() })
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let cancel = CancellationToken::new();
//! let mut tasks = TaskSet::new(cancel.clone());
//!
//! let (every_30s, _) = poker(Duration::from_secs(30), cancel.clone());
//! let worker = WorkerBuilder::new("purge-expired", cancel.clone())
//!     .notifier(every_30s)
//!     .build(PurgeExpired);
//! tasks.spawn("purge-expired", worker.run());
//!
//! // On gear stop:
//! tasks.shutdown().await;
//! # }
//! ```
//!
//! Runnable examples, one scenario each, are in `examples/` - see the README.

mod action;
mod bulkhead;
mod listener;
mod pacing;
mod poker;
mod signal;
mod task;
mod task_set;

pub use action::{Directive, WorkerAction};
pub use bulkhead::{BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit};
pub use listener::{TracingListener, WorkerListener};
pub use pacing::PacingConfig;
pub use poker::poker;
pub use signal::Signal;
pub use task::{DEFAULT_STOP_GRACE, PanicPolicy, WorkerBuilder, WorkerTask, stop_deadline};
pub use task_set::TaskSet;
