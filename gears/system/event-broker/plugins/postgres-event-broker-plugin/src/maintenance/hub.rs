//! What the operations and the jobs of one instance share: the registry and
//! every job's queue.

use super::prioritizer::{Key, Policy, Prioritizer};
use super::registry::{Check, Registry, Urgency};
use super::retention::Overdue;
use super::{counter, detacher, extender, reconcile, retention};
use crate::settings::MaintenanceSettings;

/// The maintenance state of one instance.
pub struct Hub {
    /// What this instance knows of each partition's segments.
    pub registry: Registry,
    /// Partitions that need segments.
    pub extend: Prioritizer<Urgency>,
    /// Partitions whose open segments are due a count, first offered first.
    pub count: Prioritizer<()>,
    /// Partitions whose bound may let segments go, most overdue first.
    pub retain: Prioritizer<Overdue>,
    /// Partitions with expired segments to detach, first offered first.
    pub detach: Prioritizer<()>,
    /// Partitions the cross-check found drift on, first offered first.
    pub reconcile: Prioritizer<()>,
}

impl Hub {
    /// Empty queues, postponing per `settings`.
    pub fn new(settings: &MaintenanceSettings) -> Self {
        let policy = Policy {
            warn_after: settings.warn_after,
            stuck_after: settings.stuck_after,
            stuck_age: settings.stuck_age,
            ..Policy::default()
        };
        Self {
            registry: Registry::default(),
            extend: Prioritizer::new(extender::NAME, policy),
            count: Prioritizer::new(counter::NAME, policy),
            retain: Prioritizer::new(retention::NAME, policy),
            detach: Prioritizer::new(detacher::NAME, policy),
            reconcile: Prioritizer::new(reconcile::NAME, policy),
        }
    }

    /// After an append to `key` committed up to `head`.
    pub fn appended(&self, key: Key, head: i64) {
        match self.registry.check(key, head) {
            Check::Fine => {}
            Check::Room(urgency) => self.extend.offer(key, urgency),
            Check::Count => self.count.offer(key, ()),
            Check::RoomAndCount(urgency) => {
                self.extend.offer(key, urgency);
                self.count.offer(key, ());
            }
            Check::Unknown => self.extend.offer(key, Urgency::Urgent),
        }
    }

    /// After an append to `key` found no segment for a sequence.
    pub fn no_room(&self, key: Key) {
        self.extend.offer(key, Urgency::Urgent);
    }
}
