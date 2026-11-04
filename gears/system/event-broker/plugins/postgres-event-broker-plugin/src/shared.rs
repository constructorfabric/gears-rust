//! What every instance of one registry entry holds: its settings, the pool once
//! startup has made it, the heads it knows, and the topics it has resolved.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::support::{LocalHeads, topic_uuid};
use event_broker_sdk::backend::{BackendError, Role};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::maintenance::hub::Hub;
use crate::notify::HeadsToNotify;
use crate::settings::PostgresSettings;

/// What `evbk__log` and `evbk__partition` say about one topic. Fixed once
/// provisioned, so it is cached for the life of the process and shared, never
/// copied.
#[derive(Debug)]
pub struct KnownTopic {
    /// The topic the log belongs to.
    pub id: GtsInstanceId,
    /// `topic_uuid(id)`, the key of every plugin row of the topic.
    pub uuid: Uuid,
    /// The topic's partition count.
    pub partitions: u32,
    /// The partitioned table of each partition, by partition.
    pub tables: Box<[Arc<str>]>,
}

/// A topic's `evbk__log` row with its tables, by partition.
#[derive(sqlx::FromRow)]
struct LogRow {
    topic_id: String,
    partitions: i32,
    tables: Vec<String>,
}

/// The topics resolved so far, by both of their names: operations come with
/// the topic's id, notifications with its uuid.
#[derive(Default)]
struct Topics {
    by_id: HashMap<GtsInstanceId, Arc<KnownTopic>>,
    by_uuid: HashMap<Uuid, Arc<KnownTopic>>,
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
    /// Heads committed and not notified yet, under `head_notify:
    /// after_commit`.
    pub heads_to_notify: Arc<HeadsToNotify>,
    /// Read by every operation, written once per topic per process.
    topics: RwLock<Topics>,
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
            heads_to_notify: Arc::default(),
            topics: RwLock::default(),
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

    /// Caches what `evbk__log` holds for a topic.
    pub fn remember(&self, known: KnownTopic) -> Arc<KnownTopic> {
        let known = Arc::new(known);
        let mut topics = self.topics.write().unwrap_or_else(PoisonError::into_inner);
        topics.by_id.insert(known.id.clone(), Arc::clone(&known));
        topics.by_uuid.insert(known.uuid, Arc::clone(&known));
        known
    }

    /// What the catalog holds for `topic`, cached once read; `NotProvisioned`
    /// when it holds nothing.
    pub async fn topic(
        &self,
        topic: &GtsInstanceId,
        operation: Operation,
    ) -> Result<Arc<KnownTopic>, BackendError> {
        let cached = self
            .topics
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_id
            .get(topic)
            .cloned();
        if let Some(known) = cached {
            return Ok(known);
        }
        self.load(topic_uuid(topic), operation)
            .await?
            .ok_or_else(|| BackendError::not_provisioned(operation.name()))
    }

    /// Like [`Self::topic`], by the topic's uuid, and `None` rather than
    /// `NotProvisioned` for a topic `evbk__log` does not hold.
    pub async fn lookup(
        &self,
        topic: Uuid,
        operation: Operation,
    ) -> Result<Option<Arc<KnownTopic>>, BackendError> {
        let cached = self
            .topics
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .by_uuid
            .get(&topic)
            .cloned();
        if cached.is_some() {
            return Ok(cached);
        }
        self.load(topic, operation).await
    }

    async fn load(
        &self,
        topic: Uuid,
        operation: Operation,
    ) -> Result<Option<Arc<KnownTopic>>, BackendError> {
        let row: Option<LogRow> = sqlx::query_as(
            "SELECT l.topic_id, l.partitions, ARRAY(SELECT p.table_name FROM evbk__partition p \
             WHERE p.topic_uuid = l.topic_uuid ORDER BY p.partition) AS tables \
             FROM evbk__log l WHERE l.topic_uuid = $1",
        )
        .bind(topic)
        .fetch_optional(self.pool()?)
        .await
        .map_err(|e| error::map(operation, &e))?;
        let Some(LogRow {
            topic_id,
            partitions,
            tables,
        }) = row
        else {
            return Ok(None);
        };
        let known = KnownTopic {
            id: GtsInstanceId::try_new(&topic_id).map_err(|_| {
                BackendError::Internal(format!("{operation}: evbk__log holds a malformed topic id"))
            })?,
            uuid: topic,
            partitions: u32::try_from(partitions).map_err(|_| {
                BackendError::Internal(format!(
                    "{operation}: evbk__log holds a negative partition count"
                ))
            })?,
            tables: tables.into_iter().map(Arc::from).collect(),
        };
        if u32::try_from(known.tables.len()).ok() != Some(known.partitions) {
            return Err(BackendError::Internal(format!(
                "{operation}: evbk__partition holds {} rows for a topic of {} partitions",
                known.tables.len(),
                known.partitions
            )));
        }
        Ok(Some(self.remember(known)))
    }
}
