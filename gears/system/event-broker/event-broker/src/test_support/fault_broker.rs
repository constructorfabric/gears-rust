//! A client-side fault injector around a real `EventBrokerApi`.
//!
//! A producer talks to the broker through `EventBrokerApi`; transport loss and rate limiting
//! surface there as `EventBrokerError::Transport` / `RateLimited`, which an in-process
//! `LocalBroker` never produces on its own. This wrapper arms those faults on the producer-side
//! calls - the cursor read a chained producer makes with an empty cursor cache, and publish -
//! including a lost acknowledgement (the real publish happens, its answer is lost) and a
//! disconnection that begins with one. Every unarmed call, and every other operation,
//! delegates unchanged to the wrapped broker. Each publish that reaches the wrapped broker is
//! recorded with its producer metadata and the broker's real answer, so a test can attribute
//! every outcome (stable identity across retries, duplicate or chain-violation answers).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use event_broker_sdk::api::{
    EventBrokerApi, FrameStream, IngestOutcome, JoinRequest, ProducerCursors, SeekPosition,
    SeekResult, SubscriptionAssignment,
};
use event_broker_sdk::error::EventBrokerError;
use event_broker_sdk::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use event_broker_sdk::models::{
    ConsumerGroup, ConsumerGroupQuery, CreateConsumerGroupRequest, Event, EventType, Page,
    PartitionRange, ProducerMeta, ResetScope, Subscription, Topic, TopicSegment,
};
use event_broker_sdk::producer::ProducerMode;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A transient fault the producer must retry, never dead-letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientFault {
    Transport,
    RateLimited,
}
impl TransientFault {
    fn error(self) -> EventBrokerError {
        match self {
            Self::Transport => EventBrokerError::Transport("injected transport fault".to_owned()),
            Self::RateLimited => EventBrokerError::RateLimited {
                retry_after_secs: 0,
                detail: "injected rate limit".to_owned(),
            },
        }
    }
}

#[derive(Default)]
struct Armed {
    cursor_reads: VecDeque<TransientFault>,
    publishes: VecDeque<TransientFault>,
    lost_acks: usize,
    persisted: usize,
    /// The next lost acknowledgement also disconnects the producer.
    disconnect_after_lost_ack: bool,
    /// Every publish and cursor read fails with a transport fault until reconnected.
    disconnected: bool,
}

/// The wrapped broker's real answer to one delivered publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveredOutcome {
    Accepted,
    Persisted,
    Duplicate,
    SequenceViolation,
    UnknownProducer,
    /// Any other refusal, by its display text.
    Refused(String),
}

/// One publish that reached the wrapped broker.
#[derive(Debug, Clone)]
pub struct DeliveredPublish {
    pub event_id: Uuid,
    pub subject: String,
    pub meta: Option<ProducerMeta>,
    /// The wrapped broker's answer, before any injected answer replaced it.
    pub outcome: DeliveredOutcome,
    /// The producer saw a transport fault instead of `outcome`.
    pub ack_lost: bool,
}

/// Wraps a real broker and fails the next armed producer calls.
pub struct FaultInjectingBroker {
    inner: Arc<dyn EventBrokerApi>,
    armed: Mutex<Armed>,
    cursor_reads: AtomicUsize,
    publishes: AtomicUsize,
    delivered: AtomicUsize,
    log: Mutex<Vec<DeliveredPublish>>,
}

