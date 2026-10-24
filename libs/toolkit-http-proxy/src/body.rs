//! Request and response body types.

use std::future::Future as _;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use bytes::Bytes;
use futures_util::Stream;
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::Incoming;
use tokio::time::{Instant, Sleep};

use crate::error::{ErrorKind, ProxyError};
use crate::http2::H2Body;

/// Boxed error used for request bodies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Body type accepted by the engine for outbound requests.
///
/// `Send` but not `Sync`, so any `Stream` of chunks can be adapted without
/// extra synchronisation (see [`stream_body`]).
pub type ProxyBody = UnsyncBoxBody<Bytes, BoxError>;

/// An empty request body (framed as `Content-Length: 0` where applicable).
#[must_use]
pub fn empty_body() -> ProxyBody {
    Empty::new().map_err(|never| match never {}).boxed_unsync()
}

/// A fully buffered request body (framed with `Content-Length`).
#[must_use]
pub fn full_body(bytes: Bytes) -> ProxyBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
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

/// Runs once when a response body reached its clean end (returns an
/// HTTP/1.1 connection to the pool).
pub type OnEnd = Box<dyn FnOnce() + Send>;

/// Kept alive while an exchange runs (e.g. the HTTP/2 active-stream count,
/// shared with the upload task).
pub type Hold = Arc<dyn Send + Sync>;

enum Inner {
    H1(Incoming),
    H2(H2Body),
}

impl Inner {
    fn is_end_stream(&self) -> bool {
        match self {
            Self::H1(body) => body.is_end_stream(),
            Self::H2(body) => body.is_end_stream(),
        }
    }

    fn poll_frame(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ProxyError>>> {
        match self {
            Self::H1(body) => Pin::new(body)
                .poll_frame(cx)
                .map(|f| f.map(|r| r.map_err(ProxyError::from_body))),
            Self::H2(body) => body.poll_frame(cx),
        }
    }
}

/// Upstream response body with an optional per-frame idle timeout.
///
/// An HTTP/1.1 connection goes back to the pool only when the body was read
/// to its end; dropping the body earlier, a read error or an idle timeout
/// closes the connection instead. Dropping an HTTP/2 body resets just its
/// stream. An idle timeout is terminal: the error is reported once and the
/// body ends. HTTP/2 trailers are forwarded as trailer frames.
pub struct ResponseBody {
    inner: Option<Inner>,
    idle: Option<Duration>,
    deadline: Option<Pin<Box<Sleep>>>,
    on_end: Option<OnEnd>,
    hold: Option<Hold>,
}

impl ResponseBody {
    pub(crate) fn http1(inner: Incoming, idle: Option<Duration>, on_end: Option<OnEnd>) -> Self {
        Self::new(Inner::H1(inner), idle, on_end, None)
    }

    pub(crate) fn http2(inner: H2Body, idle: Option<Duration>, hold: Option<Hold>) -> Self {
        Self::new(Inner::H2(inner), idle, None, hold)
    }

    fn new(
        inner: Inner,
        idle: Option<Duration>,
        on_end: Option<OnEnd>,
        hold: Option<Hold>,
    ) -> Self {
        let mut body = Self {
            inner: Some(inner),
            idle,
            deadline: None,
            on_end,
            hold,
        };
        // Bodiless responses (HEAD, 204, 304, `Content-Length: 0`) are often
        // never polled: release right away.
        if body.inner.as_ref().is_some_and(Inner::is_end_stream) {
            body.release();
        }
        body
    }

    /// The exchange ended cleanly.
    fn release(&mut self) {
        self.hold = None;
        if let Some(on_end) = self.on_end.take() {
            on_end();
        }
    }

    /// Abandon the exchange: the connection (HTTP/1.1) is not reused.
    fn abandon(&mut self) {
        self.on_end = None;
        self.hold = None;
        self.inner = None;
        self.deadline = None;
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
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match inner.poll_frame(cx) {
            Poll::Ready(frame) => {
                if let (Some(idle), Some(deadline)) = (this.idle, this.deadline.as_mut()) {
                    deadline.as_mut().reset(Instant::now() + idle);
                }
                let end = inner.is_end_stream();
                match &frame {
                    None => this.release(),
                    Some(Ok(_)) if end => this.release(),
                    Some(Err(_)) => this.abandon(),
                    Some(Ok(_)) => {}
                }
                Poll::Ready(frame)
            }
            Poll::Pending => {
                let Some(idle) = this.idle else {
                    return Poll::Pending;
                };
                let deadline = this
                    .deadline
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(idle)));
                ready!(deadline.as_mut().poll(cx));
                this.abandon();
                Poll::Ready(Some(Err(ProxyError::new(
                    ErrorKind::ReadTimeout,
                    "no upstream body data within the read timeout",
                ))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_none_or(Inner::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            None => SizeHint::with_exact(0),
            Some(Inner::H1(body)) => body.size_hint(),
            Some(Inner::H2(_)) => SizeHint::default(),
        }
    }
}

impl std::fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseBody")
            .field("idle", &self.idle)
            .finish_non_exhaustive()
    }
}
