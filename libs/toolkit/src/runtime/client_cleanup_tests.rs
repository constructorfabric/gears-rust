use std::{
    future::{Future, ready},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use futures_util::task::noop_waker_ref;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::runtime::{ClientCleanup, ClientCleanupError, ClientCleanupFuture};

#[derive(Default)]
struct CompletionProbe {
    entered: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

struct DropSentinel(Arc<AtomicBool>);

impl Drop for DropSentinel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl CompletionProbe {
    fn completion(
        &self,
        gate: impl Future<Output = ()> + Send + 'static,
        result: Result<(), ClientCleanupError>,
    ) -> ClientCleanupFuture {
        let entered = self.entered.clone();
        let completed = self.completed.clone();
        let sentinel = DropSentinel(self.dropped.clone());
        Box::pin(async move {
            let _sentinel = sentinel;
            entered.fetch_add(1, Ordering::SeqCst);
            gate.await;
            completed.fetch_add(1, Ordering::SeqCst);
            result
        })
    }
}

#[test]
fn an_empty_cleanup_finishes_immediately() {
    let mut finish = Box::pin(ClientCleanup::empty().finish());
    let mut cx = Context::from_waker(noop_waker_ref());
    assert_eq!(finish.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
}

#[test]
fn finishing_cleanup_waits_for_completion_then_disarms_abort() {
    let probe = CompletionProbe::default();
    let abort = CancellationToken::new();
    let (release, gate) = oneshot::channel::<()>();
    let completion = probe.completion(
        async move {
            _ = gate.await;
        },
        Ok(()),
    );
    let cleanup = ClientCleanup::new(completion, abort.clone());
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, false),
    );
    let mut finish = Box::pin(cleanup.finish());
    let mut cx = Context::from_waker(noop_waker_ref());
    assert_eq!(finish.as_mut().poll(&mut cx), Poll::Pending);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (1, 0, false),
    );
    assert_eq!(release.send(()), Ok(()));
    assert_eq!(finish.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    drop(finish);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (1, 1, false),
    );
}

#[test]
fn a_worker_failure_is_returned_without_abort_after_completion() {
    let probe = CompletionProbe::default();
    let abort = CancellationToken::new();
    let completion = probe.completion(ready(()), Err(ClientCleanupError::WorkerFailed));
    let cleanup = ClientCleanup::new(completion, abort.clone());
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, false),
    );
    let mut finish = Box::pin(cleanup.finish());
    let mut cx = Context::from_waker(noop_waker_ref());
    assert_eq!(
        finish.as_mut().poll(&mut cx),
        Poll::Ready(Err(ClientCleanupError::WorkerFailed))
    );
    drop(finish);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (1, 1, false),
    );
}

#[test]
fn a_deadline_error_is_preserved_after_completion() {
    let probe = CompletionProbe::default();
    let abort = CancellationToken::new();
    let completion = probe.completion(ready(()), Err(ClientCleanupError::DeadlineExceeded));
    let cleanup = ClientCleanup::new(completion, abort.clone());
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, false),
    );
    let mut finish = Box::pin(cleanup.finish());
    let mut cx = Context::from_waker(noop_waker_ref());
    assert_eq!(
        finish.as_mut().poll(&mut cx),
        Poll::Ready(Err(ClientCleanupError::DeadlineExceeded))
    );
    drop(finish);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (1, 1, false),
    );
}

#[test]
fn dropping_cleanup_cancels_abort_without_polling_completion() {
    let probe = CompletionProbe::default();
    let abort = CancellationToken::new();
    let completion = probe.completion(ready(()), Ok(()));
    let cleanup = ClientCleanup::new(completion, abort.clone());
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, false),
    );
    drop(cleanup);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, true),
    );
}

#[test]
fn dropping_an_unpolled_finish_cancels_abort() {
    let probe = CompletionProbe::default();
    let abort = CancellationToken::new();
    let completion = probe.completion(ready(()), Ok(()));
    let cleanup = ClientCleanup::new(completion, abort.clone());
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, false),
    );
    let finish = cleanup.finish();
    drop(finish);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, true),
    );
}

#[test]
fn dropping_a_pending_finish_cancels_abort() {
    let probe = CompletionProbe::default();
    let abort = CancellationToken::new();
    let (release, gate) = oneshot::channel::<()>();
    let completion = probe.completion(
        async move {
            _ = gate.await;
        },
        Ok(()),
    );
    let cleanup = ClientCleanup::new(completion, abort.clone());
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (0, 0, false),
    );
    let mut finish = Box::pin(cleanup.finish());
    let mut cx = Context::from_waker(noop_waker_ref());
    assert_eq!(finish.as_mut().poll(&mut cx), Poll::Pending);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled()
        ),
        (1, 0, false),
    );
    let dropped_before = probe.dropped.load(Ordering::SeqCst);
    drop(finish);
    assert_eq!(
        (
            probe.entered.load(Ordering::SeqCst),
            probe.completed.load(Ordering::SeqCst),
            abort.is_cancelled(),
            (dropped_before, probe.dropped.load(Ordering::SeqCst))
        ),
        (1, 0, true, (false, true)),
    );
    drop(release);
}
