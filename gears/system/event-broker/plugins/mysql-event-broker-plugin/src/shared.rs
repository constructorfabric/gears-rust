//! What every instance of one registry entry holds: its settings, the pools
//! once startup has made them, the heads it knows, the partitions watched,
//! and the topics it has resolved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::support::{LocalHeads, topic_uuid};
use event_broker_sdk::backend::{BackendError, Partition, Role};
use sqlx::MySqlPool;
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::heads::Watched;
use crate::maintenance::hub::Hub;
use crate::names;
use crate::settings::MysqlSettings;

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
    /// The event table of each partition, by partition.
    pub tables: Box<[Arc<str>]>,
}

/// The topics resolved so far.
#[derive(Default)]
struct Topics {
    by_id: HashMap<GtsInstanceId, Arc<KnownTopic>>,
}

/// The state every instance of one registry entry shares.
pub struct Shared {
    /// The entry's settings.
    pub settings: MysqlSettings,
    /// What this process is to the backend.
    pub role: Role,
    /// The main pool: set once the startup task has connected, checked and
    /// migrated; every operation before that is `Unavailable`.
    pool: OnceLock<MySqlPool>,
    /// The background pool, set where the maintenance runs.
    background: OnceLock<MySqlPool>,
    /// Heads known to this process, feeding its watches and waiting reads.
    pub heads: LocalHeads,
    /// The partitions open watches name, which the head poll reads.
    pub watched: Arc<Watched>,
    /// The background maintenance's registry and queue.
    pub maintenance: Hub,
    /// Read by every operation, written once per topic per process.
    topics: RwLock<Topics>,
    /// Why the last startup attempt failed, for the `Unavailable` every
    /// operation answers until one succeeds.
    problem: Mutex<Option<String>>,
    /// Set by `stop`; held while pools are installed, so none is installed
    /// after it.
    stopped: Mutex<bool>,
}

impl Shared {
    /// State for `settings` in a process that is `role`, with no pool until
    /// startup completes.
    pub fn new(settings: MysqlSettings, role: Role) -> Arc<Self> {
        Arc::new(Self {
            settings,
            role,
            pool: OnceLock::new(),
            background: OnceLock::new(),
            heads: LocalHeads::new(),
            watched: Arc::default(),
            maintenance: Hub::default(),
            topics: RwLock::default(),
            problem: Mutex::new(None),
            stopped: Mutex::new(false),
        })
    }

    /// Installs the pools startup has made, unless the backend was stopped
    /// meanwhile: then they are closed instead, and `false` says so. A second
    /// call keeps the first pools.
    pub fn install(&self, main: MySqlPool, background: Option<MySqlPool>) -> bool {
        let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        if *stopped {
            close_in_background([Some(main), background].into_iter().flatten().collect());
            return false;
        }
        if self.pool.set(main).is_err() {
            tracing::warn!(
                "mysql event-broker backend: startup completed twice; keeping the first pool"
            );
        }
        if let Some(background) = background
            && self.background.set(background).is_err()
        {
            tracing::warn!(
                "mysql event-broker backend: startup completed twice; keeping the first background pool"
            );
        }
        true
    }

    /// Records why a startup attempt failed. The text names steps, error
    /// numbers and server variables, never a value from the url or the data.
    pub fn set_problem(&self, problem: String) {
        *self.problem.lock().unwrap_or_else(PoisonError::into_inner) = Some(problem);
    }

    /// Marks the backend stopped, so no pool is installed after it, and
    /// returns the pools installed before.
    pub fn stop(&self) -> Vec<MySqlPool> {
        let mut stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        *stopped = true;
        [self.pool.get(), self.background.get()]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }

    /// Whether this process runs the maintenance: only where events are
    /// appended, so a delivery process holds no background pool and takes no
    /// named lock. A role this plugin does not know yet runs none.
    pub fn runs_maintenance(&self) -> bool {
        matches!(self.role, Role::Ingest | Role::Standalone)
    }

    /// The pool, or `Unavailable` while startup has not completed.
    pub fn pool(&self) -> Result<&MySqlPool, BackendError> {
        self.pool.get().ok_or_else(|| {
            let problem = self.problem.lock().unwrap_or_else(PoisonError::into_inner);
            BackendError::unavailable(match problem.as_deref() {
                Some(problem) => format!("the mysql backend has not finished starting: {problem}"),
                None => "the mysql backend has not finished starting".to_owned(),
            })
        })
    }

    /// Caches what `evbk__log` holds for a topic.
    pub fn remember(&self, known: KnownTopic) -> Arc<KnownTopic> {
        let known = Arc::new(known);
        self.topics
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .by_id
            .insert(known.id.clone(), Arc::clone(&known));
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
        self.load(topic, operation)
            .await?
            .ok_or_else(|| BackendError::not_provisioned(operation.name()))
    }

    async fn load(
        &self,
        topic: &GtsInstanceId,
        operation: Operation,
    ) -> Result<Option<Arc<KnownTopic>>, BackendError> {
        let pool = self.pool()?;
        let map = |e| error::map(operation, &e);
        let uuid = topic_uuid(topic);
        let row: Option<(String, i32)> = error::retried(|| {
            sqlx::query_as("SELECT topic_id, partitions FROM evbk__log WHERE topic_uuid = ?")
                .bind(uuid)
                .fetch_optional(pool)
        })
        .await
        .map_err(map)?;
        let Some((topic_id, partitions)) = row else {
            return Ok(None);
        };
        let tables: Vec<(i32, String)> = error::retried(|| {
            sqlx::query_as(
            "SELECT partition_no, table_name FROM evbk__partition WHERE topic_uuid = ? ORDER BY partition_no",
        )
        .bind(uuid)
        .fetch_all(pool)
        })
        .await
        .map_err(map)?;
        let partitions = u32::try_from(partitions).map_err(|_| {
            BackendError::Internal(format!(
                "{operation}: evbk__log holds a negative partition count"
            ))
        })?;
        let in_order = tables.iter().enumerate().all(|(i, (column, _))| {
            names::partition_from_column(*column).map(Partition::value) == u32::try_from(i).ok()
        });
        if u32::try_from(tables.len()).ok() != Some(partitions) || !in_order {
            // Provisioning has written the log row and not yet every
            // partition row: not cached, so the next call reads again.
            return Err(BackendError::unavailable(format!(
                "{operation}: the topic's partitions are still being provisioned"
            )));
        }
        let known = KnownTopic {
            id: GtsInstanceId::try_new(&topic_id).map_err(|_| {
                BackendError::Internal(format!("{operation}: evbk__log holds a malformed topic id"))
            })?,
            uuid,
            partitions,
            tables: tables
                .into_iter()
                .map(|(_, table)| Arc::from(table))
                .collect(),
        };
        Ok(Some(self.remember(known)))
    }
}

/// Closes `pools` on a task of their own: closing waits for every
/// connection to be returned, which a caller out of time cannot wait for.
pub fn close_in_background(pools: Vec<MySqlPool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for pool in pools {
            pool.close().await;
        }
    })
}
