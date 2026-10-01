//! The loader's `EventSource`, over the storage backend.
//!
//! The only place the delivery path reaches storage, and it is the *loader* that
//! reaches it, never a session (D2). A session reads its cache; this fills the
//! cache behind it.

use std::sync::Arc;

use event_broker_sdk::backend::{BackendError, Partition, TopicPartition, Wait};

use crate::domain::backend::{BackendResolver, from_sdk_event};
use crate::domain::model::{Event, Sequence};
use crate::domain::specification::SpecificationManager;
use crate::domain::streaming::source::PartitionKey;

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
        // new events belongs to the head notification, never to a permit
        // holder (feature 0007 §3.3).
        let at = TopicPartition::new(
            topic.id.clone(),
            Partition::new(key.partition.cast_unsigned()),
        );
        let events = match backend.read(&at, after, max_events, Wait::No).await {
            Ok(events) => events,
            // Registered but not provisioned on its backend yet: a partition
            // nothing was written to.
            Err(BackendError::NotProvisioned { .. }) => Vec::new(),
            Err(err) => return Err(SourceError::Failed(err.to_string())),
        };

        events
            .into_iter()
            .map(|event| from_sdk_event(&topic.id, event))
            .collect::<Result<Vec<Event>, _>>()
            .map_err(|err| SourceError::Failed(err.to_string()))
    }
}
