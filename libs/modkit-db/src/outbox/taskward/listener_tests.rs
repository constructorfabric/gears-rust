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
