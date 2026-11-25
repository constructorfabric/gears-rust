//! Request and response body types.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use http::HeaderName;
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::Incoming;

use crate::error::{ErrorKind, ProxyError};
use crate::http2::H2Body;
use crate::idle::IdleTimer;
use crate::pool::H1Conn;
use crate::sanitize;
use crate::timer::Timers;

/// Boxed error used for request bodies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Body type accepted by the engine for outbound requests.
///
/// `Send` but not `Sync`, so any `Stream` of chunks can be adapted without
/// extra synchronisation (see [`stream_body`]).
pub type ProxyBody = UnsyncBoxBody<Bytes, BoxError>;

/// An empty request body. On HTTP/1.1 it is framed as `Content-Length: 0`
/// for methods that define a meaning for a body (`POST`, `PUT`, `PATCH`,
/// ...) and sent without framing headers for the others.
#[must_use]
pub fn empty_body() -> ProxyBody {
    Empty::new().map_err(|never| match never {}).boxed_unsync()
}

/// A fully buffered request body (framed with `Content-Length`; an empty
/// one as described for [`empty_body`]).
#[must_use]
pub fn full_body(bytes: Bytes) -> ProxyBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// `body` with its trailers sanitized as request trailers
/// ([`sanitize::sanitize_request_trailers`]), also losing `hop`: the fields
/// the request head's `Connection` nominated (empty when it nominated none).
///
/// For a request that declares trailers. The size of a body that has not
/// ended is reported as unknown, so it is chunked on HTTP/1.1 and its
/// trailers are sent: hyper frames a body of exact size with
/// `Content-Length`, which carries no trailers. The request then has no
/// `Content-Length` on either protocol, and whether a declared trailer
/// arrives does not depend on the negotiated protocol.
pub fn with_sanitized_trailers(body: ProxyBody, hop: Arc<[HeaderName]>) -> ProxyBody {
    SanitizedTrailers { inner: body, hop }.boxed_unsync()
}

/// See [`with_sanitized_trailers`].
struct SanitizedTrailers {
    inner: ProxyBody,
    hop: Arc<[HeaderName]>,
}

impl Body for SanitizedTrailers {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        let mut frame = ready!(Pin::new(&mut this.inner).poll_frame(cx));
        if let Some(Ok(frame)) = &mut frame
            && let Some(trailers) = frame.trailers_mut()
        {
            sanitize::sanitize_request_trailers_without(trailers, &this.hop);
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    /// Unknown until the body has ended (see [`with_sanitized_trailers`]);
    /// the lower bound is kept.
    fn size_hint(&self) -> SizeHint {
        if self.inner.is_end_stream() {
            return self.inner.size_hint();
        }
        let mut hint = SizeHint::new();
        hint.set_lower(self.inner.size_hint().lower());
        hint
    }
}

/// A streaming request body of unknown length (chunked on HTTP/1.1, plain
/// DATA frames on HTTP/2).
pub fn stream_body<S, E>(stream: S) -> ProxyBody
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Into<BoxError> + 'static,
{
    use futures_util::TryStreamExt as _;
    StreamBody::new(stream.map_ok(Frame::data).map_err(Into::into)).boxed_unsync()
}

/// A streaming request body of exactly `len` bytes: framed with
/// `Content-Length` on HTTP/1.1 and a `content-length` field on HTTP/2,
/// where [`stream_body`] would be chunked.
///
/// A stream that ends short of `len` or yields a byte past it fails the body
/// with an [`ErrorKind::InvalidRequest`] error, which aborts the request: the
/// upstream never gets a complete message of the wrong length.
///
/// The body ends with the byte that completes `len`, without waiting for
/// the stream to end: a stream kept open after its last byte (a channel
/// whose sender lives on, an HTTP/2 client that sends `END_STREAM` late)
/// does not stall the request. Excess data is caught only if the stream has
/// it ready at that moment; the stream is polled once more without waiting,
/// then dropped. So the stream should end right after its last byte.
///
/// `len == 0` gets the same single look, here: HTTP/1.1 sends no body and
/// never polls one. Data or an error ready now fails the body; otherwise the
/// stream is dropped and the body is empty ([`empty_body`]).
pub fn sized_stream_body<S, E>(stream: S, len: u64) -> ProxyBody
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Into<BoxError> + 'static,
{
    let mut inner = Box::pin(stream);
    if len == 0 {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        return match peek_past_end(inner.as_mut(), &mut cx, 0) {
            Some(err) => failed_body(err),
            None => empty_body(),
        };
    }
    SizedStream {
        inner: Some(inner),
        len,
        remaining: len,
    }
    .boxed_unsync()
}

/// A body whose first frame is `err`. Its size is unknown, so HTTP/1.1
/// polls it (an exact size of zero would not be).
fn failed_body(err: BoxError) -> ProxyBody {
    stream_body(futures_util::stream::once(std::future::ready(Err::<
        Bytes,
        BoxError,
    >(err))))
}

/// The error for a body that yields a byte past its declared `len`.
fn too_long(len: u64) -> BoxError {
    Box::new(ProxyError::new(
        ErrorKind::InvalidRequest,
        format!("request body is longer than its declared length of {len} bytes"),
    ))
}

/// Most empty chunks a [`sized_stream_body`] skips in one go: a stream that
/// keeps yielding empty chunks must not spin its task.
const MAX_EMPTY_CHUNKS: usize = 32;

/// One look, without waiting, past the declared end of a stream of `len`
/// bytes: the error for data or a stream error already queued there, if any.
/// The look gives up after [`MAX_EMPTY_CHUNKS`] empty chunks, as if the
/// stream were not ready.
fn peek_past_end<S, E>(mut stream: Pin<&mut S>, cx: &mut Context<'_>, len: u64) -> Option<BoxError>
where
    S: Stream<Item = Result<Bytes, E>> + ?Sized,
    E: Into<BoxError>,
{
    for _ in 0..MAX_EMPTY_CHUNKS {
        match stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(data))) if data.is_empty() => {}
            Poll::Ready(Some(Ok(_))) => return Some(too_long(len)),
            Poll::Ready(Some(Err(e))) => return Some(e.into()),
            Poll::Ready(None) | Poll::Pending => return None,
        }
    }
    None
}

