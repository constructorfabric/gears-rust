use std::io::Write as _;

use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::unfold;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use oagw_sdk::body::{BodyStream, BoxError};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;
use tracing::warn;

/// Maximum size of response headers (64 KiB). Defense-in-depth cap on the
/// internal Pingora bridge; prevents unbounded memory growth if the upstream
/// (or Pingora itself) emits oversized headers.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Maximum size of a single chunked transfer-encoding chunk (8 MiB).
/// Defense-in-depth cap: prevents a malicious upstream from declaring an
/// enormous chunk size and causing unbounded memory allocation in the
/// chunked body decoder.
const MAX_CHUNK_SIZE: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Request serialization
// ---------------------------------------------------------------------------

/// Extract path and query from a full URL.
/// e.g. `"https://api.example.com/v1/chat?k=v"` → `"/v1/chat?k=v"`
fn url_path_and_query(url: &str) -> &str {
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        if let Some(path_start) = after_scheme.find('/') {
            return &after_scheme[path_start..];
        }
        return "/";
    }
    url
}

/// Serialize an HTTP/1.1 request to wire format.
///
/// - **`body = Some(bytes)`** (buffered path) — emits `Content-Length` and
///   appends the body after the blank line.
/// - **`body = None`** (streaming path) — emits `Transfer-Encoding: chunked`;
///   the caller writes each body piece in chunked encoding format and
///   terminates with the final chunk `0\r\n\r\n`. The write half of the
///   duplex must **not** be shut down — Pingora still needs the connection
///   open to relay the upstream response.
///
/// In both cases the function emits `Connection: close` (single-shot bridge,
/// no keep-alive). Any inbound `Content-Length`, `Connection`, or
/// `Transfer-Encoding` values carried in `headers` are stripped to prevent
/// duplicate framing headers.
pub(crate) fn serialize_request_wire(
    method: &Method,
    url: &str,
    headers: &HeaderMap,
    body: Option<&Bytes>,
) -> Vec<u8> {
    let body_len = body.map_or(0, |b| b.len());
    let mut buf = Vec::with_capacity(512 + body_len);
    let pq_raw = url_path_and_query(url);
    // Defense-in-depth: strip CR/LF to prevent header injection.
    // Upstream layers (http::Uri, form_urlencoded) already reject CRLF,
    // but this guards the raw write! interpolation against future misuse.
    let pq_clean;
    let pq = if pq_raw.contains('\r') || pq_raw.contains('\n') {
        warn!("CRLF in request URI stripped (possible injection attempt)");
        pq_clean = pq_raw.replace(['\r', '\n'], "");
        pq_clean.as_str()
    } else {
        pq_raw
    };
    let _ = write!(buf, "{} {} HTTP/1.1\r\n", method, pq);
    for (name, value) in headers {
        // Skip framing headers — authoritative values are appended below.
        if name == http::header::CONTENT_LENGTH
            || name == http::header::CONNECTION
            || name == http::header::TRANSFER_ENCODING
        {
            continue;
        }
        buf.extend_from_slice(name.as_str().as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(value.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    // Include Content-Length only for the buffered path so Pingora knows
    // the body boundary. The streaming path uses chunked transfer encoding.
    if let Some(b) = body {
        let _ = write!(buf, "Content-Length: {}\r\n", b.len());
    } else {
        buf.extend_from_slice(b"Transfer-Encoding: chunked\r\n");
    }
    // Single-shot bridge — no keep-alive on the in-memory session.
    buf.extend_from_slice(b"Connection: close\r\n");
    buf.extend_from_slice(b"\r\n");
    if let Some(b) = body {
        buf.extend_from_slice(b);
    }
    buf
}

/// Serialize an HTTP/1.1 upgrade request (WebSocket handshake) to wire format.
///
/// Differs from [`serialize_request_wire`]:
/// - Emits `Connection: Upgrade` (not `Connection: close`)
/// - No `Content-Length` or `Transfer-Encoding` (upgrade requests have no body)
/// - Preserves the `Upgrade` header from input
pub(crate) fn serialize_upgrade_request_wire(
    method: &Method,
    url: &str,
    headers: &HeaderMap,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);
    let pq_raw = url_path_and_query(url);
    // Defense-in-depth: strip CR/LF to prevent header injection.
    let pq_clean;
    let pq = if pq_raw.contains('\r') || pq_raw.contains('\n') {
        warn!("CRLF in request URI stripped (possible injection attempt)");
        pq_clean = pq_raw.replace(['\r', '\n'], "");
        pq_clean.as_str()
    } else {
        pq_raw
    };
    let _ = write!(buf, "{} {} HTTP/1.1\r\n", method, pq);
    for (name, value) in headers {
        // Skip framing headers — we emit our own Connection below.
        if name == http::header::CONNECTION
            || name == http::header::CONTENT_LENGTH
            || name == http::header::TRANSFER_ENCODING
        {
            continue;
        }
        buf.extend_from_slice(name.as_str().as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(value.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    buf.extend_from_slice(b"Connection: Upgrade\r\n");
    buf.extend_from_slice(b"\r\n");
    buf
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Parse only the HTTP/1.1 response status line and headers from the IO,
/// returning the parsed result and any leftover bytes read past the header
/// boundary.
///
/// Unlike [`parse_response_stream`], this does **not** consume the IO into
/// a body stream — the caller retains the IO for bidirectional WebSocket
/// forwarding via `tokio::io::copy_bidirectional`.
pub(crate) async fn parse_upgrade_response(
    io: &mut (impl AsyncRead + Unpin + Send),
) -> anyhow::Result<(StatusCode, HeaderMap, Bytes)> {
    let mut buf = BytesMut::with_capacity(4096);
    let (status, headers, body_offset) = loop {
        let mut tmp = [0u8; 4096];
        let n = io
            .read(&mut tmp)
            .await
            .context("failed to read response from proxy")?;
        if n == 0 {
            anyhow::bail!("proxy closed connection before sending response headers");
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_HEADER_BYTES {
            anyhow::bail!(
                "response headers too large ({} bytes exceeds {} byte limit)",
                buf.len(),
                MAX_HEADER_BYTES
            );
        }

        let mut parsed_headers = [httparse::EMPTY_HEADER; 128];
        let mut resp = httparse::Response::new(&mut parsed_headers);
        match resp.parse(&buf)? {
            httparse::Status::Complete(offset) => {
                let status = StatusCode::from_u16(resp.code.unwrap_or(502))?;
                let mut headers = HeaderMap::new();
                for h in resp.headers.iter() {
                    if let (Ok(name), Ok(value)) = (
                        HeaderName::from_bytes(h.name.as_bytes()),
                        HeaderValue::from_bytes(h.value),
                    ) {
                        headers.append(name, value);
                    }
                }
                break (status, headers, offset);
            }
            httparse::Status::Partial => continue,
        }
    };

    let _ = buf.split_to(body_offset);
    let remaining = buf.freeze();
    Ok((status, headers, remaining))
}

/// Read an HTTP/1.1 response from the client side of a DuplexStream.
///
/// Parses the status line and headers via `httparse`, then returns a
/// streaming body whose framing strategy depends on the response:
///
/// - **101 Switching Protocols** → raw unbounded byte stream (WebSocket)
/// - **Content-Length** → exactly N bytes
/// - **Transfer-Encoding: chunked** → decoded chunks
/// - **Otherwise** → read until EOF
pub(crate) async fn parse_response_stream(
    mut io: impl AsyncRead + Unpin + Send + 'static,
) -> anyhow::Result<(StatusCode, HeaderMap, BodyStream)> {
    // Phase 1: accumulate bytes until httparse can parse a complete header.
    let mut buf = BytesMut::with_capacity(4096);
    let (status, headers, body_offset) = loop {
        let mut tmp = [0u8; 4096];
        let n = io
            .read(&mut tmp)
            .await
            .context("failed to read response from proxy")?;
        if n == 0 {
            anyhow::bail!("proxy closed connection before sending response headers");
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_HEADER_BYTES {
            anyhow::bail!(
                "response headers too large ({} bytes exceeds {} byte limit)",
                buf.len(),
                MAX_HEADER_BYTES
            );
        }

        let mut parsed_headers = [httparse::EMPTY_HEADER; 128];
        let mut resp = httparse::Response::new(&mut parsed_headers);
        match resp.parse(&buf)? {
            httparse::Status::Complete(offset) => {
                let status = StatusCode::from_u16(resp.code.unwrap_or(502))?;
                let mut headers = HeaderMap::new();
                for h in resp.headers.iter() {
                    if let (Ok(name), Ok(value)) = (
                        HeaderName::from_bytes(h.name.as_bytes()),
                        HeaderValue::from_bytes(h.value),
                    ) {
                        headers.append(name, value);
                    }
                }
                break (status, headers, offset);
            }
            httparse::Status::Partial => continue,
        }
    };

    // Leftover body bytes that were read together with the headers.
    let _ = buf.split_to(body_offset);
    let remaining = buf.freeze();

    // Phase 2: select body-reading strategy.
    let body_stream = if status == StatusCode::SWITCHING_PROTOCOLS {
        raw_body_stream(remaining, io)
    } else if is_chunked_encoding(&headers) {
        chunked_body_stream(remaining, io)
    } else if let Some(len) = content_length_value(&headers) {
        content_length_body_stream(remaining, io, len)
    } else {
        raw_body_stream(remaining, io)
    };

    Ok((status, headers, body_stream))
}

fn content_length_value(headers: &HeaderMap) -> Option<usize> {
    headers
        .get(http::header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn is_chunked_encoding(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::TRANSFER_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
}

// ---------------------------------------------------------------------------
// Body stream builders
// ---------------------------------------------------------------------------

/// Read raw bytes until EOF (used for 101 Upgrade and connection-close).
pub(crate) fn raw_body_stream<R: AsyncRead + Unpin + Send + 'static>(
    initial: Bytes,
    io: R,
) -> BodyStream {
    struct State<R> {
        io: R,
        initial: Option<Bytes>,
    }

    Box::pin(unfold(
        State {
            io,
            initial: if initial.is_empty() {
                None
            } else {
                Some(initial)
            },
        },
        |mut state| async move {
            if let Some(initial) = state.initial.take() {
                return Some((Ok(initial), state));
            }
            let mut buf = vec![0u8; 8192];
            match state.io.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((Ok(Bytes::from(buf)), state))
                }
                Err(e) => Some((Err(Box::new(e) as BoxError), state)),
            }
        },
    ))
}

/// Read exactly `total` body bytes (Content-Length delimited).
fn content_length_body_stream<R: AsyncRead + Unpin + Send + 'static>(
    initial: Bytes,
    io: R,
    total: usize,
) -> BodyStream {
    struct State<R> {
        io: R,
        remaining: usize,
        initial: Option<Bytes>,
    }

    Box::pin(unfold(
        State {
            io,
            remaining: total,
            initial: if initial.is_empty() {
                None
            } else {
                Some(initial)
            },
        },
        |mut state| async move {
            if state.remaining == 0 {
                return None;
            }
            if let Some(initial) = state.initial.take() {
                let to_take = initial.len().min(state.remaining);
                state.remaining -= to_take;
                return Some((Ok(initial.slice(..to_take)), state));
            }
            let to_read = state.remaining.min(8192);
            let mut buf = vec![0u8; to_read];
            match state.io.read(&mut buf).await {
                Ok(0) => Some((
                    Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "upstream closed connection with {} body bytes remaining",
                            state.remaining
                        ),
                    )) as BoxError),
                    state,
                )),
                Ok(n) => {
                    buf.truncate(n);
                    state.remaining -= n;
                    Some((Ok(Bytes::from(buf)), state))
                }
                Err(e) => Some((Err(Box::new(e) as BoxError), state)),
            }
        },
    ))
}

/// Decode chunked transfer encoding into plain body chunks.
fn chunked_body_stream<R: AsyncRead + Unpin + Send + 'static>(initial: Bytes, io: R) -> BodyStream {
    struct State<R> {
        io: R,
        buf: BytesMut,
    }

    Box::pin(unfold(
        State {
            io,
            buf: BytesMut::from(initial.as_ref()),
        },
        |mut state| async move {
            loop {
                // Look for the chunk-size line terminator.
                if let Some(pos) = find_crlf(&state.buf) {
                    let line = match std::str::from_utf8(&state.buf[..pos]) {
                        Ok(s) => s,
                        Err(_) => {
                            return Some((
                                Err(Box::new(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "chunked body: chunk-size line is not valid UTF-8",
                                )) as BoxError),
                                state,
                            ));
                        }
                    };
                    // chunk-size [ chunk-ext ] — ignore optional extensions after ';'
                    let size_hex = line.split(';').next().unwrap_or("").trim();
                    let chunk_size = match usize::from_str_radix(size_hex, 16) {
                        Ok(s) => s,
                        Err(_) => {
                            return Some((
                                Err(Box::new(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    format!("chunked body: invalid chunk size hex: {size_hex:?}"),
                                )) as BoxError),
                                state,
                            ));
                        }
                    };

                    // Advance past the size line.
                    state.buf.advance(pos + 2);

                    if chunk_size == 0 {
                        return None;
                    }

                    if chunk_size > MAX_CHUNK_SIZE {
                        return Some((
                            Err(Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!(
                                    "chunked body: declared chunk size {chunk_size} \
                                     exceeds maximum of {MAX_CHUNK_SIZE} bytes"
                                ),
                            )) as BoxError),
                            state,
                        ));
                    }

                    // Ensure we have chunk_size + trailing CRLF bytes.
                    while state.buf.len() < chunk_size + 2 {
                        if let Err(e) = fill_buf(&mut state.io, &mut state.buf).await {
                            return Some((Err(Box::new(e) as BoxError), state));
                        }
                    }

                    let chunk = state.buf.split_to(chunk_size).freeze();
                    state.buf.advance(2); // trailing \r\n
                    return Some((Ok(chunk), state));
                }

                // Need more data from the stream.
                if let Err(e) = fill_buf(&mut state.io, &mut state.buf).await {
                    return Some((Err(Box::new(e) as BoxError), state));
                }
            }
        },
    ))
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

