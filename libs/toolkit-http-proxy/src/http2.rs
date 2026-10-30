//! HTTP/2 on the `h2` crate directly.
//!
//! The engine drives request uploads itself instead of handing the body to
//! hyper's pipe task: it only queues as much DATA as the peer's flow-control
//! window allows, so data never piles up in a send buffer behind a zero
//! window, and a stream whose window stays closed for `write_timeout` is
//! reset (`CANCEL`) without touching the connection's other streams. The
//! response side releases receive capacity as the caller consumes data and
//! forwards trailers.

use std::future::Future as _;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use h2::client::{ResponseFuture, SendRequest};
use h2::{Reason, RecvStream, SendStream};
use http::{HeaderName, Request, Response, Version};
use http_body::Body as _;
use tokio_util::sync::{CancellationToken, DropGuard, WaitForCancellationFutureOwned};

use crate::body::{Hold, ProxyBody};
use crate::config::H2Limits;
use crate::connect::KillableIo;
use crate::error::{ErrorKind, ProxyError};
use crate::idle::IdleTimer;
use crate::kill::Kill;
use crate::sanitize::HopFields;

/// HTTP/2 request sender for one connection (cheap to clone).
pub type H2Sender = SendRequest<Bytes>;

/// Largest DATA chunk queued at once.
const MAX_UPLOAD_CHUNK: usize = 64 * 1024;

/// Perform the HTTP/2 handshake on `io` and start the connection driver
/// (and keep-alive pings). The driver cancels `kill` when the connection
/// ends, which marks it dead for the pool.
///
/// Returns once the upstream's SETTINGS are known (a PING round trip, which
/// can only complete after its SETTINGS frame): until then h2 assumes no
/// stream limit, and streams sent beyond the real one are refused.
pub async fn handshake(
    io: KillableIo,
    limits: &H2Limits,
    kill: &Kill,
    settle_within: Duration,
) -> Result<H2Sender, ProxyError> {
    let mut builder = h2::client::Builder::new();
    builder
        .enable_push(false)
        .max_header_list_size(limits.max_header_list_size)
        .header_table_size(limits.header_table_size)
        .max_frame_size(limits.max_frame_size)
        .initial_window_size(limits.initial_stream_window_size)
        .initial_connection_window_size(limits.initial_connection_window_size)
        .max_concurrent_reset_streams(limits.max_concurrent_reset_streams)
        .max_pending_accept_reset_streams(limits.max_pending_accept_reset_streams)
        .max_local_error_reset_streams(Some(limits.max_local_error_reset_streams))
        .max_send_buffer_size(limits.max_send_buf_size);
    let (sender, mut conn) = builder
        .handshake::<_, Bytes>(io)
        .await
        .map_err(|e| from_h2(&e, "upstream HTTP/2 handshake failed").with_source(e))?;

    let mut pings = conn.ping_pong();
    let driver_kill = kill.clone();
    // How the connection ended: the reason of a GOAWAY from the upstream.
    let (ended_tx, ended_rx) = tokio::sync::oneshot::channel::<Option<Reason>>();
    tokio::spawn(async move {
        tokio::select! {
            res = &mut conn => {
                let goaway = match &res {
                    Err(e) if e.is_go_away() && e.is_remote() => e.reason(),
                    _ => None,
                };
                if let Err(e) = res {
                    tracing::debug!(error = %e, "upstream HTTP/2 connection closed with error");
                }
                let _ignored = ended_tx.send(goaway);
            }
            () = driver_kill.cancelled() => {
                tracing::debug!("upstream HTTP/2 connection closed by the engine");
            }
        }
        // Dead either way: stop handing it out.
        driver_kill.cancel();
    });

    if let Some(pings) = pings.as_mut() {
        let settled = tokio::time::timeout(settle_within, pings.ping(h2::Ping::opaque())).await;
        match settled {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                // The PING only reports a closed connection; the reason is
                // the connection's own error.
                let goaway = tokio::time::timeout(settle_within, ended_rx)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten();
                kill.cancel();
                if goaway == Some(Reason::HTTP_1_1_REQUIRED) {
                    return Err(
                        ProxyError::new(ErrorKind::Http2, "upstream requires HTTP/1.1")
                            .with_source(Http1Required),
                    );
                }
                return Err(
                    from_h2(&e, "upstream HTTP/2 connection failed during setup").with_source(e),
                );
            }
            Err(_) => {
                kill.cancel();
                return Err(ProxyError::new(
                    ErrorKind::ConnectTimeout,
                    "upstream HTTP/2 settings not received in time",
                ));
            }
        }
    }
    if let (Some(pings), Some(every)) = (pings, limits.keep_alive_interval) {
        tokio::spawn(keep_alive(
            pings,
            every,
            limits.keep_alive_timeout,
            kill.clone(),
        ));
    }
    Ok(sender)
}

