//! The lifecycle table of the shared group session, run against both of its
//! consumers: each consumer's test module implements [`Harness`] over its own
//! fixture and runs every row below. The broker calls are observed through
//! [`Recording`], which wraps the fixture's scripted broker and keeps the calls
//! in the order they were made, so each row pins the order — a leave before
//! the delete, the delete before the next group — not only the counts.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::models::EventType;
use event_broker_sdk::{
    Ack, ConsumerGroup, ConsumerGroupId, ConsumerGroupQuery, CreateConsumerGroupRequest,
    EventBrokerApi, EventBrokerError, EventV1, FrameStream, IngestOutcome, JoinRequest, Page,
    PartitionRange, ProducerCursors, ProducerId, ProducerMode, ResetScope, SeekPosition,
    SeekResult, Subscription, SubscriptionAssignment, SubscriptionId, Topic, TopicSegment,
};
use toolkit_security::SecurityContext;

/// One broker call of the session, as [`Recording`] saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// A group was created.
    Create(ConsumerGroupId),
    /// A creation was refused.
    CreateRefused,
    /// A join on `group`: the subscription it answered, `None` when refused.
    Join {
        group: ConsumerGroupId,
        subscription: Option<SubscriptionId>,
    },
    /// A leave was entered; recorded before it answers, which it may never.
    Leave(SubscriptionId),
    /// A deletion was entered.
    Delete(ConsumerGroupId),
    /// A stream was opened.
    Stream,
}

/// A broker that hands every call to `inner` and records the session's calls
/// in order.
pub struct Recording {
    inner: Arc<dyn EventBrokerApi>,
    calls: Mutex<Vec<Call>>,
}

impl Recording {
    /// Wrap `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn EventBrokerApi>) -> Self {
        Self {
            inner,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }

