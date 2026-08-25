use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::domain::ports::OagwMetricsPort;

// ---------------------------------------------------------------------------
// Close frames
// ---------------------------------------------------------------------------

/// RFC 6455 §7.4.1 status code for "Going Away".
const CLOSE_GOING_AWAY: u16 = 1001;

/// Reason text paired with [`CLOSE_GOING_AWAY`].
const CLOSE_GOING_AWAY_REASON: &str = "Going Away";

/// Payload length of the Close frame: 2 status bytes plus the reason.
///
/// Const-evaluated, so the 125-byte control-frame ceiling (RFC 6455 §5.5) is a
/// compile-time guarantee rather than a runtime check.
const CLOSE_PAYLOAD_LEN: u8 = {
    let len = 2 + CLOSE_GOING_AWAY_REASON.len();
    assert!(len <= 125, "close payload exceeds the control-frame limit");
    len as u8
};

/// Serialise the Close frame the gateway sends when it tears a tunnel down.
///
/// This is frame *construction* of one fixed shape, not parsing. Nothing about
/// the frame is parameterised except masking, because the gateway only ever
/// originates one close code: 1001 on idle timeout or shutdown. A peer's own
/// Close is relayed verbatim by the byte copy and never passes through here.
///
/// `mask` must be `Some` for frames sent toward the upstream — the gateway is
/// the client on that leg, and RFC 6455 §5.3 requires client-to-server frames
/// be masked — and `None` for frames sent toward the caller.
fn going_away_frame(mask: Option<[u8; 4]>) -> Vec<u8> {
    let capacity = 2 + mask.map_or(0, |_| 4) + usize::from(CLOSE_PAYLOAD_LEN);
    let mut frame = Vec::with_capacity(capacity);
    frame.push(0x88); // FIN | Close

    // Streamed straight into `frame`, so the payload is never materialised
    // separately.
    let payload_bytes = CLOSE_GOING_AWAY
        .to_be_bytes()
        .into_iter()
        .chain(CLOSE_GOING_AWAY_REASON.bytes());

    match mask {
        Some(key) => {
            frame.push(0x80 | CLOSE_PAYLOAD_LEN);
            frame.extend_from_slice(&key);
            frame.extend(payload_bytes.enumerate().map(|(i, b)| b ^ key[i % 4]));
        }
        None => {
            frame.push(CLOSE_PAYLOAD_LEN);
            frame.extend(payload_bytes);
        }
    }
    frame
}

