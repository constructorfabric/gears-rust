//! Lifecycle management for streamed (SSE) response bodies.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::unfold;
use oagw_sdk::body::{BodyStream, BoxError};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::domain::ports::metric_labels::StreamOutcome;

/// The signal that ends open SSE streams and WebSocket relays when the
/// gateway stops.
///
/// The streams opened under one gear start watch one token, a child of the
/// runtime's root token: a stop cancels it, and so does the runtime's own
/// cancellation, which comes first (the runtime drains HTTP before it calls
/// stop). A cancellation sticks for the streams of that start; the next start
/// gives the streams opened after it a fresh token. Each stream subscribes
/// with its own child of that token, so polling one stream touches no lock
/// shared with the others.
#[derive(Clone)]
pub(crate) struct StreamShutdown(Arc<Mutex<Tokens>>);

struct Tokens {
    /// What a stream opened now subscribes to.
    current: CancellationToken,
    /// Tokens a start replaced before a stop cancelled them (the one before
    /// the first start). The next stop cancels them too.
    replaced: Vec<CancellationToken>,
}

impl StreamShutdown {
    /// A signal that only [`Self::end_streams`] fires until the first
    /// [`Self::resume_streams`]. The gear builds it with
    /// [`Self::under`] instead.
    pub(crate) fn new() -> Self {
        Self::from_token(CancellationToken::new())
    }

    /// A signal whose streams opened before the first start also end when
    /// `parent` (the runtime's root token) is cancelled.
    pub(crate) fn under(parent: &CancellationToken) -> Self {
        Self::from_token(parent.child_token())
    }

    fn from_token(current: CancellationToken) -> Self {
        Self(Arc::new(Mutex::new(Tokens {
            current,
            replaced: Vec::new(),
        })))
    }

    /// The signal a new stream watches.
    pub(crate) fn subscribe(&self) -> StreamSignal {
        StreamSignal(self.0.lock().current.child_token())
    }

    /// End every stream open now (gear stop), whichever start it was opened
    /// under.
    pub(crate) fn end_streams(&self) {
        let mut tokens = self.0.lock();
        tokens.current.cancel();
        for token in tokens.replaced.drain(..) {
            token.cancel();
        }
    }

    /// Run the streams opened from now on under `parent` (gear start): they
    /// end when it is cancelled or at the next stop. A start does not end the
    /// streams opened before it: they keep their token, and the next stop
    /// ends them.
    pub(crate) fn resume_streams(&self, parent: &CancellationToken) {
        let mut tokens = self.0.lock();
        let earlier = std::mem::replace(&mut tokens.current, parent.child_token());
        if !earlier.is_cancelled() {
            tokens.replaced.push(earlier);
        }
    }
}

/// What one open stream watches to learn the gateway is stopping.
pub(crate) struct StreamSignal(CancellationToken);

impl StreamSignal {
    /// Resolves once the gateway stops, at once if it already has.
    /// Cancel-safe.
    pub(crate) async fn stopped(&self) {
        self.0.cancelled().await;
    }
}

#[cfg(test)]
impl From<CancellationToken> for StreamSignal {
    fn from(token: CancellationToken) -> Self {
        Self(token)
    }
}

/// Names a stream in its logs. No query string: it may carry credentials.
#[derive(Clone)]
pub(crate) struct StreamLabels {
    /// The upstream alias.
    pub(crate) alias: String,
    /// The path after the alias.
    pub(crate) path: String,
}

/// Logs an upstream body error at WARN and passes it on. The response head is
/// already sent, so the caller only sees the body abort.
pub(crate) fn log_upstream_body_error(inner: BodyStream, labels: StreamLabels) -> BodyStream {
    use futures_util::TryStreamExt as _;
    Box::pin(inner.inspect_err(move |e| {
        tracing::warn!(
            alias = %labels.alias,
            path = %labels.path,
            error = %e,
            "Upstream response body failed mid-stream"
        );
    }))
}

