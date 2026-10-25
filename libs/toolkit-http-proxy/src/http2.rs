//! HTTP/2 on the `h2` crate directly.
//!
//! The engine drives request uploads itself instead of handing the body to
//! hyper's pipe task: it only queues as much DATA as the peer's flow-control
//! window allows, so data never piles up in a send buffer behind a zero
//! window, and a stream whose window stays closed for `write_timeout` is
//! reset (`CANCEL`) without touching the connection's other streams. The
//! response side releases receive capacity as the caller consumes data and
//! forwards trailers.

use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use h2::client::{ResponseFuture, SendRequest};
use h2::{Reason, RecvStream, SendStream};
use http::{Request, Version};
use http_body::Body as _;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::body::{Hold, ProxyBody};
use crate::config::H2Limits;
use crate::connect::KillableIo;
use crate::error::{ErrorKind, ProxyError};

/// HTTP/2 request sender for one connection (cheap to clone).
pub type H2Sender = SendRequest<Bytes>;

/// Largest DATA chunk queued at once.
const MAX_UPLOAD_CHUNK: usize = 64 * 1024;

/// Perform the HTTP/2 handshake on `io` and start the connection driver
/// (and keep-alive pings). The driver cancels `kill` when the connection
/// ends, which marks it dead for the pool.
pub async fn handshake(
    io: KillableIo,
    limits: &H2Limits,
    kill: &CancellationToken,
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

    if let (Some(pings), Some(every)) = (conn.ping_pong(), limits.keep_alive_interval) {
        tokio::spawn(keep_alive(
            pings,
            every,
            limits.keep_alive_timeout,
            kill.clone(),
        ));
    }
    let kill = kill.clone();
    tokio::spawn(async move {
        tokio::select! {
            res = &mut conn => {
                if let Err(e) = res {
                    tracing::debug!(error = %e, "upstream HTTP/2 connection closed with error");
                }
            }
            () = kill.cancelled() => {
                tracing::debug!("upstream HTTP/2 connection closed by the engine");
            }
        }
        // Dead either way: stop handing it out.
        kill.cancel();
    });
    Ok(sender)
}

/// Ping on an interval; close the connection when a ping goes unanswered.
async fn keep_alive(
    mut pings: h2::PingPong,
    every: Duration,
    timeout: Duration,
    kill: CancellationToken,
) {
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
    /// The request is on the wire; await the response head.
    Sent(ResponseFuture),
    /// The connection refused a new stream before anything was sent.
    Unsent(Box<Request<ProxyBody>>, ProxyError),
}

/// Shared state of one HTTP/2 exchange (request upload + response).
///
/// The exchange is cancelled when its owner goes away: the dispatch future
/// before a response head arrives, or the response body afterwards. The
/// upload task then resets the stream and ends, so abandoned exchanges never
/// keep uploading or hold the connection busy.
#[derive(Debug, Default)]
pub struct Exchange {
    cancel: CancellationToken,
    stalled: AtomicBool,
    responded: AtomicBool,
}

impl Exchange {
    /// A new exchange and the guard that cancels it on drop.
    pub fn new() -> (Arc<Self>, DropGuard) {
        let exchange = Arc::new(Self::default());
        let guard = exchange.cancel.clone().drop_guard();
        (exchange, guard)
    }

    /// The upload was reset because the window stayed closed.
    pub fn stalled(&self) -> bool {
        self.stalled.load(Ordering::Acquire)
    }

    /// The response head arrived. From then on the caller owns the
    /// exchange's lifetime (body idle timeout, dropping the body), so a
    /// closed upload window no longer resets the stream: a reset would
    /// overwrite a response the upstream may already have completed.
    pub fn set_responded(&self) {
        self.responded.store(true, Ordering::Release);
    }

    fn responded(&self) -> bool {
        self.responded.load(Ordering::Acquire)
    }

