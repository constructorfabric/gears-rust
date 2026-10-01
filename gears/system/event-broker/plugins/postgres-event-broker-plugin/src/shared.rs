//! What every instance of one registry entry holds: its settings, the pool once
//! startup has made it, the heads it knows, and the topics it has resolved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::support::{LocalHeads, topic_uuid};
use event_broker_sdk::backend::{BackendError, Role};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error;
use crate::maintenance::hub::Hub;
use crate::settings::PostgresSettings;

/// What `evbk__log` and `evbk__partition` say about one topic.
#[derive(Debug, Clone)]
pub struct KnownTopic {
    /// The topic the log belongs to.
    pub id: GtsInstanceId,
    /// The topic's partition count.
    pub partitions: u32,
    /// The partitioned table of each partition, by partition.
    pub tables: Vec<String>,
}

/// The state every instance of one registry entry shares.
pub struct Shared {
    /// The entry's settings.
    pub settings: PostgresSettings,
    /// What this process is to the backend.
    pub role: Role,
    /// The main pool: set once the startup task has connected, checked and
    /// migrated; every operation before that is `Unavailable`.
    pool: OnceLock<PgPool>,
    /// The background pool, set where the maintenance runs.
    background: OnceLock<PgPool>,
    /// Heads known to this process, feeding its watches and waiting reads.
    pub heads: LocalHeads,
    /// The background maintenance's registry and queues.
    pub maintenance: Hub,
    topics: Mutex<HashMap<Uuid, KnownTopic>>,
}

impl Shared {
    /// State for `settings` in a process that is `role`, with no pool until
    /// startup completes.
    pub fn new(settings: PostgresSettings, role: Role) -> Arc<Self> {
        let maintenance = Hub::new(&settings.maintenance);
        Arc::new(Self {
            settings,
            role,
            pool: OnceLock::new(),
            background: OnceLock::new(),
            heads: LocalHeads::new(),
            maintenance,
            topics: Mutex::default(),
        })
    }

    /// Installs the pool startup has made; a second call keeps the first.
    pub fn set_pool(&self, pool: PgPool) {
        if self.pool.set(pool).is_err() {
            tracing::warn!(
                "postgres event-broker backend: startup completed twice; keeping the first pool"
            );
        }
    }

    /// Whether this process runs the maintenance: only where events are
    /// appended, so a delivery process holds no background pool and takes
    /// no maintenance lock. A role this plugin does not know yet runs none.
    pub fn runs_maintenance(&self) -> bool {
        matches!(self.role, Role::Ingest | Role::Standalone)
    }

    /// Installs the background pool startup has made; a second call keeps
    /// the first.
    pub fn set_background(&self, pool: PgPool) {
        if self.background.set(pool).is_err() {
            tracing::warn!(
                "postgres event-broker backend: startup completed twice; keeping the first background pool"
            );
        }
    }

    /// The background pool, once startup has made one.
    pub fn background(&self) -> Option<&PgPool> {
        self.background.get()
    }

    /// The pool, or `Unavailable` while startup has not completed.
    pub fn pool(&self) -> Result<&PgPool, BackendError> {
        self.pool.get().ok_or_else(|| {
            BackendError::unavailable("the postgres backend has not finished starting")
        })
    }

    /// Caches what `evbk__log` holds for `topic`.
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

    /// What the catalog holds for `topic`, cached once read; `NotProvisioned`
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
        let row: Option<(String, i32, Vec<String>)> = sqlx::query_as(
            "SELECT l.topic_id, l.partitions, ARRAY(SELECT p.table_name FROM evbk__partition p \
             WHERE p.topic_uuid = l.topic_uuid ORDER BY p.partition) \
             FROM evbk__log l WHERE l.topic_uuid = $1",
        )
        .bind(topic)
        .fetch_optional(self.pool()?)
        .await
        .map_err(|e| error::map(operation, &e))?;
        let Some((id, partitions, tables)) = row else {
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
            tables,
        };
        if u32::try_from(known.tables.len()).ok() != Some(known.partitions) {
            return Err(BackendError::Internal(format!(
                "{operation}: evbk__partition holds {} rows for a topic of {} partitions",
                known.tables.len(),
                known.partitions
            )));
        }
        self.remember(topic, known.clone());
        Ok(Some(known))
    }
}