/// See [`sized_stream_body`]. `len > 0`.
struct SizedStream<S> {
    /// Dropped once the body has ended or failed: nothing more is polled,
    /// and the caller's stream is released without waiting for the request.
    inner: Option<Pin<Box<S>>>,
    len: u64,
    /// Bytes still owed.
    remaining: u64,
}

impl<S, E> Body for SizedStream<S>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        let mut empty = 0;
        loop {
            let Some(inner) = this.inner.as_mut() else {
                return Poll::Ready(None);
            };
            let item = ready!(inner.as_mut().poll_next(cx));
            let frame: Result<Frame<Bytes>, BoxError> = match item {
                Some(Ok(data)) if data.is_empty() => {
                    empty += 1;
                    if empty == MAX_EMPTY_CHUNKS {
                        // Yield to the executor; the stream is ready, so
                        // poll again right away.
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    continue;
                }
                Some(Ok(data)) => {
                    let n = data.len() as u64;
                    if n > this.remaining {
                        Err(too_long(this.len))
                    } else {
                        this.remaining -= n;
                        if this.remaining > 0 {
                            return Poll::Ready(Some(Ok(Frame::data(data))));
                        }
                        match peek_past_end(inner.as_mut(), cx, this.len) {
                            Some(err) => Err(err),
                            None => Ok(Frame::data(data)),
                        }
                    }
                }
                Some(Err(e)) => Err(e.into()),
                None if this.remaining > 0 => Err(Box::new(ProxyError::new(
                    ErrorKind::InvalidRequest,
                    format!(
                        "request body ended after {} of its declared {} bytes",
                        this.len - this.remaining,
                        this.len
                    ),
                ))),
                None => {
                    this.inner = None;
                    return Poll::Ready(None);
                }
            };
            // The last byte, or a failure: the body ends here.
            this.inner = None;
            return Poll::Ready(Some(frame));
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        if self.inner.is_none() {
            return SizeHint::with_exact(0);
        }
        SizeHint::with_exact(self.remaining)
    }
}

/// Request extension holding a copy of the request body, so the engine may
/// send the request again after a failure that provably happened before the
/// upstream processed it (see [`crate::ProxyClient::send`]). The bytes must
/// be exactly the request's body; cloning [`Bytes`] does not copy data.
/// Requests with an empty body are resendable without it. For a streamed
/// body, [`crate::replay_tee`] keeps a bounded copy instead.
///
/// The copy holds data only. Do not attach it to a body that ends with
/// trailers: a resend would go out without them. A request that declares
/// trailers (`Trailer`) is never resent from the copy.
#[derive(Debug, Clone)]
pub struct ReplayableBody(pub Bytes);

