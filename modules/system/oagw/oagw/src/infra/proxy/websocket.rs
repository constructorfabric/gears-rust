use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, Bytes};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::watch;
use tracing::{debug, warn};

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
    } = bridge;
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
#[path = "websocket_tests.rs"]
mod websocket_tests;
