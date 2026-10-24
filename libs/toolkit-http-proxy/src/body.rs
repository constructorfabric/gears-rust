//! Request and response body types.

use std::future::Future as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

// ---------------------------------------------------------------------------
// Upload progress
// ---------------------------------------------------------------------------

/// How far the request body upload got; shared with the stall watchdog.
#[derive(Debug, Default)]
pub struct UploadProgress {
    done: AtomicBool,
    frames: AtomicU64,
}

impl UploadProgress {
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }
}

/// Largest data frame handed to the connection at once.
///
/// hyper's HTTP/2 pipe takes the next frame as soon as the stream has any
/// send capacity and parks it whole in the stream's send buffer. Bounding
/// frames keeps that parked amount small and, more importantly, makes the
/// pipe wait for flow-control capacity between frames, which is what lets
/// the stall watchdog see a zero-window upstream.
const MAX_UPLOAD_FRAME: usize = 64 * 1024;

/// Request body wrapper recording upload progress.
///
/// Splits large data frames (see [`MAX_UPLOAD_FRAME`]) and never reports
/// `is_end_stream` early: hyper then polls once more after the last data
/// frame, and that final `None` marks the upload done.
pub struct TrackedBody {
    inner: ProxyBody,
    /// Rest of a data frame larger than [`MAX_UPLOAD_FRAME`].
    pending: Bytes,
    progress: Arc<UploadProgress>,
    empty: bool,
    ended: bool,
}

impl TrackedBody {
    pub fn new(inner: ProxyBody) -> (Self, Arc<UploadProgress>) {
        let progress = Arc::new(UploadProgress::default());
        let empty = inner.is_end_stream();
        if empty {
            progress.done.store(true, Ordering::Release);
        }
        (
            Self {
                inner,
                pending: Bytes::new(),
                progress: Arc::clone(&progress),
                empty,
                ended: false,
            },
            progress,
        )
    }

    fn next_piece(&mut self) -> Bytes {
        let take = self.pending.len().min(MAX_UPLOAD_FRAME);
        self.progress.frames.fetch_add(1, Ordering::AcqRel);
        self.pending.split_to(take)
    }
}

impl Body for TrackedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        if !this.pending.is_empty() {
            return Poll::Ready(Some(Ok(Frame::data(this.next_piece()))));
        }
        if this.ended {
            return Poll::Ready(None);
        }
        let frame = ready!(Pin::new(&mut this.inner).poll_frame(cx));
        match frame {
            Some(Ok(frame)) => match frame.into_data() {
                Ok(data) => {
                    this.pending = data;
                    Poll::Ready(Some(Ok(Frame::data(this.next_piece()))))
                }
                Err(other) => Poll::Ready(Some(Ok(other))),
            },
            // A failed body ends the exchange; hyper aborts the request.
            Some(Err(e)) => {
                this.ended = true;
                this.progress.done.store(true, Ordering::Release);
                Poll::Ready(Some(Err(e)))
            }
            None => {
                this.ended = true;
                this.progress.done.store(true, Ordering::Release);
                Poll::Ready(None)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.empty || (self.ended && self.pending.is_empty())
    }

    fn size_hint(&self) -> SizeHint {
        let inner = self.inner.size_hint();
        let pending = self.pending.len() as u64;
        let mut hint = SizeHint::new();
        hint.set_lower(inner.lower().saturating_add(pending));
        if let Some(upper) = inner.upper() {
            hint.set_upper(upper.saturating_add(pending));
        }
        hint
    }
}

// ---------------------------------------------------------------------------
// Response body
// ---------------------------------------------------------------------------

/// Runs once when a response body reached its clean end (returns an
/// HTTP/1.1 connection to the pool).
pub type OnEnd = Box<dyn FnOnce() + Send>;

/// Kept alive for as long as the response body is (e.g. the HTTP/2
/// active-stream count).
pub type Hold = Box<dyn Send + Sync>;

/// Upstream response body with an optional per-frame idle timeout.
///
/// An HTTP/1.1 connection goes back to the pool only when the body was read
/// to its end; dropping the body earlier, a read error or an idle timeout
/// closes the connection instead. Dropping an HTTP/2 body resets just its
/// stream. An idle timeout is terminal: the error is reported once and the
/// body ends.
pub struct ResponseBody {
    inner: Option<Incoming>,
    idle: Option<Duration>,
    deadline: Option<Pin<Box<Sleep>>>,
    on_end: Option<OnEnd>,
    _hold: Option<Hold>,
}

impl ResponseBody {
    pub(crate) fn new(
        inner: Incoming,
        idle: Option<Duration>,
        on_end: Option<OnEnd>,
        hold: Option<Hold>,
    ) -> Self {
        let mut body = Self {
            inner: Some(inner),
            idle,
            deadline: None,
            on_end,
            _hold: hold,
        };
        // Bodiless responses (HEAD, 204, 304, `Content-Length: 0`) are often
        // never polled: release right away.
        if body.inner.as_ref().is_some_and(Body::is_end_stream) {
            body.release();
        }
        body
    }

    fn release(&mut self) {
        if let Some(on_end) = self.on_end.take() {
            on_end();
        }
    }

    /// Abandon the exchange: the connection is not reused.
    fn abandon(&mut self) {
        self.on_end = None;
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
        match Pin::new(&mut *inner).poll_frame(cx) {
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
                this.abandon();
                Poll::Ready(Some(Err(ProxyError::new(
                    ErrorKind::ReadTimeout,
                    "no upstream body data within the read timeout",
                ))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_none_or(Body::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        self.inner
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), Body::size_hint)
    }
}

impl std::fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseBody")
            .field("idle", &self.idle)
            .finish_non_exhaustive()
    }
}