impl FaultInjectingBroker {
    pub fn new(inner: Arc<dyn EventBrokerApi>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            armed: Mutex::new(Armed::default()),
            cursor_reads: AtomicUsize::new(0),
            publishes: AtomicUsize::new(0),
            delivered: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        })
    }
    fn armed(&self) -> MutexGuard<'_, Armed> {
        self.armed.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Fail the next `faults.len()` producer-cursor reads, in order.
    pub fn fail_cursor_reads(&self, faults: impl IntoIterator<Item = TransientFault>) {
        self.armed().cursor_reads.extend(faults);
    }
    /// Fail the next publishes before they reach the broker, in order.
    pub fn fail_publishes(&self, faults: impl IntoIterator<Item = TransientFault>) {
        self.armed().publishes.extend(faults);
    }
    /// Let the next `n` publishes reach the broker but report a transport fault to the caller.
    pub fn lose_publish_acks(&self, n: usize) {
        self.armed().lost_acks += n;
    }
    /// Report the next `n` accepted publishes as `Persisted`, the answer a durability-confirming
    /// broker gives; the in-process broker only ever answers `Accepted`. The publish is real.
    pub fn report_persisted(&self, n: usize) {
        self.armed().persisted += n;
    }
    /// Producer-cursor reads attempted, faulted or not.
    pub fn cursor_reads(&self) -> usize {
        self.cursor_reads.load(Ordering::SeqCst)
    }
    /// Publishes attempted, faulted or not.
    pub fn publishes(&self) -> usize {
        self.publishes.load(Ordering::SeqCst)
    }
    /// Publishes the wrapped broker actually received.
    pub fn delivered(&self) -> usize {
        self.delivered.load(Ordering::SeqCst)
    }
    /// Lose the next publish acknowledgement (the publish reaches the broker) and from then on
    /// fail every publish and cursor read with a transport fault until [`Self::reconnect`]:
    /// a producer that loses its connection right after the broker appended its event.
    pub fn lose_next_ack_then_disconnect(&self) {
        let mut armed = self.armed();
        armed.lost_acks += 1;
        armed.disconnect_after_lost_ack = true;
    }
    /// Whether the producer is currently disconnected.
    pub fn is_disconnected(&self) -> bool {
        self.armed().disconnected
    }
    /// End a disconnection.
    pub fn reconnect(&self) {
        self.armed().disconnected = false;
    }
    /// Every publish that reached the wrapped broker, in arrival order.
    pub fn delivered_publishes(&self) -> Vec<DeliveredPublish> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn delivered_outcome(outcome: &Result<IngestOutcome, EventBrokerError>) -> DeliveredOutcome {
    match outcome {
        Ok(IngestOutcome::Accepted) => DeliveredOutcome::Accepted,
        Ok(IngestOutcome::Persisted) => DeliveredOutcome::Persisted,
        Ok(IngestOutcome::Duplicate) => DeliveredOutcome::Duplicate,
        Err(EventBrokerError::SequenceViolation { .. }) => DeliveredOutcome::SequenceViolation,
        Err(EventBrokerError::UnknownProducer { .. }) => DeliveredOutcome::UnknownProducer,
        Err(other) => DeliveredOutcome::Refused(other.to_string()),
    }
}

#[async_trait]
impl EventBrokerApi for FaultInjectingBroker {
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        self.inner.register_producer(ctx, mode, client_agent).await
    }

    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.publishes.fetch_add(1, Ordering::SeqCst);
        let (fault, lose_ack) = {
            let mut armed = self.armed();
            if armed.disconnected {
                (Some(TransientFault::Transport), false)
            } else {
                match armed.publishes.pop_front() {
                    Some(fault) => (Some(fault), false),
                    None if armed.lost_acks > 0 => {
                        armed.lost_acks -= 1;
                        (None, true)
                    }
                    None => (None, false),
                }
            }
        };
        if let Some(fault) = fault {
            return Err(fault.error());
        }
        self.delivered.fetch_add(1, Ordering::SeqCst);
        let outcome = self.inner.publish(ctx, event).await;
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(DeliveredPublish {
                event_id: event.id,
                subject: event.subject.clone(),
                meta: event.meta.clone(),
                outcome: delivered_outcome(&outcome),
                ack_lost: lose_ack,
            });
        if lose_ack {
            let mut armed = self.armed();
            if armed.disconnect_after_lost_ack {
                armed.disconnect_after_lost_ack = false;
                armed.disconnected = true;
            }
            return Err(TransientFault::Transport.error());
        }
        match outcome {
            Ok(IngestOutcome::Accepted) => {
                let mut armed = self.armed();
                if armed.persisted > 0 {
                    armed.persisted -= 1;
                    return Ok(IngestOutcome::Persisted);
                }
                Ok(IngestOutcome::Accepted)
            }
            other => other,
        }
    }

    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.inner.publish_batch(ctx, events).await
    }

    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        self.cursor_reads.fetch_add(1, Ordering::SeqCst);
        {
            let mut armed = self.armed();
            if armed.disconnected {
                return Err(TransientFault::Transport.error());
            }
            if let Some(fault) = armed.cursor_reads.pop_front() {
                return Err(fault.error());
            }
        }
        self.inner.get_producer_cursors(ctx, producer_id).await
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
