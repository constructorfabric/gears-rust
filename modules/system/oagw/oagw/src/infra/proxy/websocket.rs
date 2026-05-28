use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::domain::ports::OagwMetricsPort;

// ---------------------------------------------------------------------------
// PrefixedReader — prepends buffered bytes before an inner AsyncRead
// ---------------------------------------------------------------------------

/// An `AsyncRead` wrapper that first yields bytes from a `Bytes` prefix,
/// then delegates to the inner reader. Used to feed leftover bytes from
/// HTTP header parsing back into the WebSocket frame relay loop.
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

impl<R: Unpin> Unpin for PrefixedReader<R> {}

// ---------------------------------------------------------------------------
// RFC 6455 frame parser/writer
// ---------------------------------------------------------------------------

/// Absolute maximum frame payload size (64 MiB). Defense-in-depth cap
/// applied before allocation, regardless of the configured `max_frame_size`.
const HARD_MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

/// WebSocket opcodes (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WsOpcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
    Unknown(u8),
}

impl WsOpcode {
    fn from_u8(v: u8) -> Self {
        match v {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xA => Self::Pong,
            other => Self::Unknown(other),
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Continuation => 0x0,
            Self::Text => 0x1,
            Self::Binary => 0x2,
            Self::Close => 0x8,
            Self::Ping => 0x9,
            Self::Pong => 0xA,
            Self::Unknown(v) => v,
        }
    }
}

/// Read a single WebSocket frame from `reader`.
///
/// Returns `None` on clean EOF (zero-byte read on the first header byte).
/// Returns `(fin, opcode, payload)` — the FIN bit is preserved for
/// fragmented message forwarding (RFC 6455 §5.4).
///
/// `max_payload` caps the allocation size before reading. If the declared
/// payload length exceeds `min(max_payload, HARD_MAX_FRAME_SIZE)`, an
/// `InvalidData` error is returned without allocating. Unmasked payload is
/// always returned regardless of wire masking.
async fn read_frame(
    reader: &mut (impl AsyncRead + Unpin),
    max_payload: Option<usize>,
) -> std::io::Result<Option<(bool, WsOpcode, Vec<u8>)>> {
    // Read the 2-byte header.
    let mut hdr = [0u8; 2];
    match reader.read(&mut hdr[..1]).await? {
        0 => return Ok(None), // clean EOF
        1 => {}
        _ => unreachable!(),
    }
    reader.read_exact(&mut hdr[1..2]).await?;

    let fin = hdr[0] & 0x80 != 0;
    let opcode = WsOpcode::from_u8(hdr[0] & 0x0F);
    let masked = hdr[1] & 0x80 != 0;
    let len_byte = (hdr[1] & 0x7F) as u64;

    let payload_len: usize = if len_byte < 126 {
        len_byte as usize
    } else if len_byte == 126 {
        let mut buf = [0u8; 2];
        reader.read_exact(&mut buf).await?;
        u16::from_be_bytes(buf) as usize
    } else {
        // len_byte == 127
        let mut buf = [0u8; 8];
        reader.read_exact(&mut buf).await?;
        u64::from_be_bytes(buf) as usize
    };

    // Enforce size limit before allocation to prevent OOM.
    let effective_max = max_payload
        .map(|m| m.min(HARD_MAX_FRAME_SIZE))
        .unwrap_or(HARD_MAX_FRAME_SIZE);
    if payload_len > effective_max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame payload {payload_len} bytes exceeds maximum {effective_max} bytes"),
        ));
    }

    let mask_key = if masked {
        let mut key = [0u8; 4];
        reader.read_exact(&mut key).await?;
        Some(key)
    } else {
        None
    };

    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        reader.read_exact(&mut payload).await?;
    }

    // Unmask if needed.
    if let Some(key) = mask_key {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= key[i % 4];
        }
    }

    Ok(Some((fin, opcode, payload)))
}

