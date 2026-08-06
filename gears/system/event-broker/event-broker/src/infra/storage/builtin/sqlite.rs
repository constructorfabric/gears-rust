//! The real, permanent SQLite implementation of the SDK's
//! `EventBrokerBackend` trait (eb-single-process-implementation D3) -
//! replaces the `InMemoryStorageBackend` shell this module used to hold.
//! Implements the outbox-retry dedup DESIGN.md already specifies
//! (D4/`DESIGN.md:835-846`): one `last_chain_sequence` per `(topic,
//! partition)`, compared against the producer chain-sequence field on each
//! incoming event - not `event.id`.
//!
//! Knows nothing about `SpecificationManager`, `Storage`, cluster
//! coordination, or idempotency beyond its own outbox-retry safety net,
//! matching DESIGN.md's "the backend knows nothing about cluster
//! coordination, notifications, idempotency, or subscriptions. It is pure
//! storage."

use async_trait::async_trait;
use event_broker_sdk::models::{Event, PartitionLeader, PartitionRange, TopicSegment};
use event_broker_sdk::{EventBrokerBackend, StorageBackendError};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use toolkit_db::DBProvider;
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::{AccessScope, SecurityContext};

use crate::infra::storage::entity::{event as event_entity, partition_state};
use crate::infra::storage::error::db_err_from_scope;

const BACKEND_INSTANCE: &str = "event-broker-sqlite-backend";

fn persist_failed(reason: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::PersistFailed {
        reason: reason.to_string(),
        detail: String::new(),
        instance: BACKEND_INSTANCE.to_owned(),
    }
}

fn read_failed(reason: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::ReadFailed {
        reason: reason.to_string(),
        detail: String::new(),
        instance: BACKEND_INSTANCE.to_owned(),
    }
}

pub struct SqliteEventBackend {
    db: std::sync::Arc<DBProvider<toolkit_db::DbError>>,
}

