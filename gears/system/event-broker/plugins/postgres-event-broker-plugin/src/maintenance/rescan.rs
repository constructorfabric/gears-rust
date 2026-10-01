//! The rescan (`docs/DESIGN.md` §4.9): on a timer, reads every partition in
//! one snapshot, replaces the registry, and offers each job the partitions
//! that need it. It catches what the in-memory checks miss: a restart,
//! another instance's appends, a lost offer. Every `crosscheck_every`
//! passes, the first included, it also cross-checks the rows with the
//! catalog.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};
use uuid::Uuid;

use super::catalog::{self, SegmentState};
use super::crosscheck::{self, Finding, Reaction, Scope, Stats};
use super::prioritizer::Key;
use super::registry::{Layout, Urgency};
use super::retention::Overdue;
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
            "SELECT p.topic_uuid, p.partition, p.hwm, r.floor, \
             l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL, \
             GREATEST(COALESCE(p.appended_bytes - r.removed_bytes - l.max_bytes, 0), 0) \
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
        for (topic, partition, hwm, floor, bounded, overdue) in partitions {
            let Some(partition) = names::partition_from_column(partition) else {
                continue;
            };
            let key = (topic, partition);
            let of_key = segments.get(&key).map_or(&[][..], Vec::as_slice);
            let layout = Layout::new(of_key, bounded);
            offers.push(Offers {
                key,
                extend: segment::needs_room(hwm, &layout.ranges).then(|| layout.urgency(hwm)),
                count: of_key
                    .iter()
                    .any(|s| s.state == SegmentState::Open && s.counted_to < hwm),
                retain: bounded.then_some(Overdue(overdue)),
                detach: of_key
                    .iter()
                    .any(|s| matches!(s.state, SegmentState::Expired | SegmentState::Detaching)),
            });
            layouts.insert(key, layout);
            stats.insert(key, Stats { hwm, floor });
        }
        // The registry first: a job offered below then replaces its entry
        // with what it reads later, never the other way round.
        hub.registry.replace_all(layouts);
        for offer in offers {
            if let Some(urgency) = offer.extend {
                hub.extend.offer(offer.key, urgency);
            }
            if offer.count {
                hub.count.offer(offer.key, ());
            }
            if let Some(overdue) = offer.retain {
                hub.retain.offer(offer.key, overdue);
            }
            if offer.detach {
                hub.detach.offer(offer.key, ());
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

/// `(topic, partition, hwm, floor, bounded, overdue)`.
type PartitionRow = (Uuid, i32, i64, i64, bool, i64);

/// What one partition is offered to.
struct Offers {
    key: Key,
    extend: Option<Urgency>,
    count: bool,
    retain: Option<Overdue>,
    detach: bool,
}

impl WorkerAction for Rescan {
    type Payload = ();
    type Error = sqlx::Error;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, sqlx::Error> {
        self.pass().await?;
        Ok(Directive::idle())
    }
}
