//! Request and response body types.

use std::future::Future as _;
use std::pin::Pin;
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
    Full::new(bytes).map_err(|never| match never {}).boxed_unsync()
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
pub(crate) type OnEnd = Box<dyn FnOnce() + Send>;

/// Upstream response body with an optional per-frame idle timeout.
///
/// An HTTP/1.1 connection goes back to the pool only when the body was read
/// to its end; dropping the body earlier (or a read error) closes the
/// connection instead. Dropping an HTTP/2 body resets just its stream.
pub struct ResponseBody {
    inner: Incoming,
    idle: Option<Duration>,
    deadline: Option<Pin<Box<Sleep>>>,
    on_end: Option<OnEnd>,
}

impl ResponseBody {
    pub(crate) fn new(inner: Incoming, idle: Option<Duration>, on_end: Option<OnEnd>) -> Self {
        let mut body = Self {
            inner,
            idle,
            deadline: None,
            on_end,
        };
        // Bodiless responses (HEAD, 204, 304, `Content-Length: 0`) are often
        // never polled: release right away.
        if body.inner.is_end_stream() {
            body.release();
        }
        body
    }

    fn release(&mut self) {
        if let Some(on_end) = self.on_end.take() {
            on_end();
        }
    }

    /// Unwrap into hyper's incoming body, dropping the idle timeout. The
    /// connection is then never returned to the pool.
    #[must_use]
    pub fn into_inner(self) -> Incoming {
        self.inner
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
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => {
                if let (Some(idle), Some(deadline)) = (this.idle, this.deadline.as_mut()) {
                    deadline.as_mut().reset(Instant::now() + idle);
                }
                match &frame {
                    None => this.release(),
                    Some(Ok(_)) if this.inner.is_end_stream() => this.release(),
                    Some(Err(_)) => this.on_end = None,
                    Some(Ok(_)) => {}
                }
                Poll::Ready(frame.map(|r| r.map_err(ProxyError::from_body)))
            }
            Poll::Pending => {
                let Some(idle) = this.idle else {
                    return Poll::Pending;
                };
                let deadline = this
                    .deadline
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(idle)));
                ready!(deadline.as_mut().poll(cx));
                Poll::Ready(Some(Err(ProxyError::new(
                    ErrorKind::ReadTimeout,
                    "no upstream body data within the read timeout",
                ))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl std::fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseBody")
            .field("idle", &self.idle)
            .finish_non_exhaustive()
    }
}
