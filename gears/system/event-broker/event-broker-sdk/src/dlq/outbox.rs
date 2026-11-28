use std::sync::Arc;

use crate::error::{ConsumerError, EventBrokerError, OutboxRole};

use super::{DeadLetterEnvelope, DeadLetterRecord};

#[derive(Clone)]
pub struct ConsumerDlqOutbox {
    outbox: Arc<toolkit_db::outbox::Outbox>,
    queue: String,
    partitions: u32,
}

pub struct ConsumerDlqOutboxBuilder {
    outbox: Arc<toolkit_db::outbox::Outbox>,
    queue: Option<String>,
    partitions: Option<u32>,
}

impl ConsumerDlqOutbox {
    pub fn builder(outbox: Arc<toolkit_db::outbox::Outbox>) -> ConsumerDlqOutboxBuilder {
        ConsumerDlqOutboxBuilder {
            outbox,
            queue: None,
            partitions: None,
        }
    }

    #[must_use]
    pub fn queue(&self) -> &str {
        &self.queue
    }

    #[must_use]
    pub fn partitions(&self) -> u32 {
        self.partitions
    }

    #[must_use]
    pub fn partition_for_record(&self, record: &DeadLetterRecord) -> u32 {
        record.partition % self.partitions
    }

    /// Enqueue a dead-letter handoff record within the caller's transaction.
    ///
    /// The row is written atomically with `runner`'s transaction, but the DLQ
    /// sequencer is **not** woken by the write itself. Call
    /// [`Wake::fire`](toolkit_db::outbox::Wake::fire) on the
    /// returned handle once that transaction has committed; on rollback, drop
    /// the handle instead. Until it is fired the record is durable but stays
    /// unsequenced until the outbox's cold reconciler discovers it.
    ///
    /// # Errors
    ///
    /// Returns an error if the envelope cannot be serialized. Returns
    /// [`EventBrokerError::Outbox`] of [`OutboxRole::DeadLetter`] if the outbox
    /// refuses the record (for example a queue it does not hold, or a payload
    /// over its size limit) or the database rejects the write; a database
    /// error is its source.
    pub async fn enqueue(
        &self,
        runner: &(impl toolkit_db::secure::DBRunner + Sync + ?Sized),
        record: DeadLetterRecord,
    ) -> Result<toolkit_db::outbox::Wake, ConsumerError> {
        let partition = self.partition_for_record(&record);
        let envelope = DeadLetterEnvelope::from_record(record);
        let payload = envelope.to_vec()?;

        let message = toolkit_db::outbox::Record::to(&self.queue, partition)
            .payload(payload, DeadLetterEnvelope::PAYLOAD_TYPE)
            .build()
            .map_err(|err| EventBrokerError::outbox(OutboxRole::DeadLetter, err))?;
        self.outbox
            .enqueue(runner, message)
            .await
            .map_err(|err| EventBrokerError::outbox(OutboxRole::DeadLetter, err))
    }
}

impl ConsumerDlqOutboxBuilder {
    #[must_use]
    pub fn queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = Some(queue.into());
        self
    }

    #[must_use]
    pub fn partitions(mut self, partitions: u32) -> Self {
        self.partitions = Some(partitions);
        self
    }

    #[must_use]
    pub fn build(self) -> ConsumerDlqOutbox {
        ConsumerDlqOutbox {
            outbox: self.outbox,
            queue: self.queue.unwrap_or_else(|| "consumer-dlq".to_owned()),
            partitions: self.partitions.unwrap_or(1).max(1),
        }
    }
}
