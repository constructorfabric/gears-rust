use super::*;

/// Extract status code and reason bytes from a Close frame payload.
fn parse_close_payload(payload: &[u8]) -> (u16, &[u8]) {
    if payload.len() >= 2 {
        let code = u16::from_be_bytes([payload[0], payload[1]]);
        (code, &payload[2..])
    } else {
        (1005, b"") // No status code provided (RFC 6455 §7.1.5)
    }
}

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
