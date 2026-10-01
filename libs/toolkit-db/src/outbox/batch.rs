use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::handler::OutboxMessage;

/// Rejection record for a message that a handler marked for dead-lettering.
#[derive(Debug, Clone)]
pub struct Rejection {
    /// Index into the original message slice.
    pub index: usize,
    pub reason: String,
}

/// Lease-aware message iterator passed to [`super::handler::LeasedHandler::handle`].
///
/// Provides single-message and chunked iteration, progress tracking, and
/// remaining lease time. The handler owns timeout decisions - `Batch` only
/// exposes facts (`remaining()`, `should_stop()`, `len()`), never timeout
/// suggestions.
pub struct Batch<'a> {
    msgs: &'a [OutboxMessage],
    cursor: usize,
    processed: u32,
    rejections: Vec<Rejection>,
    lease_deadline: Instant,
    cancel: CancellationToken,
}

impl<'a> Batch<'a> {
    pub(crate) fn new(
        msgs: &'a [OutboxMessage],
        lease_deadline: Instant,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            msgs,
            cursor: 0,
            processed: 0,
            rejections: Vec::new(),
            lease_deadline,
            cancel,
        }
    }

    /// Next single unprocessed message, or `None` if exhausted.
    pub fn next_msg(&mut self) -> Option<&OutboxMessage> {
        if self.cursor < self.msgs.len() {
            let msg = &self.msgs[self.cursor];
            self.cursor += 1;
            Some(msg)
        } else {
            None
        }
    }

    /// Next chunk of up to `n` messages. Returns an empty slice if exhausted.
    pub fn next_chunk(&mut self, n: usize) -> &[OutboxMessage] {
        let start = self.cursor;
        let end = (start + n).min(self.msgs.len());
        self.cursor = end;
        &self.msgs[start..end]
    }

    /// Mark the last `next()` message as successfully processed.
    pub fn ack(&mut self) {
        self.processed += 1;
    }

    /// Mark the last `next_chunk()` as successfully processed.
    pub fn ack_chunk(&mut self, count: u32) {
        self.processed += count;
    }

    /// Mark the current message for dead-lettering with the given reason.
    /// Increments processed count (the message is "handled", just negatively).
    pub fn reject(&mut self, reason: String) {
        let index = self.cursor.saturating_sub(1);
        self.rejections.push(Rejection { index, reason });
        self.processed += 1;
    }

    /// How much lease time remains before the cancel point
    /// (`lease_duration - ack_headroom`).
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.lease_deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO)
    }

    /// Whether the handler should stop starting new messages: the lease
    /// budget is spent or the outbox is shutting down. Checked between
    /// messages; the message already in flight keeps its budget, which
    /// [`Self::remaining`] goes on reporting from the lease alone.
    #[must_use]
    pub fn should_stop(&self) -> bool {
        self.remaining().is_zero() || self.cancel.is_cancelled()
    }

    /// Number of unconsumed messages remaining.
    #[must_use]
    pub fn len(&self) -> usize {
        self.msgs.len() - self.cursor
    }

    /// Whether all messages have been consumed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cursor >= self.msgs.len()
    }

    /// Total messages processed so far (acked + rejected).
    #[must_use]
    pub fn processed(&self) -> u32 {
        self.processed
    }

    /// Messages marked for dead-lettering.
    pub(crate) fn rejections(&self) -> &[Rejection] {
        &self.rejections
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
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
        let mut batch = Batch::new(&msgs, deadline, CancellationToken::new());

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
        let mut batch = Batch::new(&msgs, deadline, CancellationToken::new());

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
        let mut batch = Batch::new(&msgs, deadline, CancellationToken::new());

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
        let mut batch = Batch::new(&msgs, deadline, CancellationToken::new());

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
        let batch = Batch::new(&msgs, deadline, CancellationToken::new());
        let remaining = batch.remaining();
        // Should be close to 10s (allow some slack for test execution)
        assert!(remaining > Duration::from_secs(9));
        assert!(remaining <= Duration::from_secs(10));
    }

    #[test]
    fn remaining_returns_zero_when_past_deadline() {
        let msgs: Vec<OutboxMessage> = vec![];
        let deadline = Instant::now(); // already expired
        let batch = Batch::new(&msgs, deadline, CancellationToken::new());
        assert_eq!(batch.remaining(), Duration::ZERO);
    }

    #[test]
    fn should_stop_once_the_lease_is_spent() {
        let msgs: Vec<OutboxMessage> = vec![];
        let batch = Batch::new(&msgs, Instant::now(), CancellationToken::new());
        assert!(batch.should_stop());
    }

    #[test]
    fn should_stop_on_cancel_while_remaining_keeps_the_lease() {
        let msgs: Vec<OutboxMessage> = vec![];
        let deadline = Instant::now() + Duration::from_secs(10);
        let cancel = CancellationToken::new();
        let batch = Batch::new(&msgs, deadline, cancel.clone());
        assert!(!batch.should_stop());

        cancel.cancel();
        assert!(batch.should_stop());
        assert!(batch.remaining() > Duration::from_secs(9));
    }
}