/// Wrap an SSE [`BodyStream`] with idle timeout and graceful shutdown
/// awareness.
///
/// Normal chunks are forwarded unchanged; the idle timer is reset on each
/// chunk. Upstream EOF ends the stream cleanly. A stop ends it with
/// [`oagw_sdk::StreamClosedByGateway`]; `idle_timeout` without a chunk ends
/// it with [`oagw_sdk::StreamIdleTimeout`]. Either error lets the caller tell
/// a cut stream from a finished one, and both release the upstream at once.
pub(crate) fn streaming_body_with_lifecycle(
    inner: BodyStream,
    idle_timeout: Duration,
    stopped: StreamSignal,
    labels: StreamLabels,
) -> BodyStream {
    struct State {
        /// `None` once a cut was reported: the upstream is released and the
        /// next poll ends the stream.
        inner: Option<BodyStream>,
        stopped: StreamSignal,
        deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
        idle_timeout: Duration,
        labels: StreamLabels,
    }

    Box::pin(unfold(
        State {
            inner: Some(inner),
            stopped,
            deadline: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
            labels,
        },
        |mut state| async move {
            let inner = state.inner.as_mut()?;
            let cut: BoxError = tokio::select! {
                biased;
                () = state.stopped.stopped() => {
                    tracing::debug!(
                        alias = %state.labels.alias,
                        path = %state.labels.path,
                        "SSE stream terminated by shutdown"
                    );
                    Box::new(oagw_sdk::StreamClosedByGateway::new())
                }
                // Polled before the deadline, which keeps running while a slow
                // reader holds back the next poll: a chunk the upstream
                // already sent wins over an expired deadline.
                item = inner.next() => {
                    let chunk = item?;
                    state
                        .deadline
                        .as_mut()
                        .reset(tokio::time::Instant::now() + state.idle_timeout);
                    return Some((chunk, state));
                }
                // cancel-safe: only borrows the pinned Sleep held in State;
                // dropping this future leaves the deadline intact.
                () = state.deadline.as_mut() => {
                    tracing::warn!(
                        alias = %state.labels.alias,
                        path = %state.labels.path,
                        idle_timeout = ?state.idle_timeout,
                        "SSE stream cut: the upstream sent nothing for the streaming idle timeout"
                    );
                    Box::new(oagw_sdk::StreamIdleTimeout::new())
                }
            };
            state.inner = None;
            Some((Err(cut), state))
        },
    ))
}

/// Call `on_end` once, when `inner` ends: [`StreamOutcome::Completed`] at
/// EOF or once `declared_len` bytes went through,
/// [`StreamOutcome::Shutdown`] when the gateway's stop cut it,
/// [`StreamOutcome::Error`] at any other first error (an idle cut included),
/// [`StreamOutcome::Cancelled`] when it is dropped before any of these (the
/// client went away).
///
/// `declared_len` is the response's `Content-Length`: hyper's HTTP/1.1
/// server drops such a body once it has written that many bytes, without
/// polling it for its end.
pub(crate) fn on_body_end<F>(inner: BodyStream, declared_len: Option<u64>, on_end: F) -> BodyStream
where
    F: FnOnce(StreamOutcome) + Send + 'static,
{
    Box::pin(BodyEnd {
        inner,
        remaining: declared_len,
        on_end: Some(on_end),
    })
}

struct BodyEnd<F: FnOnce(StreamOutcome)> {
    inner: BodyStream,
    /// Bytes still to come of a declared length.
    remaining: Option<u64>,
    on_end: Option<F>,
}

impl<F: FnOnce(StreamOutcome)> BodyEnd<F> {
    fn end(&mut self, outcome: StreamOutcome) {
        if let Some(on_end) = self.on_end.take() {
            on_end(outcome);
        }
    }
}

// `on_end` is never pinned: the struct moves freely.
impl<F: FnOnce(StreamOutcome)> Unpin for BodyEnd<F> {}

