//! The rescan (`docs/DESIGN.md` §4.9): on a timer, reads every partition in
//! one snapshot, replaces their registry entries, and offers each job the partitions
//! that need it. It catches what the in-memory checks miss: a restart,
//! another instance's appends, a lost offer. Every `crosscheck_every`
//! passes, the first included, it also cross-checks the rows with the
//! catalog.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};
use uuid::Uuid;

use super::catalog::{self, SegmentState};
use super::crosscheck::{self, Finding, Reaction, Scope, Stats};
use super::prioritizer::Key;
use super::registry::{Layout, Urgency};
use crate::shared::Shared;
use crate::{names, segment};

/// The rescan of one instance; one worker, woken by a timer.
pub struct Rescan {
    shared: Arc<Shared>,
    pool: PgPool,
    passes: u32,
    /// What the last cross-check found.
    seen: HashSet<Finding>,
    /// What two cross-checks in a row found, and was reported.
    reported: HashSet<Finding>,
}

impl Rescan {
    /// The rescan of one instance.
    pub fn new(shared: Arc<Shared>, pool: PgPool) -> Self {
        Self {
            shared,
            pool,
            passes: 0,
            seen: HashSet::new(),
            reported: HashSet::new(),
        }
    }

    async fn pass(&mut self) -> Result<(), sqlx::Error> {
        let crosscheck = self
            .passes
            .is_multiple_of(self.shared.settings.maintenance.crosscheck_every);
        self.passes = self.passes.wrapping_add(1);

        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let partitions: Vec<PartitionRow> = sqlx::query_as(
            "SELECT p.topic_uuid, p.partition, p.hwm, r.floor, l.max_bytes IS NOT NULL AS by_bytes, \
             now() - l.max_age AS aged_before \
             FROM evbk__partition p JOIN evbk__log l USING (topic_uuid) \
             JOIN evbk__retention r USING (topic_uuid, partition)",
        )
        .fetch_all(&mut *tx)
        .await?;
        let rows = catalog::all_segments(&mut tx).await?;
        let checked = if crosscheck {
            let rows = crosscheck::rows(&mut tx, Scope::All).await?;
            let relations = crosscheck::relations(&mut tx, Scope::All).await?;
            Some((rows, relations))
        } else {
            None
        };
        tx.commit().await?;

        let mut segments: HashMap<Key, Vec<catalog::Segment>> = HashMap::new();
        for (key, segment) in rows {
            segments.entry(key).or_default().push(segment);
        }
        let hub = &self.shared.maintenance;
        let mut layouts = HashMap::with_capacity(partitions.len());
        let mut stats = HashMap::with_capacity(partitions.len());
        let mut offers = Vec::new();
        for PartitionRow {
            topic_uuid,
            partition,
            hwm,
            floor,
            by_bytes,
            aged_before,
        } in partitions
        {
            let Some(partition) = names::partition_from_column(partition) else {
                continue;
            };
            let key = (topic_uuid, partition);
            let of_key = segments.get(&key).map_or(&[][..], Vec::as_slice);
            let layout = Layout::new(of_key);
            // Retention: a byte bound is measured by retention itself, so
            // every pass while a sealed segment could go; an age bound when
            // the oldest sealed segment's newest event is older than
            // `max_age`, or it has none left.
            let oldest_sealed = of_key.iter().find(|s| s.state == SegmentState::Sealed);
            let aged = aged_before.is_some_and(|before| {
                oldest_sealed.is_some_and(|oldest| oldest.newest_at.is_none_or(|n| n < before))
            });
            if segment::needs_room(hwm, &layout.ranges) {
                offers.push((key, Offer::Extend(layout.urgency(hwm))));
            }
            if layout.due(hwm).seal {
                offers.push((key, Offer::Seal));
            }
            if aged || (by_bytes && oldest_sealed.is_some()) {
                offers.push((key, Offer::Retain));
            }
            if of_key
                .iter()
                .any(|s| matches!(s.state, SegmentState::Expired | SegmentState::Detaching))
            {
                offers.push((key, Offer::Detach));
            }
            layouts.insert(key, layout);
            stats.insert(key, Stats { hwm, floor });
        }
        // The registry first: a job offered below then replaces its entry
        // with what it reads later, never the other way round.
        hub.registry.replace_all(layouts);
        for (key, offer) in offers {
            match offer {
                Offer::Extend(urgency) => hub.extend.offer(key, urgency),
                Offer::Seal => hub.seal.offer(key, ()),
                Offer::Retain => hub.retain.offer(key, ()),
                Offer::Detach => hub.detach.offer(key, ()),
            }
        }
        if let Some((rows, relations)) = checked {
            self.react(crosscheck::compare(&rows, &relations, &stats));
        }
        Ok(())
    }

    /// Acts on what two cross-checks in a row found, and reports what is new
    /// and what has cleared.
    fn react(&mut self, findings: Vec<Finding>) {
        let hub = &self.shared.maintenance;
        let current: HashSet<Finding> = findings.into_iter().collect();
        let confirmed: HashSet<Finding> = current.intersection(&self.seen).cloned().collect();
        for finding in &confirmed {
            let Finding {
                key: (topic, partition),
                case,
                segment,
            } = finding;
            if !self.reported.contains(finding) {
                tracing::error!(%topic, %partition, ?case, segment, "event-broker maintenance: segment rows and catalog disagree");
            }
            match case.reaction() {
                Reaction::Detach => hub.detach.offer(finding.key, ()),
                Reaction::Reconcile => hub.reconcile.offer(finding.key, ()),
                Reaction::Report => {}
            }
        }
        for Finding {
            key: (topic, partition),
            case,
            segment,
        } in self.reported.difference(&confirmed)
        {
            tracing::info!(%topic, %partition, ?case, segment, "event-broker maintenance: segment rows and catalog agree again");
        }
        self.reported = confirmed;
        self.seen = current;
    }
}

/// One partition, as the rescan's snapshot reads it.
#[derive(sqlx::FromRow)]
struct PartitionRow {
    topic_uuid: Uuid,
    partition: i32,
    hwm: i64,
    floor: i64,
    /// Whether the topic has a byte bound.
    by_bytes: bool,
    /// `now() - max_age`: a sealed segment whose newest event is older is
    /// due; `None` with no age bound.
    aged_before: Option<DateTime<Utc>>,
}

/// A job a partition is offered to.
enum Offer {
    Extend(Urgency),
    Seal,
    Retain,
    Detach,
}

impl WorkerAction for Rescan {
    type Payload = ();
    type Error = sqlx::Error;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, sqlx::Error> {
        self.pass().await?;
        Ok(Directive::idle())
    }
}
