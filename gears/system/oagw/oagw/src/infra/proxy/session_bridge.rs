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
                // `watch::changed()` only fires on a *new* edge, so a stream
                // started after shutdown was already signalled must check the
                // current value up front. Mirrors websocket.rs::shutdown_signalled.
                if *state.shutdown_rx.borrow() {
                    tracing::debug!("SSE stream terminated by shutdown (pre-armed)");
                    return None;
                }
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
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio::io::DuplexStream;
    // Use fully-qualified `AsyncWriteExt` to avoid ambiguity with
    // pingora_core::protocols::Shutdown (also implemented for DuplexStream).
    use tokio::io::AsyncWriteExt as _;

    /// Shutdown the write side of a DuplexStream (disambiguated).
    async fn shut(w: &mut DuplexStream) {
        tokio::io::AsyncWriteExt::shutdown(w).await.unwrap();
    }

    // -- serialize_request_wire tests (buffered: body = Some) --

    #[test]
    fn serialize_request_line_format() {
        let headers = HeaderMap::new();
        let body = Bytes::new();
        let wire = serialize_request_wire(
            &Method::GET,
            "https://example.com/v1/chat",
            &headers,
            Some(&body),
        );
        let text = String::from_utf8_lossy(&wire);
        assert!(text.starts_with("GET /v1/chat HTTP/1.1\r\n"));
    }

    #[test]
    fn serialize_request_includes_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("example.com"));
        headers.insert("x-api-key", HeaderValue::from_static("secret"));
        let body = Bytes::new();
        let wire = serialize_request_wire(
            &Method::POST,
            "https://example.com/api",
            &headers,
            Some(&body),
        );
        let text = String::from_utf8_lossy(&wire);
        assert!(text.contains("host: example.com\r\n"));
        assert!(text.contains("x-api-key: secret\r\n"));
    }

    #[test]
    fn serialize_request_content_length_with_body() {
        let headers = HeaderMap::new();
        let body = Bytes::from_static(b"hello world");
        let wire = serialize_request_wire(
            &Method::POST,
            "https://example.com/api",
            &headers,
            Some(&body),
        );
        let text = String::from_utf8_lossy(&wire);
        assert!(text.contains("Content-Length: 11\r\n"));
        assert!(wire.ends_with(b"hello world"));
    }

    #[test]
    fn serialize_request_content_length_zero_for_empty_body() {
        let headers = HeaderMap::new();
        let body = Bytes::new();
        let wire = serialize_request_wire(
            &Method::GET,
            "https://example.com/api",
            &headers,
            Some(&body),
        );
        let text = String::from_utf8_lossy(&wire);
        assert!(text.contains("Content-Length: 0\r\n"));
        assert!(text.contains("Connection: close\r\n"));
    }

    #[test]
    fn serialize_request_url_without_scheme() {
        let wire = serialize_request_wire(
            &Method::GET,
            "/plain/path",
            &HeaderMap::new(),
            Some(&Bytes::new()),
        );
        let text = String::from_utf8_lossy(&wire);
        assert!(text.starts_with("GET /plain/path HTTP/1.1\r\n"));
    }

    #[test]
    fn serialize_request_strips_crlf_from_url() {
        let wire = serialize_request_wire(
            &Method::GET,
            "https://victim.com/path?x=1\r\nEvil-Header: pwned\r\n",
            &HeaderMap::new(),
            Some(&Bytes::new()),
        );
        let text = String::from_utf8_lossy(&wire);
        // After stripping, the injected text is harmlessly concatenated into
        // the path — the key invariant is that the request line is a single
        // well-formed line with no bare CR/LF splitting it.
        let first_line = text.lines().next().unwrap();
        assert!(
            first_line.starts_with("GET /path?x=1") && first_line.ends_with(" HTTP/1.1"),
            "request line corrupted: {first_line}"
        );
        // "Evil-Header: pwned" must NOT appear as a separate header line.
        assert!(
            !text.contains("Evil-Header: pwned\r\n"),
            "CRLF injection produced a separate header"
        );
    }

    #[test]
    fn serialize_request_no_duplicate_content_length() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("99"));
        let body = Bytes::from_static(b"hello");
        let wire = serialize_request_wire(
            &Method::POST,
            "https://example.com/api",
            &headers,
            Some(&body),
        );
        let text = String::from_utf8_lossy(&wire);
        assert_eq!(
            text.matches("Content-Length:").count(),
            1,
            "duplicate Content-Length"
        );
        assert!(text.contains("Content-Length: 5\r\n"));
    }

    #[test]
    fn serialize_request_no_duplicate_connection() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("keep-alive"),
        );
        let body = Bytes::new();
        let wire = serialize_request_wire(
            &Method::GET,
            "https://example.com/api",
            &headers,
            Some(&body),
        );
        let text = String::from_utf8_lossy(&wire);
        assert_eq!(
            text.matches("Connection:").count(),
            1,
            "duplicate Connection"
        );
        assert!(text.contains("Connection: close\r\n"));
    }

    #[test]
    fn serialize_request_buffered_strips_transfer_encoding() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        let body = Bytes::from_static(b"payload");
        let wire = serialize_request_wire(
            &Method::POST,
            "https://example.com/api",
            &headers,
            Some(&body),
        );
        let text = String::from_utf8_lossy(&wire);
        assert!(
            !text.contains("Transfer-Encoding"),
            "buffered path must not emit Transfer-Encoding"
        );
        assert!(text.contains("Content-Length: 7\r\n"));
    }

    // -- serialize_request_wire tests (streaming: body = None) --

    #[test]
    fn streaming_emits_chunked_te() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", HeaderValue::from_static("websocket"));
        let wire = serialize_request_wire(&Method::GET, "wss://example.com/ws", &headers, None);
        let text = String::from_utf8_lossy(&wire);
        assert!(!text.contains("Content-Length"));
        assert!(text.contains("Transfer-Encoding: chunked\r\n"));
        assert!(text.contains("upgrade: websocket\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(wire.ends_with(b"\r\n\r\n"));
    }

    #[test]
    fn streaming_no_duplicate_framing_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("keep-alive"),
        );
        headers.insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        headers.insert(
            http::header::TRANSFER_ENCODING,
            HeaderValue::from_static("gzip"),
        );
        let wire = serialize_request_wire(&Method::POST, "https://example.com/api", &headers, None);
        let text = String::from_utf8_lossy(&wire);
        assert_eq!(
            text.matches("Connection:").count(),
            1,
            "duplicate Connection"
        );
        assert!(text.contains("Connection: close\r\n"));
        assert!(
            !text.contains("Content-Length"),
            "streaming path must not emit Content-Length"
        );
        assert_eq!(
            text.matches("Transfer-Encoding:").count(),
            1,
            "duplicate Transfer-Encoding"
        );
        assert!(text.contains("Transfer-Encoding: chunked\r\n"));
    }

    #[test]
    fn streaming_no_body_bytes() {
        let wire = serialize_request_wire(
            &Method::GET,
            "https://example.com/api",
            &HeaderMap::new(),
            None,
        );
        // After the final \r\n\r\n there must be nothing.
        let text = String::from_utf8_lossy(&wire);
        assert!(text.ends_with("\r\n\r\n"));
        let parts: Vec<&str> = text.splitn(2, "\r\n\r\n").collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[1].is_empty());
    }

    // -- parse_response_stream tests (task 2.6) --

    #[tokio::test]
    async fn parse_response_content_length() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        let body = b"hello world";
        let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        tokio::spawn(async move {
            writer.write_all(resp.as_bytes()).await.unwrap();
            writer.write_all(body).await.unwrap();
            shut(&mut writer).await;
        });

        let (status, headers, body_stream) = parse_response_stream(reader).await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers.get("content-length").unwrap().to_str().unwrap(),
            "11"
        );

        let chunks: Vec<Bytes> = body_stream.map(|r| r.unwrap()).collect().await;
        let all: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
        assert_eq!(all, b"hello world");
    }

    #[tokio::test]
    async fn parse_response_chunked() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            writer
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            writer.write_all(b"5\r\nhello\r\n").await.unwrap();
            writer.write_all(b"6\r\n world\r\n").await.unwrap();
            writer.write_all(b"0\r\n\r\n").await.unwrap();
            shut(&mut writer).await;
        });

        let (status, _headers, body_stream) = parse_response_stream(reader).await.unwrap();
        assert_eq!(status, StatusCode::OK);

        let chunks: Vec<Bytes> = body_stream.map(|r| r.unwrap()).collect().await;
        let all: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
        assert_eq!(all, b"hello world");
    }

    #[tokio::test]
    async fn parse_response_chunked_oversized_chunk_rejected() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        // Declare a chunk larger than MAX_CHUNK_SIZE (8 MiB = 0x800000).
        tokio::spawn(async move {
            writer
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            // 0x800001 = MAX_CHUNK_SIZE + 1
            writer.write_all(b"800001\r\n").await.unwrap();
            shut(&mut writer).await;
        });

        let (status, _headers, mut body_stream) = parse_response_stream(reader).await.unwrap();
        assert_eq!(status, StatusCode::OK);

        // The first (and only) chunk poll should return an error.
        let result = body_stream
            .next()
            .await
            .expect("stream should yield an item");
        assert!(result.is_err(), "expected error for oversized chunk");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds maximum"),
            "error should mention the cap: {err_msg}"
        );
    }

    #[tokio::test]
    async fn parse_response_chunked_invalid_hex_yields_error() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            writer
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            writer.write_all(b"ZZZZ\r\n").await.unwrap();
            shut(&mut writer).await;
        });

        let (status, _headers, mut body_stream) = parse_response_stream(reader).await.unwrap();
        assert_eq!(status, StatusCode::OK);

        let result = body_stream
            .next()
            .await
            .expect("stream should yield an item");
        assert!(
            result.is_err(),
            "invalid hex must produce an error, not silent EOF"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid chunk size hex"),
            "error should describe the problem: {err_msg}"
        );
    }

    #[tokio::test]
    async fn parse_response_101_upgrade() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            writer
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\n\
                      Upgrade: websocket\r\n\
                      Connection: Upgrade\r\n\r\n\
                      raw ws frames here",
                )
                .await
                .unwrap();
            shut(&mut writer).await;
        });

        let (status, headers, body_stream) = parse_response_stream(reader).await.unwrap();
        assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(
            headers.get("upgrade").unwrap().to_str().unwrap(),
            "websocket"
        );

        let chunks: Vec<Bytes> = body_stream.map(|r| r.unwrap()).collect().await;
        let all: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
        assert_eq!(all, b"raw ws frames here");
    }

    #[tokio::test]
    async fn parse_response_error_502() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        let body = br#"{"status":502,"detail":"upstream error"}"#;
        let resp = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        tokio::spawn(async move {
            writer.write_all(resp.as_bytes()).await.unwrap();
            writer.write_all(body).await.unwrap();
            shut(&mut writer).await;
        });

        let (status, _headers, body_stream) = parse_response_stream(reader).await.unwrap();
        assert_eq!(status, StatusCode::BAD_GATEWAY);

        let chunks: Vec<Bytes> = body_stream.map(|r| r.unwrap()).collect().await;
        let all: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
        assert_eq!(all, body.as_slice());
    }

    // -- serialize_upgrade_request_wire tests --

    #[test]
    fn upgrade_wire_emits_connection_upgrade() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("example.com"));
        headers.insert("upgrade", HeaderValue::from_static("websocket"));
        headers.insert(
            "sec-websocket-key",
            HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
        );
        headers.insert("sec-websocket-version", HeaderValue::from_static("13"));

        let wire = serialize_upgrade_request_wire(&Method::GET, "wss://example.com/ws", &headers);
        let text = String::from_utf8_lossy(&wire);

        assert!(text.starts_with("GET /ws HTTP/1.1\r\n"));
        assert!(text.contains("upgrade: websocket\r\n"));
        assert!(text.contains("Connection: Upgrade\r\n"));
        assert!(text.contains("sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(!text.contains("Content-Length"));
        assert!(!text.contains("Transfer-Encoding"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[test]
    fn upgrade_wire_strips_inbound_connection_header() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", HeaderValue::from_static("websocket"));
        headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("keep-alive"),
        );

        let wire = serialize_upgrade_request_wire(&Method::GET, "wss://example.com/ws", &headers);
        let text = String::from_utf8_lossy(&wire);

        assert_eq!(
            text.matches("Connection:").count(),
            1,
            "duplicate Connection"
        );
        assert!(text.contains("Connection: Upgrade\r\n"));
    }

    #[test]
    fn upgrade_wire_no_body() {
        let wire =
            serialize_upgrade_request_wire(&Method::GET, "wss://example.com/ws", &HeaderMap::new());
        let text = String::from_utf8_lossy(&wire);
        assert!(text.ends_with("\r\n\r\n"));
        let parts: Vec<&str> = text.splitn(2, "\r\n\r\n").collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[1].is_empty());
    }

    #[test]
    fn upgrade_wire_crlf_injection_defense() {
        let wire = serialize_upgrade_request_wire(
            &Method::GET,
            "wss://victim.com/path\r\nEvil: pwned\r\n",
            &HeaderMap::new(),
        );
        let text = String::from_utf8_lossy(&wire);
        assert!(
            !text.contains("Evil: pwned\r\n"),
            "CRLF injection produced a separate header"
        );
    }

    // -- parse_upgrade_response tests --

    #[tokio::test]
    async fn parse_upgrade_101() {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            writer
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\n\
                      Upgrade: websocket\r\n\
                      Connection: Upgrade\r\n\r\n",
                )
                .await
                .unwrap();
            // Don't close — simulates a live WebSocket connection
        });

        let (status, headers, leftover) = parse_upgrade_response(&mut reader).await.unwrap();
        assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(headers.get("upgrade").unwrap(), "websocket");
        assert!(leftover.is_empty());
        // reader is still usable (not consumed)
        drop(reader);
    }

    #[tokio::test]
    async fn parse_upgrade_101_with_leftover() {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            // Headers + some WebSocket frame data in the same write
            writer
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\n\
                      Upgrade: websocket\r\n\
                      Connection: Upgrade\r\n\r\n\
                      ws-frame-data",
                )
                .await
                .unwrap();
        });

        let (status, _headers, leftover) = parse_upgrade_response(&mut reader).await.unwrap();
        assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(leftover.as_ref(), b"ws-frame-data");
    }

    #[tokio::test]
    async fn parse_upgrade_non_101() {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            writer
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            shut(&mut writer).await;
        });

        let (status, _headers, leftover) = parse_upgrade_response(&mut reader).await.unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(leftover.is_empty());
    }

    #[tokio::test]
    async fn parse_upgrade_eof_before_headers() {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            shut(&mut writer).await;
        });

        let result = parse_upgrade_response(&mut reader).await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // streaming_body_with_lifecycle tests
    // -----------------------------------------------------------------------

    fn bytes_stream(chunks: Vec<&'static str>) -> BodyStream {
        Box::pin(futures_util::stream::iter(
            chunks
                .into_iter()
                .map(|s| Ok(Bytes::from(s)) as Result<Bytes, BoxError>),
        ))
    }

    #[tokio::test]
    async fn sse_lifecycle_forwards_chunks() {
        let (_tx, rx) = watch::channel(false);
        let stream = streaming_body_with_lifecycle(
            bytes_stream(vec!["data: hello\n\n", "data: world\n\n"]),
            Duration::from_secs(60),
            rx,
        );
        let chunks: Vec<_> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "data: hello\n\n");
        assert_eq!(chunks[1], "data: world\n\n");
    }

    #[tokio::test]
    async fn sse_lifecycle_upstream_eof_ends_stream() {
        let (_tx, rx) = watch::channel(false);
        let stream =
            streaming_body_with_lifecycle(bytes_stream(vec!["one"]), Duration::from_secs(60), rx);
        let chunks: Vec<_> = stream.collect::<Vec<_>>().await;
        assert_eq!(chunks.len(), 1);
    }

    /// Build a BodyStream from an mpsc channel for testing async streams
    /// that need to be controlled from outside.
    fn channel_stream(rx: tokio::sync::mpsc::Receiver<Result<Bytes, BoxError>>) -> BodyStream {
        Box::pin(async_stream::stream! {
            let mut rx = rx;
            while let Some(item) = rx.recv().await {
                yield item;
            }
        })
    }

    #[tokio::test]
    async fn sse_lifecycle_idle_timeout_ends_stream() {
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);

        // Send one chunk, then go silent.
        inner_tx
            .send(Ok(Bytes::from("data: first\n\n")))
            .await
            .unwrap();

        let stream = streaming_body_with_lifecycle(
            channel_stream(inner_rx),
            Duration::from_millis(50),
            shutdown_rx,
        );
        let chunks: Vec<_> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // Should get the first chunk, then timeout ends the stream.
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "data: first\n\n");
    }

    #[tokio::test]
    async fn sse_lifecycle_shutdown_ends_stream() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);

        inner_tx
            .send(Ok(Bytes::from("data: first\n\n")))
            .await
            .unwrap();

        let stream = streaming_body_with_lifecycle(
            channel_stream(inner_rx),
            Duration::from_secs(60),
            shutdown_rx,
        );
        tokio::pin!(stream);

        // Read first chunk.
        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first, "data: first\n\n");

        // Signal shutdown.
        shutdown_tx.send(true).unwrap();

        // Stream should end.
        let next = stream.next().await;
        assert!(next.is_none(), "stream should end after shutdown");
    }

    #[tokio::test]
    async fn sse_lifecycle_pre_armed_shutdown_ends_immediately() {
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap(); // shutdown BEFORE the stream is ever polled
        let stream = streaming_body_with_lifecycle(
            bytes_stream(vec!["data: never\n\n"]),
            Duration::from_secs(300),
            rx,
        );
        let out = tokio::time::timeout(Duration::from_secs(1), stream.collect::<Vec<_>>())
            .await
            .expect("must not wait for the idle timeout");
        assert!(
            out.is_empty(),
            "pre-armed shutdown must terminate immediately"
        );
    }

    #[tokio::test]
    async fn sse_lifecycle_sender_drop_ends_stream() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (_inner_tx, inner_rx) = tokio::sync::mpsc::channel::<Result<Bytes, BoxError>>(1);

        let stream = streaming_body_with_lifecycle(
            channel_stream(inner_rx),
            Duration::from_secs(60),
            shutdown_rx,
        );
        tokio::pin!(stream);

        // Drop the sender — production shutdown path.
        drop(shutdown_tx);

        // Stream should terminate promptly (not block on inner or idle timeout).
        let next = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        assert!(
            matches!(next, Ok(None)),
            "stream should end when shutdown sender is dropped"
        );
    }
}
