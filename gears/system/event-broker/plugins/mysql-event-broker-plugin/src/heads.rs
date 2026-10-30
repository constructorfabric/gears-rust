//! The head watch (`docs/DESIGN.md` §4.7): the server pushes nothing, so one
//! worker per instance reads the heads of every partition an open watch names
//! in one batched query each `poll_interval`, and publishes them to the
//! instance's [`LocalHeads`](event_broker_sdk::backend::support::LocalHeads).
//! Appends through this instance publish their heads at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use event_broker_sdk::Sequence;
use event_broker_sdk::backend::{Head, Partition, TopicPartition};
use sqlx::{AssertSqlSafe, MySqlPool};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use toolkit_taskward::{
    Directive, PanicPolicy, TracingListener, WorkerAction, WorkerBuilder, poker,
};
use uuid::Uuid;

use crate::names;
use crate::shared::Shared;

/// A partition by the names its rows carry.
pub type Key = (Uuid, Partition);

/// The most partitions one head query names.
const CHUNK: usize = 500;

/// The partitions open watches name, each with the number of watches naming
/// it.
#[derive(Default)]
pub struct Watched {
    named: Mutex<HashMap<Key, (TopicPartition, usize)>>,
}

impl Watched {
    /// Counts a watch over `partitions` in, until the guard is dropped.
    pub fn enter(self: &Arc<Self>, partitions: Vec<(Key, TopicPartition)>) -> WatchGuard {
        {
            let mut named = self.named.lock().unwrap_or_else(PoisonError::into_inner);
            for (key, at) in &partitions {
                named.entry(*key).or_insert_with(|| (at.clone(), 0)).1 += 1;
            }
        }
        WatchGuard {
            watched: Arc::clone(self),
            keys: partitions.into_iter().map(|(key, _)| key).collect(),
        }
    }

    /// Every partition watched now.
    fn snapshot(&self) -> Vec<(Key, TopicPartition)> {
        self.named
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(key, (at, _))| (*key, at.clone()))
            .collect()
    }
}

/// Keeps a watch's partitions polled while it lives.
pub struct WatchGuard {
    watched: Arc<Watched>,
    keys: Vec<Key>,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        let mut named = self
            .watched
            .named
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for key in &self.keys {
            if let Some((_, count)) = named.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    named.remove(key);
                }
            }
        }
    }
}

/// The head of each of `keys` that `evbk__partition` holds, in no order; a
/// partition with no row is left out.
pub async fn hwms(pool: &MySqlPool, keys: &[Key]) -> Result<Vec<(Key, i64)>, sqlx::Error> {
    let mut heads = Vec::with_capacity(keys.len());
    for chunk in keys.chunks(CHUNK) {
        let tuples = vec!["(?, ?)"; chunk.len()].join(", ");
        let mut query = sqlx::query_as::<_, (Uuid, i32, i64)>(AssertSqlSafe(format!(
            "SELECT topic_uuid, partition_no, hwm FROM evbk__partition \
             WHERE (topic_uuid, partition_no) IN ({tuples})"
        )));
        for (topic, partition) in chunk {
            query = query.bind(*topic).bind(names::partition_column(*partition));
        }
        for (topic, partition, hwm) in query.fetch_all(pool).await? {
            if let Some(partition) = names::partition_from_column(partition) {
                heads.push(((topic, partition), hwm));
            }
        }
    }
    Ok(heads)
}

/// The worker that polls the watched heads.
struct Poller {
    shared: Arc<Shared>,
    pool: MySqlPool,
}

impl WorkerAction for Poller {
    type Payload = ();
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        let watched: HashMap<Key, TopicPartition> =
            self.shared.watched.snapshot().into_iter().collect();
        if watched.is_empty() {
            return Ok(Directive::idle());
        }
        let keys: Vec<Key> = watched.keys().copied().collect();
        let heads = hwms(&self.pool, &keys)
            .await
            .map_err(|e| format!("head poll: {}", crate::error::kind(&e)))?;
        for (key, hwm) in heads {
            if let Some(at) = watched.get(&key) {
                self.shared.heads.publish(Head {
                    at: at.clone(),
                    sequence: Sequence::assigned(hwm),
                });
            }
        }
        Ok(Directive::idle())
    }
}

/// Starts the head poll over `pool`; it runs until `stop`.
pub fn spawn(
    shared: &Arc<Shared>,
    pool: &MySqlPool,
    stop: &CancellationToken,
    tasks: &TaskTracker,
) {
    let (tick, _) = poker(shared.settings.poll_interval, stop.clone());
    let worker = WorkerBuilder::new("evbk-heads", stop.clone())
        .notifier(tick)
        .listener(TracingListener)
        .on_panic(PanicPolicy::CatchAndRetry)
        .build(Poller {
            shared: Arc::clone(shared),
            pool: pool.clone(),
        });
    tasks.spawn(worker.run());
}