/// Write a single WebSocket frame to `writer`.
///
/// The `fin` parameter controls the FIN bit — pass `true` for final/only
/// frames, `false` for non-final fragments (RFC 6455 §5.4).
/// If `masked` is true, applies a random 4-byte XOR mask (required for
/// client-to-server direction per RFC 6455 §5.3).
async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    opcode: WsOpcode,
    payload: &[u8],
    masked: bool,
    fin: bool,
) -> std::io::Result<()> {
    let len = payload.len();
    // Pre-allocate: 2 header + 8 extended length + 4 mask + payload
    let mut buf = Vec::with_capacity(14 + len);

    let fin_bit = if fin { 0x80 } else { 0x00 };
    buf.push(fin_bit | opcode.as_u8());

    let mask_bit = if masked { 0x80 } else { 0x00 };
    if len < 126 {
        buf.push(mask_bit | len as u8);
    } else if len < 65536 {
        buf.push(mask_bit | 126);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(mask_bit | 127);
        buf.extend_from_slice(&(len as u64).to_be_bytes());
    }

    if masked {
        let key: [u8; 4] = rand_mask_key();
        buf.extend_from_slice(&key);
        for (i, &byte) in payload.iter().enumerate() {
            buf.push(byte ^ key[i % 4]);
        }
    } else {
        buf.extend_from_slice(payload);
    }

    writer.write_all(&buf).await
}

/// Build a Close frame payload: 2-byte BE status code + UTF-8 reason.
fn make_close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + reason.len());
    payload.extend_from_slice(&code.to_be_bytes());
    payload.extend_from_slice(reason.as_bytes());
    payload
}

/// Extract status code and reason bytes from a Close frame payload.
#[cfg(test)]
fn parse_close_payload(payload: &[u8]) -> (u16, &[u8]) {
    if payload.len() >= 2 {
        let code = u16::from_be_bytes([payload[0], payload[1]]);
        (code, &payload[2..])
    } else {
        (1005, b"") // No status code provided (RFC 6455 §7.1.5)
    }
}