/// Generate a 4-byte mask key using OS-seeded randomness.
///
/// Uses `RandomState` (SipHash with OS-random seeds) per thread, hashing
/// an atomic counter through it. Satisfies RFC 6455 §5.3 requirement that
/// mask keys be chosen unpredictably.
fn rand_mask_key() -> [u8; 4] {
    use std::hash::{BuildHasher, Hasher};

    thread_local! {
        static HASHER_STATE: std::collections::hash_map::RandomState =
            std::collections::hash_map::RandomState::new();
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    HASHER_STATE.with(|state| {
        let mut hasher = state.build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        (hasher.finish() as u32).to_ne_bytes()
    })
}

// ---------------------------------------------------------------------------
// Leftover prefix
// ---------------------------------------------------------------------------

/// Wraps the upstream leg so bytes already pulled off it are read back first.
///
/// Parsing the 101 response can consume past the header boundary. Those bytes
/// are upstream's, so prefixing them onto upstream's *read* side puts them
/// ahead of everything upstream sends next by construction, and delivers them
/// through the relay — inside its idle timeout, shutdown arm and teardown —
/// instead of via a separate unbounded write before the relay starts.
struct PrefixedReader<R> {
    prefix: Bytes,
    inner: R,
}

impl<R> PrefixedReader<R> {
    fn new(prefix: Bytes, inner: R) -> Self {
        Self { prefix, inner }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for PrefixedReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.prefix.is_empty() {
            let n = this.prefix.len().min(buf.remaining());
            buf.put_slice(&this.prefix[..n]);
            this.prefix.advance(n);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for PrefixedReader<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------------------
// Activity tracking
// ---------------------------------------------------------------------------

/// `first_eof` sentinel: nobody has reached end-of-stream yet.
const EOF_NONE: u8 = 0;
/// `first_eof` sentinel: the caller's half ended first.
const EOF_CLIENT: u8 = 1;
/// `first_eof` sentinel: the upstream half ended first.
const EOF_UPSTREAM: u8 = 2;

/// Wraps one side of the tunnel and records when bytes last arrived.
///
/// Both sides share one cell, so "idle" means nothing moved in *either*
/// direction. This is the byte-level equivalent of the frame-level idle timer
/// the relay used to keep, and it needs no knowledge of framing.
struct ActivityIo<T> {
    inner: T,
    since: tokio::time::Instant,
    last_ms: Arc<AtomicU64>,
    /// Which side this wrapper is, one of the `EOF_*` sentinels.
    side: u8,
    /// Records the *first* side to reach end-of-stream. `copy_bidirectional`
    /// half-closes, so it only returns once both halves are done — the order
    /// is what says whether the caller left or the upstream died under it.
    first_eof: Arc<AtomicU8>,
}

impl<T> ActivityIo<T> {
    fn new(
        inner: T,
        since: tokio::time::Instant,
        last_ms: Arc<AtomicU64>,
        side: u8,
        first_eof: Arc<AtomicU8>,
    ) -> Self {
        Self {
            inner,
            since,
            last_ms,
            side,
            first_eof,
        }
    }

    fn touch(&self) {
        let elapsed = u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(elapsed, Ordering::Relaxed);
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ActivityIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(poll, Poll::Ready(Ok(()))) {
            // A ready read that filled nothing is end-of-stream. (Callers here
            // always supply a non-empty buffer, so this cannot misfire.)
            if buf.filled().len() > before {
                this.touch();
            } else {
                let _first = this.first_eof.compare_exchange(
                    EOF_NONE,
                    this.side,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
        }
        poll
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ActivityIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// Resolves once no bytes have moved in either direction for `idle`.
///
/// Re-derives the deadline from the shared cell after each sleep, so traffic
/// simply pushes the deadline out rather than resetting a timer.
async fn idle_elapsed(since: tokio::time::Instant, last_ms: &AtomicU64, idle: Duration) {
    loop {
        let deadline = since + Duration::from_millis(last_ms.load(Ordering::Relaxed)) + idle;
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep_until(deadline).await;
    }
}

/// Resolves when the server signals shutdown.
///
/// Pends forever once the sender is gone: no signal can arrive after that, and
/// returning would spin the relay's `select!` at full CPU.
async fn shutdown_signalled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Opaque relay
// ---------------------------------------------------------------------------

/// Which peer reached end-of-stream first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TunnelEnd {
    /// The caller disconnected first — the ordinary ending.
    Client,
    /// The upstream went away under a live caller — abnormal, worth a warning.
    Upstream,
    /// Neither half reported end-of-stream (the copy ended some other way).
    Unknown,
}

/// Outcome of the relay loop.
#[derive(Debug)]
pub(crate) enum RelayOutcome {
    /// Either side ended the tunnel; carries the byte tallies.
    Closed {
        to_upstream: u64,
        to_client: u64,
        ended_by: TunnelEnd,
    },
    /// No bytes moved in either direction within the idle timeout.
    IdleTimeout,
    /// Server is shutting down gracefully.
    Shutdown,
    /// IO error on either half.
    Error(std::io::Error),
}

/// Configuration for the relay loop.
struct RelayConfig {
    /// Bytes already read off the upstream leg while parsing the 101 response.
    /// Delivered to the caller ahead of anything upstream sends next.
    leftover: Bytes,
    idle_timeout: Duration,
    close_timeout: Duration,
    shutdown_rx: watch::Receiver<bool>,
}

/// Opaque bidirectional relay between the caller and the upstream tunnel.
///
/// After the 101 a WebSocket connection is just bytes, so frames are forwarded
/// verbatim and the gateway never parses one. That keeps masking, RSV bits
/// (`permessage-deflate`), fragmentation and unknown/reserved opcodes intact by
/// construction rather than by matching on each case: a client-to-server frame
/// arrives masked and leaves masked, exactly as RFC 6455 §5.3 requires.
///
/// Memory is bounded by the copy buffer, not by frame size — a huge frame
/// streams through instead of being materialised — which is why there is no
/// frame-size ceiling to enforce here.
///
/// `cfg.leftover` is prefixed onto upstream's read side, so bytes already taken
/// off that leg reach the caller first and every write toward a peer happens
/// under the timeouts and teardown below — including when the caller has
/// stopped reading and cannot accept those bytes at all.
///
/// # Cancellation
///
/// `copy_bidirectional` is not cancellation safe: dropping it can discard bytes
/// held in its transfer buffers, so the last frame toward a peer may arrive
/// truncated when the idle or shutdown arm wins. That is accepted rather than
/// prevented — both arms end the session, and neither restarts the copy, so a
/// truncated frame is immediately followed by Close 1001 and the teardown the
/// peer was going to get anyway.
async fn relay<C, U>(client: C, upstream: U, cfg: RelayConfig) -> RelayOutcome
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let RelayConfig {
        leftover,
        idle_timeout,
        close_timeout,
        mut shutdown_rx,
    } = cfg;

    let upstream = PrefixedReader::new(leftover, upstream);

    let since = tokio::time::Instant::now();
    let last_ms = Arc::new(AtomicU64::new(0));
    let first_eof = Arc::new(AtomicU8::new(EOF_NONE));
    let mut client = ActivityIo::new(
        client,
        since,
        Arc::clone(&last_ms),
        EOF_CLIENT,
        Arc::clone(&first_eof),
    );
    let mut upstream = ActivityIo::new(
        upstream,
        since,
        Arc::clone(&last_ms),
        EOF_UPSTREAM,
        Arc::clone(&first_eof),
    );

    let outcome = tokio::select! {
        result = tokio::io::copy_bidirectional(&mut client, &mut upstream) => match result {
            Ok((to_upstream, to_client)) => RelayOutcome::Closed {
                to_upstream,
                to_client,
                ended_by: match first_eof.load(Ordering::Relaxed) {
                    EOF_CLIENT => TunnelEnd::Client,
                    EOF_UPSTREAM => TunnelEnd::Upstream,
                    _ => TunnelEnd::Unknown,
                },
            },
            Err(e) => RelayOutcome::Error(e),
        },
        () = idle_elapsed(since, &last_ms, idle_timeout) => RelayOutcome::IdleTimeout,
        () = shutdown_signalled(&mut shutdown_rx) => RelayOutcome::Shutdown,
    };

    if matches!(outcome, RelayOutcome::IdleTimeout | RelayOutcome::Shutdown) {
        close_tunnel(&mut client, &mut upstream, close_timeout).await;
    }
    outcome
}

/// Announce Close 1001 to both peers and half-close both write halves.
///
/// The two announcements are independent: each runs concurrently with its own
/// `grace` budget, because the peer that triggered the teardown is exactly the
/// peer that may have stopped reading. Announcing sequentially under one shared
/// budget lets a stalled peer swallow the whole grace period, leaving the other
/// side with no Close frame and no half-close at all.
///
/// The relay is *not* resumed afterwards: the gateway has told both peers to go
/// away, so there is nothing left worth forwarding, and restarting the copy
/// would append bytes after the Close frame it just wrote.
async fn close_tunnel<C, U>(client: &mut C, upstream: &mut U, grace: Duration)
where
    C: AsyncWrite + Unpin,
    U: AsyncWrite + Unpin,
{
    let toward_client = going_away_frame(None);
    let toward_upstream = going_away_frame(Some(rand_mask_key()));

    tokio::join!(
        announce_close(client, &toward_client, "client", grace),
        announce_close(upstream, &toward_upstream, "upstream", grace),
    );
}

/// Deliver one Close frame to one peer, then half-close that write half, all
/// bounded by `grace`.
///
/// The bound covers every step deliberately: a peer that has stopped reading
/// would block `write_all` forever, and since the bridge task is detached
/// (`api::rest::handlers::proxy`), that would strand the task, the upstream
/// stream and the active-session gauge for the process's lifetime.
///
/// Failures are logged rather than returned — the session is over either way,
/// and a peer that cannot receive its Close frame gets the half-close, or
/// failing that the tunnel drop, as the fallback signal.
async fn announce_close(
    io: &mut (impl AsyncWrite + Unpin),
    frame: &[u8],
    side: &'static str,
    grace: Duration,
) {
    let announced = tokio::time::timeout(grace, async {
        io.write_all(frame).await?;
        io.flush().await?;
        // Half-close so the peer observes EOF promptly rather than waiting for
        // the tunnel to be dropped.
        io.shutdown().await
    })
    .await;

    match announced {
        Ok(Ok(())) => {}
        Ok(Err(e)) => debug!(error = %e, side, "failed to announce Close 1001"),
        Err(_elapsed) => debug!(
            side,
            grace_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX),
            "close announcement did not complete within the grace period"
        ),
    }
}

// ---------------------------------------------------------------------------
// Bridge plumbing
// ---------------------------------------------------------------------------

/// Carries the `DuplexStream` and leftover bytes from a successful 101 response
/// through the response extensions, so the Axum handler can bridge the
/// client's upgraded connection to the Pingora-managed upstream tunnel.
pub(crate) struct WebSocketBridgeIo {
    pub io: tokio::io::DuplexStream,
    pub leftover: Bytes,
    pub idle_timeout: Duration,
    pub close_timeout: Duration,
    pub shutdown_rx: watch::Receiver<bool>,
    /// Metrics port used to track session lifetime. May be `None` in tests
    /// that don't exercise metrics — the bridge still runs, just without
    /// recording.
    pub metrics: Option<Arc<dyn OagwMetricsPort>>,
    /// Upstream alias (`host` label) for session metrics. Empty when
    /// `metrics` is `None`.
    pub host: String,
}

/// RAII guard for an active WebSocket session.
///
/// Increments `active_websocket_sessions` on construction and, on drop,
/// decrements the gauge and records `websocket_session_duration_seconds`.
/// Drop semantics ensure the gauge cannot leak even if the bridge future
/// is cancelled or panics — `Drop` runs in both cases.
struct WsSessionGuard {
    metrics: Arc<dyn OagwMetricsPort>,
    host: String,
    start: Instant,
}

impl WsSessionGuard {
    fn new(metrics: Arc<dyn OagwMetricsPort>, host: String) -> Self {
        metrics.increment_active_websocket_sessions(&host);
        Self {
            metrics,
            host,
            start: Instant::now(),
        }
    }
}

impl Drop for WsSessionGuard {
    fn drop(&mut self) {
        let duration = self.start.elapsed().as_secs_f64();
        self.metrics.decrement_active_websocket_sessions(&self.host);
        self.metrics
            .record_websocket_session_duration_seconds(&self.host, duration);
    }
}

/// Wrapper that satisfies `Clone + Send + Sync + 'static` required by
/// `http::Extensions::insert`. The inner value is taken once by the handler.
#[derive(Clone)]
pub(crate) struct WebSocketBridgeHandle(pub Arc<Mutex<Option<WebSocketBridgeIo>>>);

impl WebSocketBridgeHandle {
    pub fn new(bridge: WebSocketBridgeIo) -> Self {
        Self(Arc::new(Mutex::new(Some(bridge))))
    }

    /// Take the bridge IO out of the handle. Returns `None` if already taken.
    pub fn take(&self) -> Option<WebSocketBridgeIo> {
        self.0.lock().take()
    }
}

/// Bridge a client's upgraded connection to the Pingora-managed upstream
/// tunnel.
///
/// Any bytes read past the header boundary while parsing the 101 response
/// already belong to the caller. They go in as the relay's leftover prefix
/// rather than being written here: that keeps their ordering ahead of
/// everything the upstream sends next, and leaves the relay owning every
/// bounded write, the shutdown arm and the Close 1001 teardown — so a caller
/// that never reads them cannot wedge this task, and with it the session guard,
/// for the process's lifetime.
pub(crate) async fn websocket_bridge(
    upgraded: hyper::upgrade::Upgraded,
    bridge: WebSocketBridgeIo,
) {
    use hyper_util::rt::TokioIo;

    let WebSocketBridgeIo {
        io,
        leftover,
        idle_timeout,
        close_timeout,
        shutdown_rx,
        metrics,
        host,
    } = bridge;
    // RAII guard: increments the active-sessions gauge now, records duration
    // and decrements on drop — even on panic / future cancellation.
    let _session_guard = metrics.map(|m| WsSessionGuard::new(m, host));

    // Wrap hyper's Upgraded in TokioIo so it implements tokio::io::AsyncRead/Write.
    let mut client = TokioIo::new(upgraded);
    let mut upstream = io;

    match relay(
        &mut client,
        &mut upstream,
        RelayConfig {
            leftover,
            idle_timeout,
            close_timeout,
            shutdown_rx,
        },
    )
    .await
    {
        RelayOutcome::Closed {
            to_upstream,
            to_client,
            ended_by: TunnelEnd::Upstream,
        } => warn!(
            to_upstream,
            to_client, "upstream WebSocket connection dropped unexpectedly"
        ),
        RelayOutcome::Closed {
            to_upstream,
            to_client,
            ended_by,
        } => debug!(to_upstream, to_client, ?ended_by, "WebSocket tunnel closed"),
        RelayOutcome::IdleTimeout => debug!("WebSocket idle timeout, closing"),
        RelayOutcome::Shutdown => debug!("WebSocket closed due to server shutdown"),
        RelayOutcome::Error(e) => warn!(error = %e, "WebSocket bridge error"),
    }
}

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod websocket_tests;
