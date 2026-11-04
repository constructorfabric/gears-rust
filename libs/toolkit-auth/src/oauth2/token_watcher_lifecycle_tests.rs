//! Worker lifecycle tests using virtual time and explicit request completion.
use super::*;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<FetchedToken, TokenError>>;

/// Build a worker whose exchange responses are controlled by the test.
fn controlled() -> (TokenWatcher, mpsc::Receiver<Reply>) {
    let (requests, receiver) = mpsc::channel(1);
    let watcher = TokenWatcher::run_with_fetch(
        move || {
            let requests = requests.clone();
            async move {
                let (reply, response) = oneshot::channel();
                requests.send(reply).await.unwrap();
                response.await.unwrap()
            }
        },
        WatcherConfig::new(Duration::ZERO, Duration::from_secs(10)).unwrap(),
    );
    (watcher, receiver)
}

/// Register a future and assert it is awaiting an event.
async fn pending<F: Future>(mut future: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

/// A valid response with a long lifetime.
fn fetched(value: &str) -> FetchedToken {
    FetchedToken {
        access_token: SecretString::new(value),
        lifetime_secs: 3600,
        freshness_ratio: 0.8,
    }
}

/// A representative acquisition failure.
fn rejected() -> TokenError {
    TokenError::Unavailable("IdP unavailable".into())
}

/// Acquire an initial token through the same path used by production callers.
async fn acquire(watcher: &TokenWatcher, requests: &mut mpsc::Receiver<Reply>, value: &str) {
    let get = watcher.get();
    tokio::pin!(get);
    pending(get.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Ok(fetched(value)))
        .unwrap();
    assert_eq!(get.await.unwrap().expose(), value);
}

/// Construction makes no requests; final drop cancels idle work.
#[tokio::test(start_paused = true)]
async fn unused_worker_is_idle_and_drop_stops_it() {
    let (watcher, mut requests) = controlled();
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(watcher);
    assert!(requests.recv().await.is_none());
}

/// Two callers share the same initial exchange.
#[tokio::test(start_paused = true)]
async fn concurrent_gets_share_success() {
    let (watcher, mut requests) = controlled();
    let first = watcher.get();
    let second = watcher.get();
    tokio::pin!(first, second);
    pending(first.as_mut()).await;
    pending(second.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Ok(fetched("shared")))
        .unwrap();
    assert_eq!(first.await.unwrap().expose(), "shared");
    assert_eq!(second.await.unwrap().expose(), "shared");
    assert_eq!(watcher.get().await.unwrap().expose(), "shared");
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

/// A slow waiter retains its attempt's failure after a later retry succeeds.
#[tokio::test(start_paused = true)]
async fn slow_waiter_keeps_attempt_error() {
    let (watcher, mut requests) = controlled();
    let first = watcher.get();
    let slow = watcher.get();
    tokio::pin!(first, slow);
    pending(first.as_mut()).await;
    pending(slow.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    assert!(first.await.is_err());
    acquire(&watcher, &mut requests, "recovered").await;
    assert!(slow.await.is_err());
}

/// Initial retries remain bounded and gets/invalidations cannot bypass backoff.
#[tokio::test(start_paused = true)]
async fn initial_retry_delays_are_one_to_thirty_seconds() {
    let (watcher, mut requests) = controlled();
    {
        let first = watcher.get();
        tokio::pin!(first);
        pending(first.as_mut()).await;
        requests
            .recv()
            .await
            .unwrap()
            .send(Err(rejected()))
            .unwrap();
        first.await.unwrap_err();
    }
    for delay in [1, 2, 4, 8, 16, 30, 30] {
        let waiting = watcher.get();
        let invalidate = watcher.invalidate();
        tokio::pin!(waiting, invalidate);
        pending(waiting.as_mut()).await;
        pending(invalidate.as_mut()).await;
        let start = Instant::now();
        tokio::time::advance(
            Duration::from_secs(delay)
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
        )
        .await;
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        tokio::time::advance(Duration::from_millis(1)).await;
        requests
            .recv()
            .await
            .unwrap()
            .send(Err(rejected()))
            .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(delay));
        assert!(waiting.await.is_err());
        assert!(invalidate.await.is_err());
    }
}

/// Cancelling one get never cancels another caller's shared exchange.
#[tokio::test(start_paused = true)]
async fn cancelled_get_does_not_cancel_shared_exchange() {
    let (watcher, mut requests) = controlled();
    let mut abandoned = Box::pin(watcher.get());
    pending(abandoned.as_mut()).await;
    let reply = requests.recv().await.unwrap();
    drop(abandoned);
    let other = watcher.get();
    tokio::pin!(other);
    pending(other.as_mut()).await;
    assert!(!reply.is_closed());
    reply.send(Ok(fetched("shared"))).unwrap();
    assert_eq!(other.await.unwrap().expose(), "shared");
}

/// Concurrent invalidations and gets share replacement, and failure cannot restore the old token.
#[tokio::test(start_paused = true)]
async fn invalidations_share_replacement_and_discard_failed_credential() {
    let (watcher, mut requests) = controlled();
    acquire(&watcher, &mut requests, "old").await;
    let first = watcher.invalidate();
    let second = watcher.invalidate();
    let get = watcher.get();
    tokio::pin!(first, second, get);
    pending(first.as_mut()).await;
    pending(second.as_mut()).await;
    pending(get.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    assert!(first.await.is_err());
    assert!(second.await.is_err());
    assert!(get.await.is_err());
    assert!(watcher.current.load().is_none());
    acquire(&watcher, &mut requests, "replacement").await;
    assert_eq!(watcher.get().await.unwrap().expose(), "replacement");
}

/// Invalidation during a background fetch joins it and does not enqueue an extra fetch.
#[tokio::test(start_paused = true)]
async fn invalidation_joins_in_flight_refresh() {
    let (watcher, mut requests) = controlled();
    acquire(&watcher, &mut requests, "old").await;
    tokio::time::advance(Duration::from_mins(48)).await;
    let reply = requests.recv().await.unwrap();
    let invalidation = watcher.invalidate();
    tokio::pin!(invalidation);
    pending(invalidation.as_mut()).await;
    reply.send(Ok(fetched("new"))).unwrap();
    invalidation.await.unwrap();
    tokio::task::yield_now().await;
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(watcher.get().await.unwrap().expose(), "new");
}

/// Refresh failure retains a token until expiry; subsequent get waits for replacement.
#[tokio::test(start_paused = true)]
async fn expired_token_is_never_returned() {
    let (watcher, mut requests) = controlled();
    {
        let get = watcher.get();
        tokio::pin!(get);
        pending(get.as_mut()).await;
        let mut short = fetched("short");
        short.lifetime_secs = 4;
        short.freshness_ratio = 0.5;
        requests.recv().await.unwrap().send(Ok(short)).unwrap();
        get.await.unwrap();
    }
    tokio::time::advance(Duration::from_secs(2)).await;
    let mut attempt = watcher.control.lock().attempt.subscribe();
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    attempt.changed().await.unwrap();
    assert_eq!(watcher.get().await.unwrap().expose(), "short");
    tokio::time::advance(Duration::from_secs(2)).await;
    acquire(&watcher, &mut requests, "replacement").await;
}

/// Shutdown cancels in-flight acquisition, wakes all waiters and joins the worker.
#[tokio::test(start_paused = true)]
async fn shutdown_cancels_exchange_and_all_waiters() {
    let (watcher, mut requests) = controlled();
    let get = watcher.get();
    let invalidate = watcher.invalidate();
    tokio::pin!(get, invalidate);
    pending(get.as_mut()).await;
    pending(invalidate.as_mut()).await;
    let reply = requests.recv().await.unwrap();
    let ((), ()) = tokio::join!(watcher.shutdown(), watcher.shutdown());
    assert!(reply.is_closed());
    assert!(requests.recv().await.is_none());
    assert!(matches!(get.await, Err(TokenError::Shutdown)));
    assert!(matches!(invalidate.await, Err(TokenError::Shutdown)));
    assert!(matches!(watcher.get().await, Err(TokenError::Shutdown)));
    assert!(watcher.task.lock().await.is_none());
}

/// Shutdown during retry delay prevents the next exchange.
#[tokio::test(start_paused = true)]
async fn shutdown_during_backoff_stops_retries() {
    let (watcher, mut requests) = controlled();
    let get = watcher.get();
    tokio::pin!(get);
    pending(get.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    get.await.unwrap_err();
    watcher.shutdown().await;
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert!(requests.recv().await.is_none());
}

/// Shutdown rejects even a valid cached token, and repeated shutdown is harmless.
#[tokio::test(start_paused = true)]
async fn shutdown_rejects_cached_token() {
    let (watcher, mut requests) = controlled();
    acquire(&watcher, &mut requests, "valid").await;
    watcher.shutdown().await;
    watcher.shutdown().await;
    assert!(matches!(watcher.get().await, Err(TokenError::Shutdown)));
    assert!(matches!(
        watcher.invalidate().await,
        Err(TokenError::Shutdown)
    ));
}

/// Cancelling shutdown's wait preserves the join handle for the next caller.
#[tokio::test(start_paused = true)]
async fn shutdown_wait_is_cancellation_safe() {
    let (watcher, mut requests) = controlled();
    let mut shutdown = Box::pin(watcher.shutdown());
    pending(shutdown.as_mut()).await;
    drop(shutdown);
    watcher.shutdown().await;
    assert!(watcher.task.lock().await.is_none());
    assert!(requests.recv().await.is_none());
}

/// Drop remains a fallback when explicit shutdown was omitted.
#[tokio::test(start_paused = true)]
async fn drop_cancels_in_flight_request() {
    let (watcher, mut requests) = controlled();
    let mut get = Box::pin(watcher.get());
    pending(get.as_mut()).await;
    let mut reply = requests.recv().await.unwrap();
    drop(get);
    drop(watcher);
    reply.closed().await;
    assert!(requests.recv().await.is_none());
}

/// A waiter whose success was invalidated must join replacement rather than return it.
#[tokio::test(start_paused = true)]
async fn slow_success_waiter_cannot_return_invalidated_token() {
    let (watcher, mut requests) = controlled();
    let slow = watcher.get();
    tokio::pin!(slow);
    pending(slow.as_mut()).await;
    let mut attempt = watcher.control.lock().attempt.subscribe();
    requests
        .recv()
        .await
        .unwrap()
        .send(Ok(fetched("old")))
        .unwrap();
    attempt.changed().await.unwrap();
    let invalidate = watcher.invalidate();
    tokio::pin!(invalidate);
    pending(invalidate.as_mut()).await;
    pending(slow.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Ok(fetched("replacement")))
        .unwrap();
    invalidate.await.unwrap();
    assert_eq!(slow.await.unwrap().expose(), "replacement");
}

/// An outage beyond expiry uses the bounded acquisition schedule, not the long refresh cap.
#[tokio::test(start_paused = true)]
async fn expired_cache_retries_within_thirty_seconds() {
    let (watcher, mut requests) = controlled();
    {
        let get = watcher.get();
        tokio::pin!(get);
        pending(get.as_mut()).await;
        let mut short = fetched("short");
        short.lifetime_secs = 4;
        short.freshness_ratio = 0.5;
        requests.recv().await.unwrap().send(Ok(short)).unwrap();
        get.await.unwrap();
    }
    // First refresh fails while the cache is still usable. The next one
    // is scheduled at expiry rather than after the full refresh backoff.
    tokio::time::advance(Duration::from_secs(2)).await;
    let mut attempt = watcher.control.lock().attempt.subscribe();
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    attempt.changed().await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    let mut attempt = watcher.control.lock().attempt.subscribe();
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    attempt.changed().await.unwrap();

    // The second consecutive failure is already past expiry. A waiting
    // request must not inherit the configured 10-to-300-second refresh delays.
    for delay in [2, 4, 8, 16, 30, 30] {
        let get = watcher.get();
        tokio::pin!(get);
        pending(get.as_mut()).await;
        let start = Instant::now();
        let reply = requests.recv().await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(delay));
        reply.send(Err(rejected())).unwrap();
        get.await.unwrap_err();
    }
    acquire(&watcher, &mut requests, "recovered").await;
}
