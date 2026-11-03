//! The rescan (`docs/DESIGN.md` §4.9): on a timer, reads every partition and
//! segment row, replaces their registry entries, and offers the maintenance
//! every partition that needs it. It catches what the in-memory checks miss:
//! a restart, another instance's appends, an age bound passing with no
//! append, a run cut off by a crash. Every `crosscheck_every` passes, the
//! first included, it offers every partition, so each run compares its rows
//! with the server's partitions.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::MySqlPool;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};
use uuid::Uuid;

use super::catalog::{self, Segment, SegmentState};
use super::hub::Layout;
use crate::heads::Key;
use crate::shared::Shared;
use crate::{error, names, segment};

/// The periodic pass that reloads the registry from the database.
pub struct Rescan {
    shared: Arc<Shared>,
    pool: MySqlPool,
    passes: u32,
}

/// One partition, as the rescan reads it.
#[derive(sqlx::FromRow)]
struct PartitionRow {
    topic_uuid: Uuid,
    partition_no: i32,
    hwm: i64,
    bytes: i64,
    removed_bytes: i64,
    max_bytes: Option<i64>,
    /// `now - max_age`: a sealed segment whose newest event is older is
    /// due; `None` with no age bound.
    aged_before: Option<DateTime<Utc>>,
}

impl Rescan {
    /// A rescan over `pool`, before its first pass.
    pub fn new(shared: Arc<Shared>, pool: MySqlPool) -> Self {
        Self {
            shared,
            pool,
            passes: 0,
        }
    }

    async fn pass(&mut self) -> Result<(), sqlx::Error> {
        let everything = self
            .passes
            .is_multiple_of(self.shared.settings.maintenance.crosscheck_every);
        self.passes = self.passes.wrapping_add(1);

        let mut conn = self.pool.acquire().await?;
        let partitions: Vec<PartitionRow> = sqlx::query_as(
            "SELECT p.topic_uuid, p.partition_no, p.hwm, p.bytes, r.removed_bytes, l.max_bytes, \
             UTC_TIMESTAMP(6) - INTERVAL l.max_age_ms * 1000 MICROSECOND AS aged_before \
             FROM evbk__partition p \
             JOIN evbk__retention r ON r.topic_uuid = p.topic_uuid AND r.partition_no = p.partition_no \
             JOIN evbk__log l ON l.topic_uuid = p.topic_uuid",
        )
        .fetch_all(&mut *conn)
        .await?;
        let rows = catalog::all_segments(&mut conn).await?;
        drop(conn);

        let mut segments: HashMap<Key, Vec<Segment>> = HashMap::new();
        for (key, segment) in rows {
            segments.entry(key).or_default().push(segment);
        }
        let hub = &self.shared.maintenance;
        let mut layouts = HashMap::with_capacity(partitions.len());
        let mut offers = Vec::new();
        for row in partitions {
            let Some(partition) = names::partition_from_column(row.partition_no) else {
                continue;
            };
            let key = (row.topic_uuid, partition);
            let of_key = segments.get(&key).map_or(&[][..], Vec::as_slice);
            let layout = Layout::new(of_key);
            if everything || due(&row, of_key, &layout) {
                offers.push(key);
            }
            layouts.insert(key, layout);
        }
        // The registry first: a run offered below then replaces its entry
        // with what it reads later, never the other way round.
        hub.replace_all(layouts);
        for key in offers {
            hub.queue.signal(key);
        }
        Ok(())
    }
}

/// Whether a partition needs a run: room, a seal, a bound passed, or a row
/// a DDL left behind.
fn due(row: &PartitionRow, segments: &[Segment], layout: &Layout) -> bool {
    let live: Vec<(i64, i64)> = segments
        .iter()
        .filter(|s| s.is_live())
        .map(|s| (s.first, s.end))
        .collect();
    let oldest_sealed = segments.iter().find(|s| s.state == SegmentState::Sealed);
    let aged = row.aged_before.is_some_and(|before| {
        oldest_sealed.is_some_and(|oldest| oldest.newest_at.is_none_or(|n| n < before))
    });
    let over = row.max_bytes.is_some_and(|max| {
        oldest_sealed.is_some_and(|oldest| row.bytes - row.removed_bytes - oldest.bytes >= max)
    });
    let unsettled = segments
        .iter()
        .any(|s| matches!(s.state, SegmentState::Adding | SegmentState::Expired));
    segment::needs_room(row.hwm, &live) || layout.is_due(row.hwm) || aged || over || unsettled
}

impl WorkerAction for Rescan {
    type Payload = ();
    type Error = String;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, String> {
        self.pass()
            .await
            .map_err(|e| format!("rescan: {}", error::kind(&e)))?;
        Ok(Directive::idle())
    }
}