impl<F: FnOnce(StreamOutcome)> futures_util::Stream for BodyEnd<F> {
    type Item = Result<bytes::Bytes, BoxError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let item = std::task::ready!(self.inner.poll_next_unpin(cx));
        match &item {
            Some(Ok(chunk)) => {
                if let Some(remaining) = self.remaining.as_mut() {
                    *remaining = remaining.saturating_sub(chunk.len() as u64);
                    if *remaining == 0 {
                        self.end(StreamOutcome::Completed);
                    }
                }
            }
            Some(Err(e)) if e.is::<oagw_sdk::StreamClosedByGateway>() => {
                self.end(StreamOutcome::Shutdown);
            }
            Some(Err(_)) => self.end(StreamOutcome::Error),
            None => self.end(StreamOutcome::Completed),
        }
        std::task::Poll::Ready(item)
    }
}

impl<F: FnOnce(StreamOutcome)> Drop for BodyEnd<F> {
    fn drop(&mut self) {
        self.end(StreamOutcome::Cancelled);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::wake_flag::WakeFlag;
    use super::*;
    use bytes::Bytes;
    use futures_util::StreamExt;
    use oagw_sdk::body::BoxError;

    /// [`streaming_body_with_lifecycle`] with placeholder log labels.
    pub(crate) fn sse(
        inner: BodyStream,
        idle_timeout: Duration,
        stopped: StreamSignal,
    ) -> BodyStream {
        let labels = StreamLabels {
            alias: "test".to_owned(),
            path: "/sse".to_owned(),
        };
        streaming_body_with_lifecycle(inner, idle_timeout, stopped, labels)
    }

    fn bytes_stream(chunks: Vec<&'static str>) -> BodyStream {
        Box::pin(futures_util::stream::iter(
            chunks
                .into_iter()
                .map(|s| Ok(Bytes::from(s)) as Result<Bytes, BoxError>),
        ))
    }

    #[tokio::test]
    async fn sse_lifecycle_forwards_chunks() {
        let stop = CancellationToken::new();
        let stream = sse(
            bytes_stream(vec!["data: hello\n\n", "data: world\n\n"]),
            Duration::from_secs(60),
            stop.into(),
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
        let stop = CancellationToken::new();
        let stream = sse(
            bytes_stream(vec!["one"]),
            Duration::from_secs(60),
            stop.into(),
        );
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

    /// An idle cut ends the stream with an error, so a caller cannot take
    /// the truncated stream for a complete one, and releases the upstream.
    /// It is logged as a warning that names the stream.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn sse_lifecycle_idle_timeout_ends_stream() {
        let stop = CancellationToken::new();
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);

        // Send one chunk, then go silent.
        inner_tx
            .send(Ok(Bytes::from("data: first\n\n")))
            .await
            .unwrap();

        let mut stream = sse(
            channel_stream(inner_rx),
            Duration::from_millis(50),
            stop.into(),
        );
        assert_eq!(stream.next().await.unwrap().unwrap(), "data: first\n\n");
        match stream.next().await {
            Some(Err(e)) => assert!(
                e.downcast_ref::<oagw_sdk::StreamIdleTimeout>().is_some(),
                "not the idle error: {e}"
            ),
            other => panic!("expected the idle error, got {other:?}"),
        }
        assert!(inner_tx.is_closed(), "the upstream is still held");
        assert!(stream.next().await.is_none());
        logs_assert(|lines: &[&str]| {
            let cuts: Vec<_> = lines
                .iter()
                .filter(|l| l.contains("SSE stream cut"))
                .collect();
            match cuts[..] {
                [line]
                    if line.contains(" WARN ")
                        && line.contains("alias=test")
                        && line.contains("path=/sse")
                        && line.contains("idle_timeout=50ms") =>
                {
                    Ok(())
                }
                _ => Err(format!(
                    "no single idle-cut warning with its fields: {cuts:?}"
                )),
            }
        });
    }

    /// Each chunk restarts the idle timer: a stream silent past the first
    /// deadline but within the restarted one stays open, and the restarted
    /// deadline still cuts it.
    #[tokio::test(start_paused = true)]
    async fn sse_lifecycle_chunks_reset_the_idle_timer() {
        let start = tokio::time::Instant::now();
        let stop = CancellationToken::new();
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        let mut stream = sse(
            channel_stream(inner_rx),
            Duration::from_secs(10),
            stop.into(),
        );
        tokio::time::sleep(Duration::from_secs(6)).await;
        inner_tx
            .send(Ok(Bytes::from("data: tick\n\n")))
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "data: tick\n\n");

        // Polled and silent past t=10s, the deadline without the restart.
        let next = tokio::time::timeout(Duration::from_secs(8), stream.next()).await;
        assert!(next.is_err(), "cut before the restarted deadline: {next:?}");

        match stream.next().await {
            Some(Err(e)) if e.downcast_ref::<oagw_sdk::StreamIdleTimeout>().is_some() => {}
            other => panic!("expected the idle error, got {other:?}"),
        }
        assert_eq!(start.elapsed(), Duration::from_secs(16));
        drop(inner_tx);
    }

