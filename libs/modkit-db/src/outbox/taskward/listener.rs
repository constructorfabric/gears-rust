use std::time::Duration;

use super::action::Directive;

/// Observer for worker lifecycle events.
///
/// The framework emits no logs or metrics itself — all observability is
/// delegated to listeners registered per-worker via
/// [`WorkerBuilder::listener`](super::WorkerBuilder::listener).
///
/// The type parameter `P` matches the worker's
/// [`WorkerAction::Payload`](super::WorkerAction::Payload) type.
/// Generic listeners that ignore the payload can use a blanket impl over all
/// `P`.
///
/// All methods have default no-op implementations; listeners only need to
/// override the events they care about.
pub trait WorkerListener<P = ()>: Send + Sync {
    /// Called once when the worker loop begins.
    fn on_start(&self) {}

    /// Called once when the worker loop exits.
    fn on_stop(&self) {}

    /// Called immediately before each `execute()` invocation.
    ///
    /// Use this for idle-time tracking (time since the last `on_complete` /
    /// `on_error`) or per-execution instrumentation preamble.
    fn on_execute_start(&self) {}

    /// Called after a successful `execute()`.
    ///
    /// The directive carries the typed payload accessible via
    /// [`Directive::payload()`].
    fn on_complete(&self, _duration: Duration, _directive: &Directive<P>) {}

    /// Called after a failed `execute()`.
    fn on_error(
        &self,
        _duration: Duration,
        _error: &str,
        _consecutive_failures: u32,
        _backoff: Duration,
    ) {
    }

    /// Called when the worker enters the Idle wait state.
    fn on_idle(&self) {}

    /// Called when the worker enters the Sleep wait state.
    fn on_sleep(&self, _duration: Duration) {}
}

/// Convenience [`WorkerListener`] that logs lifecycle events via `tracing`.
///
/// Implements `WorkerListener<P>` for all payload types — the payload is
/// ignored; only the direction is logged.
///
/// - Start/stop: `debug!`
/// - Successful execution: `trace!` with duration and direction
/// - Errors with `consecutive_failures <= 3`: `debug!`
/// - Errors with `consecutive_failures > 3`: `warn!`
#[derive(Debug, Default)]
pub struct TracingListener;

impl<P: Send + Sync + 'static> WorkerListener<P> for TracingListener {
    fn on_start(&self) {
        tracing::debug!("starting");
    }

    fn on_stop(&self) {
        tracing::debug!("stopped");
    }

    fn on_complete(&self, duration: Duration, directive: &Directive<P>) {
        let sched = directive.strip();
        tracing::trace!(?duration, ?sched, "executed");
    }

    fn on_error(
        &self,
        duration: Duration,
        error: &str,
        consecutive_failures: u32,
        backoff: Duration,
    ) {
        if consecutive_failures <= 3 {
            tracing::debug!(
                error,
                ?duration,
                ?backoff,
                attempt = consecutive_failures,
                "action failed, retrying",
            );
        } else {
            tracing::warn!(
                error,
                ?duration,
                ?backoff,
                attempt = consecutive_failures,
                "action repeatedly failing",
            );
        }
    }

    fn on_idle(&self) {
        tracing::trace!("idle");
    }

    fn on_sleep(&self, duration: Duration) {
        tracing::trace!(?duration, "sleeping");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopListener;
    impl WorkerListener for NoopListener {}

    #[test]
    fn noop_listener_compiles() {
        let listener = NoopListener;
        listener.on_start();
        listener.on_stop();
        listener.on_execute_start();
        listener.on_complete(Duration::from_secs(1), &Directive::proceed());
        listener.on_error(Duration::from_secs(1), "err", 1, Duration::from_secs(1));
        listener.on_idle();
        listener.on_sleep(Duration::from_secs(1));
    }

    #[test]
    fn tracing_listener_start_stop() {
        let listener: &dyn WorkerListener<()> = &TracingListener;
        // These should not panic
        listener.on_start();
        listener.on_stop();
    }

    #[test]
    fn tracing_listener_graduated_error_levels() {
        let listener: &dyn WorkerListener<()> = &TracingListener;
        // First 3 failures → debug level (no panic)
        for i in 1..=3 {
            listener.on_error(
                Duration::from_millis(10),
                "transient",
                i,
                Duration::from_secs(i.into()),
            );
        }
        // 4th failure → warn level (no panic)
        listener.on_error(
            Duration::from_millis(10),
            "persistent",
            4,
            Duration::from_secs(8),
        );
    }

    // Verify TracingListener works with a non-() payload type.
    #[test]
    fn tracing_listener_with_typed_payload() {
        let listener = TracingListener;
        let directive = Directive::Proceed(42_u32);
        listener.on_complete(Duration::from_millis(5), &directive);
    }
}
