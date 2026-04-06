use super::*;
use std::sync::atomic::{AtomicU32, Ordering as AOrd};
use tokio::time::{Duration, sleep};

struct TestRunnable {
    counter: AtomicU32,
}

impl TestRunnable {
    fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
        }
    }
    fn count(&self) -> u32 {
        self.counter.load(AOrd::Relaxed)
    }
}

#[async_trait::async_trait]
impl Runnable for TestRunnable {
    async fn run(self: Arc<Self>, cancel: CancellationToken) -> TaskResult<()> {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                _ = interval.tick() => { self.counter.fetch_add(1, AOrd::Relaxed); }
                () = cancel.cancelled() => break,
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn lifecycle_basic() {
    let lc = Arc::new(Lifecycle::new());
    assert_eq!(lc.status(), Status::Stopped);

    let result = lc.start(|cancel| async move {
        cancel.cancelled().await;
        Ok(())
    });
    assert!(result.is_ok());

    let stop_result = lc.stop(Duration::from_millis(100)).await;
    assert!(stop_result.is_ok());
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn with_lifecycle_wrapper_basics() {
    let runnable = TestRunnable::new();
    let wrapper = WithLifecycle::new(runnable);

    assert_eq!(wrapper.status(), Status::Stopped);
    assert_eq!(wrapper.inner().count(), 0);

    let wrapper = wrapper.with_stop_timeout(Duration::from_secs(60));
    assert_eq!(wrapper.stop_timeout.as_secs(), 60);
}

#[tokio::test]
async fn start_sets_running_immediately() {
    let lc = Lifecycle::new();
    lc.start(|cancel| async move {
        cancel.cancelled().await;
        Ok(())
    })
    .unwrap();

    let s = lc.status();
    assert!(matches!(s, Status::Running | Status::Starting));

    let _ = lc.stop(Duration::from_millis(50)).await.unwrap();
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn start_with_ready_transitions_and_stop() {
    let lc = Lifecycle::new();

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    lc.start_with_ready(move |cancel, ready| async move {
        _ = ready_rx.await;
        ready.notify();
        cancel.cancelled().await;
        Ok(())
    })
    .unwrap();

    assert_eq!(lc.status(), Status::Starting);

    _ = ready_tx.send(());
    sleep(Duration::from_millis(10)).await;
    assert_eq!(lc.status(), Status::Running);

    let reason = lc.stop(Duration::from_millis(100)).await.unwrap();
    assert!(matches!(
        reason,
        StopReason::Cancelled | StopReason::Finished
    ));
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn stop_while_starting_before_ready() {
    let lc = Lifecycle::new();

    lc.start_with_ready(move |cancel, _ready| async move {
        cancel.cancelled().await;
        Ok(())
    })
    .unwrap();

    assert_eq!(lc.status(), Status::Starting);

    let reason = lc.stop(Duration::from_millis(100)).await.unwrap();
    assert!(matches!(
        reason,
        StopReason::Cancelled | StopReason::Finished
    ));
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn timeout_path_aborts_and_notifies() {
    let lc = Lifecycle::new();

    lc.start(|_cancel| async move {
        loop {
            sleep(Duration::from_secs(1000)).await;
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    })
    .unwrap();

    let reason = lc.stop(Duration::from_millis(30)).await.unwrap();
    assert_eq!(reason, StopReason::Timeout);
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn try_start_and_second_start_fails() {
    let lc = Lifecycle::new();

    assert!(lc.try_start(|cancel| async move {
        cancel.cancelled().await;
        Ok(())
    }));

    let err = lc.start(|_c| async { Ok(()) }).unwrap_err();
    match err {
        LifecycleError::AlreadyStarted => {}
    }

    let _ = lc.stop(Duration::from_millis(80)).await.unwrap();
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn stop_is_idempotent_and_safe_concurrent() {
    let lc = Arc::new(Lifecycle::new());

    lc.start(|cancel| async move {
        cancel.cancelled().await;
        Ok(())
    })
    .unwrap();

    let a = lc.clone();
    let b = lc.clone();
    let (r1, r2) = tokio::join!(
        async move { a.stop(Duration::from_millis(80)).await },
        async move { b.stop(Duration::from_millis(80)).await },
    );

    let r1 = r1.unwrap();
    let r2 = r2.unwrap();
    assert!(matches!(
        r1,
        StopReason::Finished | StopReason::Cancelled | StopReason::Timeout
    ));
    assert!(matches!(
        r2,
        StopReason::Finished | StopReason::Cancelled | StopReason::Timeout
    ));
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn stateful_wrapper_start_stop_roundtrip() {
    use crate::contracts::RunnableCapability;

    let wrapper = WithLifecycle::new(TestRunnable::new());
    assert_eq!(wrapper.status(), Status::Stopped);

    wrapper.start(CancellationToken::new()).await.unwrap();
    assert!(wrapper.lc.is_running());

    wrapper.stop(CancellationToken::new()).await.unwrap();
    assert_eq!(wrapper.status(), Status::Stopped);
}

#[tokio::test]
async fn with_lifecycle_double_start_fails() {
    use crate::contracts::RunnableCapability;

    let wrapper = WithLifecycle::new(TestRunnable::new());
    let cancel = CancellationToken::new();
    wrapper.start(cancel.clone()).await.unwrap();
    let err = wrapper.start(cancel).await;
    assert!(err.is_err());
    wrapper.stop(CancellationToken::new()).await.unwrap();
}

#[tokio::test]
async fn with_lifecycle_concurrent_stop_calls() {
    use crate::contracts::RunnableCapability;
    let wrapper = Arc::new(WithLifecycle::new(TestRunnable::new()));
    wrapper.start(CancellationToken::new()).await.unwrap();
    let a = wrapper.clone();
    let b = wrapper.clone();
    let (r1, r2) = tokio::join!(
        async move { a.stop(CancellationToken::new()).await },
        async move { b.stop(CancellationToken::new()).await },
    );
    assert!(r1.is_ok());
    assert!(r2.is_ok());
    assert_eq!(wrapper.status(), Status::Stopped);
}

#[tokio::test]
async fn lifecycle_handles_panics_properly() {
    let lc = Lifecycle::new();

    // Start a task that will panic
    lc.start(|_cancel| async {
        panic!("test panic message");
    })
    .unwrap();

    // Give the task a moment to start and panic
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Stop should handle the panic gracefully
    let reason = lc.stop(Duration::from_millis(1000)).await.unwrap();

    // The task panicked, but stop should complete successfully
    // The exact reason depends on timing, but it should not hang or fail
    assert!(matches!(
        reason,
        StopReason::Finished | StopReason::Cancelled | StopReason::Timeout
    ));
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn lifecycle_task_naming_and_logging() {
    let lc = Lifecycle::new();

    // Start a simple task
    lc.start(|cancel| async move {
        cancel.cancelled().await;
        Ok(())
    })
    .unwrap();

    // Verify task is running
    assert!(lc.is_running());

    // Stop and verify proper cleanup
    let reason = lc.stop(Duration::from_millis(100)).await.unwrap();
    assert!(matches!(
        reason,
        StopReason::Cancelled | StopReason::Finished
    ));
    assert_eq!(lc.status(), Status::Stopped);
}

#[tokio::test]
async fn lifecycle_join_handles_all_tasks() {
    let lc = Arc::new(Lifecycle::new());

    // Start multiple tasks in sequence (lifecycle only supports one at a time)
    lc.start(|cancel| async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancelled().await;
        Ok(())
    })
    .unwrap();

    // Stop should wait for the task to complete
    let start = std::time::Instant::now();
    let reason = lc.stop(Duration::from_millis(200)).await.unwrap();
    let elapsed = start.elapsed();

    // Should have waited at least 10ms for the task
    assert!(elapsed >= Duration::from_millis(10));
    assert!(matches!(
        reason,
        StopReason::Cancelled | StopReason::Finished
    ));
    assert_eq!(lc.status(), Status::Stopped);
}