/// The upstream closed a new HTTP/2 connection with
/// `GOAWAY(HTTP_1_1_REQUIRED)` before it was used.
#[derive(Debug, thiserror::Error)]
#[error("upstream requires HTTP/1.1")]
pub struct Http1Required;

/// Whether connection setup failed because the upstream asked for HTTP/1.1.
pub fn requires_http1(err: &ProxyError) -> bool {
    std::error::Error::source(err).is_some_and(<dyn std::error::Error>::is::<Http1Required>)
}

/// Ping on an interval; close the connection when a ping goes unanswered.
async fn keep_alive(mut pings: h2::PingPong, every: Duration, timeout: Duration, kill: Kill) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tokio::select! {
            () = kill.cancelled() => return,
            _ = tick.tick() => {}
        }
        match tokio::time::timeout(timeout, pings.ping(h2::Ping::opaque())).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => return,
            Err(_) => {
                tracing::warn!(
                    ?timeout,
                    "upstream HTTP/2 keep-alive ping unanswered; closing"
                );
                kill.cancel();
                return;
            }
        }
    }
}

/// Outcome of starting an exchange.
pub enum Started {
    /// The request is on the wire; await the response head. The request
    /// head comes back too, so a refused request can be sent again.
    Sent(ResponseFuture, Box<http::request::Parts>),
    /// The connection refused a new stream before anything was sent.
    Unsent(Box<Request<ProxyBody>>, ProxyError),
}

/// Why the upload task gave up before the exchange's owner did.
#[derive(Debug, Clone, Copy)]
enum Abort {
    /// The flow-control window stayed closed for `write_timeout`.
    Stalled,
    /// The request body failed.
    Body,
}

/// Shared state of one HTTP/2 exchange (request upload + response).
///
/// Only the exchange's owner ends it: the dispatch future until the response
/// head arrives, the response body afterwards. Cancelling (owner dropped,
/// failed or finished) makes the upload task reset the stream. The upload
/// task never resets on its own: h2 lets a local reset erase a response that
/// was already complete, so when it gives up (stalled window, failed request
/// body) it only signals `abort` and the owner decides: a complete response
/// is still delivered, anything else fails.
#[derive(Debug, Default)]
pub struct Exchange {
    cancel: CancellationToken,
    abort: Kill,
    cause: OnceLock<Abort>,
}

impl Exchange {
    /// A new exchange and the guard that cancels it on drop.
    pub fn new() -> (Arc<Self>, DropGuard) {
        let exchange = Arc::new(Self::default());
        let guard = exchange.cancel.clone().drop_guard();
        (exchange, guard)
    }

    fn give_up(&self, cause: Abort) {
        let _first = self.cause.set(cause);
        self.abort.cancel();
    }

    fn abort_error(&self) -> ProxyError {
        match self.cause.get() {
            Some(Abort::Body) => {
                ProxyError::new(ErrorKind::Write, "request body failed; upload aborted")
            }
            Some(Abort::Stalled) | None => ProxyError::new(
                ErrorKind::WriteTimeout,
                "upstream kept the HTTP/2 flow-control window closed",
            ),
        }
    }
}

/// Wait for the response head; fails first if the upload gives up.
pub async fn response(
    head: ResponseFuture,
    exchange: &Exchange,
) -> Result<Result<Response<RecvStream>, h2::Error>, ProxyError> {
    tokio::select! {
        biased;
        head = head => Ok(head),
        () = exchange.abort.cancelled() => Err(exchange.abort_error()),
    }
}