async fn fill_buf<R: AsyncRead + Unpin>(
    io: &mut R,
    buf: &mut BytesMut,
) -> Result<usize, std::io::Error> {
    let mut tmp = [0u8; 8192];
    let n = io.read(&mut tmp).await?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "unexpected EOF in chunked body",
        ));
    }
    buf.extend_from_slice(&tmp[..n]);
    Ok(n)
}

// ---------------------------------------------------------------------------
// Streaming body lifecycle wrapper
// ---------------------------------------------------------------------------

/// Wrap a [`BodyStream`] with idle timeout and graceful shutdown awareness.
///
/// Applied to SSE responses so that long-lived streams are terminated when:
/// - No data is received from upstream within `idle_timeout`
/// - The server is shutting down (via `shutdown_rx`)
///
/// Normal chunks are forwarded unchanged; the idle timer is reset on each
/// chunk. Upstream EOF ends the stream cleanly.
pub(crate) fn streaming_body_with_lifecycle(
    inner: BodyStream,
    idle_timeout: Duration,
    shutdown_rx: watch::Receiver<bool>,
) -> BodyStream {
    struct State {
        inner: BodyStream,
        shutdown_rx: watch::Receiver<bool>,
        deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
        idle_timeout: Duration,
    }

    Box::pin(unfold(
        State {
            inner,
            shutdown_rx,
            deadline: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
        },
        |mut state| async move {
            loop {
                return tokio::select! {
                    biased;
                    result = state.shutdown_rx.changed() => {
                        // Err => sender dropped (shutdown). Ok + true => explicit signal.
                        // Both mean "stop streaming now".
                        if result.is_err() || *state.shutdown_rx.borrow() {
                            tracing::debug!("SSE stream terminated by shutdown");
                            None
                        } else {
                            // Spurious wake (value changed but still false) — re-enter select.
                            continue;
                        }
                    }
                    _ = &mut state.deadline => {
                        tracing::debug!("SSE stream idle timeout");
                        None
                    }
                    item = state.inner.next() => {
                        let chunk = item?;
                        state.deadline.as_mut().reset(
                            tokio::time::Instant::now() + state.idle_timeout
                        );
                        Some((chunk, state))
                    }
                };
            }
        },
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "session_bridge_tests.rs"]
mod session_bridge_tests;