impl SqliteEventBackend {
    #[must_use]
    pub fn new(db: std::sync::Arc<DBProvider<toolkit_db::DbError>>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl EventBrokerBackend for SqliteEventBackend {
    async fn persist(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        events: &[Event],
    ) -> Result<(), StorageBackendError> {
        if events.is_empty() {
            return Ok(());
        }
        let partition_i32 =
            i32::try_from(partition).map_err(|e| persist_failed(format!("partition too large: {e}")))?;
        let topic = topic.to_owned();
        let events = events.to_vec();

        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let state = partition_state::Entity::find()
                        .filter(partition_state::Column::Topic.eq(topic.clone()))
                        .filter(partition_state::Column::Partition.eq(partition_i32))
                        .secure()
                        .scope_with(&AccessScope::allow_all())
                        .one(tx)
                        .await
                        .map_err(|e| db_err_from_scope(&e))?;

                    let (mut next_sequence, last_chain_sequence) = match &state {
                        Some(row) => (row.next_sequence, row.last_chain_sequence),
                        None => (1, None),
                    };

                    // Outbox-retry dedup (D4): compare every incoming event's
                    // producer chain sequence against the stored
                    // `last_chain_sequence` - not `event.id`. Events with no
                    // chain sequence (stateless mode) are never deduped here,
                    // matching stateless mode's documented "no broker-side
                    // dedup".
                    let chain_sequences: Vec<i64> = events
                        .iter()
                        .filter_map(|e| e.meta.as_ref().and_then(|m| m.sequence))
                        .collect();
                    let is_retry = last_chain_sequence.is_some_and(|last| {
                        !chain_sequences.is_empty()
                            && chain_sequences.iter().all(|&s| s <= last)
                    });
                    if is_retry {
                        // Every chained/monotonic event in this batch was
                        // already durably applied - a retried persist call.
                        return Ok(());
                    }

                    let mut new_last_chain_sequence = last_chain_sequence;
                    let mut models = Vec::with_capacity(events.len());
                    for event in &events {
                        let sequence = next_sequence;
                        next_sequence += 1;
                        if let Some(chain_sequence) =
                            event.meta.as_ref().and_then(|m| m.sequence)
                        {
                            new_last_chain_sequence = Some(chain_sequence);
                        }
                        let data = event.data.clone().unwrap_or(serde_json::Value::Null);
                        models.push(event_entity::ActiveModel {
                            id: Set(event.id),
                            type_id: Set(event.type_id.clone()),
                            topic: Set(topic.clone()),
                            tenant_id: Set(event.tenant_id),
                            source: Set(event.source.clone()),
                            subject: Set(event.subject.clone()),
                            subject_type: Set(event.subject_type.clone()),
                            partition_key: Set(event.partition_key.clone()),
                            occurred_at: Set(event.occurred_at),
                            trace_parent: Set(event.trace_parent.clone()),
                            data: Set(data),
                            partition: Set(partition_i32),
                            sequence: Set(sequence),
                            sequence_time: Set(chrono::Utc::now()),
                        });
                    }

                    for model in models {
                        secure_insert::<event_entity::Entity>(
                            model,
                            &AccessScope::allow_all(),
                            tx,
                        )
                        .await
                        .map_err(|e| db_err_from_scope(&e))?;
                    }

                    // Plain `ActiveModelTrait::update`/`insert` need
                    // `ConnectionTrait`, which `DbTx` deliberately does not
                    // implement (toolkit-db's "hidden database runner
                    // capability" security model) - `update_many().secure()`
                    // + `secure_insert` fallback (same pattern as `Storage`'s
                    // `spec_cache`/`cursor` upserts) works with `&DbTx`
                    // directly.
                    let update_result = partition_state::Entity::update_many()
                        .secure()
                        .scope_with(&AccessScope::allow_all())
                        .filter(
                            Condition::all()
                                .add(partition_state::Column::Topic.eq(topic.clone()))
                                .add(partition_state::Column::Partition.eq(partition_i32)),
                        )
                        .col_expr(partition_state::Column::NextSequence, Expr::value(next_sequence))
                        .col_expr(
                            partition_state::Column::LastChainSequence,
                            Expr::value(new_last_chain_sequence),
                        )
                        .exec(tx)
                        .await
                        .map_err(|e| db_err_from_scope(&e))?;
                    if update_result.rows_affected == 0 {
                        let am = partition_state::ActiveModel {
                            topic: Set(topic.clone()),
                            partition: Set(partition_i32),
                            next_sequence: Set(next_sequence),
                            last_chain_sequence: Set(new_last_chain_sequence),
                        };
                        secure_insert::<partition_state::Entity>(am, &AccessScope::allow_all(), tx)
                            .await
                            .map_err(|e| db_err_from_scope(&e))?;
                    }
                    Ok(())
                })
            })
            .await
            .map_err(persist_failed)
    }

    async fn read(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        start_offset: i64,
        max_count: usize,
    ) -> Result<Vec<Event>, StorageBackendError> {
        let partition_i32 =
            i32::try_from(partition).map_err(|e| read_failed(format!("partition too large: {e}")))?;
        let conn = self.db.conn().map_err(|e| read_failed(e.to_string()))?;
        let limit = u64::try_from(max_count).unwrap_or(u64::MAX);
        let rows = event_entity::Entity::find()
            .filter(event_entity::Column::Topic.eq(topic))
            .filter(event_entity::Column::Partition.eq(partition_i32))
            .filter(event_entity::Column::Sequence.gt(start_offset))
            .order_by_asc(event_entity::Column::Sequence)
            .limit(limit)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await
            .map_err(|e| read_failed(e.to_string()))?;
        Ok(rows.into_iter().map(row_to_sdk_event).collect())
    }

    async fn query(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, StorageBackendError> {
        let partition_i32 =
            i32::try_from(partition).map_err(|e| read_failed(format!("partition too large: {e}")))?;
        let conn = self.db.conn().map_err(|e| read_failed(e.to_string()))?;
        let mut query = event_entity::Entity::find()
            .filter(event_entity::Column::Topic.eq(topic))
            .filter(event_entity::Column::Partition.eq(partition_i32));
        if let Some(start) = range.start_offset {
            query = query.filter(event_entity::Column::Sequence.gte(start));
        }
        if let Some(end) = range.end_offset {
            query = query.filter(event_entity::Column::Sequence.lte(end));
        }
        let rows = query
            .order_by_asc(event_entity::Column::Sequence)
            .limit(u64::from(range.limit))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await
            .map_err(|e| read_failed(e.to_string()))?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let start_sequence = rows.first().map_or(0, |r| r.sequence);
        let end_sequence = rows.last().map_or(0, |r| r.sequence);
        let start_time = rows.first().map_or_else(chrono::Utc::now, |r| r.sequence_time);
        let end_time = rows.last().map_or_else(chrono::Utc::now, |r| r.sequence_time);
        // One synthetic segment describing the whole matched range - honest
        // for what this backend actually does (no real segmented storage;
        // `docs/openapi.yaml`: "Backend-specific in shape").
        Ok(vec![TopicSegment {
            topic: topic.to_owned(),
            partition,
            start_sequence,
            end_sequence,
            start_time,
            end_time,
            segments: vec![serde_json::json!({
                "start_sequence": start_sequence,
                "end_sequence": end_sequence,
                "event_count": rows.len(),
            })],
        }])
    }

    async fn list_partition_leaders(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
    ) -> Result<Vec<PartitionLeader>, StorageBackendError> {
        // Standalone mode has no sharding - this single process is the only
        // leader for every partition of every topic. `endpoint` is empty
        // rather than a real address since nothing calls this in standalone
        // mode today (it exists for a future cluster-mode dispatcher).
        let conn = self.db.conn().map_err(|e| read_failed(e.to_string()))?;
        let partitions: Vec<i32> = event_entity::Entity::find()
            .filter(event_entity::Column::Topic.eq(topic))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(&conn)
            .await
            .map_err(|e| read_failed(e.to_string()))?
            .into_iter()
            .map(|r| r.partition)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(partitions
            .into_iter()
            .filter_map(|p| u32::try_from(p).ok())
            .map(|partition| PartitionLeader {
                partition,
                endpoint: String::new(),
            })
            .collect())
    }
}

fn row_to_sdk_event(row: event_entity::Model) -> Event {
    Event {
        id: row.id,
        type_id: row.type_id,
        topic: row.topic,
        tenant_id: row.tenant_id,
        source: row.source,
        subject: row.subject,
        subject_type: row.subject_type,
        partition_key: row.partition_key,
        occurred_at: row.occurred_at,
        trace_parent: row.trace_parent,
        data: Some(row.data),
        partition: u32::try_from(row.partition).ok(),
        sequence: Some(row.sequence),
        sequence_time: Some(row.sequence_time),
        offset: Some(row.sequence),
        offset_time: Some(row.sequence_time),
        meta: None,
    }
}

