use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

impl WorkerHandles {
    pub(crate) fn len(&self) -> usize {
        self.handles.len()
    }
}

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
