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
        r#""topic":"gts.cf.core.events.topic.v1~example.mock.showcase.rest.v1","#,
        r#""tenant_id":"01890a5d-ac96-774b-bcce-b302099a8058","#,
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