/// Open a stream for `req` and start uploading its body.
///
/// The body is uploaded by a spawned task that holds `hold` until the
/// upload ends (so the connection counts as busy while it runs) and stops
/// when `exchange` is cancelled.
pub async fn start(
    sender: H2Sender,
    req: Request<ProxyBody>,
    stall: Option<IdleTimer>,
    hold: Option<Hold>,
    exchange: Arc<Exchange>,
) -> Started {
    let mut sender = match sender.ready().await {
        Ok(sender) => sender,
        Err(e) => {
            let err = from_h2(&e, "upstream HTTP/2 connection unavailable").with_source(e);
            return Started::Unsent(Box::new(req), err);
        }
    };
    let (parts, body) = req.into_parts();
    let mut head = Request::new(());
    *head.method_mut() = parts.method.clone();
    *head.uri_mut() = parts.uri.clone();
    *head.version_mut() = Version::HTTP_2;
    *head.headers_mut() = parts.headers.clone();
    let end_of_stream = body.is_end_stream();
    match sender.send_request(head, end_of_stream) {
        Ok((response, upload)) => {
            if !end_of_stream {
                let hop = parts.extensions.get::<HopFields>().cloned();
                tokio::spawn(pump(body, upload, stall, hold, exchange, hop));
            }
            Started::Sent(response, Box::new(parts))
        }
        Err(e) => {
            let err = from_h2(&e, "upstream HTTP/2 stream refused").with_source(e);
            Started::Unsent(Box::new(Request::from_parts(parts, body)), err)
        }
    }
}

enum Capacity {
    Granted(usize),
    Stalled,
    Closed,
}

/// Wait for send capacity. `stall` (one per upload, reused across waits)
/// gives up once a single wait has lasted its limit.
async fn wait_capacity(out: &mut SendStream<Bytes>, mut stall: Option<&mut IdleTimer>) -> Capacity {
    if let Some(stall) = stall.as_deref_mut() {
        stall.touch();
    }
    loop {
        let available = out.capacity();
        if available > 0 {
            return Capacity::Granted(available);
        }
        let polled = poll_fn(|cx| {
            if let std::task::Poll::Ready(polled) = out.poll_capacity(cx) {
                return std::task::Poll::Ready(Some(polled));
            }
            match stall.as_deref_mut().map(|s| s.poll_expired(cx)) {
                Some(std::task::Poll::Ready(())) => std::task::Poll::Ready(None),
                _ => std::task::Poll::Pending,
            }
        })
        .await;
        match polled {
            None => return Capacity::Stalled,
            Some(Some(Ok(0))) => {}
            Some(Some(Ok(n))) => return Capacity::Granted(n),
            Some(Some(Err(_)) | None) => return Capacity::Closed,
        }
    }
}

/// Upload task: runs [`upload`] until it ends or the exchange is cancelled
/// (then the stream is reset). If the upload gives up, the owner is told and
/// the reset waits for its decision, unless the peer resets the stream or the
/// connection goes away first (nothing left to reset then). A response body
/// kept without being polled keeps its stream, as an unread HTTP/1.1 body
/// keeps its connection.
async fn pump(
    mut body: ProxyBody,
    mut out: SendStream<Bytes>,
    mut stall: Option<IdleTimer>,
    _hold: Option<Hold>,
    exchange: Arc<Exchange>,
    hop: Option<HopFields>,
) {
    let hop = hop.as_ref().map_or(&[][..], |h| &h.0[..]);
    let gave_up = tokio::select! {
        biased;
        () = exchange.cancel.cancelled() => None,
        outcome = upload(&mut body, &mut out, &mut stall, hop) => match outcome {
            Ok(()) => return,
            Err(cause) => Some(cause),
        },
    };
    if let Some(cause) = gave_up {
        drop(body);
        exchange.give_up(cause);
        tokio::select! {
            () = exchange.cancel.cancelled() => {}
            _ = poll_fn(|cx| out.poll_reset(cx)) => return,
        }
    }
    out.send_reset(Reason::CANCEL);
}

