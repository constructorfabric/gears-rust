//! Round-trip test for `multipart/mixed` streaming, across a real HTTP hop.
//!
//! Server side is `toolkit::http::multipart::MultipartJsonStream`; client side
//! is `toolkit_contract::runtime::multipart::parse_multipart_stream`. The two
//! live in different crates and are only ever used together, so the contract
//! between them — one JSON item per part, an exact per-part `Content-Length`,
//! and a runtime-generated boundary advertised in `Content-Type` — is worth
//! pinning end to end rather than per side.
//!
//! Lives here rather than in `toolkit` because this is the only crate that can
//! see both halves: `toolkit` is a dev-dependency here, and the reader is in
//! this crate.
//!
//! The second half of the file (`--- Generated client ---`) closes the loop the
//! rest of it leaves open: the same framer, reached through a
//! `#[streaming(multipart_mixed)] async fn` contract method and its
//! macro-generated client, rather than through a hand-written open. Fallible
//! open *and* multipart together is the combination no other test covers, and
//! the one a real consumer will write.

#![cfg(feature = "rest-client")]
// Test harness: a failed setup step should abort the test, and the `expect`
// messages here say which wire-format expectation was violated.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::pin::Pin;
use std::time::Duration;

use axum::Router;
use axum::extract::Query;
use axum::response::IntoResponse;
use axum::routing::get;
use futures_core::Stream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::http::multipart::MultipartJsonStream;
use toolkit_contract::runtime::client::build_default_http_client;
use toolkit_contract::runtime::http::body_to_byte_stream;
use toolkit_contract::runtime::multipart::{
    MultipartStream, boundary_from_content_type, parse_multipart_stream,
};
use toolkit_contract::runtime::transport_error::TransportError;

/// The boundary the hand-rolled raw-body handlers use, so the test can write
/// wire bytes the framer would not (an omitted close delimiter).
const RAW_BOUNDARY: &str = "raw-test-boundary";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Frame {
    seq: u64,
}

#[derive(Deserialize)]
struct CountParams {
    count: u64,
}

/// An item that serializes on some values and fails on others, to drive the
/// framer's mid-stream serialization-failure path (Q5).
enum MaybeFrame {
    Good(Frame),
    Unserializable,
}

