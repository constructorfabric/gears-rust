//! Decoder tests: both framings, fed the same frames, decode to the same
//! sequence. Mirrors the server encoders in
//! `event-broker/src/api/rest/handlers/delivery/encode.rs`.

use super::*;

const HEARTBEAT: &str = r#"{"kind":"heartbeat","at":"2026-01-02T03:04:05Z"}"#;
const TOPOLOGY: &str = r#"{"kind":"topology","topology_version":2,"assigned":[]}"#;

fn as_values(frames: Vec<FrameWire>) -> Vec<serde_json::Value> {
    frames
        .into_iter()
        .map(|f| serde_json::to_value(f).unwrap())
        .collect()
}

fn sse_blob() -> Vec<u8> {
    // axum SSE: `event: <kind>` then `data: <json>` then a blank line.
    format!("event: heartbeat\ndata: {HEARTBEAT}\n\nevent: topology\ndata: {TOPOLOGY}\n\n")
        .into_bytes()
}

fn multipart_blob(boundary: &str) -> Vec<u8> {
    // Mirrors `to_multipart_part`: boundary, JSON content type, blank line, body,
    // trailing CRLF.
    format!(
        "--{boundary}\r\nContent-Type: application/json\r\n\r\n{HEARTBEAT}\r\n\
         --{boundary}\r\nContent-Type: application/json\r\n\r\n{TOPOLOGY}\r\n"
    )
    .into_bytes()
}

#[test]
fn sse_and_multipart_decode_to_the_same_frames() {
    let mut sse = Decoder::sse();
    sse.push(&sse_blob());
    let sse_frames = as_values(sse.drain().unwrap());

    let mut mp = Decoder::multipart("evbk-boundary".to_owned());
    mp.push(&multipart_blob("evbk-boundary"));
    let mp_frames = as_values(mp.drain().unwrap());

    assert_eq!(sse_frames, mp_frames);
    assert_eq!(
        sse_frames,
        vec![
            serde_json::from_str::<serde_json::Value>(HEARTBEAT).unwrap(),
            serde_json::from_str::<serde_json::Value>(TOPOLOGY).unwrap(),
        ]
    );
}

#[test]
fn sse_buffers_a_partial_event_across_chunks() {
    let blob = sse_blob();
    let split = blob.len() / 2;
    let mut sse = Decoder::sse();
    sse.push(&blob[..split]);
    let first = sse.drain().unwrap();
    sse.push(&blob[split..]);
    let mut all = as_values(first);
    all.extend(as_values(sse.drain().unwrap()));
    assert_eq!(all.len(), 2);
    assert_eq!(
        all[0],
        serde_json::from_str::<serde_json::Value>(HEARTBEAT).unwrap()
    );
    assert_eq!(
        all[1],
        serde_json::from_str::<serde_json::Value>(TOPOLOGY).unwrap()
    );
}

/// Serde's message quotes the offending value, and an event frame carries
/// producer-supplied fields; the dispatcher logs this error, so it names only
/// the failure's category.
#[test]
fn an_undecodable_sse_frame_is_refused_without_quoting_its_content() {
    let body = concat!(
        r#"{"kind":"event","payload":{"#,
        r#""id":"01890a5d-ac96-774b-bcce-b302099a8057","#,
        r#""type":"gts.cf.core.events.event.v1~example.mock.showcase.rest.v1~","#,
        r#""subject_tenant":"01890a5d-ac96-774b-bcce-b302099a8058","#,
        r#""source":"svc","subject":"s-1","subject_type":"gts.x.eb.showcase.subject.v1~","#,
        r#""occurred_at":"2026-01-02T03:04:05Z","trace_parent":null,"data":{},"#,
        r#""partition":0,"sequence":"PLANTED-seq","sequence_time":null}}"#,
    );
    let serde_message = serde_json::from_str::<FrameWire>(body)
        .unwrap_err()
        .to_string();
    assert!(
        serde_message.contains("PLANTED-seq"),
        "serde's own message quotes the planted value: {serde_message}"
    );

    let mut sse = Decoder::sse();
    sse.push(format!("event: event\ndata: {body}\n\n").as_bytes());
    let Err(EventBrokerError::Transport(message)) = sse.drain() else {
        panic!("an undecodable frame is a transport error");
    };
    assert_eq!(message, "undecodable stream frame (data)");
}

