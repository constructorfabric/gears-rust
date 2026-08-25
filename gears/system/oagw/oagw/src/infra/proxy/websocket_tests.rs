use super::*;
use tokio::io::AsyncReadExt;

/// Create a no-op shutdown receiver for tests that don't exercise shutdown.
fn test_shutdown_rx() -> watch::Receiver<bool> {
    let (_tx, rx) = watch::channel(false);
    rx
}

fn cfg(idle: Duration, shutdown_rx: watch::Receiver<bool>) -> RelayConfig {
    cfg_with_leftover(idle, shutdown_rx, Bytes::new())
}

fn cfg_with_leftover(
    idle: Duration,
    shutdown_rx: watch::Receiver<bool>,
    leftover: Bytes,
) -> RelayConfig {
    RelayConfig {
        leftover,
        idle_timeout: idle,
        close_timeout: Duration::from_millis(100),
        shutdown_rx,
    }
}

/// Read exactly `n` bytes, failing the test with the underlying IO error
/// rather than returning a short buffer that panics on indexing later.
async fn read_n(io: &mut (impl AsyncRead + Unpin), n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    io.read_exact(&mut out)
        .await
        .unwrap_or_else(|e| panic!("expected {n} bytes, read failed: {e}"));
    out
}

/// IO that never completes a read or a write. Models the peer that has
/// stopped servicing its socket — the case that used to hang teardown.
struct StalledIo;

impl AsyncRead for StalledIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}

impl AsyncWrite for StalledIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Pending
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}

/// IO whose first read fails, to drive the `RelayOutcome::Error` arm.
struct BrokenIo;

impl AsyncRead for BrokenIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "peer reset",
        )))
    }
}

impl AsyncWrite for BrokenIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// -- going_away_frame ----------------------------------------------------

#[test]
fn going_away_frame_unmasked_layout() {
    let frame = going_away_frame(None);
    assert_eq!(frame[0], 0x88, "FIN | Close");
    assert_eq!(frame[1], 12, "no mask bit, 2 + 10 payload bytes");
    assert_eq!(&frame[2..4], &1001u16.to_be_bytes());
    assert_eq!(&frame[4..], b"Going Away");
}

#[test]
fn going_away_frame_masked_sets_bit_and_xors_payload() {
    let key = [0x11u8, 0x22, 0x33, 0x44];
    let frame = going_away_frame(Some(key));
    assert_eq!(frame[0], 0x88);
    assert_eq!(frame[1], 0x80 | 12, "mask bit set");
    assert_eq!(&frame[2..6], &key);

    let unmasked: Vec<u8> = frame[6..]
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % 4])
        .collect();
    assert_eq!(&unmasked[..2], &1001u16.to_be_bytes());
    assert_eq!(&unmasked[2..], b"Going Away");
}

// -- relay ---------------------------------------------------------------

#[tokio::test]
async fn relay_forwards_bytes_in_both_directions() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_secs(5), test_shutdown_rx()),
        )
        .await
    });

    client_a.write_all(b"to-upstream").await.unwrap();
    assert_eq!(read_n(&mut upstream_a, 11).await, b"to-upstream");

    upstream_a.write_all(b"to-client").await.unwrap();
    assert_eq!(read_n(&mut client_a, 9).await, b"to-client");

    // Sequence the two closures rather than dropping both peers back to back,
    // which would leave the attribution to `copy_bidirectional`'s poll order.
    // The caller leaves first, and reading upstream to EOF waits for the relay
    // to act on it: the upstream write half is shut down only once the
    // client→upstream direction has seen the caller's EOF.
    drop(client_a);
    let mut upstream_tail = Vec::new();
    upstream_a.read_to_end(&mut upstream_tail).await.unwrap();
    assert!(
        upstream_tail.is_empty(),
        "nothing more was sent after the caller hung up"
    );

    // Only now does the upstream leg end, so `TunnelEnd::Client` is the only
    // attribution the relay can reach.
    drop(upstream_a);
    let outcome = handle.await.unwrap();
    assert!(
        matches!(
            outcome,
            RelayOutcome::Closed {
                ended_by: TunnelEnd::Client,
                ..
            }
        ),
        "caller hung up first, got {outcome:?}"
    );
}

