//! Head notifications (`docs/DESIGN.md` §2.3, §4.2): one batched `NOTIFY`
//! per append, sent inside the append transaction or after its commit by a
//! worker of its own, as the `head_notify` setting says.
//!
//! A transaction that notifies holds a lock shared by the whole cluster from
//! its commit until the commit is flushed, so notifying commits run one at a
//! time. Inside the append, every append waits its turn; after the commit,
//! only the worker does, and it sends the heads of many appends at once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use event_broker_sdk::backend::Partition;
use sqlx::{PgPool, Postgres};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use toolkit_taskward::{Directive, PanicPolicy, TracingListener, WorkerAction, WorkerBuilder};
use uuid::Uuid;

use crate::names;

/// Sends one notification per `(topic, partition, sequence)` head, in one
/// statement.
pub async fn send<'e, E>(executor: E, heads: &[(Uuid, Partition, i64)]) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let payloads: Vec<String> = heads
        .iter()
        .map(|&(topic, partition, sequence)| names::head_payload(topic, partition, sequence))
        .collect();
    sqlx::query("SELECT pg_notify($1, payload) FROM UNNEST($2::text[]) AS payload")
        .bind(names::HEAD_CHANNEL)
        .bind(payloads)
        .execute(executor)
        .await?;
    Ok(())
}

/// Heads committed but not notified yet, the latest per partition: only the
/// latest matters to a listener.
#[derive(Default)]
pub struct HeadsToNotify {
    latest: Mutex<HashMap<(Uuid, Partition), i64>>,
    wakeup: Arc<Notify>,
}

impl HeadsToNotify {
    /// Queues a committed head and wakes the worker.
    pub fn offer(&self, topic: Uuid, partition: Partition, sequence: i64) {
        self.merge([((topic, partition), sequence)]);
        self.wakeup.notify_one();
    }

    fn merge(&self, offered: impl IntoIterator<Item = ((Uuid, Partition), i64)>) {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        for (key, sequence) in offered {
            latest
                .entry(key)
                .and_modify(|known| *known = (*known).max(sequence))
                .or_insert(sequence);
        }
    }

    fn take(&self) -> HashMap<(Uuid, Partition), i64> {
        std::mem::take(&mut *self.latest.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// The worker that notifies what [`HeadsToNotify`] holds.
struct Notifier {
    heads: Arc<HeadsToNotify>,
    pool: PgPool,
}

impl WorkerAction for Notifier {
    type Payload = ();
    type Error = sqlx::Error;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, sqlx::Error> {
        let taken = self.heads.take();
        if taken.is_empty() {
            return Ok(Directive::idle());
        }
        let heads: Vec<(Uuid, Partition, i64)> = taken
            .iter()
            .map(|(&(topic, partition), &sequence)| (topic, partition, sequence))
            .collect();
        if let Err(err) = self.notify(&heads).await {
            // Back for the next pass; a later head of the same partition wins.
            self.heads.merge(taken);
            return Err(err);
        }
        Ok(Directive::proceed())
    }
}

impl Notifier {
    async fn notify(&self, heads: &[(Uuid, Partition, i64)]) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        // A notification lost to a crash is lost before the flush or after it
        // alike, so this commit does not wait for the flush, nor hold the
        // cluster's notify lock across it.
        sqlx::query("SET LOCAL synchronous_commit = off")
            .execute(&mut *tx)
            .await?;
        send(&mut *tx, heads).await?;
        tx.commit().await
    }
}

/// Starts the worker notifying `heads` over `pool`; it runs until `stop`.
pub fn spawn(
    heads: &Arc<HeadsToNotify>,
    pool: &PgPool,
    stop: &CancellationToken,
    tasks: &TaskTracker,
) {
    let worker = WorkerBuilder::new("evbk-notify", stop.clone())
        .notifier(Arc::clone(&heads.wakeup))
        .listener(TracingListener)
        .on_panic(PanicPolicy::CatchAndRetry)
        .build(Notifier {
            heads: Arc::clone(heads),
            pool: pool.clone(),
        });
    tasks.spawn(worker.run());
}