/// Takes the HTTP/1.1 connection once its response body reached its clean
/// end (returns it to the pool).
pub type OnEnd = Box<dyn FnOnce(H1Conn) + Send>;

/// Kept alive while an exchange runs (e.g. the HTTP/2 active-stream count,
/// shared with the upload task).
pub type Hold = Arc<dyn Send + Sync>;

enum Inner {
    /// The body and, until the body ends, the connection it arrives on: the
    /// body drives it (see [`H1Conn::drive`]).
    H1 {
        body: Incoming,
        conn: Option<H1Conn>,
        /// Fields the response head's `Connection` nominated: the trailers
        /// lose them too. (HTTP/2 has no `Connection` field.)
        hop: Box<[HeaderName]>,
    },
    H2(H2Body),
}

impl Inner {
    fn is_end_stream(&self) -> bool {
        match self {
            Self::H1 { body, .. } => body.is_end_stream(),
            Self::H2(body) => body.is_end_stream(),
        }
    }

    fn hop(&self) -> &[HeaderName] {
        match self {
            Self::H1 { hop, .. } => hop,
            Self::H2(_) => &[],
        }
    }

    fn poll_frame(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ProxyError>>> {
        match self {
            Self::H1 { body, conn, .. } => {
                if let Some(conn) = conn {
                    conn.drive(cx);
                }
                Pin::new(body)
                    .poll_frame(cx)
                    .map(|f| f.map(|r| r.map_err(ProxyError::from_body)))
            }
            Self::H2(body) => body.poll_frame(cx),
        }
    }
}

type FrameResult = Option<Result<Frame<Bytes>, ProxyError>>;

/// Most upstream frames merged in one poll: bounds the work a burst of tiny
/// frames costs before the caller gets data (tokio's own per-task budget is
/// 128 operations; h2's queued frames do not consume it).
const MERGE_MAX_FRAMES: usize = 128;

/// What a response body takes from the client's configuration.
pub struct BodySettings {
    /// Per-frame idle timeout (`read_timeout`).
    pub idle: Option<Duration>,
    /// The client's timer service, which runs the idle timeout.
    pub timers: Timers,
    /// `response_merge_limit`.
    pub merge_limit: usize,
}

/// Upstream response body with an optional per-frame idle timeout.
///
/// An HTTP/1.1 connection goes back to the pool only when the body was read
/// to its end; dropping the body earlier, a read error or an idle timeout
/// closes the connection instead. Dropping an HTTP/2 body resets just its
/// stream. An idle timeout is terminal: the error is reported once and the
/// body ends. HTTP/2 trailers are forwarded as trailer frames.
///
/// Small chunks or DATA frames that have already arrived are merged into
/// one frame of up to
/// [`response_merge_limit`](crate::ProxyClientConfig::response_merge_limit)
/// bytes (at most 128 of them per frame); a larger upstream frame is handed
/// out as is. A burst (an event stream catching up, say) then costs the
/// caller far fewer writes than one per chunk. Nothing waits for more data,
/// so no byte is delayed, and the bytes, the trailers and the error, if any,
/// come in the same order. Chunk and frame boundaries carry no meaning in
/// HTTP (RFC 9112 §7.1, RFC 9113 §8.1).
pub struct ResponseBody {
    inner: Option<Inner>,
    idle: Option<Duration>,
    timers: Timers,
    merge_limit: usize,
    /// Armed on the first wait; frames only record progress.
    deadline: Option<IdleTimer>,
    on_end: Option<OnEnd>,
    hold: Option<Hold>,
    /// What ended the last merge (a data frame that did not fit, trailers
    /// or an error), handed out next. Rare, so boxed to keep the body small.
    stash: Option<Box<Result<Frame<Bytes>, ProxyError>>>,
}

impl ResponseBody {
    /// `conn` is returned through `on_end` at the clean end of the body, or
    /// dropped (closed) without one, or earlier on any failure.
    /// `hop` lists the fields the response head's `Connection` nominated.
    pub(crate) fn http1(
        body: Incoming,
        conn: H1Conn,
        settings: BodySettings,
        on_end: Option<OnEnd>,
        hop: Vec<HeaderName>,
    ) -> Self {
        let inner = Inner::H1 {
            body,
            conn: Some(conn),
            hop: hop.into_boxed_slice(),
        };
        Self::new(inner, settings, on_end, None)
    }

