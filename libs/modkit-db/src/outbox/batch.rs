use std::time::Duration;

use tokio::time::Instant;

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
/// exposes facts (`remaining()`, `len()`), never timeout suggestions.
pub struct Batch<'a> {
    msgs: &'a [OutboxMessage],
    cursor: usize,
    processed: u32,
    rejections: Vec<Rejection>,
    lease_deadline: Instant,
}

impl<'a> Batch<'a> {
    pub(crate) fn new(msgs: &'a [OutboxMessage], lease_deadline: Instant) -> Self {
        Self {
            msgs,
            cursor: 0,
            processed: 0,
            rejections: Vec::new(),
            lease_deadline,
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
#[path = "batch_tests.rs"]
mod tests;