/// Serves an SSE delivery stream that neither sends nor closes once it has sent its
/// heartbeat, as a half-open connection looks from the client: no FIN, no RST, no
/// bytes. The first stream opened sends one heartbeat, and every later one too when
/// `heartbeat_on_every_open`; the others send nothing. Returns the base URL and the
/// count of streams opened.
async fn half_open_server(
    heartbeat_on_every_open: bool,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use futures_util::StreamExt as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let opened = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&opened);
    let handler = move || {
        let counter = std::sync::Arc::clone(&counter);
        async move {
            let first = counter.fetch_add(1, Ordering::SeqCst) == 0;
            let heartbeat = (first || heartbeat_on_every_open)
                .then(|| format!("event: heartbeat\ndata: {HEARTBEAT}\n\n"));
            let body = futures_util::stream::iter(heartbeat)
                .map(|heartbeat| {
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(heartbeat))
                })
                .chain(futures_util::stream::pending());
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(body),
            )
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("resolve the bound address");
    let router =
        axum::Router::new().route("/event-broker/v1/events:sse", axum::routing::get(handler));
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    (format!("http://{addr}"), opened)
}

fn idle_timing_out_broker(base_url: String) -> RestBroker {
    RestBroker::new(base_url)
        .expect("the client builds")
        .with_stream_transport(StreamTransport::Sse)
        .with_stream_idle_timeout(Duration::from_millis(200))
}

/// A stream that goes silent past the idle timeout is treated like a dropped
/// connection: the client reconnects, and once the reconnect budget is spent on
/// connections that deliver nothing the stream ends, so the consumer re-JOINs
/// instead of waiting on it forever.
#[tokio::test]
async fn a_stream_silent_past_the_idle_timeout_reconnects_then_ends() {
    use futures_util::StreamExt as _;

    let (base_url, opened) = half_open_server(false).await;
    let stream = open(
        &idle_timing_out_broker(base_url),
        &SecurityContext::anonymous(),
        SubscriptionId(uuid::Uuid::from_u128(0x5e)),
    )
    .await
    .expect("the stream opens");
    let frames = tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>())
        .await
        .expect("the silent stream ends instead of hanging");

    assert!(
        matches!(frames.as_slice(), [Ok(WireFrame::Heartbeat { .. })]),
        "only the first connection delivers: {frames:?}"
    );
    assert_eq!(
        opened.load(std::sync::atomic::Ordering::SeqCst),
        MAX_RECONNECTS as usize + 1
    );
}

/// The reconnect budget counts consecutive connections that deliver nothing: a
/// connection that delivers a frame before it stalls starts the count afresh, so a
/// long-lived stream is not ended by stalls spread over its life.
#[tokio::test]
async fn a_connection_that_delivers_before_stalling_restarts_the_reconnect_budget() {
    use futures_util::StreamExt as _;

    let (base_url, opened) = half_open_server(true).await;
    let stream = open(
        &idle_timing_out_broker(base_url),
        &SecurityContext::anonymous(),
        SubscriptionId(uuid::Uuid::from_u128(0x5f)),
    )
    .await
    .expect("the stream opens");
    let past_the_budget = MAX_RECONNECTS as usize + 3;
    let frames = tokio::time::timeout(
        Duration::from_secs(10),
        stream.take(past_the_budget).collect::<Vec<_>>(),
    )
    .await
    .expect("the frames arrive within the bound");

    assert_eq!(frames.len(), past_the_budget, "{frames:?}");
    assert!(
        frames
            .iter()
            .all(|frame| matches!(frame, Ok(WireFrame::Heartbeat { .. }))),
        "{frames:?}"
    );
    assert_eq!(
        opened.load(std::sync::atomic::Ordering::SeqCst),
        past_the_budget
    );
}