/// Generate a 4-byte mask key using OS-seeded randomness.
///
/// Uses `RandomState` (SipHash with OS-random seeds) per thread, hashing
/// an atomic counter through it. Satisfies RFC 6455 §5.3 requirement that
/// mask keys be chosen unpredictably.
fn rand_mask_key() -> [u8; 4] {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

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
// Frame-aware relay
// ---------------------------------------------------------------------------

/// Outcome of the frame relay loop.
#[derive(Debug)]
pub(crate) enum RelayOutcome {
    /// Both sides completed the Close handshake.
    CleanClose,
    /// No data in either direction within the idle timeout.
    IdleTimeout,
    /// Upstream connection dropped unexpectedly.
    UpstreamDrop,
    /// Caller disconnected unexpectedly.
    CallerDrop,
    /// A frame exceeded the configured max size.
    FrameTooLarge,
    /// Server is shutting down gracefully.
    Shutdown,
    /// IO or protocol error.
    Error(std::io::Error),
}

/// Configuration for the frame relay loop.
struct RelayConfig {
    idle_timeout: Duration,
    close_timeout: Duration,
    max_frame_size: Option<usize>,
    shutdown_rx: watch::Receiver<bool>,
}

/// Frame-aware WebSocket relay with idle timeout, close handshake,
/// and optional max frame size enforcement.
///
/// Forwards frames bidirectionally between client and upstream, preserving
/// FIN bits and continuation frames for fragmented messages (RFC 6455 §5.4).
/// Client→upstream frames are re-masked; upstream→client frames are unmasked.
async fn frame_relay(
    client_read: &mut (impl AsyncRead + Unpin),
    client_write: &mut (impl AsyncWrite + Unpin),
    upstream_read: &mut (impl AsyncRead + Unpin),
    upstream_write: &mut (impl AsyncWrite + Unpin),
    cfg: RelayConfig,
) -> RelayOutcome {
    let RelayConfig {
        idle_timeout,
        close_timeout,
        max_frame_size,
        mut shutdown_rx,
    } = cfg;
    let deadline = tokio::time::sleep(idle_timeout);
    tokio::pin!(deadline);

    // Main relay loop (Open state).
    loop {
        tokio::select! {
            result = read_frame(client_read, max_frame_size) => {
                match result {
                    Ok(Some((fin, opcode, payload))) => {
                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        match opcode {
                            WsOpcode::Close => {
                                // Forward close to upstream, then enter closing state.
                                let _ = write_frame(upstream_write, WsOpcode::Close, &payload, true, true).await;
                                return await_close_response(upstream_read, close_timeout).await;
                            }
                            WsOpcode::Text | WsOpcode::Binary | WsOpcode::Continuation => {
                                if let Err(e) = write_frame(upstream_write, opcode, &payload, true, fin).await {
                                    debug!(error = %e, "failed to forward frame to upstream");
                                    return RelayOutcome::Error(e);
                                }
                            }
                            WsOpcode::Ping | WsOpcode::Pong => {
                                let _ = write_frame(upstream_write, opcode, &payload, true, true).await;
                            }
                            WsOpcode::Unknown(_) => {
                                // Forward unknown opcodes transparently.
                                let _ = write_frame(upstream_write, opcode, &payload, true, fin).await;
                            }
                        }
                    }
                    Ok(None) => {
                        // Client EOF — send Close to upstream.
                        let close = make_close_payload(1001, "Going Away");
                        let _ = write_frame(upstream_write, WsOpcode::Close, &close, true, true).await;
                        return RelayOutcome::CallerDrop;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        // Frame exceeded max size — send Close 1009 to both sides.
                        let close = make_close_payload(1009, "Message Too Big");
                        let _ = write_frame(client_write, WsOpcode::Close, &close, false, true).await;
                        let _ = write_frame(upstream_write, WsOpcode::Close, &close, true, true).await;
                        return RelayOutcome::FrameTooLarge;
                    }
                    Err(_) => {
                        let close = make_close_payload(1001, "Going Away");
                        let _ = write_frame(upstream_write, WsOpcode::Close, &close, true, true).await;
                        return RelayOutcome::CallerDrop;
                    }
                }
            }
            result = read_frame(upstream_read, max_frame_size) => {
                match result {
                    Ok(Some((fin, opcode, payload))) => {
                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        match opcode {
                            WsOpcode::Close => {
                                // Forward close to client, then enter closing state.
                                let _ = write_frame(client_write, WsOpcode::Close, &payload, false, true).await;
                                return await_close_response(client_read, close_timeout).await;
                            }
                            WsOpcode::Text | WsOpcode::Binary | WsOpcode::Continuation => {
                                if let Err(e) = write_frame(client_write, opcode, &payload, false, fin).await {
                                    debug!(error = %e, "failed to forward frame to client");
                                    return RelayOutcome::Error(e);
                                }
                            }
                            WsOpcode::Ping | WsOpcode::Pong => {
                                let _ = write_frame(client_write, opcode, &payload, false, true).await;
                            }
                            WsOpcode::Unknown(_) => {
                                let _ = write_frame(client_write, opcode, &payload, false, fin).await;
                            }
                        }
                    }
                    Ok(None) => {
                        // Upstream EOF — send Close 1001 to client.
                        // RFC 6455 §7.4.1: status 1006 MUST NOT be sent on the wire;
                        // use 1001 (Going Away) instead.
                        let close = make_close_payload(1001, "Going Away");
                        let _ = write_frame(client_write, WsOpcode::Close, &close, false, true).await;
                        return RelayOutcome::UpstreamDrop;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        // Upstream frame exceeded max size — send Close 1009 to upstream
                        // and close the client side.
                        let close = make_close_payload(1009, "Message Too Big");
                        let _ = write_frame(upstream_write, WsOpcode::Close, &close, true, true).await;
                        let _ = write_frame(client_write, WsOpcode::Close, &close, false, true).await;
                        return RelayOutcome::FrameTooLarge;
                    }
                    Err(_) => {
                        let close = make_close_payload(1001, "Going Away");
                        let _ = write_frame(client_write, WsOpcode::Close, &close, false, true).await;
                        return RelayOutcome::UpstreamDrop;
                    }
                }
            }
            _ = &mut deadline => {
                // Idle timeout — send Close 1001 to both sides.
                let close = make_close_payload(1001, "Going Away");
                let _ = write_frame(client_write, WsOpcode::Close, &close, false, true).await;
                let _ = write_frame(upstream_write, WsOpcode::Close, &close, true, true).await;
                return RelayOutcome::IdleTimeout;
            }
            result = shutdown_rx.changed() => {
                // Graceful server shutdown — close both sides cleanly.
                if result.is_ok() && *shutdown_rx.borrow() {
                    let close = make_close_payload(1001, "Going Away");
                    let _ = write_frame(client_write, WsOpcode::Close, &close, false, true).await;
                    let _ = write_frame(upstream_write, WsOpcode::Close, &close, true, true).await;
                    return RelayOutcome::Shutdown;
                }
            }
        }
    }
}

/// Wait for a Close frame response from `reader`, up to `timeout`.
///
/// Loops past non-Close frames (Ping, Pong, data) that the peer may send
/// before responding with Close (permitted by RFC 6455 §5.5.1). Returns
/// `CleanClose` regardless of whether the Close response arrives in time.
async fn await_close_response(
    reader: &mut (impl AsyncRead + Unpin),
    timeout: Duration,
) -> RelayOutcome {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            debug!("close handshake timed out");
            break;
        }
        // Close frame payload is at most 125 bytes per RFC 6455 §5.5.
        match tokio::time::timeout(remaining, read_frame(reader, Some(125))).await {
            Ok(Ok(Some((_, WsOpcode::Close, _)))) => {
                debug!("received Close response, completing handshake");
                break;
            }
            Ok(Ok(Some(_))) => {
                debug!("received non-Close frame during close handshake, skipping");
                continue;
            }
            Ok(Ok(None)) => {
                debug!("connection closed during close handshake");
                break;
            }
            Ok(Err(e)) => {
                debug!(error = %e, "error during close handshake");
                break;
            }
            Err(_) => {
                debug!("close handshake timed out");
                break;
            }
        }
    }
    RelayOutcome::CleanClose
}