    /// An HTTP/2 response body; `hold` keeps the exchange counted on its
    /// connection until the body ends or is dropped.
    pub(crate) fn http2(inner: H2Body, settings: BodySettings, hold: Option<Hold>) -> Self {
        Self::new(Inner::H2(inner), settings, None, hold)
    }

    fn new(
        inner: Inner,
        settings: BodySettings,
        on_end: Option<OnEnd>,
        hold: Option<Hold>,
    ) -> Self {
        let mut body = Self {
            inner: Some(inner),
            idle: settings.idle,
            timers: settings.timers,
            merge_limit: settings.merge_limit,
            deadline: None,
            on_end,
            hold,
            stash: None,
        };
        // Bodiless responses (HEAD, 204, 304, `Content-Length: 0`) are often
        // never polled: release right away.
        if body.inner.as_ref().is_some_and(Inner::is_end_stream) {
            body.release();
        }
        body
    }

    /// Replace the per-frame idle timeout, which defaults to the client's
    /// `read_timeout`; `None` disables it. For long-lived streams (e.g.
    /// server-sent events) whose caller enforces its own idle policy.
    pub fn set_idle_timeout(&mut self, limit: Option<Duration>) {
        self.idle = limit;
        self.deadline = None;
    }

    /// The exchange ended cleanly.
    fn release(&mut self) {
        self.hold = None;
        let on_end = self.on_end.take();
        if let Some(Inner::H1 { conn, .. }) = self.inner.as_mut()
            && let Some(conn) = conn.take()
        {
            match on_end {
                Some(on_end) => on_end(conn),
                None => drop(conn),
            }
        }
    }

    /// Abandon the exchange: the connection (HTTP/1.1) is not reused.
    fn abandon(&mut self) {
        self.on_end = None;
        self.hold = None;
        self.inner = None;
        self.deadline = None;
    }

    fn upstream_ended(&self) -> bool {
        self.inner.as_ref().is_none_or(Inner::is_end_stream)
    }

    /// The next upstream frame, without the idle timeout. Trailers are
    /// sanitized here, so every path out (merged or not) gets them clean.
    fn poll_upstream(&mut self, cx: &mut Context<'_>) -> Poll<FrameResult> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };
        let mut frame = ready!(inner.poll_frame(cx));
        if let Some(Ok(f)) = frame.as_mut()
            && let Some(trailers) = f.trailers_mut()
        {
            sanitize::sanitize_trailers_without(trailers, inner.hop());
        }
        let end = inner.is_end_stream();
        if let Some(deadline) = self.deadline.as_mut() {
            deadline.touch();
        }
        match &frame {
            None => self.release(),
            Some(Ok(_)) if end => self.release(),
            Some(Err(_)) => self.abandon(),
            Some(Ok(_)) => {}
        }
        Poll::Ready(frame)
    }

