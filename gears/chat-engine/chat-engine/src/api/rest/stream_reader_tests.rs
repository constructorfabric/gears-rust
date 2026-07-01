use super::*;
use crate::domain::error::ChatEngineError;
use crate::infra::db::repo::stream_event_repo::BufferedEvent;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use time::OffsetDateTime;

/// In-memory fake buffer for deterministic reader tests.
#[derive(Default)]
struct FakeBuffer {
    events: Mutex<Vec<(Uuid, u64, JsonValue)>>,
}

#[async_trait]
impl StreamEventBuffer for FakeBuffer {
    async fn append(
        &self,
        message_id: Uuid,
        seq: u64,
        event: JsonValue,
        _expires_at: OffsetDateTime,
    ) -> Result<(), ChatEngineError> {
        self.events.lock().push((message_id, seq, event));
        Ok(())
    }

    async fn read_since(
        &self,
        message_id: Uuid,
        after_seq: Option<u64>,
    ) -> Result<Vec<BufferedEvent>, ChatEngineError> {
        let evs = self.events.lock();
        Ok(evs
            .iter()
            .filter(|(mid, seq, _)| *mid == message_id && after_seq.is_none_or(|a| *seq > a))
            .map(|(_, seq, event)| BufferedEvent {
                seq: *seq,
                event: event.clone(),
            })
            .collect())
    }

    async fn delete_expired(&self, _now: OffsetDateTime) -> Result<u64, ChatEngineError> {
        Ok(0)
    }
}

#[tokio::test]
async fn reader_drains_buffered_stream_until_terminal() {
    let mid = Uuid::new_v4();
    let buf = Arc::new(FakeBuffer::default());
    let ttl = OffsetDateTime::now_utc();
    buf.append(
        mid,
        0,
        json!({"type": "message.start", "message_id": mid, "seq": 0}),
        ttl,
    )
    .await
    .unwrap();
    buf.append(
        mid,
        1,
        json!({"type": "message.text.delta", "seq": 1, "op": "append", "path": "parts/0/content/text", "value": "hi"}),
        ttl,
    )
    .await
    .unwrap();
    buf.append(mid, 2, json!({"type": "message.complete", "seq": 2}), ttl)
        .await
        .unwrap();

    // Terminal event is already buffered → the reader drains in one batch
    // and ends without any live polling.
    let chunks: Vec<Vec<u8>> = buffer_reader_stream(buf, mid, None, CancellationToken::new())
        .collect()
        .await;
    let text: String = chunks
        .into_iter()
        .map(|b| String::from_utf8(b).unwrap())
        .collect();
    assert!(text.contains("event: message.start"));
    assert!(text.contains("event: message.text.delta"));
    assert!(text.contains("event: message.complete"));
    assert!(text.contains("id: 2"));
}

#[tokio::test]
async fn reader_replays_only_after_last_event_id() {
    let mid = Uuid::new_v4();
    let buf = Arc::new(FakeBuffer::default());
    let ttl = OffsetDateTime::now_utc();
    for seq in 0..3u64 {
        let ty = if seq == 2 {
            "message.complete"
        } else {
            "message.text.delta"
        };
        buf.append(mid, seq, json!({"type": ty, "seq": seq}), ttl)
            .await
            .unwrap();
    }
    // Resume from seq 0 → only seq 1 and 2 (terminal) are replayed.
    let chunks: Vec<Vec<u8>> = buffer_reader_stream(buf, mid, Some(0), CancellationToken::new())
        .collect()
        .await;
    let text: String = chunks
        .into_iter()
        .map(|b| String::from_utf8(b).unwrap())
        .collect();
    assert!(!text.contains("id: 0\n"));
    assert!(text.contains("id: 1"));
    assert!(text.contains("id: 2"));
}

#[tokio::test]
async fn cancelled_reader_stops() {
    let mid = Uuid::new_v4();
    let buf = Arc::new(FakeBuffer::default());
    // No terminal event buffered; a pre-cancelled token must stop the
    // reader immediately instead of polling forever.
    let cancel = CancellationToken::new();
    cancel.cancel();
    let chunks: Vec<Vec<u8>> = buffer_reader_stream(buf, mid, None, cancel).collect().await;
    assert!(chunks.is_empty());
}