impl Serialize for MaybeFrame {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            MaybeFrame::Good(frame) => frame.serialize(serializer),
            MaybeFrame::Unserializable => {
                Err(serde::ser::Error::custom("this item cannot be serialized"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// `count` frames, framed by the real framer, closed gracefully.
async fn frames_handler(Query(params): Query<CountParams>) -> impl IntoResponse {
    MultipartJsonStream::new(futures_util::stream::iter(
        (0..params.count).map(|seq| Ok::<_, CanonicalError>(Frame { seq })),
    ))
}

/// One good frame, then an item that will not serialize.
///
/// The items are spaced out so this is a genuinely *mid-stream* failure: the
/// `200`, its headers, and part 0 are all on the wire and read by the client
/// before the failing item is reached. That is the situation Q5 is about — the
/// status is long gone and can no longer carry the error. (Produce all three
/// items in one poll instead and the truncation is detected even earlier, at
/// the client's `send()`, which is loud in a different way but not the path
/// under test.)
async fn broken_handler() -> impl IntoResponse {
    let items = futures_util::stream::iter(vec![
        Ok::<_, CanonicalError>(MaybeFrame::Good(Frame { seq: 0 })),
        Ok(MaybeFrame::Unserializable),
        Ok(MaybeFrame::Good(Frame { seq: 2 })),
    ])
    .then(|item| async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        item
    });
    MultipartJsonStream::new(items)
}

/// One good frame, then a mid-stream **domain** `Err` (#4740 3B). Unlike
/// `broken_handler`'s serialize failure, this is a `Result::Err` the service
/// yields: the framer must render it as a typed `application/problem+json` error
/// part followed by a clean close — not an abort. Spaced out so it is genuinely
/// mid-stream. The trailing `Ok` proves an error is terminal (never framed).
async fn error_item_handler() -> impl IntoResponse {
    let items = futures_util::stream::iter(vec![
        Ok(Frame { seq: 0 }),
        Err(CanonicalError::internal("the feed dried up mid-stream").create()),
        Ok(Frame { seq: 2 }),
    ])
    .then(|item| async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        item
    });
    MultipartJsonStream::new(items)
}

/// Two well-formed parts and then a graceful end with **no** close delimiter —
/// what a producer that never writes one, or a connection that goes away
/// between parts, looks like on the wire.
async fn no_close_handler() -> impl IntoResponse {
    let body = format!("{}{}", raw_part(0), raw_part(1));
    raw_multipart_response(body)
}

/// Two well-formed parts followed by the close delimiter, hand-rolled so the
/// close-delimiter assertion is not merely asserting the framer's own output
/// back at itself.
async fn raw_closed_handler() -> impl IntoResponse {
    let body = format!("{}{}--{RAW_BOUNDARY}--\r\n", raw_part(0), raw_part(1));
    raw_multipart_response(body)
}

fn raw_part(seq: u64) -> String {
    let json = format!("{{\"seq\":{seq}}}");
    format!(
        "--{RAW_BOUNDARY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}\r\n",
        json.len()
    )
}

fn raw_multipart_response(body: String) -> axum::response::Response {
    axum::response::Response::builder()
        .header(
            http::header::CONTENT_TYPE,
            format!("multipart/mixed; boundary={RAW_BOUNDARY}"),
        )
        .body(axum::body::Body::from(body))
        .unwrap()
}

async fn start_server() -> String {
    start_server_with_state(ContractState::default()).await.0
}

async fn start_server_with_state(state: ContractState) -> (String, ContractState) {
    let app = Router::new()
        .route("/frames", get(frames_handler))
        .route("/broken", get(broken_handler))
        .route("/error-item", get(error_item_handler))
        .route("/no-close", get(no_close_handler))
        .route("/raw-closed", get(raw_closed_handler))
        // Routes behind the generated client. The path is
        // `<base_path><path_template>` from the REST projection below; these are
        // `#[server_manual]`, so nothing registers them but this.
        .route(
            "/api/frames/v1/frames",
            get(contract_frames_handler).with_state(state.clone()),
        )
        .route("/api/frames/v1/closed", get(contract_conflict_handler))
        .route("/api/frames/v1/mislabelled", get(mislabelled_handler))
        .route("/api/frames/v1/truncated", get(contract_truncated_handler))
        .route("/api/frames/v1/broker", get(broker_frames_handler))
        // Immediate-open (non-`async` `#[streaming(multipart_mixed)] fn`) routes.
        .route(
            "/api/frames/v1/frames-now",
            get(contract_frames_handler).with_state(state.clone()),
        )
        .route("/api/frames/v1/mislabelled-now", get(mislabelled_handler))
        .route(
            "/api/frames/v1/flapping-now",
            get(flapping_handler).with_state(state.clone()),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

/// A server whose `/channel` route frames whatever the test pushes onto `rx`,
/// one part per item, closing the stream when the sender is dropped. Lets a test
/// control exactly when each part's bytes exist on the wire — no wall clock.
/// Single-use: the receiver is taken by the first connection.
async fn start_channel_server(rx: tokio::sync::mpsc::UnboundedReceiver<Frame>) -> String {
    let rx = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
    let app = Router::new().route(
        "/channel",
        get(move || {
            let rx = rx.clone();
            async move {
                let rx = rx
                    .lock()
                    .unwrap()
                    .take()
                    .expect("the channel server takes one connection");
                let items = futures_util::stream::unfold(rx, |mut rx| async move {
                    rx.recv()
                        .await
                        .map(|frame| (Ok::<_, CanonicalError>(frame), rx))
                });
                MultipartJsonStream::new(items)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

type ByteStream = Pin<
    Box<
        dyn Stream<Item = Result<bytes::Bytes, Box<dyn std::error::Error + Send + Sync + 'static>>>
            + Send,
    >,
>;

/// Open `url` and adapt the response into the typed multipart stream, the way
/// a hand-written client is expected to: read the boundary out of the
/// advertised `Content-Type`, then hand the body's byte stream to the reader.
async fn open(url: &str) -> MultipartStream<Frame, ByteStream> {
    let client = build_default_http_client("multipart-roundtrip-test", false).unwrap();
    let response = client.get(url).send().await.unwrap();
    assert!(
        response.status().is_success(),
        "unexpected status {}",
        response.status()
    );
    let content_type = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .expect("a multipart response must advertise its boundary")
        .to_str()
        .unwrap()
        .to_owned();
    let boundary = boundary_from_content_type(&content_type).unwrap();
    let bytes: ByteStream = Box::pin(body_to_byte_stream(response.into_body()));
    parse_multipart_stream::<Frame, _, _>(bytes, &boundary)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn framer_and_reader_round_trip_in_order() {
    let base = start_server().await;
    let mut stream = Box::pin(open(&format!("{base}/frames?count=5")).await);
    let collected: Vec<_> = stream.by_ref().collect().await;
    let frames: Vec<Frame> = collected.into_iter().map(Result::unwrap).collect();

    assert_eq!(
        frames,
        (0..5).map(|seq| Frame { seq }).collect::<Vec<_>>(),
        "every item must arrive exactly once, in order"
    );
    assert!(
        stream.saw_close_delimiter(),
        "the framer closes a completed stream with `--<boundary>--`"
    );
}

#[tokio::test]
async fn an_empty_stream_round_trips_as_no_items() {
    let base = start_server().await;
    let mut stream = Box::pin(open(&format!("{base}/frames?count=0")).await);
    let collected: Vec<_> = stream.by_ref().collect().await;
    assert!(collected.is_empty(), "got {collected:?}");
    assert!(stream.saw_close_delimiter());
}

#[tokio::test]
async fn a_part_arrives_before_the_next_parts_bytes_exist() {
    use futures_util::FutureExt as _;

    // The latency property D3 exists to buy: the per-part `Content-Length` means
    // frame N is delivered from its own bytes, so immediately after frame N the
    // reader must have *nothing* — frame N+1's bytes do not exist yet. A framing
    // that waited for the next delimiter would deliver frames in pairs and this
    // would find frame 1 already waiting. Driven from a channel rather than a
    // wall clock: the test decides exactly when each part's bytes exist, so
    // there is no timing window a loaded runner can miss and no sleeps to wait
    // on. (Was a 50ms "nothing arrives" race + a ~900ms sleep, #4740.)
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Frame>();
    let base = start_channel_server(rx).await;
    let mut stream = Box::pin(open(&format!("{base}/channel")).await);

    // Frame 0 pushed → the client yields it.
    tx.send(Frame { seq: 0 }).unwrap();
    let first = stream.next().await;
    assert_eq!(first.map(Result::unwrap), Some(Frame { seq: 0 }));

    // Nothing more pushed: the very next poll resolves to nothing. Frame 0 came
    // from its own bytes, so a correct reader has no frame 1 buffered; a
    // pair-buffering framing would already have it here.
    assert!(
        stream.next().now_or_never().is_none(),
        "frame 1's bytes do not exist yet, so the next poll must be pending",
    );

    // Push frame 1 → it arrives, so the reader was pending, not stuck.
    tx.send(Frame { seq: 1 }).unwrap();
    let second = stream.next().await;
    assert_eq!(second.map(Result::unwrap), Some(Frame { seq: 1 }));
}

#[tokio::test]
async fn graceful_eof_with_the_close_delimiter_is_a_clean_end() {
    let base = start_server().await;
    let mut stream = Box::pin(open(&format!("{base}/raw-closed")).await);
    let collected: Vec<_> = stream.by_ref().collect().await;
    let frames: Vec<Frame> = collected.into_iter().map(Result::unwrap).collect();
    assert_eq!(frames, vec![Frame { seq: 0 }, Frame { seq: 1 }]);
    assert!(stream.saw_close_delimiter());
}

#[tokio::test]
async fn graceful_eof_without_the_close_delimiter_leaves_the_parser_clean() {
    // The *parser* yields the parts and then a plain `None`; it does not itself
    // manufacture an error, and records the framing fact via
    // `saw_close_delimiter()`. Detecting the truncation is the streaming
    // client's job one layer up, in `end_of_stream_error` — driven here through
    // a `MultipartStream` directly, that layer is not present, which is exactly
    // why the missing-delimiter error is asserted through the generated client
    // in `a_truncated_multipart_stream_ends_with_a_framing_error` (#4740).
    let base = start_server().await;
    let mut stream = Box::pin(open(&format!("{base}/no-close")).await);
    let collected: Vec<_> = stream.by_ref().collect().await;
    assert!(
        collected.iter().all(Result::is_ok),
        "the parser must not manufacture an error of its own: {collected:?}"
    );
    let frames: Vec<Frame> = collected.into_iter().map(Result::unwrap).collect();
    assert_eq!(frames, vec![Frame { seq: 0 }, Frame { seq: 1 }]);
    assert!(!stream.saw_close_delimiter());
}

#[tokio::test]
async fn an_unserializable_item_reaches_the_reader_as_an_error_never_a_clean_end() {
    // Q4 and Q5 are only compatible on the distinction between a graceful EOF
    // (clean) and an aborted body (error). This is the test that keeps the
    // framer's `Err`-vs-`return` choice from regressing: if the framer ever
    // *ends* the body on a serialization failure instead of erroring it, the
    // chunked encoding terminates properly, the reader sees a graceful EOF,
    // and Q4 makes that a clean end — silently hiding the failure and
    // truncating the stream. So the assertion is not just "an error appears"
    // but "the stream does not end cleanly".
    let base = start_server().await;
    let stream = Box::pin(open(&format!("{base}/broken")).await);
    let collected: Vec<Result<Frame, TransportError>> = stream.collect().await;

    assert_eq!(
        collected.first().map(|r| r.as_ref().ok()),
        Some(Some(&Frame { seq: 0 })),
        "the parts written before the failure must still be delivered: {collected:?}"
    );
    let last = collected
        .last()
        .expect("the stream must yield at least the good frame");
    assert!(
        last.is_err(),
        "an aborted body must surface as an error, not as a clean end: {collected:?}"
    );
    // The abort reaches the reader as a truncated body, i.e. a byte-stream
    // failure, which is the `Network` class — not `Framing`, since the bytes
    // that did arrive were well-framed.
    assert!(
        matches!(last, Err(TransportError::Network(_))),
        "expected Network, got {last:?}"
    );
    // The item after the failure must NOT appear: the framer stops at the
    // failure rather than skipping past it.
    assert!(
        !collected
            .iter()
            .any(|r| matches!(r, Ok(frame) if frame.seq == 2)),
        "the framer must abort, not skip the unserializable item: {collected:?}"
    );
}

#[tokio::test]
async fn a_mid_stream_domain_err_arrives_as_a_typed_problem_then_a_clean_close() {
    // 3B: a `Result::Err` yielded by the service (not a serialize failure) is a
    // domain error. It must reach the reader as a *typed* `TransportError::Problem`
    // — recoverable to a `CanonicalError` — and the stream must end CLEANLY, not
    // as a truncation/abort. This is the end-to-end proof that a post-open
    // failure over multipart/mixed is no longer only a synthetic transport error.
    let base = start_server().await;
    let mut stream = Box::pin(open(&format!("{base}/error-item")).await);
    let collected: Vec<Result<Frame, TransportError>> = stream.by_ref().collect().await;

    assert_eq!(
        collected.first().map(|r| r.as_ref().ok()),
        Some(Some(&Frame { seq: 0 })),
        "the good frame before the error must be delivered: {collected:?}"
    );
    let last = collected
        .last()
        .expect("at least the good frame and the error");
    let TransportError::Problem { problem, .. } = last
        .as_ref()
        .expect_err("the last item must be the typed error")
    else {
        panic!(
            "a domain error must arrive as a typed Problem, not a Network/Framing fault: {last:?}"
        );
    };
    // The category/status of the source `CanonicalError::internal` must survive
    // the multipart round-trip (500 Internal) — the whole point of 3B — rather
    // than degrading to a transport (Network/Framing) fault.
    assert_eq!(
        problem.status,
        Some(500),
        "the source error's status/category must round-trip, got {problem:?}"
    );
    // The error is terminal: the trailing good frame is never framed.
    assert!(
        !collected
            .iter()
            .any(|r| matches!(r, Ok(frame) if frame.seq == 2)),
        "an error is terminal; later items must not appear: {collected:?}"
    );
    // The body ended cleanly — the reader saw the closing delimiter, so no
    // missing-terminator error was manufactured.
    assert!(
        stream.saw_close_delimiter(),
        "a typed error part is followed by the close delimiter, so the end is clean"
    );
}

// ---------------------------------------------------------------------------
// Generated client: contract -> `#[streaming(multipart_mixed)] async fn`
//                            -> generated client -> framer -> real server
// ---------------------------------------------------------------------------
//
// Everything above reaches the framer through a hand-written open. This half
// reaches it through the macro instead, over the *fallible* open shape — the
// one combination nothing else exercises, and the one #4346 will write.

/// Records what the generated client actually put on the wire, so the `Accept`
/// header can be asserted against the declared framing rather than assumed.
#[derive(Clone, Default)]
struct ContractState {
    last_accept: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Number of connections the flapping handler has seen, to count reopens on
    /// the immediate path (#4740 #12).
    connections: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

#[derive(Debug, thiserror::Error)]
enum FrameError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
}

/// `count` frames through the real framer, recording the request's `Accept`.
async fn contract_frames_handler(
    axum::extract::State(state): axum::extract::State<ContractState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<CountParams>,
) -> impl IntoResponse {
    if let Some(v) = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|h| h.to_str().ok())
    {
        *state.last_accept.lock().unwrap() = Some(v.to_owned());
    }
    MultipartJsonStream::new(futures_util::stream::iter(
        (0..params.count).map(|seq| Ok::<_, CanonicalError>(Frame { seq })),
    ))
}

/// A `409` carrying an RFC 9457 problem: an open-time domain failure, before
/// any item exists.
async fn contract_conflict_handler() -> impl IntoResponse {
    (
        http::StatusCode::CONFLICT,
        [(http::header::CONTENT_TYPE, "application/problem+json")],
        concat!(
            r#"{"type":"https://example.test/probs/stream-closed","#,
            r#""title":"StreamClosed","status":409,"#,
            r#""detail":"this subscription is already being streamed"}"#
        ),
    )
}

/// A `200` whose `Content-Type` names no `multipart/mixed` boundary. The status
/// says the open succeeded; the framing says it cannot be read.
async fn mislabelled_handler() -> impl IntoResponse {
    (
        [(http::header::CONTENT_TYPE, "application/json")],
        r#"{"seq":0}"#,
    )
}

/// Two well-formed parts then a graceful end with **no** close delimiter, behind
/// the generated client. Unlike `/no-close` (which a hand-written reader drives
/// directly), this exercises the client's own end-of-stream handling, which must
/// surface the truncation as a framing error (#4740).
async fn contract_truncated_handler() -> impl IntoResponse {
    raw_multipart_response(format!("{}{}", raw_part(0), raw_part(1)))
}

/// Counts each connection and always truncates (two parts, no close delimiter),
/// so the transient framing fault is retried. Drives the immediate path's
/// reopen-under-`stream_reconnect` (#4740 #12).
async fn flapping_handler(
    axum::extract::State(state): axum::extract::State<ContractState>,
) -> impl IntoResponse {
    state
        .connections
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    raw_multipart_response(format!("{}{}", raw_part(0), raw_part(1)))
}

// ---------------------------------------------------------------------------
// A `#[serde(tag = "kind")]` union as the item type
// ---------------------------------------------------------------------------
//
// event-broker's wire frame is one internally-tagged union with four kinds
// (`0004-consumption-transport.md`, "Shared Frame Schema"). The plan's non-goals
// deliberately keep frame typing in the *contract's item type* rather than
// introducing a toolkit-side envelope, so the claim under test is that this
// needs nothing from the toolkit but plain serde: one part carries one tagged
// frame, and the union decodes per part.
//
// The reason the non-goals reject a toolkit envelope is the shape's asymmetry,
// so it is reproduced faithfully rather than regularised: `event` nests its body
// under `payload`, while `heartbeat`, `control` and `topology` carry their fields
// **inline** beside the tag. A generic `{kind, payload}` envelope could not
// express the latter three, and normalising them would test a schema the broker
// does not emit.

/// One entry of a `control` frame's `positions` or a `topology` frame's
/// `assigned` — the same shape in both, per the doc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Position {
    topic: String,
    partition: i32,
    offset: i64,
    last_examined: i64,
}

/// Stand-in for `EventEnvelope`: only its presence under `payload` matters here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct EventEnvelope {
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    partition: i32,
    offset: i64,
}

/// The broker's four frame kinds as one internally-tagged union.
///
/// Note this type has **no** `GrpcRepr`, `ToSchema` or `ResponseApiDto` impl. A
/// `multipart_mixed` REST-only streaming item needs only
/// `Serialize + DeserializeOwned + Send + 'static`, and that is worth pinning:
/// `ProtoBridge` rejects payload-carrying enum variants, so a tagged union could
/// not be the item type of a method that is also projected to gRPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BrokerFrame {
    /// The only kind with a nested body.
    Event {
        payload: EventEnvelope,
    },
    Heartbeat {
        at: String,
    },
    Control {
        code: String,
        /// Present only on `terminal`, so it must be optional without turning
        /// every other control frame's absence into a decode failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        positions: Vec<Position>,
    },
    Topology {
        topology_version: i32,
        assigned: Vec<Position>,
    },
}

fn position(topic: &str, partition: i32) -> Position {
    Position {
        topic: topic.to_owned(),
        partition,
        offset: 42,
        last_examined: 99,
    }
}

/// The frame sequence a real consumer sees: a `topology` baseline at open, an
/// `event`, a `heartbeat` during an idle gap, a sparse `progress` control frame,
/// and finally the `terminal` control frame that carries `reason`.
fn broker_frame_sequence() -> Vec<BrokerFrame> {
    vec![
        BrokerFrame::Topology {
            topology_version: 7,
            assigned: vec![position("orders", 0), position("orders", 1)],
        },
        BrokerFrame::Event {
            payload: EventEnvelope {
                id: "evt-1".to_owned(),
                event_type: "order.created".to_owned(),
                partition: 0,
                offset: 42,
            },
        },
        BrokerFrame::Heartbeat {
            at: "2026-09-08T12:00:00Z".to_owned(),
        },
        BrokerFrame::Control {
            code: "progress".to_owned(),
            reason: None,
            positions: vec![position("orders", 1)],
        },
        BrokerFrame::Control {
            code: "terminal".to_owned(),
            reason: Some("rebalanced".to_owned()),
            positions: vec![position("orders", 0), position("orders", 1)],
        },
    ]
}

/// The sequence above through the real framer, one frame per part.
async fn broker_frames_handler() -> impl IntoResponse {
    MultipartJsonStream::new(futures_util::stream::iter(
        broker_frame_sequence()
            .into_iter()
            .map(Ok::<_, CanonicalError>),
    ))
}

// The base contract carries the bare marker: a framing selector names an HTTP
// media type and is rejected on a transport-agnostic base trait (#4734 Q12).
#[toolkit_contract::contract(gear = "frames", version = "v1")]
trait FrameApi: Send + Sync {
    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn frames(
        &self,
        ctx: toolkit_security::SecurityContext,
        count: u64,
    ) -> Result<Frame, FrameError>;

    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn closed(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;

    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn mislabelled(
        &self,
        ctx: toolkit_security::SecurityContext,
    ) -> Result<Frame, FrameError>;

    /// A body that ends without the close delimiter — a truncation the client
    /// must surface (#4740).
    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn truncated(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;

    /// Item type is a `#[serde(tag = "kind")]` union rather than a struct.
    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn broker_frames(
        &self,
        ctx: toolkit_security::SecurityContext,
    ) -> Result<BrokerFrame, FrameError>;

    // --- Immediate open (non-`async` `fn`) --------------------------------
    // The methods above are `async fn`: their open is a distinct fallible step
    // (`Result<Stream, E>`). These three are non-`async`, so the open is
    // *immediate* — the stream is handed back directly, a boundary failure
    // lands as the first item, and a transient drop is retried under the
    // client's `stream_reconnect`. That combination on the multipart framing is
    // what no awaited method exercises (#4740 #12).

    /// Immediate-open success path.
    #[idempotency(SafeRead)]
    #[streaming]
    fn frames_now(
        &self,
        ctx: toolkit_security::SecurityContext,
        count: u64,
    ) -> Result<Frame, FrameError>;

    /// Immediate-open boundary failure: surfaces as the stream's first item.
    #[idempotency(SafeRead)]
    #[streaming]
    fn mislabelled_now(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;

    /// Immediate-open reopen: a truncating peer retried under `stream_reconnect`.
    #[idempotency(SafeRead)]
    #[streaming]
    fn flapping_now(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;
}

#[toolkit_contract::rest_contract(base_path = "/api/frames/v1")]
trait FrameApiRest: FrameApi {
    #[get("/frames")]
    #[streaming(multipart_mixed, open = fallible)]
    #[server_manual]
    async fn frames(
        &self,
        ctx: toolkit_security::SecurityContext,
        count: u64,
    ) -> Result<Frame, FrameError>;

    #[get("/closed")]
    #[streaming(multipart_mixed, open = fallible)]
    #[server_manual]
    async fn closed(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;

    #[get("/mislabelled")]
    #[streaming(multipart_mixed, open = fallible)]
    #[server_manual]
    async fn mislabelled(
        &self,
        ctx: toolkit_security::SecurityContext,
    ) -> Result<Frame, FrameError>;

    #[get("/truncated")]
    #[streaming(multipart_mixed, open = fallible)]
    #[server_manual]
    async fn truncated(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;

    #[get("/broker")]
    #[streaming(multipart_mixed, open = fallible)]
    #[server_manual]
    async fn broker_frames(
        &self,
        ctx: toolkit_security::SecurityContext,
    ) -> Result<BrokerFrame, FrameError>;

    // Immediate open: non-`async`, so the emitted signature is `-> Stream`
    // rather than `-> Result<Stream, E>` (#4740 #12).
    #[get("/frames-now")]
    #[streaming(multipart_mixed)]
    #[server_manual]
    fn frames_now(
        &self,
        ctx: toolkit_security::SecurityContext,
        count: u64,
    ) -> Result<Frame, FrameError>;

    #[get("/mislabelled-now")]
    #[streaming(multipart_mixed)]
    #[server_manual]
    fn mislabelled_now(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;

    #[get("/flapping-now")]
    #[streaming(multipart_mixed)]
    #[server_manual]
    fn flapping_now(&self, ctx: toolkit_security::SecurityContext) -> Result<Frame, FrameError>;
}

fn frame_client(base_url: &str) -> FrameApiRestClient {
    FrameApiRestClient::new(toolkit_contract::runtime::config::ClientConfig::new(
        base_url,
    ))
    .unwrap()
}

fn ctx() -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::anonymous()
}

#[tokio::test]
async fn generated_multipart_client_round_trips_through_the_framer() {
    let (base_url, state) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);

    let stream = FrameApi::frames(&client, ctx(), 4)
        .await
        .expect("the open must succeed");
    let frames: Vec<Frame> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("every item must decode");

    assert_eq!(frames, (0..4).map(|seq| Frame { seq }).collect::<Vec<_>>());

    // The projection trait's delegating defaults are part of the generated
    // surface too, so keep them exercised rather than merely emitted.
    let _projection: &dyn FrameApiRest = &client;

    // The declared framing decides the header. Getting this wrong is invisible
    // against a server that ignores `Accept`, which is exactly why it is
    // asserted against one that records it.
    assert_eq!(
        state.last_accept.lock().unwrap().clone().as_deref(),
        Some("multipart/mixed"),
    );
}

/// A **completed** multipart stream — one whose body ends with the
/// `--<boundary>--` close delimiter — ends cleanly, with no trailing error item.
/// The truncation check (#4740) keys on the *absence* of that delimiter, so a
/// properly-closed body must stay clean; this is the counterpart to
/// `a_truncated_multipart_stream_ends_with_a_framing_error`.
#[tokio::test]
async fn a_completed_multipart_stream_ends_without_an_error_item() {
    let (base_url, _) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);
    let stream = FrameApi::frames(&client, ctx(), 2).await.unwrap();
    let collected: Vec<Result<Frame, FrameError>> = stream.collect().await;
    assert_eq!(collected.len(), 2, "no trailing error item: {collected:?}");
    assert!(collected.iter().all(Result::is_ok), "{collected:?}");
}

/// #4740: a multipart body that ends gracefully WITHOUT the `--<boundary>--`
/// close delimiter is a truncation — a proxy idle-timeout, LB half-close, or
/// rolling deploy that closed the connection mid-body. On the boxed public
/// stream the caller cannot otherwise tell it from a complete body, so the
/// client surfaces it as a `Framing` error *after* the parts that did arrive.
#[tokio::test]
async fn a_truncated_multipart_stream_ends_with_a_framing_error() {
    let (base_url, _) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);
    let stream = FrameApi::truncated(&client, ctx())
        .await
        .expect("the open succeeds; the truncation is only discovered mid-stream");
    let collected: Vec<Result<Frame, FrameError>> = stream.collect().await;

    // The two well-formed parts are delivered first...
    let frames: Vec<&Frame> = collected.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(
        frames,
        vec![&Frame { seq: 0 }, &Frame { seq: 1 }],
        "the parts before the truncation must still be delivered: {collected:?}"
    );
    // ...then the missing close delimiter surfaces as the terminal item.
    let last = collected
        .last()
        .expect("the stream must yield at least the good frames plus an error");
    assert!(
        matches!(
            last,
            Err(FrameError::Transport(TransportError::Framing { framing, .. }))
                if *framing == toolkit_contract::StreamFraming::MultipartMixed
        ),
        "a missing close delimiter must surface as a MultipartMixed framing error, got {last:?}"
    );
}

/// The fallible open, over multipart: a `409` with a domain problem is an `Err`
/// from the awaited call, and no stream is produced.
#[tokio::test]
async fn a_multipart_open_that_fails_yields_err_and_no_stream() {
    let (base_url, _) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);
    let opened = FrameApi::closed(&client, ctx()).await;
    let err = opened
        .map(|_| ())
        .expect_err("a 409 at open must not become a stream");
    let FrameError::Transport(TransportError::Problem { problem, .. }) = err else {
        panic!("expected a Problem-carrying transport error, got {err:?}");
    };
    assert_eq!(problem.status, Some(409));
    assert_eq!(problem.title, "StreamClosed");
}

/// Multipart's boundary lives in the response's `Content-Type`, so its open can
/// still fail *after* a `200`. That failure is resolved eagerly, which is what
/// puts it in the `Err` where an open-time state machine can act on it — rather
/// than in a stream the caller has already been handed.
#[tokio::test]
async fn a_mislabelled_success_response_fails_the_open_not_the_stream() {
    let (base_url, _) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);
    let opened = FrameApi::mislabelled(&client, ctx()).await;
    let err = opened
        .map(|_| ())
        .expect_err("a 200 with no usable boundary must fail the open");
    let FrameError::Transport(TransportError::Framing { framing, .. }) = err else {
        panic!("expected a Framing error, got {err:?}");
    };
    assert_eq!(framing, toolkit_contract::StreamFraming::MultipartMixed);
}

/// Immediate open (a non-`async` `#[streaming(multipart_mixed)] fn`): the client
/// hands back the stream directly — no `.await`, no `Result` around it — the
/// lazy open still negotiates `Accept: multipart/mixed`, and items decode one
/// per part. The awaited methods above never exercise the immediate multipart
/// open (#4740 #12).
#[tokio::test]
async fn immediate_multipart_open_streams_items_and_sends_accept() {
    let (base_url, state) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);

    // Non-`async`: the call returns the stream itself, not a future of one.
    let items: Vec<Result<Frame, FrameError>> =
        FrameApi::frames_now(&client, ctx(), 3).collect().await;
    let frames: Vec<Frame> = items
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("every item must decode");

    assert_eq!(frames, (0..3).map(|seq| Frame { seq }).collect::<Vec<_>>());
    assert_eq!(
        state.last_accept.lock().unwrap().clone().as_deref(),
        Some("multipart/mixed"),
        "the lazy open still sends the declared framing's Accept header",
    );
}

/// On the immediate path there is no fallible open to carry a framing failure,
/// so a `200` with no usable boundary surfaces as the stream's **first item** —
/// the exact contrast with `a_mislabelled_success_response_fails_the_open_not_
/// the_stream`, where the awaited open returns it as an `Err` before any item
/// (#4740 #12).
#[tokio::test]
async fn immediate_multipart_mislabelled_200_is_the_first_item() {
    let (base_url, _) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);

    let items: Vec<Result<Frame, FrameError>> =
        FrameApi::mislabelled_now(&client, ctx()).collect().await;
    let first = items.into_iter().next().expect("a first item");
    let err = first.expect_err("a mislabelled 200 must arrive as an error item");
    let FrameError::Transport(TransportError::Framing { framing, .. }) = err else {
        panic!("expected a Framing error item, got {err:?}");
    };
    assert_eq!(framing, toolkit_contract::StreamFraming::MultipartMixed);
}

/// The immediate open honours the client's `stream_reconnect`: a transient
/// mid-stream failure (a truncated multipart body) reopens up to the budget,
/// delivering each connection's parts before the budget is spent and the
/// framing error finally surfaces. A *fallible* open can't show this — it uses
/// `ReconnectConfig::disabled()` regardless of client config (#4740 #12).
#[tokio::test]
async fn immediate_multipart_reopens_under_stream_reconnect() {
    use std::sync::atomic::Ordering;

    const MAX_ATTEMPTS: u32 = 2;
    let (base_url, state) = start_server_with_state(ContractState::default()).await;
    let cfg = toolkit_contract::runtime::config::ClientConfig::new(&base_url)
        .with_stream_reconnect(toolkit_contract::runtime::config::ReconnectConfig::enabled(
            MAX_ATTEMPTS,
            Duration::from_millis(1),
        ));
    let client = FrameApiRestClient::new(cfg).unwrap();

    let items: Vec<Result<Frame, FrameError>> =
        FrameApi::flapping_now(&client, ctx()).collect().await;

    // Each connection delivers its two parts before truncating; once the budget
    // is spent the last connection's truncation surfaces as the framing error.
    assert!(
        matches!(
            items.last(),
            Some(Err(FrameError::Transport(TransportError::Framing { .. })))
        ),
        "the spent budget must surface the framing error last: {items:?}",
    );
    let delivered = items.iter().filter(|i| i.is_ok()).count();
    assert_eq!(
        delivered,
        2 * (MAX_ATTEMPTS as usize + 1),
        "2 parts per connection"
    );
    assert_eq!(
        state.connections.load(Ordering::SeqCst),
        MAX_ATTEMPTS + 1,
        "initial open + MAX_ATTEMPTS reopens",
    );
}

/// A four-variant `#[serde(tag = "kind")]` union survives framer -> generated
/// client -> reader, one frame per part, with every variant's payload intact.
///
/// This is the shape event-broker's consumer will actually decode, and the claim
/// is that it costs the toolkit nothing: the item type is only ever `Serialize`
/// on the way out and `DeserializeOwned` on the way back, so an internally
/// tagged enum is no different from a struct to everything in between.
#[tokio::test]
async fn a_tagged_union_item_type_round_trips_one_frame_per_part() {
    let (base_url, _) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);

    let stream = FrameApi::broker_frames(&client, ctx())
        .await
        .expect("the open must succeed");
    let frames: Vec<BrokerFrame> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("every frame must decode");

    assert_eq!(
        frames,
        broker_frame_sequence(),
        "every variant must round-trip in order, payloads included"
    );
}

/// Each *variant* is decoded, not merely the sequence: a union whose tag were
/// mishandled could still produce the right item count.
#[tokio::test]
async fn every_frame_kind_decodes_to_its_own_variant() {
    let (base_url, _) = start_server_with_state(ContractState::default()).await;
    let client = frame_client(&base_url);

    let frames: Vec<BrokerFrame> = FrameApi::broker_frames(&client, ctx())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();

    // The nested-body kind.
    let Some(BrokerFrame::Event { payload }) = frames
        .iter()
        .find(|f| matches!(f, BrokerFrame::Event { .. }))
        .cloned()
    else {
        panic!("no event frame in {frames:?}");
    };
    assert_eq!(payload.event_type, "order.created");
    assert_eq!(payload.offset, 42);

    // The inline-field kinds.
    let Some(BrokerFrame::Topology {
        topology_version,
        assigned,
    }) = frames
        .iter()
        .find(|f| matches!(f, BrokerFrame::Topology { .. }))
        .cloned()
    else {
        panic!("no topology frame in {frames:?}");
    };
    assert_eq!(topology_version, 7);
    assert_eq!(assigned.len(), 2);

    assert!(
        frames
            .iter()
            .any(|f| matches!(f, BrokerFrame::Heartbeat { at } if at.starts_with("2026-"))),
        "no heartbeat frame in {frames:?}"
    );

    // Both control codes, and `reason` present on exactly the terminal one —
    // the optional field is what makes a single `Control` variant serve both.
    let controls: Vec<_> = frames
        .iter()
        .filter_map(|f| match f {
            BrokerFrame::Control {
                code,
                reason,
                positions,
            } => Some((code.as_str(), reason.as_deref(), positions.len())),
            _ => None,
        })
        .collect();
    assert_eq!(
        controls,
        vec![("progress", None, 1), ("terminal", Some("rebalanced"), 2)],
        "control frames must keep their code, optional reason and positions"
    );
}

/// The tag is a **sibling** of each variant's fields for three of the four
/// kinds, and nests only for `event`.
///
/// This asymmetry is the reason the plan's non-goals put frame typing in the
/// contract's item type instead of a toolkit-side `{kind, payload}` envelope: no
/// single generic envelope describes both shapes. Asserting it on the wire bytes
/// keeps that rationale from quietly becoming false.
#[test]
fn the_tag_is_inline_for_every_kind_but_event() {
    let control = serde_json::to_value(BrokerFrame::Control {
        code: "terminal".to_owned(),
        reason: Some("rebalanced".to_owned()),
        positions: vec![position("orders", 0)],
    })
    .unwrap();
    let obj = control.as_object().expect("a frame is a JSON object");
    assert_eq!(obj["kind"], "control");
    // Inline: siblings of the tag, with no `payload` indirection.
    assert_eq!(obj["code"], "terminal");
    assert_eq!(obj["reason"], "rebalanced");
    assert!(obj["positions"].is_array());
    assert!(
        !obj.contains_key("payload"),
        "control must not nest its fields: {control}"
    );

    // A progress control frame omits `reason` entirely rather than sending null.
    let progress = serde_json::to_value(BrokerFrame::Control {
        code: "progress".to_owned(),
        reason: None,
        positions: vec![],
    })
    .unwrap();
    assert!(
        !progress.as_object().expect("object").contains_key("reason"),
        "an absent reason must be omitted, not null: {progress}"
    );

    // And `event` is the one kind that does nest.
    let event = serde_json::to_value(BrokerFrame::Event {
        payload: EventEnvelope {
            id: "evt-1".to_owned(),
            event_type: "order.created".to_owned(),
            partition: 0,
            offset: 42,
        },
    })
    .unwrap();
    assert_eq!(event["kind"], "event");
    assert_eq!(event["payload"]["type"], "order.created");
}
