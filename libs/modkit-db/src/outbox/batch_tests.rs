use super::*;
use crate::outbox::handler::OutboxMessage;

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

#[test]
fn next_iterates_all_messages() {
    let msgs: Vec<OutboxMessage> = (1..=3).map(make_msg).collect();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut batch = Batch::new(&msgs, deadline);

    assert_eq!(batch.len(), 3);
    assert!(!batch.is_empty());

    assert_eq!(batch.next_msg().unwrap().seq, 1);
    assert_eq!(batch.next_msg().unwrap().seq, 2);
    assert_eq!(batch.next_msg().unwrap().seq, 3);
    assert!(batch.next_msg().is_none());
    assert!(batch.is_empty());
}

#[test]
fn next_chunk_returns_correct_slices() {
    let msgs: Vec<OutboxMessage> = (1..=7).map(make_msg).collect();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut batch = Batch::new(&msgs, deadline);

    let chunk1 = batch.next_chunk(3);
    assert_eq!(chunk1.len(), 3);
    assert_eq!(chunk1[0].seq, 1);

    let chunk2 = batch.next_chunk(3);
    assert_eq!(chunk2.len(), 3);
    assert_eq!(chunk2[0].seq, 4);

    let chunk3 = batch.next_chunk(3);
    assert_eq!(chunk3.len(), 1); // only 1 remaining
    assert_eq!(chunk3[0].seq, 7);

    assert!(batch.next_chunk(3).is_empty());
}

#[test]
fn ack_and_ack_chunk_track_progress() {
    let msgs: Vec<OutboxMessage> = (1..=5).map(make_msg).collect();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut batch = Batch::new(&msgs, deadline);

    assert_eq!(batch.processed(), 0);

    batch.next_msg();
    batch.ack();
    assert_eq!(batch.processed(), 1);

    batch.next_chunk(3);
    batch.ack_chunk(3);
    assert_eq!(batch.processed(), 4);
}

#[test]
fn reject_tracks_rejection_and_progress() {
    let msgs: Vec<OutboxMessage> = (1..=3).map(make_msg).collect();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut batch = Batch::new(&msgs, deadline);

    batch.next_msg(); // msg 1
    batch.ack();
    batch.next_msg(); // msg 2
    batch.reject("bad payload".into());
    batch.next_msg(); // msg 3
    batch.ack();

    assert_eq!(batch.processed(), 3);
    assert_eq!(batch.rejections().len(), 1);
    assert_eq!(batch.rejections()[0].index, 1);
    assert_eq!(batch.rejections()[0].reason, "bad payload");
}

#[test]
fn remaining_returns_time_until_deadline() {
    let msgs: Vec<OutboxMessage> = vec![];
    let deadline = Instant::now() + Duration::from_secs(10);
    let batch = Batch::new(&msgs, deadline);
    let remaining = batch.remaining();
    // Should be close to 10s (allow some slack for test execution)
    assert!(remaining > Duration::from_secs(9));
    assert!(remaining <= Duration::from_secs(10));
}

#[test]
fn remaining_returns_zero_when_past_deadline() {
    let msgs: Vec<OutboxMessage> = vec![];
    let deadline = Instant::now(); // already expired
    let batch = Batch::new(&msgs, deadline);
    assert_eq!(batch.remaining(), Duration::ZERO);
}
