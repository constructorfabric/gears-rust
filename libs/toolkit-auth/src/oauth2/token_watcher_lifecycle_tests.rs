//! Deterministic lifecycle tests with a controlled token source and virtual time.
use super::*;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<FetchedToken, TokenError>>;

/// Create a source whose requests are completed explicitly by the test.
fn controlled(initial: Option<Arc<CachedToken>>) -> (TokenWatcher, mpsc::Receiver<Reply>) {
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
        initial,
    );
    (watcher, receiver)
}

/// Register an async waiter without allowing it to finish or advancing time.
async fn pending<F: Future>(mut future: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

/// Construct a valid token response for the controlled source.
fn fetched(value: &str) -> FetchedToken {
    FetchedToken {
        access_token: SecretString::new(value),
        lifetime_secs: 3600,
        freshness_ratio: 0.8,
    }
}

/// Construct a representative typed authentication failure.
fn rejected() -> TokenError {
    TokenError::HttpStatus {
        operation: super::super::error::HttpOperation::TokenRequest,
        status: http::StatusCode::UNAUTHORIZED,
    }
}

/// Lazy construction remains idle even when virtual time moves far ahead.
#[tokio::test(start_paused = true)]
async fn unused_watcher_never_fetches_and_drop_stops_task() {
    let (watcher, mut requests) = controlled(None);
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(watcher);
    assert!(requests.recv().await.is_none());
}

/// Concurrent waiters receive the same result with no duplicate acquisition.
#[tokio::test(start_paused = true)]
async fn concurrent_waiters_share_success() {
    let (watcher, mut requests) = controlled(None);
    let first = watcher.wait_ready();
    let second = watcher.wait_ready();
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
    assert_eq!(watcher.valid_token().unwrap().expose(), "shared");
    assert!(matches!(
        requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

/// A slow waiter retains its attempt's error even after a later success.
#[tokio::test(start_paused = true)]
async fn slow_waiter_keeps_original_failure_across_recovery() {
    let (watcher, mut requests) = controlled(None);
    let first = watcher.wait_ready();
    let slow = watcher.wait_ready();
    tokio::pin!(first, slow);
    pending(first.as_mut()).await;
    pending(slow.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    assert_eq!(
        first.await.unwrap_err().status(),
        Some(http::StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        watcher.valid_token().unwrap_err().status(),
        Some(http::StatusCode::UNAUTHORIZED)
    );
    let recovered = watcher.wait_ready();
    tokio::pin!(recovered);
    pending(recovered.as_mut()).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Ok(fetched("recovered")))
        .unwrap();
    assert_eq!(recovered.await.unwrap().expose(), "recovered");
    assert_eq!(
        slow.await.unwrap_err().status(),
        Some(http::StatusCode::UNAUTHORIZED)
    );
    assert_eq!(watcher.valid_token().unwrap().expose(), "recovered");
}

/// Repeated reads and invalidations cannot bypass the bounded initial backoff.
#[tokio::test(start_paused = true)]
async fn initial_retry_delays_are_one_to_thirty_seconds() {
    let (watcher, mut requests) = controlled(None);
    let first = watcher.wait_ready();
    tokio::pin!(first);
    pending(first.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    first.await.unwrap_err();
    for delay in [1, 2, 4, 8, 16, 30, 30] {
        let waiting = watcher.wait_ready();
        tokio::pin!(waiting);
        pending(waiting.as_mut()).await;
        assert!(watcher.start_if_pending());
        assert_eq!(
            watcher.valid_token().unwrap_err().status(),
            Some(http::StatusCode::UNAUTHORIZED)
        );
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
        waiting.await.unwrap_err();
    }
}

/// Dropping a waiter does not cancel the acquisition shared with another waiter.
#[tokio::test(start_paused = true)]
async fn cancelled_waiter_does_not_cancel_shared_request() {
    let (watcher, mut requests) = controlled(None);
    let mut cancelled = Box::pin(watcher.wait_ready());
    pending(cancelled.as_mut()).await;
    let reply = requests.recv().await.unwrap();
    drop(cancelled);
    let remaining = watcher.wait_ready();
    tokio::pin!(remaining);
    pending(remaining.as_mut()).await;
    assert!(!reply.is_closed());
    reply.send(Ok(fetched("shared"))).unwrap();
    assert_eq!(remaining.await.unwrap().expose(), "shared");
}

/// Drop cancels an in-flight fetch and closes its attempt channel.
#[tokio::test(start_paused = true)]
async fn drop_cancels_in_flight_request() {
    let (watcher, mut requests) = controlled(None);
    assert!(watcher.start_if_pending());
    let mut reply = requests.recv().await.unwrap();
    let mut attempt = watcher.attempt.load().as_ref().clone();
    drop(watcher);
    reply.closed().await;
    assert!(attempt.changed().await.is_err());
    assert!(requests.recv().await.is_none());
}

/// Drop during initial backoff prevents all subsequent attempts.
#[tokio::test(start_paused = true)]
async fn drop_during_backoff_stops_retries() {
    let (watcher, mut requests) = controlled(None);
    {
        let waiting = watcher.wait_ready();
        tokio::pin!(waiting);
        pending(waiting.as_mut()).await;
        requests
            .recv()
            .await
            .unwrap()
            .send(Err(rejected()))
            .unwrap();
        waiting.await.unwrap_err();
    }
    drop(watcher);
    assert!(requests.recv().await.is_none());
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert!(requests.recv().await.is_none());
}

/// Refresh preserves a valid token on failure and rejects it once expired.
#[tokio::test(start_paused = true)]
async fn refresh_failure_keeps_token_only_until_expiry() {
    let initial = Arc::new(CachedToken::new(SecretString::new("initial"), 4, 0.5).unwrap());
    let (watcher, mut requests) = controlled(Some(initial));
    assert_eq!(watcher.wait_ready().await.unwrap().expose(), "initial");
    tokio::time::advance(Duration::from_secs(2)).await;
    let mut attempt = watcher.attempt.load().as_ref().clone();
    requests
        .recv()
        .await
        .unwrap()
        .send(Err(rejected()))
        .unwrap();
    attempt.changed().await.unwrap();
    assert_eq!(watcher.valid_token().unwrap().expose(), "initial");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        watcher.valid_token(),
        Err(TokenError::Unavailable(_))
    ));
    let waiting = watcher.wait_ready();
    tokio::pin!(waiting);
    pending(waiting.as_mut()).await;
    requests
        .recv()
        .await
        .unwrap()
        .send(Ok(fetched("refreshed")))
        .unwrap();
    assert_eq!(waiting.await.unwrap().expose(), "refreshed");
}

/// Invalid lifetimes are published as attempt errors rather than cached tokens.
#[tokio::test(start_paused = true)]
async fn invalid_initial_token_is_reported_to_reads_and_waiters() {
    let (watcher, mut requests) = controlled(None);
    let waiting = watcher.wait_ready();
    tokio::pin!(waiting);
    pending(waiting.as_mut()).await;
    let mut invalid = fetched("invalid");
    invalid.lifetime_secs = 0;
    requests.recv().await.unwrap().send(Ok(invalid)).unwrap();
    assert!(matches!(
        waiting.await,
        Err(TokenError::InvalidTokenLifetime(_))
    ));
    assert!(matches!(
        watcher.valid_token(),
        Err(TokenError::InvalidTokenLifetime(_))
    ));
}
