use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use gts::{GtsInstanceId, GtsTypeId};

use crate::consumer::Delivery;
use crate::error::ConsumerError;
use crate::gts::EventV1;
use crate::ids::ConsumerGroupId;
use crate::sequence::Sequence;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterRecord {
    pub consumer_group: Option<ConsumerGroupId>,
    pub topic: GtsInstanceId,
    pub event_type: GtsTypeId,
    pub subject: String,
    pub subject_type: GtsTypeId,
    pub partition: u32,
    pub offset: Sequence,
    pub attempts: Option<u16>,
    pub reason: String,
    pub payload: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
    pub event: Uuid,
}

impl DeadLetterRecord {
    /// Starts a record of `event`, delivered as `delivery` says, parked for `reason`.
    ///
    /// The topic, the partition and `attempts` come from `delivery`; the event type,
    /// subject, subject type and id from `event`. `offset` is the event's
    /// sequence, or [`Sequence::NONE`] (no position) for an event that carries none.
    /// `payload` is the event's data, or JSON `null` for a body-less event. `occurred_at`
    /// is the time `builder` is called (the dead-lettering time), unless set.
    #[must_use]
    pub fn builder(
        event: &EventV1,
        delivery: &Delivery,
        reason: impl Into<String>,
    ) -> DeadLetterRecordBuilder {
        DeadLetterRecordBuilder {
            reason: reason.into(),
            consumer_group: None,
            topic: delivery.topic().clone(),
            event_type: event.event_type().clone(),
            subject: event.subject().to_owned(),
            subject_type: event.subject_type().clone(),
            partition: delivery.partition(),
            offset: event.sequence().unwrap_or(Sequence::NONE),
            attempts: Some(delivery.attempts()),
            payload: event.data().cloned().unwrap_or(serde_json::Value::Null),
            occurred_at: Utc::now(),
            event: event.id(),
        }
    }

    /// A record built as [`DeadLetterRecord::builder`] starts it.
    #[must_use]
    pub fn from_event(event: &EventV1, delivery: &Delivery, reason: impl Into<String>) -> Self {
        Self::builder(event, delivery, reason).build()
    }
}

pub struct DeadLetterRecordBuilder {
    reason: String,
    consumer_group: Option<ConsumerGroupId>,
    topic: GtsInstanceId,
    event_type: GtsTypeId,
    subject: String,
    subject_type: GtsTypeId,
    partition: u32,
    offset: Sequence,
    attempts: Option<u16>,
    payload: serde_json::Value,
    occurred_at: DateTime<Utc>,
    event: Uuid,
}

// Leaves the payload out: it may carry personal or secret data.
impl std::fmt::Debug for DeadLetterRecordBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeadLetterRecordBuilder")
            .field("reason", &self.reason)
            .field("consumer_group", &self.consumer_group)
            .field("topic", &self.topic)
            .field("event_type", &self.event_type)
            .field("subject", &self.subject)
            .field("subject_type", &self.subject_type)
            .field("partition", &self.partition)
            .field("offset", &self.offset)
            .field("attempts", &self.attempts)
            .field("occurred_at", &self.occurred_at)
            .field("event", &self.event)
            .finish_non_exhaustive()
    }
}

impl DeadLetterRecordBuilder {
    #[must_use]
    pub fn consumer_group(mut self, consumer_group: ConsumerGroupId) -> Self {
        self.consumer_group = Some(consumer_group);
        self
    }

    #[must_use]
    pub fn attempts(mut self, attempts: u16) -> Self {
        self.attempts = Some(attempts);
        self
    }

    #[must_use]
    pub fn occurred_at(mut self, occurred_at: DateTime<Utc>) -> Self {
        self.occurred_at = occurred_at;
        self
    }

    #[must_use]
    pub fn build(self) -> DeadLetterRecord {
        DeadLetterRecord {
            consumer_group: self.consumer_group,
            topic: self.topic,
            event_type: self.event_type,
            subject: self.subject,
            subject_type: self.subject_type,
            partition: self.partition,
            offset: self.offset,
            attempts: self.attempts,
            reason: self.reason,
            payload: self.payload,
            occurred_at: self.occurred_at,
            event: self.event,
        }
    }
}

#[async_trait::async_trait]
pub trait DeadLetterSink: Send + Sync {
    async fn park(&self, record: DeadLetterRecord) -> Result<(), ConsumerError>;
}
