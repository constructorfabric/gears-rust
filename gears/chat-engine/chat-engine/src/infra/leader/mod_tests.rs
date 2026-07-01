use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[tokio::test]
async fn noop_runs_work_directly() {
    let executed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&executed);

    let elector = NoopLeaderElector;
    let cancel = CancellationToken::new();

    let c = cancel.clone();
    let result = tokio::spawn(async move {
        elector
            .run_role(
                "test",
                c,
                work_fn(move |_cancel| {
                    let f = Arc::clone(&flag);
                    async move {
                        f.store(true, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            )
            .await
    })
    .await;

    assert!(result.is_ok());
    assert!(executed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn noop_respects_cancellation() {
    let elector = NoopLeaderElector;
    let cancel = CancellationToken::new();

    let c = cancel.clone();
    let handle = tokio::spawn(async move {
        elector
            .run_role(
                "test",
                c,
                work_fn(|cancel| async move {
                    cancel.cancelled().await;
                    Ok(())
                }),
            )
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cancel.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    assert!(result.is_ok());
}
