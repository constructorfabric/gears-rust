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

/// Request extension holding a copy of the request body, so the engine may
/// send the request again after a failure that provably happened before the
/// upstream processed it (see [`crate::ProxyClient::send`]). The bytes must
/// be exactly the request's body; cloning [`Bytes`] does not copy data.
/// Requests with an empty body are resendable without it.
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
}
