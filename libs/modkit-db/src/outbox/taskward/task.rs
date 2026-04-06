use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::FutureExt as _;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use tracing::Instrument;

use super::action::{Directive, WorkerAction};
use super::bulkhead::Bulkhead;
use super::listener::WorkerListener;
use super::pacing::PacingConfig;

/// Extract a human-readable message from a panic payload.
fn panic_message(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        format!("panic: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("panic: {s}")
    } else {
        "panic: <non-string payload>".to_owned()
    }
}

/// Controls how the worker loop handles panics inside `execute()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanicPolicy {
    /// Catch the panic, treat it as an error (bulkhead escalates), and keep
    /// the worker running. Use this when losing a worker is unacceptable
    /// (e.g., singleton sequencer).
    CatchAndRetry,
    /// Let the panic propagate, killing the worker's tokio task. Follows
    /// Rust convention that panics indicate invariant violations.
    #[default]
    Propagate,
}

/// Builder for [`WorkerTask`]. Flat configuration — no type-state markers.
///
/// ```text
/// WorkerBuilder::new(name, cancel)
///     .notifier(event_notify)
///     .pacing(PacingConfig::default())
///     .bulkhead(bulkhead)
///     .listener(TracingListener::default())
///     .build(action)  → WorkerTask<A>
/// ```
pub struct WorkerBuilder<P = ()> {
    name: String,
    cancel: CancellationToken,
    bulkhead: Bulkhead,
    notifiers: Vec<Arc<Notify>>,
    listeners: Vec<Arc<dyn WorkerListener<P>>>,
    panic_policy: PanicPolicy,
    pacing: Option<PacingConfig>,
}

impl<P: Send + Sync + 'static> WorkerBuilder<P> {
    #[must_use]
    pub fn new(name: impl Into<String>, cancel: CancellationToken) -> Self {
        Self {
            name: name.into(),
            cancel,
            bulkhead: Bulkhead::default(),
            notifiers: Vec::new(),
            listeners: Vec::new(),
            panic_policy: PanicPolicy::default(),
            pacing: None,
        }
    }

    /// Subscribe to an external notification source.
    #[must_use]
    pub fn notifier(mut self, notify: Arc<Notify>) -> Self {
        self.notifiers.push(notify);
        self
    }

    /// Set the adaptive pacing configuration for this worker.
    /// Defaults to `PacingConfig::default()` if not set.
    #[must_use]
    pub fn pacing(mut self, pacing: impl Into<PacingConfig>) -> Self {
        self.pacing = Some(pacing.into());
        self
    }

    /// Configure the bulkhead (concurrency gate + error-driven backoff).
    #[must_use]
    pub fn bulkhead(mut self, bulkhead: Bulkhead) -> Self {
        self.bulkhead = bulkhead;
        self
    }

    /// Set the panic handling policy for this worker.
    #[must_use]
    pub fn on_panic(mut self, policy: PanicPolicy) -> Self {
        self.panic_policy = policy;
        self
    }

    /// Register a lifecycle listener.
    #[must_use]
    pub fn listener(mut self, listener: impl WorkerListener<P> + 'static) -> Self {
        self.listeners.push(Arc::new(listener));
        self
    }

    /// Build the worker task.
    #[must_use]
    pub fn build<A: WorkerAction<Payload = P>>(self, action: A) -> WorkerTask<A> {
        let pacing = self.pacing.unwrap_or_default();

        // Pokers (periodic wake timers) are the caller's responsibility
        // via .notifier(). Taskward handles pacing only.
        WorkerTask {
            name: self.name,
            action,
            notifiers: self.notifiers,
            cancel: self.cancel,
            bulkhead: self.bulkhead,
            listeners: self.listeners,
            panic_policy: self.panic_policy,
            pacing,
        }
    }
}

/// A generic worker that repeatedly executes an action and uses the returned
/// directive to decide when to execute again.
///
/// The worker never exits on action errors — the [`Bulkhead`] absorbs them
/// with escalating backoff. The loop only exits on cancellation.
pub struct WorkerTask<A: WorkerAction> {
    name: String,
    action: A,
    notifiers: Vec<Arc<Notify>>,
    cancel: CancellationToken,
    bulkhead: Bulkhead,
    listeners: Vec<Arc<dyn WorkerListener<A::Payload>>>,
    panic_policy: PanicPolicy,
    pacing: PacingConfig,
}

