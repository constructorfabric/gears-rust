use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use tokio::sync::{Notify, RwLock};

use super::dialect::Dialect;
use super::manager::OutboxBuilder;
use super::prioritizer::SharedPrioritizer;
use super::types::{EnqueueMessage, OutboxConfig, OutboxError, OutboxMessageId};
use crate::Db;
use crate::secure::SeaOrmRunner;

/// Per-partition notify map shared between sequencer and processors.
type PartitionNotifyMap = Arc<HashMap<i64, Arc<Notify>>>;

/// Maximum payload size in bytes (64 KiB).
const MAX_PAYLOAD_SIZE: usize = 64 * 1024;

/// Max rows per multi-row INSERT statement to avoid parameter limits.
const BATCH_CHUNK_SIZE: usize = 100;

/// Core outbox handle. Holds partition cache and notification channels.
pub struct Outbox {
    config: OutboxConfig,
    /// Cached partition lookup: `partitions[queue_name][partition_number] = partitions.id` (PK).
    partitions: DashMap<String, Vec<i64>>,
    /// Reverse map: `partition_id → queue_name`. Populated during `register_queue`.
    partition_to_queue: DashMap<i64, String>,
    /// Flattened, sorted, deduplicated snapshot of all partition IDs.
    /// Rebuilt on each `register_queue` call.
    all_partition_ids: RwLock<Vec<i64>>,
    /// Shared prioritizer for dirty partition tracking. Set during `start()`.
    pub(crate) prioritizer: RwLock<Option<Arc<SharedPrioritizer>>>,
    /// Per-partition notify map for direct signaling from sequencer to processors.
    /// Set once during `start()` after all processors are spawned.
    partition_notify: RwLock<Option<PartitionNotifyMap>>,
}

#[derive(Debug, FromQueryResult)]
struct PartitionRow {
    id: i64,
}

impl Outbox {
    /// Create a fluent builder for the outbox pipeline.
    ///
    /// This is the main entry point. See [`OutboxBuilder`] for usage.
    #[must_use]
    pub fn builder(db: Db) -> OutboxBuilder {
        OutboxBuilder::new(db)
    }

    /// Create a new outbox. Construction goes through [`OutboxBuilder::start()`].
    #[must_use]
    pub(crate) fn new(config: OutboxConfig) -> Self {
        Self {
            config,
            partitions: DashMap::new(),
            partition_to_queue: DashMap::new(),
            all_partition_ids: RwLock::new(Vec::new()),
            prioritizer: RwLock::new(None),
            partition_notify: RwLock::new(None),
        }
    }

    /// Register a queue with `num_partitions` partitions `[0, num_partitions)`.
    ///
    /// Idempotent when the partition count matches. Returns
    /// [`OutboxError::PartitionCountMismatch`] if the count differs.
    ///
    /// # Errors
    ///
    /// Returns an error if the database operation fails or if the partition
    /// count does not match an existing registration.
    ///
    /// # Concurrency note
    ///
    /// There is a TOCTOU window between the partition-count check and the
    /// INSERT. During hot upgrades, multiple instances may call
    /// `register_queue` concurrently for the same queue. The INSERT uses
    /// `ON CONFLICT DO NOTHING`, so concurrent inserts are safe — the
    /// second caller will see the already-inserted rows on its read-back.
    pub async fn register_queue(
        &self,
        db: &Db,
        queue: &str,
        num_partitions: u16,
    ) -> Result<(), OutboxError> {
        super::validation::validate_queue_name(queue)?;
        let conn = db.sea_internal();
        let txn = conn.begin().await?;
        let backend = txn.get_database_backend();
        let dialect = Dialect::from(backend);

        let ids =
            Self::ensure_partition_rows(&txn, backend, &dialect, queue, num_partitions).await?;
        Self::ensure_processor_rows(&txn, backend, &dialect, &ids).await?;
        Self::ensure_vacuum_counter_rows(&txn, backend, &dialect, &ids).await?;

        txn.commit().await?;

        self.populate_caches(queue, &ids).await;
        Ok(())
    }

