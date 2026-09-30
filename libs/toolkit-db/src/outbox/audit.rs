//! Keeping the channel counters honest.
//!
//! Each channel's depth is the difference between its own two counters, so
//! anything that removes rows by a route the pipeline does not know about - an
//! operator with a SQL prompt, a botched migration, a crash that lost an
//! unpublished journal - leaves that difference permanently wrong. A bounded
//! queue would then refuse work forever with no way back, so the difference has
//! to converge on the truth rather than be trusted.
//!
//! Two things keep the correction cheap.
//!
//! **It only runs where something depends on it.** A queue with no configured
//! bound has a reading that gates nothing, so drift there is harmless.
//!
//! **It checks each channel where the answer is free.** A channel that holds no
//! rows has a depth of provably zero, so the check is one index probe and no
//! counting at all. Because the two channels are counted separately, each can
//! be proved empty on its own, and a correction says which side drifted.
//!
//! There is deliberately no exhaustive recount. Comparing the counters against
//! a row count is unsound while any instance holds an unpublished journal: the
//! rows already reflect work whose counter delta has not been written yet, so a
//! row-derived correction cancels a delta that is still coming and the channel
//! lands off by twice it. Publishing this instance's journal first closes only
//! the local half - another instance's journal can be neither read nor flushed
//! from here. A channel with no rows is the one state where the two sides
//! cannot disagree, which is why that is the only evidence acted on.
//!
//! The consequence, stated plainly: a partition that never empties keeps its
//! drift until it does. There is no correct way to repair it while it is busy.
//!
//! Every correction is a delta, never an assignment. A counter with more than
//! one writer is always racing somebody: an assignment discards whatever
//! committed between the read that decided the value and the write that
//! installs it, whereas a delta composes with it and the next pass finds a
//! smaller remaining difference.

use sea_orm::{ConnectionTrait, FromQueryResult, Statement};

use super::admission::Channels;
use super::core::Outbox;
use super::limits::Volume;
use super::store::OutboxStore;
use super::types::OutboxError;
use crate::Db;

#[derive(Debug, FromQueryResult)]
struct CountRow {
    entities: i64,
    bytes: i64,
}

impl Outbox {
    /// Correct one partition's channel counters, for whichever of its two
    /// channels is provably empty.
    ///
    /// Returns whether a correction was applied.
    ///
    /// # Errors
    ///
    /// Returns an error if a statement fails.
    pub(crate) async fn audit_partition(
        &self,
        db: &Db,
        partition_id: i64,
    ) -> Result<bool, OutboxError> {
        // Only a bounded queue has a reading that gates anything, so only a
        // bounded queue is worth a statement.
        let Some(queue) = self.queue_of_partition(partition_id) else {
            return Ok(false);
        };
        if !self.is_bounded(&queue) {
            return Ok(false);
        }

        // Publish first. A repair compares the table against the rows, so
        // this instance's own unpublished work must be in the table or the
        // difference it measures is merely a flush that has not happened yet.
        self.publish_journal(db, partition_id).await?;

        let store = OutboxStore::new(self.statements());
        let conn = db.sea_internal();
        let runner = crate::secure::SeaOrmRunner::Conn(&conn);

        let incoming_empty = !self
            .any_row(&conn, store.any_incoming(), partition_id)
            .await?;
        let outgoing_empty = !self
            .any_row(&conn, store.any_outgoing_past_cursor(), partition_id)
            .await?;
        if !incoming_empty && !outgoing_empty {
            return Ok(false);
        }

        // Read after the probes, so a channel the probe found empty cannot be
        // corrected by more than it actually holds: work arriving in between
        // is in the reading and not in the correction, which leaves the depth
        // overstated for one pass rather than understated.
        let claimed = self.read_partition_channels(&runner, partition_id).await?;
        let mut correction = Channels::ZERO;
        if incoming_empty {
            correction.incoming_out = claimed.incoming_depth();
        }
        if outgoing_empty {
            correction.outgoing_out = claimed.outgoing_depth();
        }
        if correction.is_zero() {
            return Ok(false);
        }

        self.apply_channel_deltas(db, partition_id, correction)
            .await?;
        tracing::warn!(
            partition_id,
            queue = %queue,
            incoming_entities = correction.incoming_out.entities,
            outgoing_entities = correction.outgoing_out.entities,
            "outbox channel counters disagreed with an empty channel and were corrected",
        );
        self.observe_channels(partition_id, claimed + correction);
        Ok(true)
    }

