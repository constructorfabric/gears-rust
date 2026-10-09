# ToolKit Taskward

Runtime for a gear's long-lived background workers.

## Overview

A worker is a loop a gear owns for its whole lifetime: sweep expired rows, drain a queue, refresh a cache, push reports downstream. `cf-gears-toolkit-taskward` runs that loop. The gear writes one pass of work, and the crate decides:

- **when to run** - on a timer, on an event, or right away while there is a backlog;
- **what a failure does** - errors back off and never end the worker; a panic either ends it or is retried, by explicit choice;
- **how it stops** - on cancellation, within a bounded grace, joined and accounted for;
- **what it reports** - lifecycle events to listeners for logs and metrics.

The transactional outbox in `toolkit-db` runs all of its workers on this crate.

## When to use it

Use a taskward worker when a gear has background work that:

- repeats for the gear's lifetime, periodically or when woken;
- must survive errors in the work it does, and back off while they last;
- must stop promptly and predictably when the gear stops;
- should be observable without logging written into every loop.

## When not to

| Need | Use instead |
|---|---|
| A one-off future, fire and forget | `tokio::spawn` |
| Work that must happen if, and only if, a transaction commits | `toolkit_db::outbox` |
| Cron or wall-clock schedules ("daily at 02:00") | not provided; a timer `poker` gives intervals only |
| Concurrency inside a single request | `tokio::task::JoinSet`, `futures` |

## Before and after

A periodic cleanup as gears write it today:

```rust
let handle = tokio::spawn(async move {
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(interval) => {
                let result = engine.run_sweep().await;
                tracing::info!(?result, "cleanup sweep completed");
            }
        }
    }
});
```

It works, and leaves open what the gear actually needs to know:

- a panic in `run_sweep` silently ends cleanup for the rest of the process's life;
- a failing sweep is retried at full rate, every interval;
- a sweep with more than one batch of work waits a full interval for the next batch;
- stopping waits for whatever `run_sweep` is doing, however long that takes;
- nothing reports when the loop starts, fails or stops.

The same cleanup as a worker:

```rust
struct Sweep(Arc<Engine>);

impl WorkerAction for Sweep {
    type Payload = ();
    type Error = SweepError;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, SweepError> {
        let result = self.0.run_sweep().await?;
        Ok(if result.batch_was_full { Directive::proceed() } else { Directive::idle() })
    }
}

let (tick, _) = poker(interval, cancel.clone());
tasks.spawn(
    "cleanup-sweep",
    WorkerBuilder::new("cleanup-sweep", cancel.clone())
        .notifier(tick)
        .bulkhead(Bulkhead::new("cleanup-sweep", BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig::new(Duration::from_secs(5), Duration::from_secs(300), 2.0),
        }))
        .on_panic(PanicPolicy::CatchAndRetry)
        .stop_grace(Duration::from_secs(2))
        .listener(TracingListener)
        .build(Sweep(engine))
        .run(),
);
// On gear stop: tasks.shutdown().await
```

Each of those points is now an explicit choice: `on_panic`, the bulkhead backoff, `Proceed` from the pass, `stop_grace`, the listener.

## Concepts

| Item | Role |
|---|---|
| `WorkerAction`, `Directive` | One pass of work, and what happens next: `Proceed` (more to do), `Idle` (wait for a wakeup), `Sleep(d)` (wait up to `d`) |
| `WorkerBuilder`, `WorkerTask` | Assemble a worker from an action and its policies; `run()` is the loop |
| `poker`, `Signal`, `Arc<Notify>` | Wakeup sources: a timer, a set of subjects, a bare event |
| `PacingConfig` | Delay between back-to-back passes while the action returns `Proceed` |
| `Bulkhead`, `ConcurrencyLimit`, `BackoffConfig` | A permit per pass from a shared pool, and an error backoff floor |
| `PanicPolicy` | Whether a panicking pass ends the worker or counts as one more failure |
| `TaskSet`, `stop_deadline`, `DEFAULT_STOP_GRACE` | Named tasks under one token, joined on shutdown; the bound on a pass still running at cancellation |
| `WorkerListener`, `TracingListener` | Lifecycle hooks for metrics and logs |

A worker starts idle: its first pass runs on its first wakeup, not at spawn. A worker with no notifier never runs. After an error it also goes idle, so the retry needs a wakeup too - usually a timer.

## Examples

Each example is one scenario, runs in about a second, and prints what happens:

```sh
cargo run -p cf-gears-toolkit-taskward --example <name>
```

| Example | Scenario | Shows |
|---|---|---|
| [`periodic_sweep`](examples/periodic_sweep.rs) | Replace a hand-rolled `tokio::spawn` cleanup loop | `WorkerBuilder`, `poker`, `Proceed` vs `Idle`, `PacingConfig` |
| [`wake_on_event`](examples/wake_on_event.rs) | Run jobs as soon as they arrive, with a timer for unannounced ones | several notifiers on one worker |
| [`wake_on_subject`](examples/wake_on_subject.rs) | Recompute prices only for the products that changed, duplicates collapsed | `Signal` |
| [`failure_and_backoff`](examples/failure_and_backoff.rs) | A downstream outage, then a panicking pass | `Bulkhead`, `BackoffConfig`, `PanicPolicy` |
| [`shared_concurrency`](examples/shared_concurrency.rs) | Workers sharing a database that takes two queries at a time | `ConcurrencyLimit::Fixed`, `ConcurrencyLimit::Tiered` |
| [`ordered_shutdown`](examples/ordered_shutdown.rs) | Stop ingest, flush the writer, then stop it; a worker that ignores cancellation | `TaskSet`, `stop_grace`, `stop_deadline` |
| [`observability`](examples/observability.rs) | Metrics from a custom listener next to tracing logs | `WorkerListener`, `TracingListener`, typed payloads |
| [`gear_background_worker`](examples/gear_background_worker.rs) | Workers wired into a gear's `start`/`stop` | `TaskSet` on a child token, stop against a deadline |

`tests/showcase.rs` holds further scenarios as tests on virtual time, with realistic durations (hours, seconds).

## License

Licensed under Apache-2.0.
