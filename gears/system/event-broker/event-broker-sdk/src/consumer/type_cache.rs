use std::collections::HashMap;
use std::sync::Arc;

use gts::GtsInstanceId;
use toolkit_security::SecurityContext;

use crate::api::EventBrokerApi;
use crate::error::EventBrokerError;

/// Which topic each event type publishes to, so a delivered event can be
/// attributed to a `(topic, partition)` without the frame naming the topic.
///
/// The counterpart to [`ProducerSchemaCache`](crate::producer) on the consuming
/// side, and deliberately smaller: a consumer validates nothing, so it holds the
/// topic binding and not the compiled payload contract.
///
/// An event type resolves to exactly one topic - its `topic` trait - which is why
/// this mapping is sound and why `partition` alone is not: partition 0 exists on
/// every topic a subscription may be assigned.
#[derive(Default)]
pub(crate) struct ConsumerTypeCache {
    topics: tokio::sync::RwLock<HashMap<String, GtsInstanceId>>,
}

impl ConsumerTypeCache {
    /// Resolves every event type bound to one of `declared_topics`.
    ///
    /// Called once when the consumer starts, so that resolution during delivery
    /// is a local read. A declared topic with no event types registered against
    /// it is not an error: a topic with no types yet is a legitimate state, and
    /// no event can arrive on it.
    ///
    /// # Errors
    /// Propagates the broker's error if the event types cannot be listed.
    pub(crate) async fn prime(
        &self,
        broker: &Arc<dyn EventBrokerApi>,
        ctx: &SecurityContext,
        declared_topics: &[String],
    ) -> Result<(), EventBrokerError> {
        let event_types = broker.list_event_types(ctx).await?;
        let mut cached = self.topics.write().await;
        for event_type in event_types {
            if declared_topics
                .iter()
                .any(|topic| topic == event_type.topic.as_ref())
            {
                cached.insert(event_type.id.as_ref().to_owned(), event_type.topic);
            }
        }
        Ok(())
    }

    /// The topic an event of `type_id` belongs to.
    ///
    /// Resolves against the broker when the type is not held, which covers a type
    /// registered after the consumer started, and records the result so a second
    /// event of that type costs nothing.
    ///
    /// # Errors
    /// Propagates [`EventBrokerError::EventTypeUnknown`] when the broker does not
    /// know the type. That fails the delivery it was resolved for, naming the
    /// type; it does not end the subscription.
    pub(crate) async fn topic_of(
        &self,
        broker: &Arc<dyn EventBrokerApi>,
        ctx: &SecurityContext,
        type_id: &str,
    ) -> Result<GtsInstanceId, EventBrokerError> {
        if let Some(topic) = self.topics.read().await.get(type_id) {
            return Ok(topic.clone());
        }

        let event_type = broker.get_event_type(ctx, type_id).await?;
        let topic = event_type.topic;
        self.topics
            .write()
            .await
            .insert(type_id.to_owned(), topic.clone());
        Ok(topic)
    }
}
