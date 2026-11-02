//! The work queues over the gear's outbox (DESIGN 14 §4.4): a work item is enqueued in the
//! transaction that writes its cause, so it commits with it, and carries `tenant_id` and the object
//! id only. `rating.child_work` is partitioned by `hash(subscription_id)`, `rating.fact_bind` by
//! `hash(fact_id)`.
//!
//! The outbox is started by the gear's `serve`; until then (and in a gear that is not served) an
//! enqueue fails, which rolls its transaction back.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::json;
use toolkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxMessage, Partitions, Record,
};
use toolkit_db::secure::DbTx;
use uuid::Uuid;

use crate::domain::child::ChildWork;
use crate::domain::error::{Cause, DomainError};
use crate::domain::fact::intake::{FactIntake, RebindOutcome};
use crate::domain::work::{FactBindWork, Wakeup, WorkQueue};
use crate::infra::queues::{CHILD_WORK_QUEUE, FACT_BIND_QUEUE, QueueLayout};

/// The JSON payload type of every work item.
pub const PAYLOAD_TYPE: &str = "application/json";

/// The partition of `id` among `partitions`: its first four bytes, big-endian, modulo the count.
#[must_use]
pub fn partition_of(id: Uuid, partitions: Partitions) -> u32 {
    let bytes = id.as_bytes();
    let high = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    high % u32::from(partitions.count())
}

/// [`WorkQueue`] over the outbox.
pub struct OutboxWorkQueue {
    outbox: OnceLock<Arc<Outbox>>,
    child_work: Partitions,
    fact_bind: Partitions,
}

impl OutboxWorkQueue {
    /// The queues of `layout`, not started yet.
    #[must_use]
    pub fn new(layout: &QueueLayout) -> Self {
        let partitions = layout.partitions();
        Self {
            outbox: OnceLock::new(),
            child_work: partitions.child_work,
            fact_bind: partitions.fact_bind,
        }
    }

    /// Hands over the started outbox; a second call keeps the first.
    pub fn start(&self, outbox: Arc<Outbox>) {
        if self.outbox.set(outbox).is_err() {
            tracing::warn!(target: "bss_rating.queues", "the work queues were started twice");
        }
    }

    fn outbox(&self) -> Result<&Arc<Outbox>, DomainError> {
        // Transient: the gear's `serve` starts the outbox, and a work item enqueued before it
        // rolls its transaction back to be retried.
        self.outbox.get().ok_or_else(|| {
            DomainError::Database("the work queues are not started".to_owned(), None)
        })
    }

    async fn enqueue(
        &self,
        tx: &DbTx<'_>,
        queue: &str,
        partition: u32,
        payload: &serde_json::Value,
    ) -> Result<Wakeup, DomainError> {
        let outbox = self.outbox()?;
        let bytes = serde_json::to_vec(payload)
            .map_err(|e| DomainError::Internal(format!("work item: {e}"), Some(Cause::new(e))))?;
        let record = Record::to(queue, partition)
            .payload(bytes, PAYLOAD_TYPE)
            .build()
            .map_err(|e| DomainError::Internal(format!("work item: {e}"), Some(Cause::new(e))))?;
        let wake = outbox.enqueue(tx, record).await.map_err(|e| {
            DomainError::Database(format!("enqueue on {queue}"), Some(Cause::new(e)))
        })?;
        Ok(Wakeup::new(move || wake.fire()))
    }
}

#[async_trait]
impl WorkQueue for OutboxWorkQueue {
    async fn enqueue_children(
        &self,
        tx: &DbTx<'_>,
        items: &[ChildWork],
    ) -> Result<Wakeup, DomainError> {
        let mut wakeup = Wakeup::default();
        for item in items {
            let payload = json!({ "tenant_id": item.tenant_id, "child_id": item.child_id });
            let partition = partition_of(item.subscription_id, self.child_work);
            wakeup.merge(
                self.enqueue(tx, CHILD_WORK_QUEUE, partition, &payload)
                    .await?,
            );
        }
        Ok(wakeup)
    }

    async fn enqueue_fact_bind(
        &self,
        tx: &DbTx<'_>,
        item: FactBindWork,
    ) -> Result<Wakeup, DomainError> {
        let payload = json!({ "tenant_id": item.tenant_id, "fact_id": item.fact_id });
        let partition = partition_of(item.fact_id, self.fact_bind);
        self.enqueue(tx, FACT_BIND_QUEUE, partition, &payload).await
    }
}

/// The worker of `rating.fact_bind`: binds a pending usage fact again.
pub struct FactBindWorker {
    intake: FactIntake,
}

impl FactBindWorker {
    /// The worker over `intake`.
    #[must_use]
    pub const fn new(intake: FactIntake) -> Self {
        Self { intake }
    }
}

#[derive(serde::Deserialize)]
struct FactBindPayload {
    tenant_id: Uuid,
    fact_id: Uuid,
}

#[async_trait]
impl LeasedMessageHandler for FactBindWorker {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let Ok(item) = serde_json::from_slice::<FactBindPayload>(&msg.payload) else {
            return MessageResult::Reject("a rating.fact_bind item does not read".to_owned());
        };
        match self.intake.rebind(item.tenant_id, item.fact_id).await {
            Ok(outcome) => {
                if outcome == RebindOutcome::StillPending {
                    tracing::info!(
                        target: "bss_rating.facts",
                        tenant_id = %item.tenant_id,
                        fact_id = %item.fact_id,
                        "a usage fact still waits for its derived declaration"
                    );
                }
                MessageResult::Ok
            }
            Err(error) if error.is_transient() => {
                tracing::warn!(
                    target: "bss_rating.facts",
                    tenant_id = %item.tenant_id,
                    fact_id = %item.fact_id,
                    %error,
                    "binding a fact again failed; retried"
                );
                MessageResult::Retry
            }
            Err(error) => {
                tracing::error!(
                    target: "bss_rating.facts",
                    tenant_id = %item.tenant_id,
                    fact_id = %item.fact_id,
                    %error,
                    "binding a fact again failed for good"
                );
                MessageResult::Reject(error.to_string())
            }
        }
    }
}

#[cfg(test)]
#[path = "work_queue_tests.rs"]
mod tests;