/// Upload `body` on `out`, never queueing more than the window allows.
/// Trailers are sanitized, also losing `hop` (the fields the request head's
/// `Connection` nominated). `Ok` when the upload ended (or the stream did),
/// `Err` when it gave up.
async fn upload(
    body: &mut ProxyBody,
    out: &mut SendStream<Bytes>,
    stall: &mut Option<IdleTimer>,
    hop: &[HeaderName],
) -> Result<(), Abort> {
    loop {
        // Stop as soon as the peer resets the stream, even while the body
        // is idle.
        let next = poll_fn(|cx| {
            if out.poll_reset(cx).is_ready() {
                return std::task::Poll::Ready(None);
            }
            Pin::new(&mut *body).poll_frame(cx).map(Some)
        })
        .await;
        let Some(frame) = next else { return Ok(()) };
        match frame {
            None => {
                let _sent = out.send_data(Bytes::new(), true);
                return Ok(());
            }
            Some(Err(e)) => {
                tracing::debug!(error = %e, "request body failed; aborting the upstream exchange");
                return Err(Abort::Body);
            }
            Some(Ok(frame)) => match frame.into_data() {
                Ok(mut data) => {
                    let last = body.is_end_stream();
                    if data.is_empty() && last {
                        let _sent = out.send_data(Bytes::new(), true);
                        return Ok(());
                    }
                    while !data.is_empty() {
                        out.reserve_capacity(data.len().min(MAX_UPLOAD_CHUNK));
                        match wait_capacity(out, stall.as_mut()).await {
                            Capacity::Granted(n) => {
                                let chunk = data.split_to(n.min(data.len()).min(MAX_UPLOAD_CHUNK));
                                if out.send_data(chunk, last && data.is_empty()).is_err() {
                                    return Ok(());
                                }
                            }
                            Capacity::Stalled => {
                                tracing::warn!(
                                    limit = ?stall.as_ref().map(IdleTimer::limit),
                                    "upstream kept the HTTP/2 flow-control window closed; aborting the exchange"
                                );
                                return Err(Abort::Stalled);
                            }
                            Capacity::Closed => return Ok(()),
                        }
                    }
                    if last {
                        return Ok(());
                    }
                }
                Err(frame) => {
                    if let Ok(mut trailers) = frame.into_trailers() {
                        crate::sanitize::sanitize_trailers_without(&mut trailers, hop);
                        let _sent = out.send_trailers(trailers);
                        return Ok(());
                    }
                }
            },
        }
    }
}

/// Response body half of an HTTP/2 exchange; it owns the exchange once the
/// head arrived. Its clean end, a failure or dropping it ends the exchange
/// (stopping any upload still running).
pub struct H2Body {
    recv: RecvStream,
    data_done: bool,
    /// The clean end was observed; nothing after it can turn into an error.
    finished: bool,
    exchange: Arc<Exchange>,
    aborted: Option<Pin<Box<WaitForCancellationFutureOwned>>>,
    _owner: DropGuard,
}

impl H2Body {
    /// The body of `exchange`, read from `recv`; `owner` resets the stream
    /// (and stops the upload) when the body is dropped before its end.
    pub fn new(recv: RecvStream, exchange: Arc<Exchange>, owner: DropGuard) -> Self {
        let mut body = Self {
            recv,
            data_done: false,
            finished: false,
            exchange,
            aborted: None,
            _owner: owner,
        };
        // A head with END_STREAM: the response is already complete.
        if body.recv.is_end_stream() {
            body.finish();
        }
        body
    }

    /// The response is complete: an upload still running has no use any
    /// more and is stopped, so a caller holding on to a finished body never
    /// keeps the stream busy.
    fn finish(&mut self) {
        self.finished = true;
        self.exchange.cancel.cancel();
    }

    /// h2 reports the end only once `END_STREAM` arrived and every frame was
    /// handed out; a reset before that never counts as an end.
    pub fn is_end_stream(&self) -> bool {
        self.finished || self.recv.is_end_stream()
    }

