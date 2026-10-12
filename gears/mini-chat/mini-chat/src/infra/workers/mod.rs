//! Background workers spawned during gear startup.
//!
//! Each worker is an autonomous async task with its own interval loop
//! and graceful shutdown via [`CancellationToken`].
//!
//! - [`orphan_watchdog`] and [`upload_reaper`] require leader election (K8s Lease or noop).
//! - [`thread_summary_worker`] and [`cleanup_worker`] are outbox handlers
//!   processed by the outbox pipeline (decoupled strategy, parallel across replicas).

pub mod cleanup_worker;
pub mod orphan_watchdog;
pub mod thread_summary_worker;
pub mod upload_reaper;

use std::future::Future;
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// Handles to spawned worker tasks -- used for graceful shutdown.
#[derive(Debug)]
pub struct WorkerHandles {
    handles: Vec<(&'static str, JoinHandle<anyhow::Result<()>>)>,
}

impl WorkerHandles {
    #[must_use]
    pub fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    /// Spawn a worker and track its handle.
    ///
    /// `cancel` must be the same token passed to the worker future.
    /// The wrapper uses it to distinguish runtime failures (logged
    /// immediately) from graceful-shutdown exits (logged by [`join_all`]).
    pub fn spawn(
        &mut self,
        name: &'static str,
        cancel: CancellationToken,
        fut: impl Future<Output = anyhow::Result<()>> + Send + 'static,
    ) {
        let handle = tokio::spawn(async move {
            let result = fut.await;
            if !cancel.is_cancelled() {
                match &result {
                    Ok(()) => {
                        tracing::warn!(
                            worker = name,
                            "worker exited before shutdown was requested"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            worker = name,
                            error = %e,
                            "worker failed during runtime"
                        );
                    }
                }
            }
            result
        });
        self.handles.push((name, handle));
    }

    /// Await all worker tasks. Log errors but do not propagate -- a single
    /// worker failure must not prevent other workers from shutting down.
    #[allow(clippy::cognitive_complexity)] // inflated by tokio::select! macro
    pub async fn join_all(self, hard_stop: CancellationToken, shutdown_timeout: Duration) {
        let hard_stop_armed = !hard_stop.is_cancelled();
        let shutdown_deadline = Instant::now() + shutdown_timeout;
        let mut force_abort = false;

        for (name, mut handle) in self.handles {
            if force_abort || (hard_stop_armed && hard_stop.is_cancelled()) {
                force_abort = true;
                handle.abort();
                log_worker_result(name, handle.await);
                continue;
            }

            let remaining = shutdown_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                force_abort = true;
                handle.abort();
                log_worker_result(name, handle.await);
                continue;
            }

            if hard_stop_armed {
                tokio::select! {
                    result = &mut handle => {
                        log_worker_result(name, result);
                    }
                    () = sleep(remaining) => {
                        force_abort = true;
                        handle.abort();
                        log_worker_result(name, handle.await);
                    }
                    () = hard_stop.cancelled() => {
                        force_abort = true;
                        handle.abort();
                        log_worker_result(name, handle.await);
                    }
                }
            } else {
                tokio::select! {
                    result = &mut handle => {
                        log_worker_result(name, result);
                    }
                    () = sleep(remaining) => {
                        force_abort = true;
                        handle.abort();
                        log_worker_result(name, handle.await);
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.handles.len()
    }
}

#[allow(clippy::cognitive_complexity)] // inflated by tracing macros
fn log_worker_result(name: &str, result: Result<anyhow::Result<()>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => tracing::info!(worker = name, "worker stopped"),
        Ok(Err(e)) => tracing::warn!(worker = name, error = %e, "worker exited with error"),
        Err(e) if e.is_cancelled() => {
            tracing::warn!(worker = name, "worker aborted during shutdown");
        }
        Err(e) => tracing::warn!(worker = name, error = %e, "worker task panicked"),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn join_all_waits_for_graceful_exit_even_if_hard_stop_is_already_cancelled() {
        let completed = Arc::new(AtomicBool::new(false));
        let completed_flag = Arc::clone(&completed);

        let mut handles = WorkerHandles::new();
        let worker_cancel = CancellationToken::new();
        let worker_future_cancel = worker_cancel.clone();
        handles.spawn("test", worker_cancel.clone(), async move {
            worker_future_cancel.cancelled().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            completed_flag.store(true, Ordering::SeqCst);
            Ok(())
        });

        worker_cancel.cancel();
        let hard_stop = CancellationToken::new();
        hard_stop.cancel();

        handles.join_all(hard_stop, Duration::from_secs(1)).await;
        assert!(completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn join_all_aborts_after_timeout() {
        let mut handles = WorkerHandles::new();
        let worker_cancel = CancellationToken::new();
        handles.spawn("test", worker_cancel.clone(), async move {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(())
        });

        let started = Instant::now();
        handles
            .join_all(CancellationToken::new(), Duration::from_millis(20))
            .await;
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