    /// `first` followed by the data frames that are already available, up to
    /// the merge limit in bytes and [`MERGE_MAX_FRAMES`] frames. A frame that
    /// ends the merge is stashed for the next poll; the end of the body needs
    /// no stash (the upstream body keeps reporting it). Waiting ends the merge
    /// too, without arming the idle timeout: the caller gets data now and
    /// polls again.
    ///
    /// The frames are gathered first and copied once into a buffer of the
    /// exact size: growing one buffer frame by frame would allocate every
    /// size class on the way and leave the allocator holding them.
    fn merge_ready(&mut self, first: Bytes, cx: &mut Context<'_>) -> Bytes {
        let mut more_frames: Vec<Bytes> = Vec::new();
        let mut len = first.len();
        while more_frames.len() + 1 < MERGE_MAX_FRAMES
            && len < self.merge_limit
            && !self.upstream_ended()
        {
            let Poll::Ready(next) = self.poll_upstream(cx) else {
                break;
            };
            match next {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(more) if len + more.len() <= self.merge_limit => {
                        len += more.len();
                        more_frames.push(more);
                    }
                    Ok(more) => {
                        self.stash = Some(Box::new(Ok(Frame::data(more))));
                        break;
                    }
                    Err(frame) => {
                        self.stash = Some(Box::new(Ok(frame)));
                        break;
                    }
                },
                Some(Err(err)) => {
                    self.stash = Some(Box::new(Err(err)));
                    break;
                }
                None => break,
            }
        }
        if more_frames.is_empty() {
            return first;
        }
        let mut merged = BytesMut::with_capacity(len);
        merged.extend_from_slice(&first);
        for more in &more_frames {
            merged.extend_from_slice(more);
        }
        merged.freeze()
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = ProxyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(next) = this.stash.take() {
            return Poll::Ready(Some(*next));
        }
        match this.poll_upstream(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                let frame = match frame.into_data() {
                    Ok(data) => Frame::data(this.merge_ready(data, cx)),
                    Err(frame) => frame,
                };
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(other) => Poll::Ready(other),
            Poll::Pending => {
                let Some(idle) = this.idle else {
                    return Poll::Pending;
                };
                let timers = &this.timers;
                let deadline = this
                    .deadline
                    .get_or_insert_with(|| IdleTimer::new(idle, timers));
                ready!(deadline.poll_expired(cx));
                this.abandon();
                Poll::Ready(Some(Err(ProxyError::new(
                    ErrorKind::ReadTimeout,
                    "no upstream body data within the read timeout",
                ))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.stash.is_none() && self.upstream_ended()
    }

    fn size_hint(&self) -> SizeHint {
        let hint = match &self.inner {
            None => SizeHint::with_exact(0),
            Some(Inner::H1 { body, .. }) => body.size_hint(),
            Some(Inner::H2(_)) => SizeHint::default(),
        };
        // A stashed data frame is no longer counted by the upstream body.
        if let Some(Ok(frame)) = self.stash.as_deref()
            && let Some(data) = frame.data_ref()
        {
            return grown(&hint, data.len() as u64);
        }
        hint
    }
}

/// `hint` plus `n` bytes. Built afresh: raising the lower bound of an exact
/// hint before its upper bound would break `SizeHint`'s `lower <= upper`.
fn grown(hint: &SizeHint, n: u64) -> SizeHint {
    let mut out = SizeHint::new();
    out.set_lower(hint.lower().saturating_add(n));
    if let Some(upper) = hint.upper() {
        out.set_upper(upper.saturating_add(n));
    }
    out
}

impl std::fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseBody")
            .field("idle", &self.idle)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;

    #[test]
    fn a_stashed_frame_grows_an_exact_hint() {
        // An exact hint (a `Content-Length` body), including the
        // fully-consumed case where a stashed final frame is all that is left.
        for remaining in [0, 10] {
            let hint = grown(&SizeHint::with_exact(remaining), 5);
            assert_eq!(hint.exact(), Some(remaining + 5));
        }
        let open = grown(&SizeHint::default(), 5);
        assert_eq!((open.lower(), open.upper()), (5, None));
    }

    fn chunks(parts: &[&'static str]) -> impl Stream<Item = Result<Bytes, BoxError>> + Send {
        let items: Vec<Result<Bytes, BoxError>> = parts
            .iter()
            .map(|p| Ok(Bytes::from_static(p.as_bytes())))
            .collect();
        futures_util::stream::iter(items)
    }

    /// The body's data, or the message of the error that ended it.
    async fn drain(mut body: ProxyBody) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => out.extend_from_slice(&frame.into_data().unwrap_or_default()),
                Err(e) => {
                    let kind = e.downcast_ref::<ProxyError>().map(ProxyError::kind);
                    assert_eq!(kind, Some(ErrorKind::InvalidRequest), "{e}");
                    return Err(e.to_string());
                }
            }
        }
        assert!(body.is_end_stream());
        Ok(out)
    }

    #[tokio::test]
    async fn sized_stream_of_the_declared_length_is_exact() {
        let body = sized_stream_body(chunks(&["ab", "", "cde"]), 5);
        assert_eq!(body.size_hint().exact(), Some(5));
        assert!(!body.is_end_stream());
        assert_eq!(drain(body).await.as_deref(), Ok(&b"abcde"[..]));
    }

