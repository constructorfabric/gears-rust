use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

fn make_msg(seq: i64) -> OutboxMessage {
    OutboxMessage {
        partition_id: 1,
        seq,
        payload: vec![],
        payload_type: "test".into(),
        created_at: chrono::Utc::now(),
        attempts: 0,
    }
}

// --- LeasedMessageHandler blanket impl tests ---

struct LeasedCountingHandler {
    count: AtomicU32,
}

impl LeasedCountingHandler {
    fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
        }
    }
}

#[async_trait::async_trait]
impl LeasedMessageHandler for LeasedCountingHandler {
    async fn handle(&self, _msg: &OutboxMessage) -> MessageResult {
        self.count.fetch_add(1, Ordering::Relaxed);
        MessageResult::Ok
    }
}

struct LeasedFailAtHandler {
    fail_at: u32,
    count: AtomicU32,
    reject: bool,
}

#[async_trait::async_trait]
impl LeasedMessageHandler for LeasedFailAtHandler {
    async fn handle(&self, _msg: &OutboxMessage) -> MessageResult {
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        if n == self.fail_at {
            if self.reject {
                return MessageResult::Reject("bad".into());
            }
            return MessageResult::Retry;
        }
        MessageResult::Ok
    }
}

#[tokio::test]
async fn leased_blanket_all_success() {
    let handler = LeasedCountingHandler::new();
    let msgs: Vec<OutboxMessage> = (1..=5).map(make_msg).collect();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut batch = Batch::new(&msgs, deadline);

    let result = LeasedHandler::handle(&handler, &mut batch).await;
    assert!(matches!(result, HandlerResult::Success));
    assert_eq!(batch.processed(), 5);
    assert_eq!(handler.count.load(Ordering::Relaxed), 5);
}

#[tokio::test]
async fn leased_blanket_retry_at_third() {
    let handler = LeasedFailAtHandler {
        fail_at: 2,
        count: AtomicU32::new(0),
        reject: false,
    };
    let msgs: Vec<OutboxMessage> = (1..=5).map(make_msg).collect();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut batch = Batch::new(&msgs, deadline);

    let result = LeasedHandler::handle(&handler, &mut batch).await;
    assert!(matches!(result, HandlerResult::Retry { .. }));
    assert_eq!(batch.processed(), 2);
}

#[tokio::test]
async fn leased_blanket_reject_continues() {
    let handler = LeasedFailAtHandler {
        fail_at: 1,
        count: AtomicU32::new(0),
        reject: true,
    };
    let msgs: Vec<OutboxMessage> = (1..=5).map(make_msg).collect();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut batch = Batch::new(&msgs, deadline);

    let result = LeasedHandler::handle(&handler, &mut batch).await;
    // Reject at msg 2 (index 1), but blanket impl continues with rest
    assert!(matches!(result, HandlerResult::Success));
    assert_eq!(batch.processed(), 5);
    assert_eq!(batch.rejections().len(), 1);
    assert_eq!(batch.rejections()[0].index, 1);
}