    /// Load every bounded queue's saved counters into its admission state.
    ///
    /// Called once at start, before the pipeline can accept work. A failure is
    /// logged and not fatal: the flush worker takes a reading on its first
    /// tick, so the only cost is that the bound is briefly blind rather than
    /// the outbox refusing to start.
    pub(crate) async fn restore_admission(&self, db: &Db) {
        let conn = db.sea_internal();
        let runner = crate::secure::SeaOrmRunner::Conn(&conn);
        for (queue, admission) in self.bounded_queues() {
            let Some(ids) = self.queue_partition_ids(&queue) else {
                continue;
            };
            match self.read_queue_channels(&runner, &queue).await {
                Ok(rows) => {
                    for (partition_id, channels) in rows {
                        if let Some(partition) = ids.iter().position(|&id| id == partition_id) {
                            admission.observe(partition, channels);
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    queue = %queue,
                    error = %e,
                    "could not read this queue's saved counters at start; the bound is blind until the first refresh",
                ),
            }
        }
    }

    /// Write this instance's unpublished work for one partition, and mark it
    /// published.
    ///
    /// The flusher's unit of work, and the first thing every repair path does.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement fails. Nothing is marked published in
    /// that case, so the next attempt recomputes the identical delta.
    pub(crate) async fn publish_journal(
        &self,
        db: &Db,
        partition_id: i64,
    ) -> Result<(), OutboxError> {
        let Some((queue, partition)) = self.locate_partition(partition_id) else {
            return Ok(());
        };
        let Some(admission) = self.admission_of(&queue) else {
            return Ok(());
        };
        let delta = admission.unpublished_at(partition);
        if delta.is_zero() {
            return Ok(());
        }
        self.apply_channel_deltas(db, partition_id, delta).await?;
        admission.published(partition, delta);
        Ok(())
    }

    /// Add `deltas` to a partition's channel counters.
    ///
    /// The single statement that writes a channel counter, shared by the
    /// flusher and both repair paths.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement fails.
    pub(crate) async fn apply_channel_deltas(
        &self,
        db: &Db,
        partition_id: i64,
        deltas: Channels,
    ) -> Result<(), OutboxError> {
        let store = OutboxStore::new(self.statements());
        let conn = db.sea_internal();
        conn.execute_raw(Statement::from_sql_and_values(
            store.backend(),
            store.apply_channel_deltas(),
            [
                deltas.incoming_in.entities.into(),
                deltas.incoming_in.bytes.into(),
                deltas.incoming_out.entities.into(),
                deltas.incoming_out.bytes.into(),
                deltas.outgoing_in.entities.into(),
                deltas.outgoing_in.bytes.into(),
                deltas.outgoing_out.entities.into(),
                deltas.outgoing_out.bytes.into(),
                partition_id.into(),
            ],
        ))
        .await?;
        Ok(())
    }

    /// Count one partition's rows in both channels.
    ///
    /// The only way to answer for a queue nobody counts, which is how
    /// `depth` reports an unbounded queue.
    ///
    /// # Errors
    ///
    /// Returns an error if a statement fails.
    pub(crate) async fn count_partition_rows(
        &self,
        runner: &crate::secure::SeaOrmRunner<'_>,
        partition_id: i64,
    ) -> Result<Volume, OutboxError> {
        let store = OutboxStore::new(self.statements());
        let conn = runner.executor();
        let incoming = Self::count_one(&conn, store.count_incoming(), partition_id).await?;
        let outgoing =
            Self::count_one(&conn, store.count_outgoing_past_cursor(), partition_id).await?;
        Ok(incoming + outgoing)
    }

    async fn count_one(
        conn: &sea_orm::DatabaseExecutor<'_>,
        sql: &str,
        partition_id: i64,
    ) -> Result<Volume, OutboxError> {
        let row = CountRow::find_by_statement(Statement::from_sql_and_values(
            conn.get_database_backend(),
            sql,
            [partition_id.into()],
        ))
        .one(conn)
        .await?;
        Ok(row.map_or(Volume::ZERO, |row| Volume {
            entities: row.entities,
            bytes: row.bytes,
        }))
    }

    async fn any_row(
        &self,
        conn: &sea_orm::DatabaseConnection,
        sql: &str,
        partition_id: i64,
    ) -> Result<bool, OutboxError> {
        Ok(conn
            .query_one_raw(Statement::from_sql_and_values(
                self.statements().backend(),
                sql,
                [partition_id.into()],
            ))
            .await?
            .is_some())
    }
}