    /// Check existing partition rows; insert new ones if absent.
    ///
    /// Returns the partition IDs (PKs) for the queue — whether they were
    /// already present or freshly inserted.
    async fn ensure_partition_rows<C: ConnectionTrait>(
        conn: &C,
        backend: DbBackend,
        dialect: &Dialect,
        queue: &str,
        num_partitions: u16,
    ) -> Result<Vec<i64>, OutboxError> {
        let existing = PartitionRow::find_by_statement(Statement::from_sql_and_values(
            backend,
            dialect.register_queue_select(),
            [queue.into()],
        ))
        .all(conn)
        .await?;

        if !existing.is_empty() {
            if existing.len() != usize::from(num_partitions) {
                return Err(OutboxError::PartitionCountMismatch {
                    queue: queue.to_owned(),
                    expected: num_partitions,
                    found: existing.len(),
                });
            }
            return Ok(existing.into_iter().map(|r| r.id).collect());
        }

        // First registration — insert partition rows
        for p in 0..num_partitions {
            conn.execute(Statement::from_sql_and_values(
                backend,
                dialect.register_queue_insert(),
                #[allow(clippy::cast_possible_wrap)]
                [queue.into(), (p as i16).into()],
            ))
            .await?;
        }

        // Read back inserted rows to get their PKs
        let rows = PartitionRow::find_by_statement(Statement::from_sql_and_values(
            backend,
            dialect.register_queue_select(),
            [queue.into()],
        ))
        .all(conn)
        .await?;

        Ok(rows.into_iter().map(|r| r.id).collect())
    }

    /// Insert a processor row for each partition ID (idempotent via
    /// `ON CONFLICT DO NOTHING`).
    async fn ensure_processor_rows<C: ConnectionTrait>(
        conn: &C,
        backend: DbBackend,
        dialect: &Dialect,
        ids: &[i64],
    ) -> Result<(), OutboxError> {
        for &id in ids {
            conn.execute(Statement::from_sql_and_values(
                backend,
                dialect.insert_processor_row(),
                [id.into()],
            ))
            .await?;
        }
        Ok(())
    }

    /// Insert a vacuum counter row for each partition ID (idempotent via
    /// `ON CONFLICT DO NOTHING`).
    async fn ensure_vacuum_counter_rows<C: ConnectionTrait>(
        conn: &C,
        backend: DbBackend,
        dialect: &Dialect,
        ids: &[i64],
    ) -> Result<(), OutboxError> {
        for &id in ids {
            conn.execute(Statement::from_sql_and_values(
                backend,
                dialect.insert_vacuum_counter_row(),
                [id.into()],
            ))
            .await?;
        }
        Ok(())
    }

    /// Update in-memory caches with the partition IDs for a queue.
    async fn populate_caches(&self, queue: &str, ids: &[i64]) {
        for &id in ids {
            self.partition_to_queue.insert(id, queue.to_owned());
        }
        self.partitions.insert(queue.to_owned(), ids.to_vec());
        self.rebuild_partition_id_cache().await;
    }

    /// Resolve the `partition_id` (PK) for a `(queue, partition)` pair from cache.
    fn resolve_partition(&self, queue: &str, partition: u32) -> Result<i64, OutboxError> {
        let entry = self
            .partitions
            .get(queue)
            .ok_or_else(|| OutboxError::QueueNotRegistered(queue.to_owned()))?;
        let ids = entry.value();
        ids.get(partition as usize)
            .copied()
            .ok_or_else(|| OutboxError::PartitionOutOfRange {
                queue: queue.to_owned(),
                partition,
                #[allow(clippy::cast_possible_truncation)]
                max: ids.len() as u32,
            })
    }

    /// Validate payload size.
    fn validate_payload(payload: &[u8]) -> Result<(), OutboxError> {
        if payload.len() > MAX_PAYLOAD_SIZE {
            return Err(OutboxError::PayloadTooLarge {
                size: payload.len(),
                max: MAX_PAYLOAD_SIZE,
            });
        }
        Ok(())
    }

