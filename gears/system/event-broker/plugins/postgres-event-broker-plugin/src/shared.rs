//! What every instance of one registry entry holds: its settings, the pool once
//! startup has made it, the heads it knows, and the topics it has resolved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::BackendError;
use event_broker_sdk::backend::support::{LocalHeads, topic_uuid};
use sqlx::PgPool;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::error;
use crate::settings::PostgresSettings;

/// What `evbk__log` says about one topic.
#[derive(Debug, Clone)]
pub struct KnownTopic {
    pub id: GtsInstanceId,
    pub partitions: u32,
}

pub struct Shared {
    pub settings: PostgresSettings,
    /// Set once the startup task has connected, checked and migrated; every
    /// operation before that is `Unavailable`.
    pool: OnceLock<PgPool>,
    pub heads: LocalHeads,
    topics: Mutex<HashMap<Uuid, KnownTopic>>,
    /// Wakes the maintenance worker for an immediate pass.
    pub maintenance: Notify,
}

impl Shared {
    pub fn new(settings: PostgresSettings) -> Arc<Self> {
        Arc::new(Self {
            settings,
            pool: OnceLock::new(),
            heads: LocalHeads::new(),
            topics: Mutex::default(),
            maintenance: Notify::new(),
        })
    }

    pub fn set_pool(&self, pool: PgPool) {
        if self.pool.set(pool).is_err() {
            tracing::warn!(
                "postgres event-broker backend: startup completed twice; keeping the first pool"
            );
        }
    }

    /// The pool, or `Unavailable` while startup has not completed.
    pub fn pool(&self) -> Result<&PgPool, BackendError> {
        self.pool.get().ok_or_else(|| {
            BackendError::unavailable("the postgres backend has not finished starting")
        })
    }

    pub fn remember(&self, topic: Uuid, known: KnownTopic) {
        self.topics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(topic, known);
    }

    fn cached(&self, topic: Uuid) -> Option<KnownTopic> {
        self.topics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&topic)
            .cloned()
    }

    /// What `evbk__log` holds for `topic`, cached once read; `NotProvisioned`
    /// when it holds nothing.
    pub async fn topic(
        &self,
        topic: &GtsInstanceId,
        operation: &str,
    ) -> Result<(Uuid, KnownTopic), BackendError> {
        let key = topic_uuid(topic);
        if let Some(known) = self.cached(key) {
            return Ok((key, known));
        }
        let known = self
            .lookup(key, operation)
            .await?
            .ok_or_else(|| BackendError::not_provisioned(operation))?;
        Ok((key, known))
    }

    /// Like [`Self::topic`], but `None` rather than `NotProvisioned` for a topic `evbk__log` does not hold.
    pub async fn lookup(
        &self,
        topic: Uuid,
        operation: &str,
    ) -> Result<Option<KnownTopic>, BackendError> {
        if let Some(known) = self.cached(topic) {
            return Ok(Some(known));
        }
        let row: Option<(String, i32)> =
            sqlx::query_as("SELECT topic_id, partitions FROM evbk__log WHERE topic_uuid = $1")
                .bind(topic)
                .fetch_optional(self.pool()?)
                .await
                .map_err(|e| error::map(operation, &e))?;
        let Some((id, partitions)) = row else {
            return Ok(None);
        };
        let known = KnownTopic {
            id: GtsInstanceId::try_new(&id).map_err(|_| {
                BackendError::Internal(format!("{operation}: evbk__log holds a malformed topic id"))
            })?,
            partitions: u32::try_from(partitions).map_err(|_| {
                BackendError::Internal(format!(
                    "{operation}: evbk__log holds a negative partition count"
                ))
            })?,
        };
        self.remember(topic, known.clone());
        Ok(Some(known))
    }
}