    /// The calls so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn count(&self, matches: impl Fn(&Call) -> bool) -> usize {
        self.calls().iter().filter(|c| matches(c)).count()
    }

    fn streams(&self) -> usize {
        self.count(|c| matches!(c, Call::Stream))
    }

    fn created(&self) -> Vec<ConsumerGroupId> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Create(group) => Some(group),
                _ => None,
            })
            .collect()
    }

    fn subscriptions(&self) -> Vec<SubscriptionId> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Join {
                    subscription: Some(subscription),
                    ..
                } => Some(subscription),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl EventBrokerApi for Recording {
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        name: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        self.inner.register_producer(ctx, mode, name).await
    }
    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &EventV1,
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.inner.publish(ctx, event, ack).await
    }
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[EventV1],
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.inner.publish_batch(ctx, events, ack).await
    }
    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        self.inner.get_producer_cursors(ctx, producer).await
    }
    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        self.inner.reset_producer_chain(ctx, producer, scope).await
    }
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        let answer = self.inner.create_consumer_group(ctx, req).await;
        self.record(match &answer {
            Ok(group) => Call::Create(group.id),
            Err(_) => Call::CreateRefused,
        });
        answer
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
        self.record(Call::Delete(*id));
        self.inner.delete_consumer_group(ctx, id).await
    }
    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let group = req.group;
        let answer = self.inner.join(ctx, req).await;
        self.record(Call::Join {
            group,
            subscription: answer.as_ref().ok().map(|a| a.subscription_id),
        });
        answer
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
        self.record(Call::Leave(id));
        self.inner.leave(ctx, id).await
    }
    async fn stream(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        let answer = self.inner.stream(ctx, id).await;
        if answer.is_ok() {
            self.record(Call::Stream);
        }
        answer
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

/// One consumer under the table: its fixture, with the scripted broker's knobs
/// and the consumer's start and stop.
pub trait Harness {
    /// The fixture's scripted broker, unplugged.
    fn scripted(&self) -> Arc<dyn EventBrokerApi>;
    /// Plug `broker` into the consumer's hub.
    fn plug(&self, broker: Arc<dyn EventBrokerApi>);
    /// The scripted broker refuses the next `n` group creations.
    fn refuse_creates(&self, n: usize);
    /// The scripted broker refuses the next `n` joins.
    fn refuse_joins(&self, n: usize);
    /// The scripted broker's `leave` never answers from now on.
    fn hang_leaves(&self);
    /// Send a terminal control frame on the open stream.
    fn send_terminal(&self);
    /// End the open stream.
    fn end_stream(&self);
    /// Start the consumer: it holds a group as soon as the broker lets it.
    async fn start(&mut self);
    /// Stop the consumer and wait until it released what it held.
    async fn stop(&mut self);
    /// What the consumer logged.
    fn logs(&self) -> String;
}

/// Plug a [`Recording`] of the harness's scripted broker.
fn record(h: &impl Harness) -> Arc<Recording> {
    let recording = Arc::new(Recording::new(h.scripted()));
    h.plug(Arc::clone(&recording) as Arc<dyn EventBrokerApi>);
    recording
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// The one group and the one subscription of a session that joined once.
fn first(recording: &Recording) -> (ConsumerGroupId, SubscriptionId) {
    (recording.created()[0], recording.subscriptions()[0])
}

/// Stop releases what the session holds: the subscription is left, then the
/// group deleted, once each.
pub async fn stop_leaves_the_subscription_then_deletes_the_group(mut h: impl Harness) {
    let recording = record(&h);
    h.start().await;
    until("the stream to open", || recording.streams() == 1).await;
    let (group, subscription) = first(&recording);

    h.stop().await;

    assert_eq!(
        recording.calls(),
        vec![
            Call::Create(group),
            Call::Join {
                group,
                subscription: Some(subscription),
            },
            Call::Stream,
            Call::Leave(subscription),
            Call::Delete(group),
        ]
    );
}

/// A refused creation is retried until one group holds, and a refused join is
/// retried on that group: one group however many refusals, deleted on stop.
pub async fn refusals_are_retried_on_one_group_and_leak_none(mut h: impl Harness) {
    let recording = record(&h);
    h.refuse_creates(2);
    h.refuse_joins(3);
    h.start().await;
    until("the stream to open", || recording.streams() == 1).await;
    let (group, subscription) = first(&recording);
    h.stop().await;

    let refused = Call::Join {
        group,
        subscription: None,
    };
    assert_eq!(
        recording.calls(),
        vec![
            Call::CreateRefused,
            Call::CreateRefused,
            Call::Create(group),
            refused.clone(),
            refused.clone(),
            refused,
            Call::Join {
                group,
                subscription: Some(subscription),
            },
            Call::Stream,
            Call::Leave(subscription),
            Call::Delete(group),
        ]
    );
    let logs = h.logs();
    assert!(logs.contains("consumer group refused"), "{logs}");
    assert!(logs.contains("join refused"), "{logs}");
}

/// A terminal control frame ends the subscription: it is left and the session
/// joins again through the same group.
pub async fn a_terminal_frame_rejoins_through_the_same_group(mut h: impl Harness) {
    let recording = record(&h);
    h.start().await;
    until("the stream to open", || recording.streams() == 1).await;
    let (group, first_subscription) = first(&recording);

    h.send_terminal();
    until("the stream to open again", || recording.streams() == 2).await;
    let second_subscription = recording.subscriptions()[1];
    h.stop().await;

    assert_eq!(
        recording.calls(),
        vec![
            Call::Create(group),
            Call::Join {
                group,
                subscription: Some(first_subscription),
            },
            Call::Stream,
            Call::Leave(first_subscription),
            Call::Join {
                group,
                subscription: Some(second_subscription),
            },
            Call::Stream,
            Call::Leave(second_subscription),
            Call::Delete(group),
        ]
    );
}

/// A stream that ends is a lost broker: the session warns, leaves and deletes
/// its group, waits its backoff and creates a new group.
pub async fn a_lost_stream_releases_the_group_and_a_new_one_is_created(mut h: impl Harness) {
    let recording = record(&h);
    h.start().await;
    until("the stream to open", || recording.streams() == 1).await;
    let (lost_group, lost_subscription) = first(&recording);

    h.end_stream();
    until("a stream on a new group", || recording.streams() == 2).await;
    let group = recording.created()[1];
    let subscription = recording.subscriptions()[1];
    h.stop().await;

    assert_ne!(group, lost_group);
    assert_eq!(
        recording.calls(),
        vec![
            Call::Create(lost_group),
            Call::Join {
                group: lost_group,
                subscription: Some(lost_subscription),
            },
            Call::Stream,
            Call::Leave(lost_subscription),
            Call::Delete(lost_group),
            Call::Create(group),
            Call::Join {
                group,
                subscription: Some(subscription),
            },
            Call::Stream,
            Call::Leave(subscription),
            Call::Delete(group),
        ]
    );
    let logs = h.logs();
    assert!(logs.contains("event broker lost"), "{logs}");
}

/// A stop that cancels a rejoin's leave of the previous subscription still
/// leaves it on release, before the group is deleted; no second join is made.
pub async fn a_stop_during_a_rejoins_leave_still_leaves_before_the_delete(mut h: impl Harness) {
    let recording = record(&h);
    h.start().await;
    until("the stream to open", || recording.streams() == 1).await;
    let (group, subscription) = first(&recording);
    h.hang_leaves();

    h.send_terminal();
    until("the rejoin's leave to start", || {
        recording.count(|c| matches!(c, Call::Leave(_))) == 1
    })
    .await;
    h.stop().await;

    assert_eq!(
        recording.calls(),
        vec![
            Call::Create(group),
            Call::Join {
                group,
                subscription: Some(subscription),
            },
            Call::Stream,
            Call::Leave(subscription),
            Call::Leave(subscription),
            Call::Delete(group),
        ]
    );
}
