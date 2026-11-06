use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::api::{
    EventBrokerApi, FrameStream, IngestOutcome, JoinRequest, ProducerCursors, ProducerMode,
    SeekPosition, SeekResult, SubscriptionAssignment,
};
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::models::{
    ConsumerGroup, ConsumerGroupQuery, CreateConsumerGroupRequest, Event, EventType, Page,
    PartitionRange, ResetScope, Subscription, Topic, TopicSegment,
};

/// The `EventBrokerApi` surface for unit-test doubles. A double implements
/// only the calls its test drives; any other call panics with the double's
/// name, so a test that strays off its stubbed path fails loudly instead of
/// passing against a fake answer.
#[async_trait]
pub(crate) trait BrokerStub: Send + Sync {
    async fn register_producer(
        &self,
        _ctx: &SecurityContext,
        _mode: ProducerMode,
        _client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        unimplemented!(
            "{} does not stub register_producer",
            std::any::type_name::<Self>()
        )
    }

    async fn publish(
        &self,
        _ctx: &SecurityContext,
        _event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        unimplemented!("{} does not stub publish", std::any::type_name::<Self>())
    }

    async fn publish_batch(
        &self,
        _ctx: &SecurityContext,
        _events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        unimplemented!(
            "{} does not stub publish_batch",
            std::any::type_name::<Self>()
        )
    }

    async fn get_producer_cursors(
        &self,
        _ctx: &SecurityContext,
        _producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        unimplemented!(
            "{} does not stub get_producer_cursors",
            std::any::type_name::<Self>()
        )
    }

    async fn reset_producer_chain(
        &self,
        _ctx: &SecurityContext,
        _producer_id: ProducerId,
        _scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        unimplemented!(
            "{} does not stub reset_producer_chain",
            std::any::type_name::<Self>()
        )
    }

    async fn create_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        unimplemented!(
            "{} does not stub create_consumer_group",
            std::any::type_name::<Self>()
        )
    }

    async fn get_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        unimplemented!(
            "{} does not stub get_consumer_group",
            std::any::type_name::<Self>()
        )
    }

    async fn list_consumer_groups(
        &self,
        _ctx: &SecurityContext,
        _query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        unimplemented!(
            "{} does not stub list_consumer_groups",
            std::any::type_name::<Self>()
        )
    }

    async fn delete_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        unimplemented!(
            "{} does not stub delete_consumer_group",
            std::any::type_name::<Self>()
        )
    }

    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        unimplemented!("{} does not stub join", std::any::type_name::<Self>())
    }

    async fn get_subscription(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        unimplemented!(
            "{} does not stub get_subscription",
            std::any::type_name::<Self>()
        )
    }

    async fn list_subscriptions(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        unimplemented!(
            "{} does not stub list_subscriptions",
            std::any::type_name::<Self>()
        )
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        unimplemented!("{} does not stub leave", std::any::type_name::<Self>())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        unimplemented!("{} does not stub stream", std::any::type_name::<Self>())
    }

    async fn seek(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
        _topology_version: i64,
        _positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        unimplemented!("{} does not stub seek", std::any::type_name::<Self>())
    }

    async fn list_topics(&self, _ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        unimplemented!(
            "{} does not stub list_topics",
            std::any::type_name::<Self>()
        )
    }

    async fn list_topic_segments(
        &self,
        _ctx: &SecurityContext,
        _topic: &str,
        _partition: u32,
        _range: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        unimplemented!(
            "{} does not stub list_topic_segments",
            std::any::type_name::<Self>()
        )
    }

    async fn list_event_types(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        unimplemented!(
            "{} does not stub list_event_types",
            std::any::type_name::<Self>()
        )
    }

    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        _id: &str,
    ) -> Result<EventType, EventBrokerError> {
        unimplemented!(
            "{} does not stub get_event_type",
            std::any::type_name::<Self>()
        )
    }
}

#[async_trait]
impl<T: BrokerStub> EventBrokerApi for T {
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        BrokerStub::register_producer(self, ctx, mode, client_agent).await
    }

    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        BrokerStub::publish(self, ctx, event).await
    }

    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        BrokerStub::publish_batch(self, ctx, events).await
    }

    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        BrokerStub::get_producer_cursors(self, ctx, producer_id).await
    }

    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        BrokerStub::reset_producer_chain(self, ctx, producer_id, scope).await
    }

    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        BrokerStub::create_consumer_group(self, ctx, req).await
    }

    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        BrokerStub::get_consumer_group(self, ctx, id).await
    }

    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
        query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        BrokerStub::list_consumer_groups(self, ctx, query).await
    }

    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        BrokerStub::delete_consumer_group(self, ctx, id).await
    }

    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        BrokerStub::join(self, ctx, req).await
    }

    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        BrokerStub::get_subscription(self, ctx, id).await
    }

    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        BrokerStub::list_subscriptions(self, ctx).await
    }

    async fn leave(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        BrokerStub::leave(self, ctx, id).await
    }

    async fn stream(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        BrokerStub::stream(self, ctx, id).await
    }

    async fn seek(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
        topology_version: i64,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        BrokerStub::seek(self, ctx, id, topology_version, positions).await
    }

    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        BrokerStub::list_topics(self, ctx).await
    }

    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        BrokerStub::list_topic_segments(self, ctx, topic, partition, range).await
    }

    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        BrokerStub::list_event_types(self, ctx).await
    }

    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        BrokerStub::get_event_type(self, ctx, id).await
    }
}