/// Race all notifiers — returns when any one fires.
/// If the list is empty, pends forever (only cancellation can wake).
async fn wait_any(notifiers: &[Arc<Notify>]) {
    if notifiers.is_empty() {
        return std::future::pending().await;
    }
    if notifiers.len() == 1 {
        notifiers[0].notified().await;
        return;
    }
    let pinned: Vec<_> = notifiers.iter().map(|n| Box::pin(n.notified())).collect();
    futures_util::future::select_all(pinned).await;
}

impl<A: WorkerAction> WorkerTask<A> {
    /// Run the worker loop until cancellation.
    pub async fn run(mut self) {
        let span = tracing::info_span!("worker", name = %self.name);
        let result = AssertUnwindSafe(self.run_inner().instrument(span))
            .catch_unwind()
            .await;

        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    fn notify_listeners<F: Fn(&dyn WorkerListener<A::Payload>)>(&self, f: F) {
        for listener in &self.listeners {
            f(listener.as_ref());
        }
    }

    async fn run_inner(&mut self) {
        self.notify_listeners(|l| l.on_start());

        let mut directive = Directive::idle();
        let mut last_execute = tokio::time::Instant::now();
        let mut current_pace = self.pacing.active_interval;
        loop {
            match directive {
                Directive::Proceed(()) => {
                    // More work — sleep at current pace, then ramp down for next.
                    tokio::select! {
                        () = self.cancel.cancelled() => break,
                        () = tokio::time::sleep(current_pace) => {}
                    }
                    current_pace = current_pace
                        .saturating_sub(self.pacing.ramp_step)
                        .max(self.pacing.min_interval);
                }
                Directive::Idle(()) => {
                    // No work — wait for external signal.
                    current_pace = self.pacing.active_interval;
                    self.notify_listeners(|l| l.on_idle());
                    // Bulkhead error backoff floor
                    let error_floor = self.bulkhead.min_interval();
                    if !error_floor.is_zero() {
                        let elapsed = last_execute.elapsed();
                        if elapsed < error_floor {
                            tokio::select! {
                                () = self.cancel.cancelled() => break,
                                () = tokio::time::sleep(error_floor.saturating_sub(elapsed)) => {}
                            }
                        }
                    }
                    tokio::select! {
                        () = self.cancel.cancelled() => break,
                        () = wait_any(&self.notifiers) => {}
                    }
                }
                Directive::Sleep(d, ()) => {
                    // Soft sleep — rest for up to d, wake early on notification.
                    current_pace = self.pacing.active_interval;
                    self.notify_listeners(|l| l.on_sleep(d));
                    tokio::select! {
                        () = self.cancel.cancelled() => break,
                        () = wait_any(&self.notifiers) => {}
                        () = tokio::time::sleep(d) => {}
                    }
                }
            }

            if self.cancel.is_cancelled() {
                break;
            }

            let Some(_permit) = self.bulkhead.acquire(&self.cancel).await else {
                break;
            };

            self.notify_listeners(|l| l.on_execute_start());
            last_execute = tokio::time::Instant::now();
            let result = match self.panic_policy {
                PanicPolicy::CatchAndRetry => {
                    AssertUnwindSafe(self.action.execute(&self.cancel))
                        .catch_unwind()
                        .await
                }
                PanicPolicy::Propagate => Ok(self.action.execute(&self.cancel).await),
            };
            match result {
                Ok(Ok(d)) => {
                    let duration = last_execute.elapsed();
                    self.bulkhead.reset();
                    directive = d.strip();
                    self.notify_listeners(|l| l.on_complete(duration, &d));
                }
                Ok(Err(e)) => {
                    let duration = last_execute.elapsed();
                    self.bulkhead.escalate();
                    let failures = self.bulkhead.consecutive_failures();
                    let backoff = self.bulkhead.min_interval();
                    let error_str = e.to_string();
                    self.notify_listeners(|l| {
                        l.on_error(duration, &error_str, failures, backoff);
                    });
                    directive = Directive::idle();
                }
                Err(panic_payload) => {
                    let duration = last_execute.elapsed();
                    self.bulkhead.escalate();
                    let failures = self.bulkhead.consecutive_failures();
                    let backoff = self.bulkhead.min_interval();
                    let panic_msg = panic_message(&panic_payload);
                    self.notify_listeners(|l| {
                        l.on_error(duration, &panic_msg, failures, backoff);
                    });
                    directive = Directive::idle();
                }
            }
        }

        self.notify_listeners(|l| l.on_stop());
    }
}

#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;