    /// An upstream body error is logged as a warning that names the stream,
    /// and passed on after the chunks before it.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn an_upstream_body_error_is_logged_and_passed_on() {
        let inner: BodyStream = Box::pin(futures_util::stream::iter([
            Ok(Bytes::from("data: first\n\n")),
            Err::<Bytes, BoxError>("connection reset".into()),
        ]));
        let labels = StreamLabels {
            alias: "test".to_owned(),
            path: "/sse".to_owned(),
        };
        let mut stream = log_upstream_body_error(inner, labels);

        assert_eq!(stream.next().await.unwrap().unwrap(), "data: first\n\n");
        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.to_string(), "connection reset");
        assert!(stream.next().await.is_none());
        assert!(logs_contain("WARN"));
        assert!(logs_contain("Upstream response body failed mid-stream"));
        assert!(logs_contain("alias=test"));
        assert!(logs_contain("path=/sse"));
        assert!(logs_contain("error=connection reset"));
    }

    /// A reader that polls late (client backpressure) still gets a chunk the
    /// upstream sent before the idle deadline passed, not the idle error.
    #[tokio::test(start_paused = true)]
    async fn sse_lifecycle_a_ready_chunk_beats_an_expired_idle_deadline() {
        let stop = CancellationToken::new();
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        let mut stream = sse(
            channel_stream(inner_rx),
            Duration::from_secs(10),
            stop.into(),
        );
        inner_tx
            .send(Ok(Bytes::from("data: ready\n\n")))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(stream.next().await.unwrap().unwrap(), "data: ready\n\n");
    }

    /// Polls `stream` once more within a second: the shutdown error, then
    /// the end of the stream.
    async fn assert_ends_with_shutdown_error(
        stream: &mut (impl futures_util::Stream<Item = Result<Bytes, BoxError>> + Unpin),
    ) {
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("the stream must not wait for the idle timeout or the upstream");
        assert_shutdown_error(next);
        assert!(stream.next().await.is_none());
    }

    /// `item` is the error a stop ends a stream with.
    pub(crate) fn assert_shutdown_error(item: Option<Result<Bytes, BoxError>>) {
        match item {
            Some(Err(e)) => assert!(
                e.downcast_ref::<oagw_sdk::StreamClosedByGateway>()
                    .is_some(),
                "not the shutdown error: {e}"
            ),
            other => panic!("expected the shutdown error, got {other:?}"),
        }
    }

    /// A shutdown cuts an open stream with an error, so the caller sees an
    /// aborted body, not a complete one.
    #[tokio::test]
    async fn sse_lifecycle_shutdown_ends_stream() {
        let streams = StreamShutdown::new();
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        inner_tx
            .send(Ok(Bytes::from("data: first\n\n")))
            .await
            .unwrap();
        let mut stream = sse(
            channel_stream(inner_rx),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        assert_eq!(stream.next().await.unwrap().unwrap(), "data: first\n\n");

        // Parked on the signal and the silent upstream when the stop comes.
        let flag = Arc::new(WakeFlag::default());
        flag.park(std::pin::pin!(stream.next()));
        streams.end_streams();
        assert!(flag.woken(), "the stop wakes the waiting stream");
        assert_ends_with_shutdown_error(&mut stream).await;
    }

    /// A stream opened after the stop ends at once: the stop is already
    /// active when it subscribes, and neither the 300 s idle timeout, the
    /// silent upstream nor a chunk the upstream already has may hold it open
    /// (#4987).
    #[tokio::test]
    async fn sse_lifecycle_ends_when_shutdown_was_already_signalled() {
        let streams = StreamShutdown::new();
        streams.end_streams();
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        let mut stream = sse(
            channel_stream(inner_rx),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap();
        assert_shutdown_error(next);
        // The upstream is released with the error, not at the next poll.
        assert!(inner_tx.is_closed(), "the upstream is still held");
        assert!(stream.next().await.is_none());

        let mut stream = sse(
            bytes_stream(vec!["data: late\n\n"]),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        assert_ends_with_shutdown_error(&mut stream).await;
    }

    /// A stop is sticky for the streams open at that moment: a start right
    /// after it, before they poll again, does not let them run on. Streams
    /// opened after the start run normally.
    #[tokio::test]
    async fn a_stop_followed_by_a_start_still_ends_the_open_streams() {
        let streams = StreamShutdown::new();
        let (_inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        let mut open = sse(
            channel_stream(inner_rx),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        streams.end_streams();
        streams.resume_streams(&CancellationToken::new());
        assert_ends_with_shutdown_error(&mut open).await;

        let mut later = sse(
            bytes_stream(vec!["data: after\n\n"]),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        assert_eq!(later.next().await.unwrap().unwrap(), "data: after\n\n");
        assert!(later.next().await.is_none());
    }

    /// `StreamShutdown` alone: a start with no stop before it leaves the
    /// open streams running, and the next stop ends them together with the
    /// streams of the new start. (The gear stops before a repeated start,
    /// see `gear_tests.rs`.)
    #[tokio::test]
    async fn a_start_leaves_open_streams_to_the_next_stop() {
        let streams = StreamShutdown::new();
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        let mut before = sse(
            channel_stream(inner_rx),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        streams.resume_streams(&CancellationToken::new());
        inner_tx
            .send(Ok(Bytes::from("data: on\n\n")))
            .await
            .unwrap();
        let next = tokio::time::timeout(Duration::from_secs(1), before.next()).await;
        assert_eq!(next.unwrap().unwrap().unwrap(), "data: on\n\n");

        let (_after_tx, after_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        let mut after = sse(
            channel_stream(after_rx),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        streams.end_streams();
        assert_ends_with_shutdown_error(&mut before).await;
        assert_ends_with_shutdown_error(&mut after).await;
    }

    /// Streams opened under [`StreamShutdown::under`] before the first start
    /// end when the parent is cancelled, also after a start replaced their
    /// token.
    #[tokio::test]
    async fn the_parent_ends_streams_opened_before_the_first_start() {
        let root = CancellationToken::new();
        let streams = StreamShutdown::under(&root);
        let (_inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);
        let mut open = sse(
            channel_stream(inner_rx),
            Duration::from_secs(300),
            streams.subscribe(),
        );
        streams.resume_streams(&root);
        root.cancel();
        assert_ends_with_shutdown_error(&mut open).await;
    }

    /// Each stream gets its own token: dropping one stream leaves the
    /// others' signal intact.
    #[tokio::test]
    async fn each_stream_watches_its_own_token() {
        let streams = StreamShutdown::new();
        let first = streams.subscribe();
        let second = streams.subscribe();
        drop(first);
        streams.end_streams();
        tokio::time::timeout(Duration::from_secs(1), second.stopped())
            .await
            .expect("the stop reaches the remaining stream");
    }

    /// `items`, then a body that never ends, wrapped in [`on_body_end`]; the
    /// outcomes it reports.
    fn ending_body(
        items: Vec<Result<Bytes, BoxError>>,
        declared_len: Option<u64>,
    ) -> (BodyStream, Arc<Mutex<Vec<StreamOutcome>>>) {
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&outcomes);
        let inner =
            Box::pin(futures_util::stream::iter(items).chain(futures_util::stream::pending()));
        let body = on_body_end(inner, declared_len, move |outcome| {
            recorded.lock().push(outcome);
        });
        (body, outcomes)
    }

    /// A body is complete once its `Content-Length` went through, before
    /// anyone polls it for its end: hyper drops it there. A longer body
    /// does not panic, a shorter one dropped early is cancelled.
    #[tokio::test]
    async fn a_declared_length_completes_the_body() {
        let (mut body, outcomes) = ending_body(
            vec![
                Ok(Bytes::from_static(b"hel")),
                Ok(Bytes::from_static(b"lo")),
            ],
            Some(5),
        );
        body.next().await.unwrap().unwrap();
        assert!(outcomes.lock().is_empty());
        body.next().await.unwrap().unwrap();
        drop(body);
        assert_eq!(*outcomes.lock(), [StreamOutcome::Completed]);

        let (mut body, outcomes) = ending_body(vec![Ok(Bytes::from_static(b"hello"))], Some(3));
        body.next().await.unwrap().unwrap();
        drop(body);
        assert_eq!(*outcomes.lock(), [StreamOutcome::Completed]);

        let (mut body, outcomes) = ending_body(vec![Ok(Bytes::from_static(b"hel"))], Some(5));
        body.next().await.unwrap().unwrap();
        drop(body);
        assert_eq!(*outcomes.lock(), [StreamOutcome::Cancelled]);
    }

    /// The gateway's stop ends a body as `shutdown`, not `error`; an idle
    /// cut and an upstream failure stay errors.
    #[tokio::test]
    async fn a_stop_is_not_an_error() {
        let cases: [(BoxError, StreamOutcome); 3] = [
            (
                Box::new(oagw_sdk::StreamClosedByGateway::new()),
                StreamOutcome::Shutdown,
            ),
            (
                Box::new(oagw_sdk::StreamIdleTimeout::new()),
                StreamOutcome::Error,
            ),
            ("upstream reset".into(), StreamOutcome::Error),
        ];
        for (err, expected) in cases {
            let (mut body, outcomes) = ending_body(vec![Err(err)], None);
            assert!(body.next().await.unwrap().is_err());
            drop(body);
            assert_eq!(*outcomes.lock(), [expected]);
        }
    }
}

/// Explicit wake-up checks for tests, independent of how the runtime
/// schedules tasks.
#[cfg(test)]
pub(crate) mod wake_flag {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Context;

    use futures_util::task::{ArcWake, waker};

    /// A waker that records whether it was woken. A test polls a future once
    /// with it ([`Self::park`]), fires a signal, and checks [`Self::woken`].
    #[derive(Default)]
    pub(crate) struct WakeFlag(AtomicBool);

    impl ArcWake for WakeFlag {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.0.store(true, Ordering::SeqCst);
        }
    }

    impl WakeFlag {
        /// Polls `fut` once with this flag's waker and asserts it is pending
        /// and has not woken it yet.
        pub(crate) fn park<F: Future + ?Sized>(self: &Arc<Self>, fut: Pin<&mut F>) {
            let waker = waker(Arc::clone(self));
            assert!(
                fut.poll(&mut Context::from_waker(&waker)).is_pending(),
                "the future must wait"
            );
            assert!(!self.woken(), "woken before the signal");
        }

        /// Whether the waker was woken since [`Self::park`].
        pub(crate) fn woken(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }
}