    /// The error to report for a failed exchange: a reset we sent because
    /// the upload stalled is a write timeout, anything else is classified
    /// from the h2 error.
    pub fn error(&self, e: &h2::Error, message: &'static str) -> ProxyError {
        if self.stalled() {
            return stall_error();
        }
        from_h2(e, message)
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
    write_timeout: Option<Duration>,
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
                tokio::spawn(pump(body, upload, write_timeout, hold, exchange));
            }
            Started::Sent(response)
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

async fn wait_capacity(
    out: &mut SendStream<Bytes>,
    stall: Option<Duration>,
    exchange: &Exchange,
) -> Capacity {
    loop {
        let available = out.capacity();
        if available > 0 {
            return Capacity::Granted(available);
        }
        let next = poll_fn(|cx| out.poll_capacity(cx));
        let polled = match stall {
            Some(limit) if !exchange.responded() => match tokio::time::timeout(limit, next).await {
                Ok(polled) => polled,
                Err(_) if exchange.responded() => continue,
                Err(_) => return Capacity::Stalled,
            },
            _ => next.await,
        };
        match polled {
            Some(Ok(0)) => {}
            Some(Ok(n)) => return Capacity::Granted(n),
            Some(Err(_)) | None => return Capacity::Closed,
        }
    }
}

/// Upload task: runs [`upload`] until it ends or the exchange is
/// cancelled, in which case the stream is reset.
async fn pump(
    mut body: ProxyBody,
    mut out: SendStream<Bytes>,
    stall: Option<Duration>,
    _hold: Option<Hold>,
    exchange: Arc<Exchange>,
) {
    let cancelled = tokio::select! {
        biased;
        () = exchange.cancel.cancelled() => true,
        () = upload(&mut body, &mut out, stall, &exchange) => false,
    };
    if cancelled {
        out.send_reset(Reason::CANCEL);
    }
}

/// Upload `body` on `out`, never queueing more than the window allows.
async fn upload(
    body: &mut ProxyBody,
    out: &mut SendStream<Bytes>,
    stall: Option<Duration>,
    exchange: &Exchange,
) {
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
        let Some(frame) = next else { return };
        match frame {
            None => {
                let _sent = out.send_data(Bytes::new(), true);
                return;
            }
            Some(Err(e)) => {
                tracing::debug!(error = %e, "request body failed; resetting the upstream stream");
                out.send_reset(Reason::CANCEL);
                return;
            }
            Some(Ok(frame)) => match frame.into_data() {
                Ok(mut data) => {
                    let last = body.is_end_stream();
                    if data.is_empty() && last {
                        let _sent = out.send_data(Bytes::new(), true);
                        return;
                    }
                    while !data.is_empty() {
                        out.reserve_capacity(data.len().min(MAX_UPLOAD_CHUNK));
                        match wait_capacity(out, stall, exchange).await {
                            Capacity::Granted(n) => {
                                let chunk = data.split_to(n.min(data.len()).min(MAX_UPLOAD_CHUNK));
                                if out.send_data(chunk, last && data.is_empty()).is_err() {
                                    return;
                                }
                            }
                            Capacity::Stalled => {
                                exchange.stalled.store(true, Ordering::Release);
                                tracing::warn!(
                                    ?stall,
                                    "upstream kept the HTTP/2 flow-control window closed; resetting the stream"
                                );
                                out.send_reset(Reason::CANCEL);
                                return;
                            }
                            Capacity::Closed => return,
                        }
                    }
                    if last {
                        return;
                    }
                }
                Err(frame) => {
                    if let Ok(trailers) = frame.into_trailers() {
                        let _sent = out.send_trailers(trailers);
                        return;
                    }
                }
            },
        }
    }
}

fn stall_error() -> ProxyError {
    ProxyError::new(
        ErrorKind::WriteTimeout,
        "upstream kept the HTTP/2 flow-control window closed",
    )
}

/// Response body half of an HTTP/2 exchange. Dropping it cancels the
/// exchange (and with it any upload still running).
pub struct H2Body {
    recv: RecvStream,
    data_done: bool,
    /// The clean end was observed; nothing after it can turn it into an
    /// error.
    finished: bool,
    exchange: Arc<Exchange>,
    _owner: DropGuard,
}

impl H2Body {
    pub fn new(recv: RecvStream, exchange: Arc<Exchange>, owner: DropGuard) -> Self {
        Self {
            recv,
            data_done: false,
            finished: false,
            exchange,
            _owner: owner,
        }
    }

    fn error(&self, e: &h2::Error) -> ProxyError {
        if self.exchange.stalled() {
            return stall_error();
        }
        from_h2_body(e)
    }

    /// The response is complete: an upload still running has no use any
    /// more and is stopped (its stream reset), so a caller holding on to a
    /// finished body never keeps the stream busy.
    fn finish(&mut self) {
        self.finished = true;
        self.exchange.cancel.cancel();
    }

    /// h2 reports the end only once `END_STREAM` arrived and every frame was
    /// handed out; a reset before that never counts as an end.
    pub fn is_end_stream(&self) -> bool {
        self.finished || self.recv.is_end_stream()
    }

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
                    // Credit the window now that the caller has the bytes.
                    let _released = self.recv.flow_control().release_capacity(data.len());
                    return Poll::Ready(Some(Ok(http_body::Frame::data(data))));
                }
                Poll::Ready(Some(Err(e))) => {
                    let err = self.error(&e).with_source(e);
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(None) => self.data_done = true,
                Poll::Pending => return Poll::Pending,
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
                let err = self.error(&e).with_source(e);
                Poll::Ready(Some(Err(err)))
            }
            Poll::Pending => Poll::Pending,
        }
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
