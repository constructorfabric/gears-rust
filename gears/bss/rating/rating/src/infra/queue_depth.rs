//! The depth of `rating.child_work`, which the feed readers' backpressure reads (feature 04 §3.5).
//!
//! `toolkit_db::outbox` keeps no depth of its own (`…-upreq-outbox-queue-depth` asks for one), so it
//! is read from the queue's tables: the messages the sequencer has not numbered yet (`incoming`)
//! plus, per partition, the sequences assigned beyond the one its worker processed
//! (`partitions.sequence − processor.processed_seq`).
//! The sequencer moves a message from one to the other in one transaction, so the three reads run
//! in one repeatable-read snapshot and never count a message twice or not at all.

use std::collections::BTreeMap;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use toolkit_db::Db;
use toolkit_db::secure::{
    AccessScope, DbTx, SecureEntityExt, TxAccessMode, TxConfig, TxIsolationLevel,
};

use crate::domain::error::DomainError;
use crate::domain::usage::reader::QueueDepth;
use crate::infra::queues::CHILD_WORK_QUEUE;
use crate::infra::storage::entity::outbox::{incoming, partitions, processor};
use crate::infra::storage::scope_failure;

/// [`QueueDepth`] over the outbox tables.
#[derive(Clone)]
pub struct OutboxQueueDepth {
    db: Db,
}

impl OutboxQueueDepth {
    /// The depth read from `db`.
    #[must_use]
    pub const fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl QueueDepth for OutboxQueueDepth {
    async fn child_work_depth(&self) -> Result<u64, DomainError> {
        let snapshot = TxConfig {
            isolation: Some(TxIsolationLevel::RepeatableRead),
            access_mode: Some(TxAccessMode::ReadOnly),
        };
        self.db
            .transaction_ref_mapped_with_config(snapshot, |tx| Box::pin(depth_in(tx)))
            .await
    }
}

async fn depth_in(tx: &DbTx<'_>) -> Result<u64, DomainError> {
    // Queue bookkeeping holds no tenant data: the tables are read by their queue name alone.
    let scope = AccessScope::allow_all();
    let assigned: BTreeMap<i64, i64> = partitions::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(partitions::Column::Queue.eq(CHILD_WORK_QUEUE)))
        .all(tx)
        .await
        .map_err(|e| scope_failure("read queue partitions", e))?
        .into_iter()
        .map(|partition| (partition.id, partition.sequence))
        .collect();
    if assigned.is_empty() {
        return Ok(0);
    }
    let ids: Vec<i64> = assigned.keys().copied().collect();
    let processed: BTreeMap<i64, i64> = processor::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(processor::Column::PartitionId.is_in(ids.clone())))
        .all(tx)
        .await
        .map_err(|e| scope_failure("read queue processors", e))?
        .into_iter()
        .map(|row| (row.partition_id, row.processed_seq))
        .collect();
    let unsequenced = incoming::Entity::find()
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(incoming::Column::PartitionId.is_in(ids)))
        .count(tx)
        .await
        .map_err(|e| scope_failure("count unsequenced messages", e))?;
    let sequenced: u64 = assigned
        .iter()
        .map(|(id, sequence)| {
            let done = processed.get(id).copied().unwrap_or(0);
            u64::try_from(sequence.saturating_sub(done)).unwrap_or(0)
        })
        .fold(0, u64::saturating_add);
    Ok(sequenced.saturating_add(unsequenced))
}

#[cfg(test)]
#[path = "queue_depth_tests.rs"]
mod tests;