/// The point of the opaque relay: bytes the old frame layer would have
/// rewritten — and that tungstenite / fastwebsockets reject outright — pass
/// through untouched.
#[tokio::test]
async fn relay_forwards_masked_rsv_and_unknown_opcode_frames_verbatim() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_secs(5), test_shutdown_rx()),
        )
        .await
    });

    // RSV1 set (permessage-deflate) + reserved opcode 3 + client masking.
    // The old relay stripped RSV and re-masked with a fresh key.
    let mask = [0x11u8, 0x22, 0x33, 0x44];
    let mut frame = vec![0xC3, 0x83];
    frame.extend_from_slice(&mask);
    for (i, b) in b"abc".iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }

    client_a.write_all(&frame).await.unwrap();
    assert_eq!(
        read_n(&mut upstream_a, frame.len()).await,
        frame,
        "frame must reach upstream byte-for-byte"
    );

    drop(client_a);
    drop(upstream_a);
    let _outcome = handle.await.unwrap();
}

#[tokio::test]
async fn relay_peer_disconnect_ends_the_session() {
    let (client_a, client_b) = tokio::io::duplex(4096);
    let (upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_secs(5), test_shutdown_rx()),
        )
        .await
    });

    drop(client_a);
    drop(upstream_a);

    let outcome = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("relay must end when both peers disconnect")
        .unwrap();
    assert!(matches!(outcome, RelayOutcome::Closed { .. }));
}

#[tokio::test]
async fn relay_idle_timeout_sends_close_1001_to_both_sides() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_millis(100), test_shutdown_rx()),
        )
        .await
    });

    // Caller side: unmasked Close 1001 — 2 header + 12 payload bytes.
    let to_client = read_n(&mut client_a, 14).await;
    assert_eq!(to_client[0], 0x88);
    assert_eq!(to_client[1], 12, "server-to-client frames are not masked");
    assert_eq!(&to_client[2..4], &1001u16.to_be_bytes());
    assert_eq!(&to_client[4..14], b"Going Away");

    // Upstream side: masked Close 1001 (the gateway is the client there) —
    // 2 header + 4 mask + 12 payload bytes.
    let to_upstream = read_n(&mut upstream_a, 18).await;
    assert_eq!(to_upstream[0], 0x88);
    assert_eq!(
        to_upstream[1],
        0x80 | 12,
        "client-to-server frames are masked"
    );
    let key = [
        to_upstream[2],
        to_upstream[3],
        to_upstream[4],
        to_upstream[5],
    ];
    let unmasked: Vec<u8> = to_upstream[6..18]
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % 4])
        .collect();
    assert_eq!(&unmasked[..2], &1001u16.to_be_bytes());
    assert_eq!(&unmasked[2..], b"Going Away");

    let outcome = handle.await.unwrap();
    assert!(matches!(outcome, RelayOutcome::IdleTimeout));
}

#[tokio::test]
async fn relay_idle_timer_is_pushed_out_by_traffic() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_millis(300), test_shutdown_rx()),
        )
        .await
    });

    // Keep the tunnel busy well past one idle window.
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        client_a.write_all(b"ping").await.unwrap();
        assert_eq!(read_n(&mut upstream_a, 4).await, b"ping");
    }

    assert!(!handle.is_finished(), "traffic must keep the session alive");

    drop(client_a);
    drop(upstream_a);
    let _outcome = handle.await.unwrap();
}

#[tokio::test]
async fn relay_shutdown_sends_close_1001() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (upstream_a, upstream_b) = tokio::io::duplex(4096);
    let (tx, rx) = watch::channel(false);

    let handle =
        tokio::spawn(
            async move { relay(client_b, upstream_b, cfg(Duration::from_secs(5), rx)).await },
        );

    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(true).unwrap();

    let to_client = read_n(&mut client_a, 14).await;
    assert_eq!(to_client[0], 0x88);
    assert_eq!(to_client[1], 12, "server-to-client frames are not masked");
    assert_eq!(&to_client[2..4], &1001u16.to_be_bytes());
    assert_eq!(&to_client[4..14], b"Going Away");

    drop(upstream_a);
    let outcome = handle.await.unwrap();
    assert!(matches!(outcome, RelayOutcome::Shutdown));
}