// ---------------------------------------------------------------------------
// Bridge types and entry point
// ---------------------------------------------------------------------------

/// Carries the DuplexStream and leftover bytes from a successful 101 response
/// through the response extensions, so the Axum handler can bridge the
/// client's upgraded connection to the Pingora-managed upstream tunnel.
pub(crate) struct WebSocketBridgeIo {
    pub io: tokio::io::DuplexStream,
    pub leftover: Bytes,
    pub idle_timeout: Duration,
    pub close_timeout: Duration,
    pub max_frame_size: Option<usize>,
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
/// tunnel via frame-aware WebSocket relay.
///
/// Any leftover bytes read past the header boundary during 101 response
/// parsing are prepended to the upstream read stream via [`PrefixedReader`],
/// so they enter the frame relay loop and are parsed as WebSocket frames
/// rather than being written raw to the client.
pub(crate) async fn websocket_bridge(
    upgraded: hyper::upgrade::Upgraded,
    bridge: WebSocketBridgeIo,
) {
    use hyper_util::rt::TokioIo;
    use tokio::io::split;

    let WebSocketBridgeIo {
        io,
        leftover,
        idle_timeout,
        close_timeout,
        max_frame_size,
        shutdown_rx,
        metrics,
        host,
    } = bridge;
    // RAII guard: increments the active-sessions gauge now, records duration
    // and decrements on drop — even on panic / future cancellation.
    let _session_guard = metrics.map(|m| WsSessionGuard::new(m, host));
    let (upstream_read, mut upstream_write) = split(io);
    // Prepend any leftover bytes from 101 header parsing to the upstream
    // read stream so they are parsed as WebSocket frames by the relay.
    let mut upstream_read = PrefixedReader::new(leftover, upstream_read);
    // Wrap hyper's Upgraded in TokioIo so it implements tokio::io::AsyncRead/Write.
    let tokio_upgraded = TokioIo::new(upgraded);
    let (mut client_read, mut client_write) = split(tokio_upgraded);

    match frame_relay(
        &mut client_read,
        &mut client_write,
        &mut upstream_read,
        &mut upstream_write,
        RelayConfig {
            idle_timeout,
            close_timeout,
            max_frame_size,
            shutdown_rx,
        },
    )
    .await
    {
        RelayOutcome::CleanClose => debug!("WebSocket closed normally"),
        RelayOutcome::IdleTimeout => debug!("WebSocket idle timeout, closing"),
        RelayOutcome::UpstreamDrop => warn!("upstream WebSocket connection dropped unexpectedly"),
        RelayOutcome::CallerDrop => debug!("caller disconnected"),
        RelayOutcome::FrameTooLarge => warn!("WebSocket frame exceeded max size"),
        RelayOutcome::Shutdown => debug!("WebSocket closed due to server shutdown"),
        RelayOutcome::Error(e) => debug!(error = %e, "WebSocket bridge error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a no-op shutdown receiver for tests that don't exercise shutdown.
    fn test_shutdown_rx() -> watch::Receiver<bool> {
        let (_tx, rx) = watch::channel(false);
        rx
    }

    // -- Frame parser/writer tests --

    #[tokio::test]
    async fn write_and_read_text_frame() {
        let (mut writer_end, mut reader_end) = tokio::io::duplex(4096);
        write_frame(&mut writer_end, WsOpcode::Text, b"hello", false, true)
            .await
            .unwrap();
        drop(writer_end);

        let (fin, op, payload) = read_frame(&mut reader_end, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Text);
        assert_eq!(payload, b"hello");
    }

    #[tokio::test]
    async fn write_and_read_close_frame() {
        let (mut writer_end, mut reader_end) = tokio::io::duplex(4096);
        let payload = make_close_payload(1000, "Normal Closure");
        write_frame(&mut writer_end, WsOpcode::Close, &payload, false, true)
            .await
            .unwrap();
        drop(writer_end);

        let (fin, op, data) = read_frame(&mut reader_end, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Close);
        let (code, reason) = parse_close_payload(&data);
        assert_eq!(code, 1000);
        assert_eq!(reason, b"Normal Closure");
    }

    #[tokio::test]
    async fn read_masked_frame() {
        let (mut writer_end, mut reader_end) = tokio::io::duplex(4096);
        // Write a masked frame.
        write_frame(
            &mut writer_end,
            WsOpcode::Binary,
            b"masked data",
            true,
            true,
        )
        .await
        .unwrap();
        drop(writer_end);

        // read_frame should unmask automatically.
        let (fin, op, payload) = read_frame(&mut reader_end, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Binary);
        assert_eq!(payload, b"masked data");
    }

    #[tokio::test]
    async fn extended_length_payloads() {
        // 126-byte extended length (2-byte encoding).
        let payload_126 = vec![0xAB; 200];
        let (mut w, mut r) = tokio::io::duplex(4096);
        write_frame(&mut w, WsOpcode::Binary, &payload_126, false, true)
            .await
            .unwrap();
        drop(w);
        let (_, _, data) = read_frame(&mut r, None).await.unwrap().unwrap();
        assert_eq!(data.len(), 200);
        assert_eq!(data, payload_126);

        // 127-byte extended length (8-byte encoding) — use 70000 bytes.
        let payload_127 = vec![0xCD; 70_000];
        let (mut w, mut r) = tokio::io::duplex(256 * 1024);
        write_frame(&mut w, WsOpcode::Binary, &payload_127, false, true)
            .await
            .unwrap();
        drop(w);
        let (_, _, data) = read_frame(&mut r, None).await.unwrap().unwrap();
        assert_eq!(data.len(), 70_000);
        assert_eq!(data, payload_127);
    }

    #[tokio::test]
    async fn parse_close_payload_no_code() {
        let (code, reason) = parse_close_payload(b"");
        assert_eq!(code, 1005);
        assert!(reason.is_empty());
    }

    #[tokio::test]
    async fn eof_returns_none() {
        let (writer_end, mut reader_end) = tokio::io::duplex(4096);
        drop(writer_end);
        let result = read_frame(&mut reader_end, None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_payload() {
        let (mut writer_end, mut reader_end) = tokio::io::duplex(4096);
        // Write a frame with 200-byte payload.
        write_frame(&mut writer_end, WsOpcode::Text, &[0xAA; 200], false, true)
            .await
            .unwrap();
        drop(writer_end);

        // Reading with max_payload=50 should fail before allocation.
        let err = read_frame(&mut reader_end, Some(50)).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn fin_bit_preserved_for_fragments() {
        let (mut writer_end, mut reader_end) = tokio::io::duplex(4096);

        // Write a non-final text frame (FIN=0).
        write_frame(&mut writer_end, WsOpcode::Text, b"part1", false, false)
            .await
            .unwrap();
        // Write a continuation frame (FIN=0).
        write_frame(
            &mut writer_end,
            WsOpcode::Continuation,
            b"part2",
            false,
            false,
        )
        .await
        .unwrap();
        // Write a final continuation frame (FIN=1).
        write_frame(
            &mut writer_end,
            WsOpcode::Continuation,
            b"part3",
            false,
            true,
        )
        .await
        .unwrap();
        drop(writer_end);

        let (fin, op, payload) = read_frame(&mut reader_end, None).await.unwrap().unwrap();
        assert!(!fin);
        assert_eq!(op, WsOpcode::Text);
        assert_eq!(payload, b"part1");

        let (fin, op, payload) = read_frame(&mut reader_end, None).await.unwrap().unwrap();
        assert!(!fin);
        assert_eq!(op, WsOpcode::Continuation);
        assert_eq!(payload, b"part2");

        let (fin, op, payload) = read_frame(&mut reader_end, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Continuation);
        assert_eq!(payload, b"part3");
    }

    // -- Frame relay tests --

    #[tokio::test]
    async fn relay_close_propagation() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client sends Close 1000.
        let close = make_close_payload(1000, "Normal");
        write_frame(&mut client_a, WsOpcode::Close, &close, true, true)
            .await
            .unwrap();

        // Upstream should receive the forwarded Close.
        let (fin, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1000);

        // Upstream sends Close response.
        let resp_close = make_close_payload(1000, "Normal");
        write_frame(&mut upstream_a, WsOpcode::Close, &resp_close, false, true)
            .await
            .unwrap();

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    #[tokio::test]
    async fn relay_upstream_drop_sends_1001() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Drop upstream — triggers EOF.
        drop(upstream_a);

        // Client should receive Close 1001 (not 1006, which MUST NOT be sent
        // on the wire per RFC 6455 §7.4.1).
        let (fin, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1001);

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::UpstreamDrop));
    }

    #[tokio::test]
    async fn relay_idle_timeout_sends_1001() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_millis(50),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::IdleTimeout));

        // Both sides should have received Close 1001.
        let (_, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1001);

        let (_, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1001);
    }

