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
