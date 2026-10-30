//! Lifecycle management for streamed (SSE) response bodies.

use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::unfold;
use oagw_sdk::body::BodyStream;
use tokio::sync::watch;

/// Wrap a [`BodyStream`] with idle timeout and graceful shutdown awareness.
///
/// Applied to SSE responses so that long-lived streams are terminated when:
/// - No data is received from upstream within `idle_timeout`
/// - The server is shutting down (via `shutdown_rx`)
///
/// Normal chunks are forwarded unchanged; the idle timer is reset on each
/// chunk. Upstream EOF ends the stream cleanly.
pub(crate) fn streaming_body_with_lifecycle(
    inner: BodyStream,
    idle_timeout: Duration,
    shutdown_rx: watch::Receiver<bool>,
) -> BodyStream {
    struct State {
        inner: BodyStream,
        shutdown_rx: watch::Receiver<bool>,
        deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
        idle_timeout: Duration,
    }

    Box::pin(unfold(
        State {
            inner,
            shutdown_rx,
            deadline: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
        },
        |mut state| async move {
            loop {
                return tokio::select! {
                    biased;
                    result = state.shutdown_rx.changed() => {
                        // Err => sender dropped (shutdown). Ok + true => explicit signal.
                        // Both mean "stop streaming now".
                        if result.is_err() || *state.shutdown_rx.borrow() {
                            tracing::debug!("SSE stream terminated by shutdown");
                            None
                        } else {
                            // Spurious wake (value changed but still false) — re-enter select.
                            continue;
                        }
                    }
                    _ = &mut state.deadline => {
                        tracing::debug!("SSE stream idle timeout");
                        None
                    }
                    item = state.inner.next() => {
                        let chunk = item?;
                        state.deadline.as_mut().reset(
                            tokio::time::Instant::now() + state.idle_timeout
                        );
                        Some((chunk, state))
                    }
                };
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::StreamExt;
    use oagw_sdk::body::BoxError;

    fn bytes_stream(chunks: Vec<&'static str>) -> BodyStream {
        Box::pin(futures_util::stream::iter(
            chunks
                .into_iter()
                .map(|s| Ok(Bytes::from(s)) as Result<Bytes, BoxError>),
        ))
    }

    #[tokio::test]
    async fn sse_lifecycle_forwards_chunks() {
        let (_tx, rx) = watch::channel(false);
        let stream = streaming_body_with_lifecycle(
            bytes_stream(vec!["data: hello\n\n", "data: world\n\n"]),
            Duration::from_secs(60),
            rx,
        );
        let chunks: Vec<_> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "data: hello\n\n");
        assert_eq!(chunks[1], "data: world\n\n");
    }

    #[tokio::test]
    async fn sse_lifecycle_upstream_eof_ends_stream() {
        let (_tx, rx) = watch::channel(false);
        let stream =
            streaming_body_with_lifecycle(bytes_stream(vec!["one"]), Duration::from_secs(60), rx);
        let chunks: Vec<_> = stream.collect::<Vec<_>>().await;
        assert_eq!(chunks.len(), 1);
    }

    /// Build a BodyStream from an mpsc channel for testing async streams
    /// that need to be controlled from outside.
    fn channel_stream(rx: tokio::sync::mpsc::Receiver<Result<Bytes, BoxError>>) -> BodyStream {
        Box::pin(async_stream::stream! {
            let mut rx = rx;
            while let Some(item) = rx.recv().await {
                yield item;
            }
        })
    }

    #[tokio::test]
    async fn sse_lifecycle_idle_timeout_ends_stream() {
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);

        // Send one chunk, then go silent.
        inner_tx
            .send(Ok(Bytes::from("data: first\n\n")))
            .await
            .unwrap();

        let stream = streaming_body_with_lifecycle(
            channel_stream(inner_rx),
            Duration::from_millis(50),
            shutdown_rx,
        );
        let chunks: Vec<_> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // Should get the first chunk, then timeout ends the stream.
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "data: first\n\n");
    }

    #[tokio::test]
    async fn sse_lifecycle_shutdown_ends_stream() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);

        inner_tx
            .send(Ok(Bytes::from("data: first\n\n")))
            .await
            .unwrap();

        let stream = streaming_body_with_lifecycle(
            channel_stream(inner_rx),
            Duration::from_secs(60),
            shutdown_rx,
        );
        tokio::pin!(stream);

        // Read first chunk.
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first, "data: first\n\n");

        // Signal shutdown.
        shutdown_tx.send(true).unwrap();

        // Stream should end.
        let next = stream.next().await;
        assert!(next.is_none(), "stream should end after shutdown");
    }

    #[tokio::test]
    async fn sse_lifecycle_sender_drop_ends_stream() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (_inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);

        let stream = streaming_body_with_lifecycle(
            channel_stream(inner_rx),
            Duration::from_secs(60),
            shutdown_rx,
        );
        tokio::pin!(stream);

        // Drop the sender — production shutdown path.
        drop(shutdown_tx);

        // Stream should terminate promptly (not block on inner or idle timeout).
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        assert!(
            matches!(next, Ok(None)),
            "stream should end when shutdown sender is dropped"
        );
    }
}