    /// Nothing to deliver right now: fail if the upload gave up, else wait.
    /// Everything the upstream already sent is delivered first, so a
    /// response completed despite a stalled upload is never lost.
    fn poll_abort(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, ProxyError>>> {
        use std::task::Poll;
        let exchange = &self.exchange;
        let aborted = self
            .aborted
            .get_or_insert_with(|| Box::pin(exchange.abort.cancelled_owned()));
        match aborted.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Some(Err(self.exchange.abort_error()))),
            Poll::Pending => Poll::Pending,
        }
    }

    /// The next data or trailers frame. Every data frame handed out credits
    /// the flow-control window, unless the upload was aborted.
    pub fn poll_frame(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, ProxyError>>> {
        use std::task::Poll;
        if self.finished {
            return Poll::Ready(None);
        }
        if !self.data_done {
            match self.recv.poll_data(cx) {
                Poll::Ready(Some(Ok(data))) => {
                    // Credit the window now that the caller has the bytes,
                    // unless the upload gave up: then the upstream gets no
                    // more credit, so what it can still send (and the drain
                    // before the abort surfaces) is bounded by the window.
                    if !self.exchange.abort.is_cancelled() {
                        let _released = self.recv.flow_control().release_capacity(data.len());
                    }
                    if self.recv.is_end_stream() {
                        self.finish();
                    }
                    return Poll::Ready(Some(Ok(http_body::Frame::data(data))));
                }
                Poll::Ready(Some(Err(e))) => {
                    let err = from_h2_body(&e).with_source(e);
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(None) => self.data_done = true,
                Poll::Pending => return self.poll_abort(cx),
            }
        }
        match self.recv.poll_trailers(cx) {
            Poll::Ready(Ok(Some(trailers))) => {
                self.finish();
                Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))))
            }
            Poll::Ready(Ok(None)) => {
                self.finish();
                Poll::Ready(None)
            }
            Poll::Ready(Err(e)) => {
                let err = from_h2_body(&e).with_source(e);
                Poll::Ready(Some(Err(err)))
            }
            Poll::Pending => self.poll_abort(cx),
        }
    }
}

/// Why a failed HTTP/2 exchange may be sent again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The peer reset the stream with `REFUSED_STREAM`: it was not
    /// processed (RFC 9113 §8.7). Safe to resend for any method.
    Refused,
    /// The peer requires HTTP/1.1 for this request (`HTTP_1_1_REQUIRED`).
    Http1Required,
    /// A graceful `GOAWAY` (`NO_ERROR`). Streams above its last stream id
    /// were not processed, but h2 reports the same error to a stream the
    /// peer may have processed when the connection then drops, so only an
    /// idempotent request may be resent.
    GoingAway,
}

/// Classify an error from the peer that leaves the request resendable.
pub fn refusal(e: &h2::Error) -> Option<Refusal> {
    if !e.is_remote() {
        return None;
    }
    match e.reason()? {
        Reason::REFUSED_STREAM if e.is_reset() => Some(Refusal::Refused),
        Reason::HTTP_1_1_REQUIRED => Some(Refusal::Http1Required),
        Reason::NO_ERROR if e.is_go_away() => Some(Refusal::GoingAway),
        _ => None,
    }
}

/// Whether an h2 error means the whole connection is gone.
pub fn is_connection_error(e: &h2::Error) -> bool {
    e.is_go_away() || e.is_io()
}

/// Classify an `h2` error raised before or while receiving the response head.
pub fn from_h2(e: &h2::Error, message: &'static str) -> ProxyError {
    let kind = match e.get_io() {
        Some(io) => crate::error::classify_io(io),
        None => ErrorKind::Http2,
    };
    ProxyError::new(kind, message)
}

fn from_h2_body(e: &h2::Error) -> ProxyError {
    let kind = match e.get_io() {
        Some(io) => match crate::error::classify_io(io) {
            ErrorKind::ConnectionClosed => ErrorKind::Read,
            other => other,
        },
        None => ErrorKind::Http2,
    };
    ProxyError::new(kind, "upstream response body failed")
}