    /// Enqueue a single message. Accepts `&impl DBRunner` — use within a transaction
    /// for atomicity with business data, or with a standalone connection.
    ///
    /// # Errors
    ///
    /// Returns an error on validation failure or database error.
    pub async fn enqueue(
        &self,
        db: &(impl crate::secure::DBRunner + Sync + ?Sized),
        queue: &str,
        partition: u32,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> Result<OutboxMessageId, OutboxError> {
        super::validation::validate_queue_name(queue)?;
        super::validation::validate_payload_type(payload_type)?;
        Self::validate_payload(&payload)?;
        let partition_id = self.resolve_partition(queue, partition)?;

        let runner = db.as_seaorm();
        let incoming_id =
            Self::insert_body_and_incoming(&runner, partition_id, payload, payload_type).await?;

        self.push_dirty(partition_id);

        Ok(OutboxMessageId(incoming_id))
    }

    /// Enqueue a batch of items for a single queue.
    /// All validation happens before any DB writes — a single invalid message
    /// rejects the entire batch.
    ///
    /// # Errors
    ///
    /// Returns an error on validation failure or database error.
    pub async fn enqueue_batch(
        &self,
        db: &(impl crate::secure::DBRunner + Sync + ?Sized),
        queue: &str,
        items: &[EnqueueMessage<'_>],
    ) -> Result<Vec<OutboxMessageId>, OutboxError> {
        // Validate ALL items first
        super::validation::validate_queue_name(queue)?;
        let mut resolved = Vec::with_capacity(items.len());
        for item in items {
            super::validation::validate_payload_type(item.payload_type)?;
            Self::validate_payload(&item.payload)?;
            let partition_id = self.resolve_partition(queue, item.partition)?;
            resolved.push(partition_id);
        }

        let runner = db.as_seaorm();
        let ids = Self::insert_batch(&runner, &resolved, items).await?;

        // Push dirty for each distinct partition_id in the batch
        for &pid in &resolved {
            self.push_dirty(pid);
        }

        Ok(ids)
    }

    /// Insert a batch of body + incoming rows using multi-row INSERTs.
    async fn insert_batch(
        runner: &SeaOrmRunner<'_>,
        partition_ids: &[i64],
        items: &[EnqueueMessage<'_>],
    ) -> Result<Vec<OutboxMessageId>, OutboxError> {
        let (conn, backend): (&dyn ConnectionTrait, DbBackend) = match runner {
            SeaOrmRunner::Conn(c) => (*c, c.get_database_backend()),
            SeaOrmRunner::Tx(t) => (*t, t.get_database_backend()),
        };
        let dialect = Dialect::from(backend);

        if items.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_body_ids: Vec<i64> = Vec::with_capacity(items.len());

        // Insert body rows in chunks
        for chunk in items.chunks(BATCH_CHUNK_SIZE) {
            let payloads: Vec<(&[u8], &str)> = chunk
                .iter()
                .map(|item| (item.payload.as_slice(), item.payload_type))
                .collect();
            let chunk_ids = dialect
                .exec_insert_body_batch(conn, backend, &payloads)
                .await?;
            all_body_ids.extend(chunk_ids);
        }

        let mut all_incoming_ids: Vec<OutboxMessageId> = Vec::with_capacity(items.len());

        // Insert incoming rows in chunks
        for chunk_start in (0..items.len()).step_by(BATCH_CHUNK_SIZE) {
            let chunk_end = (chunk_start + BATCH_CHUNK_SIZE).min(items.len());
            let entries: Vec<(i64, i64)> = (chunk_start..chunk_end)
                .map(|i| (partition_ids[i], all_body_ids[i]))
                .collect();
            let chunk_ids = dialect
                .exec_insert_incoming_batch(conn, backend, &entries)
                .await?;
            all_incoming_ids.extend(chunk_ids.into_iter().map(OutboxMessageId));
        }

        Ok(all_incoming_ids)
    }

    /// Insert body + incoming rows, returning the `incoming_id`.
    async fn insert_body_and_incoming(
        runner: &SeaOrmRunner<'_>,
        partition_id: i64,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> Result<i64, OutboxError> {
        let (conn, backend): (&dyn ConnectionTrait, DbBackend) = match runner {
            SeaOrmRunner::Conn(c) => (*c, c.get_database_backend()),
            SeaOrmRunner::Tx(t) => (*t, t.get_database_backend()),
        };
        let dialect = Dialect::from(backend);

        let incoming_id = dialect
            .exec_insert_body_and_incoming(conn, backend, partition_id, payload, payload_type)
            .await?;

        Ok(incoming_id)
    }

    /// List dead-lettered messages with optional filtering.
    ///
    /// Dead letters are an **exceptional recovery mechanism** for messages that
    /// handlers explicitly rejected. They are operator-level tools, not part of
    /// the normal processing pipeline. If dead letter replay is a regular part
    /// of your workflow, consider fixing the handler instead.
    ///
    /// # Errors
    /// Returns error if the database operation fails.
    pub async fn dead_letter_list(
        &self,
        db: &(impl crate::secure::DBRunner + Sync),
        filter: &super::dead_letter::DeadLetterFilter,
    ) -> Result<Vec<super::dead_letter::DeadLetterMessage>, OutboxError> {
        super::dead_letter::dead_letter_list(db.as_seaorm(), filter).await
    }

    /// Count dead-lettered messages matching the filter.
    ///
    /// # Errors
    /// Returns error if the database operation fails.
    pub async fn dead_letter_count(
        &self,
        db: &(impl crate::secure::DBRunner + Sync),
        filter: &super::dead_letter::DeadLetterFilter,
    ) -> Result<u64, OutboxError> {
        super::dead_letter::dead_letter_count(db.as_seaorm(), filter).await
    }

    /// Claim dead letters for reprocessing. Returns the claimed messages.
    ///
    /// The caller decides what to do — process inline, re-enqueue, etc.
    /// Call `dead_letter_resolve()` on success or `dead_letter_reject()` on failure.
    ///
    /// # Errors
    /// Returns error if the database operation fails.
    pub async fn dead_letter_replay(
        &self,
        db: &(impl crate::secure::DBRunner + Sync),
        scope: &super::dead_letter::DeadLetterScope,
        timeout: std::time::Duration,
    ) -> Result<Vec<super::dead_letter::DeadLetterMessage>, OutboxError> {
        super::dead_letter::dead_letter_replay(db.as_seaorm(), scope, timeout).await
    }

    /// Transition claimed dead letters to `resolved`.
    ///
    /// # Errors
    /// Returns error if the database operation fails.
    pub async fn dead_letter_resolve(
        &self,
        db: &(impl crate::secure::DBRunner + Sync),
        ids: &[i64],
    ) -> Result<u64, OutboxError> {
        super::dead_letter::dead_letter_resolve(db.as_seaorm(), ids).await
    }

    /// Transition claimed dead letters back to `pending` with attempts++.
    ///
    /// # Errors
    /// Returns error if the database operation fails.
    pub async fn dead_letter_reject(
        &self,
        db: &(impl crate::secure::DBRunner + Sync),
        ids: &[i64],
        reason: &str,
    ) -> Result<u64, OutboxError> {
        super::dead_letter::dead_letter_reject(db.as_seaorm(), ids, reason).await
    }

    /// Discard pending dead letters — transitions to `discarded`.
    ///
    /// # Errors
    /// Returns error if the database operation fails.
    pub async fn dead_letter_discard(
        &self,
        db: &(impl crate::secure::DBRunner + Sync),
        scope: &super::dead_letter::DeadLetterScope,
    ) -> Result<u64, OutboxError> {
        super::dead_letter::dead_letter_discard(db.as_seaorm(), scope).await
    }

    /// Delete terminal-state dead letters (`resolved` + `discarded`).
    ///
    /// # Errors
    /// Returns error if the database operation fails.
    pub async fn dead_letter_cleanup(
        &self,
        db: &(impl crate::secure::DBRunner + Sync),
        scope: &super::dead_letter::DeadLetterScope,
    ) -> Result<u64, OutboxError> {
        super::dead_letter::dead_letter_cleanup(db.as_seaorm(), scope).await
    }

    /// Install the shared prioritizer. Called once during `start()`.
    pub(crate) async fn set_prioritizer(&self, prioritizer: Arc<SharedPrioritizer>) {
        *self.prioritizer.write().await = Some(prioritizer);
    }

    /// Push a partition into the prioritizer (dirty signal).
    /// No-op if the prioritizer is not yet installed (before `start()`).
    fn push_dirty(&self, partition_id: i64) {
        if let Some(guard) = self.prioritizer.try_read().ok()
            && let Some(p) = guard.as_ref()
        {
            p.push_dirty(partition_id);
        }
    }

    /// Install the per-partition notify map. Called once during `start()`.
    pub(crate) async fn set_partition_notify(&self, map: PartitionNotifyMap) {
        *self.partition_notify.write().await = Some(map);
    }

    /// Signal a partition's processor that new outgoing rows are available.
    pub(crate) fn notify_partition(&self, partition_id: i64) {
        if let Some(guard) = self.partition_notify.try_read().ok()
            && let Some(map) = guard.as_ref()
            && let Some(notify) = map.get(&partition_id)
        {
            notify.notify_one();
        }
    }

    /// Notify the sequencer that new items are available.
    /// Multiple flushes coalesce into a single wakeup.
    /// No-op before `set_prioritizer()` (during startup).
    pub fn flush(&self) {
        if let Ok(guard) = self.prioritizer.try_read()
            && let Some(p) = guard.as_ref()
        {
            p.wake_sequencers();
        }
    }

    /// Execute a closure inside a database transaction, then auto-flush
    /// the sequencer notification channel on success.
    pub async fn transaction<F, T>(&self, db: Db, f: F) -> (Db, anyhow::Result<T>)
    where
        F: for<'a> FnOnce(
                &'a crate::DbTx<'a>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = anyhow::Result<T>> + Send + 'a>,
            > + Send,
        T: Send + 'static,
    {
        let (db, result) = db.transaction(f).await;
        if result.is_ok() {
            self.flush();
        }
        (db, result)
    }

    /// Returns all registered partition IDs in deterministic order (sorted by PK).
    /// Reads from a pre-computed cache that is rebuilt on each `register_queue` call.
    #[allow(dead_code)] // used in integration tests
    pub(crate) fn all_partition_ids(&self) -> Vec<i64> {
        // try_read is non-blocking and always succeeds when no writer is active.
        // Writers only hold the lock briefly during register_queue (startup).
        self.all_partition_ids
            .try_read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Rebuild the flattened partition ID cache from the `DashMap`.
    async fn rebuild_partition_id_cache(&self) {
        let mut ids: Vec<i64> = self
            .partitions
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        *self.all_partition_ids.write().await = ids;
    }

    /// Access the outbox config.
    #[must_use]
    pub fn config(&self) -> &OutboxConfig {
        &self.config
    }

    /// Returns the partition IDs for a specific queue, in order.
    #[must_use]
    pub(crate) fn partition_ids_for_queue(&self, queue: &str) -> Vec<i64> {
        self.partitions
            .get(queue)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Look up the queue name for a partition ID.
    #[must_use]
    pub fn partition_to_queue(&self, partition_id: i64) -> Option<String> {
        self.partition_to_queue
            .get(&partition_id)
            .map(|v| v.clone())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::outbox::types::*;

    fn make_outbox(config: OutboxConfig) -> Arc<Outbox> {
        Arc::new(Outbox::new(config))
    }

    fn make_default_outbox() -> Arc<Outbox> {
        make_outbox(OutboxConfig::default())
    }

    // -- resolve_partition tests --

    #[test]
    fn resolve_partition_cache_hit() {
        let outbox = make_default_outbox();
        outbox
            .partitions
            .insert("orders".to_owned(), vec![10, 20, 30]);

        assert_eq!(outbox.resolve_partition("orders", 0).unwrap(), 10);
        assert_eq!(outbox.resolve_partition("orders", 1).unwrap(), 20);
        assert_eq!(outbox.resolve_partition("orders", 2).unwrap(), 30);
    }

    #[test]
    fn resolve_partition_unregistered_queue() {
        let outbox = make_default_outbox();

        let err = outbox.resolve_partition("nonexistent", 0).unwrap_err();
        assert!(matches!(err, OutboxError::QueueNotRegistered(q) if q == "nonexistent"));
    }

    #[test]
    fn resolve_partition_out_of_range() {
        let outbox = make_default_outbox();
        outbox
            .partitions
            .insert("orders".to_owned(), vec![10, 20, 30]);

        let err = outbox.resolve_partition("orders", 3).unwrap_err();
        assert!(matches!(
            err,
            OutboxError::PartitionOutOfRange { queue, partition: 3, max: 3 } if queue == "orders"
        ));
    }

    // -- validate_payload tests --

    #[test]
    fn validate_payload_oversized() {
        let oversized = vec![0u8; MAX_PAYLOAD_SIZE + 1];
        let err = Outbox::validate_payload(&oversized).unwrap_err();
        assert!(matches!(err, OutboxError::PayloadTooLarge { .. }));
    }

    #[test]
    fn validate_payload_at_exact_limit() {
        let exact = vec![0u8; MAX_PAYLOAD_SIZE];
        assert!(Outbox::validate_payload(&exact).is_ok());
    }

    #[test]
    fn validate_payload_empty() {
        assert!(Outbox::validate_payload(&[]).is_ok());
    }

    // -- enqueue_batch validation tests (no DB needed) --

    #[tokio::test]
    async fn enqueue_batch_rejects_out_of_range_partition() {
        let outbox = make_default_outbox();
        outbox.partitions.insert("q".to_owned(), vec![10, 20]);

        let err = outbox.resolve_partition("q", 5).unwrap_err();
        assert!(matches!(err, OutboxError::PartitionOutOfRange { .. }));
    }

    #[tokio::test]
    async fn enqueue_batch_rejects_oversized_payload() {
        let oversized = vec![0u8; MAX_PAYLOAD_SIZE + 1];
        let err = Outbox::validate_payload(&oversized).unwrap_err();
        assert!(matches!(err, OutboxError::PayloadTooLarge { .. }));
    }

    // -- flush tests --

    #[tokio::test]
    async fn flush_triggers_notify() {
        use crate::outbox::prioritizer::SharedPrioritizer;
        let prioritizer = Arc::new(SharedPrioritizer::new());
        let notifier = prioritizer.notifier();
        let outbox = Arc::new(Outbox::new(OutboxConfig::default()));
        outbox.set_prioritizer(Arc::clone(&prioritizer)).await;

        outbox.flush();
        // Notify was signaled via prioritizer — notified() resolves immediately
        tokio::time::timeout(std::time::Duration::from_millis(50), notifier.notified())
            .await
            .expect("notify should fire");
    }

    #[tokio::test]
    async fn flush_before_prioritizer_is_noop() {
        let outbox = Arc::new(Outbox::new(OutboxConfig::default()));
        // flush() before set_prioritizer() — should not panic
        outbox.flush();
        outbox.flush();
    }

    #[tokio::test]
    async fn flush_does_not_block() {
        use crate::outbox::prioritizer::SharedPrioritizer;
        let prioritizer = Arc::new(SharedPrioritizer::new());
        let outbox = Arc::new(Outbox::new(OutboxConfig::default()));
        outbox.set_prioritizer(prioritizer).await;
        // Multiple flushes should not block or panic
        outbox.flush();
        outbox.flush();
        outbox.flush();
    }

    // -- config defaults test --

    #[test]
    fn config_defaults_match_constants() {
        let config = OutboxConfig::default();
        assert_eq!(config.sequencer.batch_size, DEFAULT_SEQUENCER_BATCH_SIZE);
        assert_eq!(config.sequencer.poll_interval, DEFAULT_POLL_INTERVAL);
    }
}
