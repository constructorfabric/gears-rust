use super::*;

#[tokio::test]
async fn stop_waits_for_handler_cleanup() {
    let cancel = CancellationToken::new();
    let child = cancel.clone();
    let (finished, observed) = tokio::sync::oneshot::channel();
    let mut task = AbortOnDrop(tokio::spawn(async move {
        child.cancelled().await;
        tokio::task::yield_now().await;
        finished.send(()).unwrap();
    }));
    assert!(task.stop(&cancel, 1).await);
    assert!(observed.await.is_ok());
}

#[tokio::test]
async fn uncooperative_handler_is_not_confirmed_stopped() {
    let mut task = AbortOnDrop(tokio::spawn(std::future::pending::<()>()));
    assert!(!task.stop(&CancellationToken::new(), 0).await);
    // Abort is requested, but shutdown must not await an unbounded join.
    assert!((&mut task.0).await.unwrap_err().is_cancelled());
}