/// A peer that has stopped reading must not be able to wedge teardown.
/// Before the grace period covered the Close writes, `write_all` here
/// blocked forever and stranded the detached bridge task, its upstream
/// stream and the active-session gauge.
#[tokio::test]
async fn relay_teardown_is_bounded_when_a_peer_stops_reading() {
    let (upstream_a, upstream_b) = tokio::io::duplex(4096);

    let started = tokio::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        relay(
            StalledIo,
            upstream_b,
            cfg(Duration::from_millis(100), test_shutdown_rx()),
        ),
    )
    .await
    .expect("teardown must not hang on an unresponsive peer");

    assert!(matches!(outcome, RelayOutcome::IdleTimeout));
    // idle (100ms) + close grace (100ms), with slack for scheduling.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "teardown took {:?}; the close grace period is not bounding it",
        started.elapsed()
    );
    drop(upstream_a);
}

/// Each side's close announcement gets its own grace budget. When the two ran
/// sequentially under one shared timeout, a stalled caller consumed the whole
/// period and the upstream was never told anything at all.
#[tokio::test]
async fn relay_announces_close_to_upstream_even_when_the_caller_has_stalled() {
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            StalledIo,
            upstream_b,
            cfg(Duration::from_millis(100), test_shutdown_rx()),
        )
        .await
    });

    // Masked Close 1001: 2 header + 4 mask + 12 payload bytes.
    let to_upstream = tokio::time::timeout(Duration::from_secs(2), read_n(&mut upstream_a, 18))
        .await
        .expect("upstream must be told to go away regardless of the caller");
    assert_eq!(to_upstream[0], 0x88);
    assert_eq!(to_upstream[1], 0x80 | 12);

    // ...and the upstream leg is half-closed, not left waiting on the caller.
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), upstream_a.read_to_end(&mut rest))
        .await
        .expect("upstream write half must be shut down within the grace period")
        .unwrap();
    assert!(rest.is_empty(), "nothing follows the Close frame");

    let outcome = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("teardown must not hang on an unresponsive caller")
        .unwrap();
    assert!(matches!(outcome, RelayOutcome::IdleTimeout));
}

/// The claim that justifies dropping `websocket_max_frame_size_bytes`: a
/// payload far larger than the relay's copy buffer streams through instead
/// of being materialised.
#[tokio::test]
async fn relay_streams_a_payload_far_larger_than_the_copy_buffer() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_secs(10), test_shutdown_rx()),
        )
        .await
    });

    // 512 KiB — two orders of magnitude past the 4 KiB duplex buffer.
    const LEN: usize = 512 * 1024;
    let payload: Vec<u8> = (0..LEN).map(|i| (i % 251) as u8).collect();
    let expected = payload.clone();

    let writer = tokio::spawn(async move {
        client_a.write_all(&payload).await.unwrap();
        client_a
    });

    let mut got = vec![0u8; LEN];
    upstream_a.read_exact(&mut got).await.unwrap();
    assert_eq!(got, expected, "payload must arrive intact and in order");

    let client_a = writer.await.unwrap();
    drop(client_a);
    drop(upstream_a);
    let _outcome = handle.await.unwrap();
}

/// An upstream that dies under a live caller is abnormal and must be
/// attributed as such, so the bridge can log it above debug. Note the
/// half-close dance: `copy_bidirectional` shuts the caller's write half
/// once the upstream ends, and only returns after the caller ends too — so
/// the *order* of the EOFs is what carries the signal.
#[tokio::test]
async fn relay_reports_an_upstream_drop_distinctly() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_secs(5), test_shutdown_rx()),
        )
        .await
    });

    // Only the upstream goes away; the caller is still connected.
    drop(upstream_a);

    // The relay half-closes toward the caller, which surfaces as EOF here.
    let mut drained = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), client_a.read_to_end(&mut drained))
        .await
        .expect("relay must half-close the caller when the upstream dies")
        .unwrap();

    // Caller then hangs up, completing the other direction.
    drop(client_a);

    let outcome = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("relay ends once both halves are done")
        .unwrap();
    assert!(
        matches!(
            outcome,
            RelayOutcome::Closed {
                ended_by: TunnelEnd::Upstream,
                ..
            }
        ),
        "expected an upstream-attributed close, got {outcome:?}"
    );
}

#[tokio::test]
async fn relay_surfaces_io_errors() {
    let (upstream_a, upstream_b) = tokio::io::duplex(4096);

    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        relay(
            BrokenIo,
            upstream_b,
            cfg(Duration::from_secs(5), test_shutdown_rx()),
        ),
    )
    .await
    .expect("a broken peer must end the relay promptly");

    assert!(matches!(outcome, RelayOutcome::Error(_)), "got {outcome:?}");
    drop(upstream_a);
}

