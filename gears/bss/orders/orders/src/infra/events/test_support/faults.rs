//! Deterministic faults at the Orders producer/client boundary; successful appends
//! always go through the real broker before their acknowledgement can be changed.
use event_broker_sdk::models::{EventType, ProducerMeta};
use event_broker_sdk::*;
use parking_lot::Mutex;
use std::{collections::VecDeque, sync::Arc};
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub enum TransientFault {
    Transport,
    RateLimited,
}
impl TransientFault {
    fn error(self) -> EventBrokerError {
        match self {
            Self::Transport => {
                EventBrokerError::Transport("Orders test transport interrupted".into())
            }
            Self::RateLimited => EventBrokerError::RateLimited {
                retry_after_secs: 0,
                detail: "Orders test rate limit".into(),
            },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveredOutcome {
    Accepted,
    Persisted,
    Duplicate,
    SequenceViolation,
    UnknownProducer,
    Refused(String),
}
#[derive(Debug, Clone)]
pub struct DeliveredPublish {
    pub subject: String,
    pub event_id: Uuid,
    pub meta: Option<ProducerMeta>,
    pub outcome: DeliveredOutcome,
    pub ack_lost: bool,
}
#[derive(Default)]
struct Faults {
    publishes: VecDeque<TransientFault>,
    cursors: VecDeque<TransientFault>,
    cursor_reads: usize,
    lost_acks: usize,
    persisted: usize,
    disconnect_after_ack: bool,
    disconnected: bool,
    delivered: Vec<DeliveredPublish>,
}
pub struct FaultInjectingBroker {
    inner: Arc<dyn EventBrokerApi>,
    faults: Mutex<Faults>,
}
impl FaultInjectingBroker {
    pub fn new(inner: Arc<dyn EventBrokerApi>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            faults: Mutex::new(Faults::default()),
        })
    }
    pub fn report_persisted(&self, count: usize) {
        self.faults.lock().persisted += count;
    }
    pub fn lose_publish_acks(&self, count: usize) {
        self.faults.lock().lost_acks += count;
    }
    pub fn lose_next_ack_then_disconnect(&self) {
        let mut f = self.faults.lock();
        f.lost_acks += 1;
        f.disconnect_after_ack = true;
    }
    pub fn is_disconnected(&self) -> bool {
        self.faults.lock().disconnected
    }
    pub fn reconnect(&self) {
        self.faults.lock().disconnected = false;
    }
    pub fn fail_publishes(&self, faults: impl IntoIterator<Item = TransientFault>) {
        self.faults.lock().publishes.extend(faults);
    }
    pub fn fail_cursor_reads(&self, faults: impl IntoIterator<Item = TransientFault>) {
        self.faults.lock().cursors.extend(faults);
    }
    pub fn cursor_reads(&self) -> usize {
        self.faults.lock().cursor_reads
    }
    pub fn delivered(&self) -> usize {
        self.faults.lock().delivered.len()
    }
    pub fn delivered_publishes(&self) -> Vec<DeliveredPublish> {
        self.faults.lock().delivered.clone()
    }

    async fn publish_with_faults(
        &self,
        ctx: &SecurityContext,
        event: &Event,
        sync: bool,
    ) -> Result<IngestOutcome, EventBrokerError> {
        {
            let mut f = self.faults.lock();
            if f.disconnected {
                return Err(TransientFault::Transport.error());
            }
            if let Some(fault) = f.publishes.pop_front() {
                return Err(fault.error());
            }
        }
        let mut result = if sync {
            self.inner.publish_sync(ctx, event).await
        } else {
            self.inner.publish(ctx, event).await
        };
        let mut f = self.faults.lock();
        // Simulate only the response variant after an actual successful admission.
        // The fixture's backend assertions independently prove eventual persistence.
        if matches!(result, Ok(IngestOutcome::Accepted)) && f.persisted > 0 {
            f.persisted -= 1;
            result = Ok(IngestOutcome::Persisted);
        }
        let outcome = match &result {
            Ok(IngestOutcome::Accepted) => DeliveredOutcome::Accepted,
            Ok(IngestOutcome::Persisted) => DeliveredOutcome::Persisted,
            Ok(IngestOutcome::Duplicate) => DeliveredOutcome::Duplicate,
            Err(EventBrokerError::SequenceViolation { .. }) => DeliveredOutcome::SequenceViolation,
            Err(EventBrokerError::UnknownProducer { .. }) => DeliveredOutcome::UnknownProducer,
            Err(error) => DeliveredOutcome::Refused(error.to_string()),
        };
        let ack_lost = result.is_ok() && f.lost_acks > 0;
        if ack_lost {
            f.lost_acks -= 1;
            if f.disconnect_after_ack {
                f.disconnect_after_ack = false;
                f.disconnected = true;
            }
        }
        f.delivered.push(DeliveredPublish {
            subject: event.subject.clone(),
            event_id: event.id,
            meta: event.meta.clone(),
            outcome,
            ack_lost,
        });
        if ack_lost {
            Err(TransientFault::Transport.error())
        } else {
            result
        }
    }
}

#[async_trait::async_trait]
impl EventBrokerApi for FaultInjectingBroker {
    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.publish_with_faults(ctx, event, false).await
    }
    async fn publish_sync(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.publish_with_faults(ctx, event, true).await
    }
    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        {
            let mut f = self.faults.lock();
            f.cursor_reads += 1;
            if f.disconnected {
                return Err(TransientFault::Transport.error());
            }
            if let Some(fault) = f.cursors.pop_front() {
                return Err(fault.error());
            }
        }
        self.inner.get_producer_cursors(ctx, producer_id).await
    }
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        self.inner.register_producer(ctx, mode, client_agent).await
    }
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.inner.publish_batch(ctx, events).await
    }
    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        self.inner
            .reset_producer_chain(ctx, producer_id, scope)
            .await
    }
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        self.inner.create_consumer_group(ctx, req).await
    }
    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        self.inner.get_consumer_group(ctx, id).await
    }
    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
        query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        self.inner.list_consumer_groups(ctx, query).await
    }
    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        self.inner.delete_consumer_group(ctx, id).await
    }
    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        self.inner.join(ctx, req).await
    }
    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        self.inner.get_subscription(ctx, id).await
    }
    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        self.inner.list_subscriptions(ctx).await
    }
    async fn leave(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        self.inner.leave(ctx, id).await
    }
    async fn stream(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        self.inner.stream(ctx, id).await
    }
    async fn seek(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
        topology_version: i64,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        self.inner.seek(ctx, id, topology_version, positions).await
    }
    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        self.inner.list_topics(ctx).await
    }
    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        self.inner
            .list_topic_segments(ctx, topic, partition, range)
            .await
    }
    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        self.inner.list_event_types(ctx).await
    }
    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        self.inner.get_event_type(ctx, id).await
    }
}
