#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "bootstrap")]

//! Smoke tests for `init_panic_tracing`.

use std::sync::{Arc, Mutex};
use std::thread;

use tracing::Level;
use tracing_subscriber::layer::SubscriberExt;

use modkit::bootstrap::host::init_panic_tracing;

/// A minimal tracing layer that captures formatted event messages.
#[derive(Clone, Default)]
struct CapturedEvents {
    events: Arc<Mutex<Vec<String>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturedEvents {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // Only capture ERROR-level events (the panic hook emits at ERROR).
        if *event.metadata().level() != Level::ERROR {
            return;
        }

        let mut visitor = FieldCollector(String::new());
        event.record(&mut visitor);

        // Prepend the static message so assertions can match on it.
        let msg = format!("{} {}", event.metadata().name(), visitor.0);
        self.events.lock().unwrap().push(msg);
    }
}

/// Simple visitor that concatenates all field values into one string.
struct FieldCollector(String);

impl tracing::field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        #[allow(clippy::use_debug)]
        {
            _ = write!(self.0, "{}={:?} ", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        _ = write!(self.0, "{}={} ", field.name(), value);
    }
}

#[test]
fn panic_hook_emits_error_event_with_payload() {
    let captured = CapturedEvents::default();
    let events = captured.events.clone();

    let subscriber = tracing_subscriber::registry().with(captured);

    // Build a `Dispatch` so we can propagate it into the spawned thread.
    // `with_default` only covers the current thread; the panic hook runs
    // on the panicking thread, so we need the dispatch active there.
    let dispatch = tracing::Dispatch::new(subscriber);

    tracing::dispatcher::with_default(&dispatch, || {
        // Install the panic hook while the test subscriber is active.
        init_panic_tracing();
    });

    // Spawn a thread that carries the same dispatch, then panics.
    let dispatch_clone = dispatch;
    let handle = thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatch_clone, || {
            panic!("test_panic_payload");
        });
    });

    // The thread should finish (with a panic).
    let result = handle.join();
    assert!(result.is_err(), "spawned thread must have panicked");

    let captured_events = events.lock().unwrap();
    assert!(
        !captured_events.is_empty(),
        "expected at least one captured ERROR event from the panic hook"
    );

    let combined = captured_events.join("\n");
    assert!(
        combined.contains("PANIC"),
        "expected 'PANIC' in captured events, got: {combined}"
    );
    assert!(
        combined.contains("test_panic_payload"),
        "expected panic payload 'test_panic_payload' in captured events, got: {combined}"
    );
}
