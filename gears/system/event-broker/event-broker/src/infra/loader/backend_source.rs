//! The loader's `EventSource`, over the storage backend.
//!
//! The only place the delivery path reaches storage, and it is the *loader* that
//! reaches it, never a session (D2). A session reads its cache; this fills the
//! cache behind it.

use std::sync::Arc;

use event_broker_sdk::backend::support::poll_heads;
use event_broker_sdk::backend::{BackendError, Partition, TopicPartition, Wait, Watch};
use tokio_stream::StreamExt;
use toolkit_gts::GtsInstanceId;

use crate::domain::backend::{BackendResolver, from_sdk_event};
use crate::domain::model::{Event, Sequence};
use crate::domain::specification::SpecificationManager;
use crate::domain::streaming::source::PartitionKey;

use super::heads::{HeadSource, HeadStream, PartitionHead};
use super::source::{EventSource, SourceError};

/// Reads one partition forward through whichever backend serves its topic.
pub struct BackendEventSource {
    specs: Arc<dyn SpecificationManager>,
    backends: Arc<dyn BackendResolver>,
}

impl BackendEventSource {
    /// Two arguments of mutually distinguishable types, so neither can be
    /// passed in the other's place.
    #[must_use]
    pub fn new(specs: Arc<dyn SpecificationManager>, backends: Arc<dyn BackendResolver>) -> Self {
        Self { specs, backends }
    }
}

impl EventSource for BackendEventSource {
    async fn read(
        &self,
        key: &PartitionKey,
        after: Sequence,
        max_events: usize,
    ) -> Result<Vec<Event>, SourceError> {
        // A topic that has gone from the registry is not a read failure and must
        // not be retried as one: there is nothing to fetch and the demand should
        // not survive.
        let topic = self
            .specs
            .get_topic(&key.topic)
            .await
            .ok_or_else(|| SourceError::Failed(format!("unknown topic {}", key.topic)))?;

        let backend = self.backends.resolve(&topic);

        // `Wait::No`: the loader fetches under a pool permit, and a wait for
        // new events belongs to the head watch, never to a permit holder
        // (feature 0007 §4.3).
        let at = TopicPartition::new(
            topic.id.clone(),
            Partition::new(key.partition.cast_unsigned()),
        );
        let events = match backend.read(&at, after, max_events, Wait::No).await {
            Ok(events) => events,
            // Registered but not provisioned on its backend yet: a partition
            // nothing was written to.
            Err(BackendError::NotProvisioned { .. }) => Vec::new(),
            Err(err) => return Err(source_error(&err)),
        };

        events
            .into_iter()
            .map(|event| from_sdk_event(&topic.id, event))
            .collect::<Result<Vec<Event>, _>>()
            .map_err(|err| SourceError::Failed(err.to_string()))
    }
}

impl HeadSource for BackendEventSource {
    async fn watch(
        &self,
        topic: &GtsInstanceId,
        partitions: &[i32],
    ) -> Result<HeadStream, SourceError> {
        let topic = self
            .specs
            .get_topic(topic)
            .await
            .ok_or_else(|| SourceError::Failed(format!("unknown topic {topic}")))?;
        let backend = self.backends.resolve(&topic);
        let watched: Vec<TopicPartition> = partitions
            .iter()
            .map(|p| TopicPartition::new(topic.id.clone(), Partition::new(p.cast_unsigned())))
            .collect();

        let heads = match backend.watch() {
            Watch::Native(medium) => medium.watch(&watched),
            Watch::Poll(interval) => poll_heads(Arc::clone(&backend), &watched, interval),
        };
        Ok(Box::pin(heads.map(|item| {
            item.map(|head| PartitionHead {
                key: PartitionKey::new(head.at.topic, head.at.partition.value().cast_signed()),
                head: head.sequence,
            })
            .map_err(|err| source_error(&err))
        })))
    }
}

/// `Unavailable` keeps its meaning - the same call may succeed later; any
/// other backend error is a failed read.
fn source_error(err: &BackendError) -> SourceError {
    match err {
        BackendError::Unavailable { .. } => SourceError::Unavailable,
        other => SourceError::Failed(other.to_string()),
    }
}