    #[tokio::test]
    async fn relay_shutdown_sends_1001_to_both_sides() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx,
                },
            )
            .await
        });

        // Signal shutdown.
        shutdown_tx.send(true).unwrap();

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::Shutdown));

        // Both sides should have received Close 1001 (Going Away).
        let (_, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1001);

        let (_, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1001);
    }

    #[tokio::test]
    async fn relay_max_frame_size_sends_1009() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: Some(10), // max 10 bytes
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client sends a frame exceeding the limit.
        write_frame(
            &mut client_a,
            WsOpcode::Text,
            b"this is way too long",
            true,
            true,
        )
        .await
        .unwrap();

        // Client should receive Close 1009.
        let (_, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1009);

        // Upstream should also receive Close 1009.
        let (_, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1009);

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::FrameTooLarge));
    }

    #[tokio::test]
    async fn relay_close_timeout_enforced() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (_upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_millis(50), // very short close timeout
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client sends Close. The upstream side (_upstream_a) never responds.
        let close = make_close_payload(1000, "bye");
        write_frame(&mut client_a, WsOpcode::Close, &close, true, true)
            .await
            .unwrap();

        // Should still complete with CleanClose after close timeout.
        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    // -- PrefixedReader tests --

    #[tokio::test]
    async fn prefixed_reader_yields_prefix_then_inner() {
        let prefix = Bytes::from_static(b"prefix-");
        let inner = &b"inner"[..];
        let mut reader = PrefixedReader::new(prefix, inner);

        let mut buf = vec![0u8; 32];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"prefix-");

        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"inner");
    }

    #[tokio::test]
    async fn prefixed_reader_empty_prefix_reads_inner_directly() {
        let prefix = Bytes::new();
        let inner = &b"data"[..];
        let mut reader = PrefixedReader::new(prefix, inner);

        let mut buf = vec![0u8; 32];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[tokio::test]
    async fn relay_leftover_bytes_forwarded_as_frames() {
        // Simulate leftover bytes from 101 header parsing: a complete
        // upstream WebSocket text frame sitting in the leftover buffer.
        let mut leftover_buf = Vec::new();
        // Build an unmasked text frame with payload "from-upstream".
        let payload = b"from-upstream";
        leftover_buf.push(0x81); // FIN + text opcode
        leftover_buf.push(payload.len() as u8); // no mask, len < 126
        leftover_buf.extend_from_slice(payload);
        let leftover = Bytes::from(leftover_buf);

        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (ur, mut uw) = tokio::io::split(upstream_b);
        // Wrap upstream_read with leftover bytes, same as websocket_bridge does.
        let mut ur = PrefixedReader::new(leftover, ur);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client should receive the leftover frame parsed as a proper text frame.
        let (fin, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Text);
        assert_eq!(data, b"from-upstream");

        // Clean up: send Close from client to terminate the relay.
        let close = make_close_payload(1000, "done");
        write_frame(&mut client_a, WsOpcode::Close, &close, true, true)
            .await
            .unwrap();

        // Upstream receives the forwarded Close.
        let (_, op, _) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);

        // Respond with Close to complete handshake.
        let resp = make_close_payload(1000, "done");
        write_frame(&mut upstream_a, WsOpcode::Close, &resp, false, true)
            .await
            .unwrap();

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    // -- await_close_response loop tests --

    #[tokio::test]
    async fn await_close_response_skips_non_close_frames() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        let (mut reader, _) = tokio::io::split(reader);

        // Write a Pong frame, a Text frame, then a Close frame.
        write_frame(&mut writer, WsOpcode::Pong, b"", false, true)
            .await
            .unwrap();
        write_frame(&mut writer, WsOpcode::Text, b"queued", false, true)
            .await
            .unwrap();
        let close = make_close_payload(1000, "bye");
        write_frame(&mut writer, WsOpcode::Close, &close, false, true)
            .await
            .unwrap();

        let outcome = await_close_response(&mut reader, Duration::from_secs(2)).await;
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    #[tokio::test]
    async fn await_close_response_timeout_with_only_data_frames() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        let (mut reader, _) = tokio::io::split(reader);

        // Write a few Pong frames but never a Close.
        for _ in 0..3 {
            write_frame(&mut writer, WsOpcode::Pong, b"", false, true)
                .await
                .unwrap();
        }
        // Keep writer alive so reads block (no EOF).
        let _writer = writer;

        let outcome = await_close_response(&mut reader, Duration::from_millis(50)).await;
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    // -- Caller disconnect tests --

    #[tokio::test]
    async fn relay_caller_drop_sends_close_to_upstream() {
        let (client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Drop client mid-session — triggers EOF on the client read side.
        drop(client_a);

        // Upstream should receive a Close 1001 (Going Away).
        let (_, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1001);

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CallerDrop));
    }

    #[tokio::test]
    async fn relay_caller_drop_during_upstream_activity() {
        // Upstream is actively sending data when the caller disconnects.
        let (client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Upstream sends a message first.
        write_frame(&mut upstream_a, WsOpcode::Text, b"data", false, true)
            .await
            .unwrap();

        // Small delay to let the relay forward it, then drop client.
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(client_a);

        // Upstream should receive Close 1001.
        // May need to read past the frame that was in flight.
        loop {
            match read_frame(&mut upstream_a, None).await {
                Ok(Some((_, WsOpcode::Close, data))) => {
                    let (code, _) = parse_close_payload(&data);
                    assert_eq!(code, 1001);
                    break;
                }
                Ok(Some(_)) => continue,    // skip in-flight frames
                Ok(None) | Err(_) => break, // EOF is also acceptable
            }
        }

        let outcome = handle.await.unwrap();
        assert!(matches!(
            outcome,
            RelayOutcome::CallerDrop | RelayOutcome::Error(_)
        ));
    }

    // -- Upstream Close during active client transmission --

    #[tokio::test]
    async fn relay_upstream_sends_close_while_client_is_sending() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client sends a text frame.
        write_frame(&mut client_a, WsOpcode::Text, b"hello", true, true)
            .await
            .unwrap();

        // Upstream receives the frame...
        let (_, op, _) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Text);

        // ...then upstream initiates Close while client might still be sending.
        let close = make_close_payload(1000, "Server done");
        write_frame(&mut upstream_a, WsOpcode::Close, &close, false, true)
            .await
            .unwrap();

        // Client should receive the Close frame forwarded by the relay.
        let (_, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1000);

        // Client sends Close response to complete the handshake.
        let resp = make_close_payload(1000, "OK");
        write_frame(&mut client_a, WsOpcode::Close, &resp, true, true)
            .await
            .unwrap();

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    // -- Ping/Pong forwarding --

    #[tokio::test]
    async fn relay_ping_forwarded_to_upstream() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client sends Ping.
        write_frame(&mut client_a, WsOpcode::Ping, b"ping-data", true, true)
            .await
            .unwrap();

        // Upstream should receive the Ping.
        let (_, op, payload) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Ping);
        assert_eq!(payload, b"ping-data");

        // Upstream sends Pong back.
        write_frame(&mut upstream_a, WsOpcode::Pong, b"pong-data", false, true)
            .await
            .unwrap();

        // Client should receive the Pong.
        let (_, op, payload) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Pong);
        assert_eq!(payload, b"pong-data");

        // Verify Ping resets the idle timer by sending after a short pause.
        tokio::time::sleep(Duration::from_millis(10)).await;
        write_frame(&mut client_a, WsOpcode::Ping, b"", true, true)
            .await
            .unwrap();
        let (_, op, _) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Ping);

        // Clean up.
        let close = make_close_payload(1000, "done");
        write_frame(&mut client_a, WsOpcode::Close, &close, true, true)
            .await
            .unwrap();
        let _ = read_frame(&mut upstream_a, None).await; // consume forwarded Close
        write_frame(&mut upstream_a, WsOpcode::Close, &close, false, true)
            .await
            .unwrap();

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    // -- Upstream oversized frame sends 1009 --

    #[tokio::test]
    async fn relay_upstream_oversized_frame_sends_1009() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: Some(10), // max 10 bytes
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Upstream sends an oversized frame.
        write_frame(&mut upstream_a, WsOpcode::Text, &[0xBB; 50], false, true)
            .await
            .unwrap();

        // Upstream should receive Close 1009.
        let (_, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1009);

        // Client should also receive Close 1009.
        let (_, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert_eq!(op, WsOpcode::Close);
        let (code, _) = parse_close_payload(&data);
        assert_eq!(code, 1009);

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::FrameTooLarge));
    }

    // -- Fragmented message relay --

    #[tokio::test]
    async fn relay_fragmented_message_preserved() {
        // Verify that fragmented messages (FIN=0 + continuation frames)
        // are forwarded through the relay with FIN bits intact.
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client sends a fragmented text message: non-final + continuation + final.
        write_frame(&mut client_a, WsOpcode::Text, b"frag1", true, false)
            .await
            .unwrap();
        write_frame(&mut client_a, WsOpcode::Continuation, b"frag2", true, false)
            .await
            .unwrap();
        write_frame(&mut client_a, WsOpcode::Continuation, b"frag3", true, true)
            .await
            .unwrap();

        // Upstream should receive all three fragments with FIN bits preserved.
        let (fin, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert!(!fin);
        assert_eq!(op, WsOpcode::Text);
        assert_eq!(data, b"frag1");

        let (fin, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert!(!fin);
        assert_eq!(op, WsOpcode::Continuation);
        assert_eq!(data, b"frag2");

        let (fin, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Continuation);
        assert_eq!(data, b"frag3");

        // Clean up.
        let close = make_close_payload(1000, "done");
        write_frame(&mut client_a, WsOpcode::Close, &close, true, true)
            .await
            .unwrap();
        let _ = read_frame(&mut upstream_a, None).await;
        write_frame(&mut upstream_a, WsOpcode::Close, &close, false, true)
            .await
            .unwrap();

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    // -- Binary frame opcode preservation --

    #[tokio::test]
    async fn relay_binary_opcode_preserved() {
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_secs(5),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Client sends a binary frame.
        let binary_data = vec![0x00, 0xFF, 0x42, 0x13, 0x37];
        write_frame(&mut client_a, WsOpcode::Binary, &binary_data, true, true)
            .await
            .unwrap();

        // Upstream receives it as Binary (not Text).
        let (fin, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Binary);
        assert_eq!(data, binary_data);

        // Upstream responds with Binary.
        let response_data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        write_frame(
            &mut upstream_a,
            WsOpcode::Binary,
            &response_data,
            false,
            true,
        )
        .await
        .unwrap();

        // Client receives it as Binary.
        let (fin, op, data) = read_frame(&mut client_a, None).await.unwrap().unwrap();
        assert!(fin);
        assert_eq!(op, WsOpcode::Binary);
        assert_eq!(data, response_data);

        // Clean up.
        let close = make_close_payload(1000, "done");
        write_frame(&mut client_a, WsOpcode::Close, &close, true, true)
            .await
            .unwrap();
        let _ = read_frame(&mut upstream_a, None).await;
        write_frame(&mut upstream_a, WsOpcode::Close, &close, false, true)
            .await
            .unwrap();

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::CleanClose));
    }

    // -- Idle timer reset on activity --

    #[tokio::test]
    async fn relay_idle_timer_resets_on_data() {
        // With a 100ms idle timeout, send a frame every 60ms to keep the
        // connection alive — verify it survives past the original deadline.
        let (mut client_a, client_b) = tokio::io::duplex(4096);
        let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

        let (mut cr, mut cw) = tokio::io::split(client_b);
        let (mut ur, mut uw) = tokio::io::split(upstream_b);

        let handle = tokio::spawn(async move {
            frame_relay(
                &mut cr,
                &mut cw,
                &mut ur,
                &mut uw,
                RelayConfig {
                    idle_timeout: Duration::from_millis(100),
                    close_timeout: Duration::from_secs(2),
                    max_frame_size: None,
                    shutdown_rx: test_shutdown_rx(),
                },
            )
            .await
        });

        // Send 5 messages spaced 60ms apart. Total time ~300ms, well past
        // the 100ms idle timeout — but each message resets the timer.
        for i in 0..5 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let msg = format!("msg-{i}");
            write_frame(&mut client_a, WsOpcode::Text, msg.as_bytes(), true, true)
                .await
                .unwrap();
            let (_, op, data) = read_frame(&mut upstream_a, None).await.unwrap().unwrap();
            assert_eq!(op, WsOpcode::Text);
            assert_eq!(data, msg.as_bytes());
        }

        // Now stop sending and let the idle timeout fire.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, RelayOutcome::IdleTimeout));
    }
}