/// A session opened after shutdown was already announced must close at
/// once, without waiting for a `changed()` edge that will never come.
#[tokio::test]
async fn relay_closes_when_shutdown_was_already_signalled() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (upstream_a, upstream_b) = tokio::io::duplex(4096);
    let (_tx, rx) = watch::channel(true);

    let handle =
        tokio::spawn(
            async move { relay(client_b, upstream_b, cfg(Duration::from_secs(30), rx)).await },
        );

    let to_client = read_n(&mut client_a, 14).await;
    assert_eq!(to_client[0], 0x88);
    assert_eq!(to_client[1], 12, "server-to-client frames are not masked");
    assert_eq!(&to_client[2..4], &1001u16.to_be_bytes());
    assert_eq!(&to_client[4..14], b"Going Away");

    drop(upstream_a);
    let outcome = handle.await.unwrap();
    assert!(matches!(outcome, RelayOutcome::Shutdown));
}

// -- leftover hand-off ---------------------------------------------------

#[tokio::test]
async fn leftover_bytes_reach_the_caller_ahead_of_later_upstream_data() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg_with_leftover(
                Duration::from_secs(5),
                test_shutdown_rx(),
                Bytes::from_static(b"post-101-bytes"),
            ),
        )
        .await
    });

    upstream_a.write_all(b"-then-live-data").await.unwrap();
    assert_eq!(
        read_n(&mut client_a, 29).await,
        b"post-101-bytes-then-live-data",
        "leftover must land before anything upstream sends next"
    );

    drop(client_a);
    drop(upstream_a);
    let _outcome = handle.await.unwrap();
}

#[tokio::test]
async fn empty_leftover_forwards_nothing_of_its_own() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (upstream_a, upstream_b) = tokio::io::duplex(4096);

    let handle = tokio::spawn(async move {
        relay(
            client_b,
            upstream_b,
            cfg(Duration::from_secs(5), test_shutdown_rx()),
        )
        .await
    });

    drop(upstream_a);
    let mut sink = Vec::new();
    client_a.read_to_end(&mut sink).await.unwrap();
    assert!(sink.is_empty(), "an empty leftover must be a no-op");

    drop(client_a);
    let _outcome = handle.await.unwrap();
}

/// The leftover prefix must not reintroduce an unbounded write outside the
/// relay. It used to be handed to the caller *before* the relay started, so a
/// caller that never read wedged the detached bridge task — and its session
/// guard — for the process's lifetime.
#[tokio::test]
async fn relay_tears_down_when_leftover_bytes_meet_a_stalled_caller() {
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);

    let started = tokio::time::Instant::now();
    let handle = tokio::spawn(async move {
        relay(
            StalledIo,
            upstream_b,
            cfg_with_leftover(
                Duration::from_millis(100),
                test_shutdown_rx(),
                Bytes::from_static(b"never-accepted"),
            ),
        )
        .await
    });

    // Upstream is still told to go away, even though the caller never accepted
    // the leftover bytes: masked Close 1001, 2 header + 4 mask + 12 payload.
    let to_upstream = tokio::time::timeout(Duration::from_secs(2), read_n(&mut upstream_a, 18))
        .await
        .expect("leftover delivery must not outlive the idle timeout");
    assert_eq!(to_upstream[0], 0x88);
    assert_eq!(to_upstream[1], 0x80 | 12);

    let outcome = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("relay must return so the session guard drops")
        .unwrap();
    assert!(matches!(outcome, RelayOutcome::IdleTimeout));
    // idle (100ms) + close grace (100ms), with slack for scheduling.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "teardown took {:?}; leftover delivery is not bounded",
        started.elapsed()
    );
}

/// A dropped shutdown sender can never deliver a signal; the branch must go
/// inert rather than resolving `Err` on every poll and spinning the loop.
#[tokio::test]
async fn relay_survives_a_dropped_shutdown_sender() {
    let (mut client_a, client_b) = tokio::io::duplex(4096);
    let (mut upstream_a, upstream_b) = tokio::io::duplex(4096);
    let (tx, rx) = watch::channel(false);
    drop(tx);

    let handle =
        tokio::spawn(
            async move { relay(client_b, upstream_b, cfg(Duration::from_secs(5), rx)).await },
        );

    client_a.write_all(b"still-works").await.unwrap();
    assert_eq!(read_n(&mut upstream_a, 11).await, b"still-works");

    drop(client_a);
    drop(upstream_a);
    let outcome = handle.await.unwrap();
    assert!(matches!(outcome, RelayOutcome::Closed { .. }));
}
