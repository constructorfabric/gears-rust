//! What one backend instance holds: its settings, the pool and hub once
//! startup has opened the file, and the topics it has resolved.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{BackendError, Partition, Role, TopicPartition};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::hub::Hub;
use crate::settings::SqliteSettings;

/// What `evbk__log` says about one topic. Fixed once provisioned, so it is
/// cached for the life of the instance.
#[derive(Debug, Clone, Copy)]
pub struct KnownTopic {
    /// `topic_uuid(id)`, the key of every row of the topic.
    pub uuid: Uuid,
    /// The topic's partition count.
    pub partitions: u32,
}

/// The open file: this instance's pool and the hub it shares with every
/// other instance over the file in this process.
pub struct Opened {
    /// This instance's connection pool over the file.
    pub pool: SqlitePool,
    /// The hub every instance over the file in this process shares.
    pub hub: Arc<Hub>,
}

/// The state one plugin instance shares across its operations.
pub struct Shared {
    /// The entry's settings.
    pub settings: SqliteSettings,
    /// The node role this instance runs under.
    pub role: Role,
    opened: OnceLock<Opened>,
    topics: RwLock<HashMap<GtsInstanceId, KnownTopic>>,
}

impl Shared {
    /// An instance whose file is not opened yet.
    pub fn new(settings: SqliteSettings, role: Role) -> Arc<Self> {
        Arc::new(Self {
            settings,
            role,
            opened: OnceLock::new(),
            topics: RwLock::default(),
        })
    }

    /// Whether this instance runs the retention sweep: only where events are
    /// appended. A role this plugin does not know yet runs none.
    pub fn runs_sweep(&self) -> bool {
        matches!(self.role, Role::Ingest | Role::Standalone)
    }

    /// Installs what startup opened; a second call keeps the first.
    pub fn set_opened(&self, opened: Opened) {
        if self.opened.set(opened).is_err() {
            tracing::warn!("sqlite event-broker backend: opened twice; keeping the first pool");
        }
    }

    /// The open file, or `Unavailable` while startup has not opened it.
    pub fn opened(&self) -> Result<&Opened, BackendError> {
        self.opened.get().ok_or_else(|| {
            BackendError::unavailable("the sqlite backend has not opened its file yet")
        })
    }

    /// Caches a topic this instance has provisioned or looked up.
    pub fn remember(&self, topic: &GtsInstanceId, known: KnownTopic) {
        self.topics
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(topic.clone(), known);
    }

    /// What `evbk__log` holds for `topic`, cached once read;
    /// `NotProvisioned` when it holds nothing.
    pub async fn topic(
        &self,
        topic: &GtsInstanceId,
        operation: Operation,
    ) -> Result<KnownTopic, BackendError> {
        let cached = self
            .topics
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(topic)
            .copied();
        if let Some(known) = cached {
            return Ok(known);
        }
        let uuid = topic_uuid(topic);
        let partitions: Option<i64> =
            sqlx::query_scalar("SELECT partitions FROM evbk__log WHERE topic_uuid = ?1")
                .bind(uuid)
                .fetch_optional(&self.opened()?.pool)
                .await
                .map_err(|e| error::map(operation, &e))?;
        let partitions =
            partitions.ok_or_else(|| BackendError::not_provisioned(operation.name()))?;
        let known = KnownTopic {
            uuid,
            partitions: u32::try_from(partitions).map_err(|_| {
                BackendError::Internal(format!(
                    "{operation}: evbk__log holds a partition count out of range"
                ))
            })?,
        };
        self.remember(topic, known);
        Ok(known)
    }

    /// The topic's uuid and the partition's column value, after checking
    /// both exist.
    pub async fn partition(
        &self,
        at: &TopicPartition,
        operation: Operation,
    ) -> Result<(Uuid, i64), BackendError> {
        let known = self.topic(&at.topic, operation).await?;
        if at.partition.value() >= known.partitions {
            return Err(BackendError::PartitionNotFound {
                detail: format!("{operation}: the topic has no such partition on this backend"),
            });
        }
        Ok((known.uuid, column(at.partition)))
    }
}

/// A partition as its column stores it.
pub fn column(partition: Partition) -> i64 {
    i64::from(partition.value())
}