    #[tokio::test]
    async fn sized_stream_shorter_than_declared_fails() {
        let err = drain(sized_stream_body(chunks(&["ab", "c"]), 5)).await;
        let err = err.expect_err("short body must fail");
        assert!(
            err.contains("ended after 3 of its declared 5 bytes"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn sized_stream_longer_than_declared_fails() {
        // Within the chunk that crosses the length, and in a later chunk.
        for parts in [&["ab", "cdef"][..], &["abcde", "f"][..]] {
            let err = drain(sized_stream_body(chunks(parts), 5)).await;
            let err = err.expect_err("long body must fail");
            assert!(
                err.contains("longer than its declared length of 5"),
                "{err}"
            );
        }
    }

    #[tokio::test]
    async fn sized_stream_ends_with_its_last_byte() {
        // The stream stays open after the declared bytes (a channel whose
        // sender lives on): the body must not wait for its end.
        let open = chunks(&["ab", "cde"]).chain(futures_util::stream::pending());
        let body = sized_stream_body(open, 5);
        let drained = tokio::time::timeout(Duration::from_secs(1), drain(body)).await;
        assert_eq!(drained.expect("no stall").as_deref(), Ok(&b"abcde"[..]));
    }

    #[tokio::test]
    async fn sized_stream_of_zero_bytes_is_checked_up_front() {
        let ended = sized_stream_body(chunks(&[]), 0);
        assert!(ended.is_end_stream());
        assert_eq!(ended.size_hint().exact(), Some(0));

        // Data that is ready fails the body, which HTTP/1.1 then polls.
        let long = sized_stream_body(chunks(&["", "x"]), 0);
        assert_eq!(long.size_hint().exact(), None);
        let err = drain(long).await.expect_err("data in a zero-length body");
        assert!(err.contains("declared length of 0"), "{err}");

        // Not ready yet: an empty body all the same (`Content-Length: 0`),
        // and the stream is dropped.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Bytes, BoxError>>();
        let body = sized_stream_body(UnboundedReceiverStream(rx), 0);
        assert!(body.is_end_stream());
        assert_eq!(body.size_hint().exact(), Some(0));
        assert!(tx.is_closed());
    }

    /// A channel as a stream; tests check its sender for a dropped receiver.
    struct UnboundedReceiverStream(tokio::sync::mpsc::UnboundedReceiver<Result<Bytes, BoxError>>);

    impl Stream for UnboundedReceiverStream {
        type Item = Result<Bytes, BoxError>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.0.poll_recv(cx)
        }
    }

    #[tokio::test]
    async fn sized_stream_drops_the_stream_at_its_end() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Bytes, BoxError>>();
        let mut body = sized_stream_body(UnboundedReceiverStream(rx), 3);
        tx.send(Ok(Bytes::from_static(b"ab")))
            .expect("receiver alive");
        assert!(body.frame().await.is_some_and(|f| f.is_ok()));
        assert!(!tx.is_closed());
        tx.send(Ok(Bytes::from_static(b"c")))
            .expect("receiver alive");
        assert!(body.frame().await.is_some_and(|f| f.is_ok()));
        // The sender lives on, yet the body has ended and let the stream go.
        assert!(body.is_end_stream());
        assert!(tx.is_closed());
    }

    #[tokio::test]
    async fn sized_stream_bounds_runs_of_empty_chunks() {
        let empties = || futures_util::stream::repeat_with(|| Ok::<_, BoxError>(Bytes::new()));

        // Past the declared end, and for a zero length up front: endless
        // empty chunks end the look as if the stream were not ready.
        let body = sized_stream_body(empties(), 0);
        assert!(body.is_end_stream());
        let tail = chunks(&["abc"]).chain(empties());
        let drained =
            tokio::time::timeout(Duration::from_secs(1), drain(sized_stream_body(tail, 3)));
        assert_eq!(drained.await.expect("no spin").as_deref(), Ok(&b"abc"[..]));

        // Within the body: the task yields and is woken to read on.
        let mut endless = sized_stream_body(empties(), 3);
        let first = futures_util::poll!(endless.frame());
        assert!(first.is_pending());
        let head = empties()
            .take(10 * MAX_EMPTY_CHUNKS)
            .chain(chunks(&["abc"]));
        let drained =
            tokio::time::timeout(Duration::from_secs(1), drain(sized_stream_body(head, 3)));
        assert_eq!(drained.await.expect("woken").as_deref(), Ok(&b"abc"[..]));
    }

    #[tokio::test]
    async fn sized_stream_passes_on_stream_errors() {
        let items: Vec<Result<Bytes, BoxError>> = vec![
            Ok(Bytes::from_static(b"ab")),
            Err("client went away".into()),
        ];
        let mut body = sized_stream_body(futures_util::stream::iter(items), 5);
        assert!(body.frame().await.is_some_and(|f| f.is_ok()));
        let err = body.frame().await.and_then(Result::err);
        assert_eq!(
            err.map(|e| e.to_string()).as_deref(),
            Some("client went away")
        );
        assert!(body.frame().await.is_none());
    }
}
