use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::api::EventBrokerBackend;
use crate::error::StorageBackendError;
use crate::models::Event;
use crate::models::{PartitionLeader, PartitionRange, TopicSegment};

use super::core::MockBroker;
use super::ingest::ingest_one;

/// The mock implements `EventBrokerStorageBackend` over the same `Core.log` as
/// the transport seam (O2 - flattening playground). `persist` goes through the
/// same `ingest_one` pipeline so storage and transport stay in sync.
///
/// `truncate` and `segments` are stubs marked with `TODO(M5b-flattening)` -
/// they will become the primary surface when the backend-contract is reshaped to
/// the PRD's `persist/query/truncate/segments` form.
#[async_trait]
impl EventBrokerBackend for MockBroker {
    async fn persist(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
        _partition: u32,
        events: &[Event],
    ) -> Result<(), StorageBackendError> {
        {
            let faults = self.faults.lock().await;
            if let Some(reason) = &faults.reject_persist {
                return Err(StorageBackendError::PersistFailed {
                    reason: reason.clone(),
                    detail: "fault injected via MockBrokerHandle::reject_persist".to_owned(),
                    instance: String::new(),
                });
            }
        }
        let mut core = self.core.lock().await;
        for event in events {
            let mut e = event.clone();
            if e.topic.is_empty() {
                e.topic = topic.to_owned();
            }
            ingest_one(&mut core, &e).map_err(|err| StorageBackendError::PersistFailed {
                reason: err.to_string(),
                detail: String::new(),
                instance: String::new(),
            })?;
        }
        self.notify.notify_waiters();
        Ok(())
    }

    async fn read(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        start_offset: i64,
        max_count: usize,
    ) -> Result<Vec<Event>, StorageBackendError> {
        let core = self.core.lock().await;
        Ok(core
            .topics
            .get(topic)
            .map(|t| {
                t.read(partition, start_offset, max_count)
                    .into_iter()
                    .map(|se| se.event.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn query(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        _range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, StorageBackendError> {
        let core = self.core.lock().await;
        let t = match core.topics.get(topic) {
            Some(t) => t,
            None => return Ok(vec![]),
        };
        let log = t.log.get(&partition);
        Ok(if let Some(events) = log {
            if events.is_empty() {
                vec![]
            } else {
                let start = events.first().and_then(|e| e.event.sequence).unwrap_or(0);
                let end = events.last().and_then(|e| e.event.sequence).unwrap_or(0);
                let ts = events
                    .first()
                    .and_then(|e| e.event.sequence_time)
                    .unwrap_or_else(chrono::Utc::now);
                let te = events
                    .last()
                    .and_then(|e| e.event.sequence_time)
                    .unwrap_or_else(chrono::Utc::now);
                vec![TopicSegment {
                    topic: topic.to_owned(),
                    partition,
                    start_sequence: start,
                    end_sequence: end,
                    start_time: ts,
                    end_time: te,
                    segments: vec![],
                }]
            }
        } else {
            vec![]
        })
    }

    async fn list_partition_leaders(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
    ) -> Result<Vec<PartitionLeader>, StorageBackendError> {
        let core = self.core.lock().await;
        let partitions = core.topics.get(topic).map(|t| t.partitions).unwrap_or(0);
        Ok((0..partitions)
            .map(|p| PartitionLeader {
                partition: p,
                endpoint: "mock://in-process".to_owned(),
            })
            .collect())
    }
}
