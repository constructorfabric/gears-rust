//! Tests for [`ConsumerTypeCache`], the consumer's map from an event type to the
//! topic it publishes to.
//!
//! `ConsumerTypeCache` is `pub(crate)`, so these are in-crate unit tests; the
//! real broker lives in the gear crate and cannot be reached here (that would be
//! a dependency cycle). The cache only ever calls `list_event_types` /
//! `get_event_type`, so a `CannedBroker` that answers just those two - every
//! other `EventBrokerApi` method is `unimplemented!()` - is enough to drive it.
//! A second lookup is proved to make no broker call by passing a *different*
//! `CannedBroker` that does not know the type: if the lookup still succeeds, it
//! was served from what the cache already held.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use ::gts::{GtsInstanceId, GtsTypeId};

use super::type_cache::ConsumerTypeCache;
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

const TOPIC: &str = "gts.cf.core.events.topic.v1~example.cache.broker.orders.v1";
const OTHER_TOPIC: &str = "gts.cf.core.events.topic.v1~example.cache.broker.audit.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~example.cache.orders.created.v1~";
const OTHER_TYPE: &str = "gts.cf.core.events.event.v1~example.cache.audit.logged.v1~";

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(Uuid::from_u128(1))
        .subject_id(Uuid::from_u128(1))
        .build()
        .expect("test security context")
}

fn topic_id(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("a valid topic instance id")
}

/// A broker that answers only type lookups from a fixed catalog - the sole
/// surface `ConsumerTypeCache` touches. Not a broker: every other operation
/// panics, so a test that leans on one fails loudly rather than passing against
/// a fake.
struct CannedBroker {
    event_types: Vec<EventType>,
}

impl CannedBroker {
    fn new(bindings: &[(&str, &str)]) -> Self {
        let event_types = bindings
            .iter()
            .map(|(topic, event_type)| EventType {
                id: GtsTypeId::try_new(event_type).expect("a valid event type id"),
                topic: topic_id(topic),
                description: None,
                allowed_subject_types: Vec::new(),
                partition_key: "/tenant_id".to_owned(),
                data_schema: json!({ "type": "object" }),
            })
            .collect();
        Self { event_types }
    }
}

fn broker_with(bindings: &[(&str, &str)]) -> Arc<dyn EventBrokerApi> {
    Arc::new(CannedBroker::new(bindings))
}

#[async_trait]
impl EventBrokerApi for CannedBroker {
    async fn list_event_types(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        Ok(self.event_types.clone())
    }

    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        self.event_types
            .iter()
            .find(|event_type| event_type.id.as_ref() == id)
            .cloned()
            .ok_or_else(|| EventBrokerError::EventTypeUnknown {
                type_id: id.to_owned(),
                detail: "no such event type".to_owned(),
            })
    }

    async fn register_producer(
        &self,
        _ctx: &SecurityContext,
        _mode: ProducerMode,
        _client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn publish(
        &self,
        _ctx: &SecurityContext,
        _event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn publish_batch(
        &self,
        _ctx: &SecurityContext,
        _events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn get_producer_cursors(
        &self,
        _ctx: &SecurityContext,
        _producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn reset_producer_chain(
        &self,
        _ctx: &SecurityContext,
        _producer_id: ProducerId,
        _scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn create_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn get_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn list_consumer_groups(
        &self,
        _ctx: &SecurityContext,
        _query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn delete_consumer_group(
        &self,
        _ctx: &SecurityContext,
        _id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn get_subscription(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn list_subscriptions(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn leave(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn seek(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
        _topology_version: i64,
        _positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn list_topics(&self, _ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
    async fn list_topic_segments(
        &self,
        _ctx: &SecurityContext,
        _topic: &str,
        _partition: u32,
        _range: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        unimplemented!("CannedBroker only serves type lookups")
    }
}

#[tokio::test]
async fn priming_resolves_the_types_of_declared_topics() {
    let broker = broker_with(&[(TOPIC, EVENT_TYPE), (OTHER_TOPIC, OTHER_TYPE)]);
    let cache = ConsumerTypeCache::default();

    cache
        .prime(&broker, &ctx(), &[topic_id(TOPIC)])
        .await
        .expect("priming succeeds");

    // Served from what priming held: a broker that knows nothing would fail.
    let empty = broker_with(&[]);
    assert_eq!(
        cache
            .topic_of(&empty, &ctx(), EVENT_TYPE)
            .await
            .expect("the declared topic's type was primed")
            .as_ref(),
        TOPIC
    );
}

#[tokio::test]
async fn priming_ignores_types_of_topics_the_consumer_did_not_declare() {
    let broker = broker_with(&[(TOPIC, EVENT_TYPE), (OTHER_TOPIC, OTHER_TYPE)]);
    let cache = ConsumerTypeCache::default();

    cache
        .prime(&broker, &ctx(), &[topic_id(TOPIC)])
        .await
        .expect("priming succeeds");

    let empty = broker_with(&[]);
    assert!(
        cache.topic_of(&empty, &ctx(), OTHER_TYPE).await.is_err(),
        "a type on an undeclared topic is not primed, so it needs the broker"
    );
}

#[tokio::test]
async fn a_consumer_declaring_a_topic_with_no_types_still_primes() {
    let broker = broker_with(&[]);

    ConsumerTypeCache::default()
        .prime(&broker, &ctx(), &[topic_id(TOPIC)])
        .await
        .expect("a topic with no registered event types is a legitimate state");
}

#[tokio::test]
async fn a_type_absent_at_priming_is_resolved_on_first_sight() {
    let broker = broker_with(&[]);
    let cache = ConsumerTypeCache::default();
    cache
        .prime(&broker, &ctx(), &[topic_id(TOPIC)])
        .await
        .expect("priming an empty broker succeeds");

    // Registered after the consumer would have started.
    let later = broker_with(&[(TOPIC, EVENT_TYPE)]);
    assert_eq!(
        cache
            .topic_of(&later, &ctx(), EVENT_TYPE)
            .await
            .expect("a type registered later resolves on first sight")
            .as_ref(),
        TOPIC
    );
}

#[tokio::test]
async fn a_resolved_type_is_not_resolved_twice() {
    let broker = broker_with(&[(TOPIC, EVENT_TYPE)]);
    let cache = ConsumerTypeCache::default();

    cache
        .topic_of(&broker, &ctx(), EVENT_TYPE)
        .await
        .expect("first lookup resolves through the broker");

    let empty = broker_with(&[]);
    assert_eq!(
        cache
            .topic_of(&empty, &ctx(), EVENT_TYPE)
            .await
            .expect("the second lookup is served locally")
            .as_ref(),
        TOPIC
    );
}

#[tokio::test]
async fn an_unresolvable_type_is_reported_with_its_identifier() {
    let broker = broker_with(&[]);

    let err = ConsumerTypeCache::default()
        .topic_of(&broker, &ctx(), EVENT_TYPE)
        .await
        .expect_err("a type no broker knows cannot resolve to a topic");

    assert!(
        matches!(err, EventBrokerError::EventTypeUnknown { ref type_id, .. } if type_id == EVENT_TYPE),
        "the failure must name the type it could not resolve: {err:?}"
    );
}
