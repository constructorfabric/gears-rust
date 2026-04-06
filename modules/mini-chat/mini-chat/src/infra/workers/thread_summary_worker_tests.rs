use super::*;
use modkit_db::outbox::LeasedMessageHandler;

fn make_msg() -> OutboxMessage {
    OutboxMessage {
        partition_id: 1,
        seq: 1,
        payload: b"{}".to_vec(),
        payload_type: "application/json".to_owned(),
        created_at: chrono::Utc::now(),
        attempts: 0i16,
    }
}

#[tokio::test]
async fn stub_returns_retry() {
    let handler = ThreadSummaryHandler;
    let msg = make_msg();
    let result = LeasedMessageHandler::handle(&handler, &msg).await;
    assert!(matches!(result, MessageResult::Retry));
}
