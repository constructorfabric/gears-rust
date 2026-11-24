//! Applies [`DirectoryBackend`](crate::directory_backend::DirectoryBackend)
//! records to the local [`GearManager`] read model in backend version order.

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use toolkit::runtime::{GearManager, GrpcServiceNameConflict};
use uuid::Uuid;

use crate::directory_backend::DirectoryRecord;

type Key = (String, Uuid);

/// The backend version last applied for one instance, or the tombstone of
/// its removed generation.
#[derive(Clone, Copy, Debug)]
struct Version {
    generation: u64,
    revision: u64,
    /// Tombstone: every record at or below `generation` is dropped.
    removed: bool,
    /// When `generation` was removed or expired locally; write-through
    /// results issued earlier are dropped.
    fenced_at: Option<Instant>,
    /// When a write-through or feed record was applied (`None` from a
    /// snapshot); a snapshot scan started earlier does not override it.
    written_at: Option<Instant>,
    /// Last change, for eviction.
    updated_at: Instant,
}

impl Version {
    fn applied(record: &DirectoryRecord, written_at: Option<Instant>, now: Instant) -> Self {
        Self {
            generation: record.generation,
            revision: record.revision,
            removed: false,
            fenced_at: None,
            written_at,
            updated_at: now,
        }
    }

    /// Whether `record` is older than this version or belongs to a removed
    /// generation. Revisions are compared only within a generation.
    fn supersedes(&self, record: &DirectoryRecord) -> bool {
        record.generation < self.generation
            || (record.generation == self.generation
                && (self.removed || record.revision < self.revision))
    }

    fn tombstone(generation: u64, now: Instant) -> Self {
        Self {
            generation,
            revision: 0,
            removed: true,
            fenced_at: Some(now),
            written_at: None,
            updated_at: now,
        }
    }
}

/// The [`GearManager`] read model plus the backend versions that order the
/// write-through results, feed changes and snapshots applied to it. Each
/// update compares versions and touches the manager under one lock, so a
/// delayed or duplicated record never overwrites newer state.
///
/// Tombstones are evicted after `ttl + grace` untouched: any record they could
/// still fence has a lease lapsed past grace, which
/// [`GearManager::upsert_record`] stores as unobservable.
pub struct DirectoryReplica {
    manager: Arc<GearManager>,
    versions: Mutex<HashMap<Key, Version>>,
}

impl DirectoryReplica {
    pub fn new(manager: Arc<GearManager>) -> Self {
        Self {
            manager,
            versions: Mutex::new(HashMap::new()),
        }
    }

    pub fn manager(&self) -> &Arc<GearManager> {
        &self.manager
    }

    /// Apply the record returned by this replica's own write through the
    /// backend, issued at `started`. Dropped if a newer version is held or the
    /// generation was removed or expired after `started`; the feed delivers
    /// the outcome either way.
    ///
    /// # Errors
    /// Returns [`GrpcServiceNameConflict`] if the record advertises a gRPC
    /// service name pinned to a different gear; nothing is applied.
    pub fn apply_write(
        &self,
        record: DirectoryRecord,
        started: Instant,
    ) -> Result<(), GrpcServiceNameConflict> {
        self.apply(record, Some(started))
    }

    /// Apply a record delivered by the backend change feed. Dropped if a
    /// newer version is held or its generation was removed.
    ///
    /// # Errors
    /// Returns [`GrpcServiceNameConflict`] if the record advertises a gRPC
    /// service name pinned to a different gear; nothing is applied.
    pub fn apply_change(&self, record: DirectoryRecord) -> Result<(), GrpcServiceNameConflict> {
        self.apply(record, None)
    }

    fn apply(
        &self,
        record: DirectoryRecord,
        started: Option<Instant>,
    ) -> Result<(), GrpcServiceNameConflict> {
        let key = (record.instance.gear.clone(), record.instance.instance_id);
        let mut versions = self.versions.lock();
        if let Some(held) = versions.get(&key) {
            if held.supersedes(&record) {
                return Ok(());
            }
            if let (Some(started), Some(fenced_at)) = (started, held.fenced_at)
                && record.generation == held.generation
                && started < fenced_at
            {
                return Ok(());
            }
        }
        let now = Instant::now();
        let version = Version::applied(&record, Some(started.unwrap_or(now)), now);
        self.manager
            .upsert_record(record.instance, record.lease_expires_at)?;
        versions.insert(key, version);
        Ok(())
    }

    /// Remove `generation` of an instance, as reported by the feed or by this
    /// replica's own deregistration. A delayed removal of an older generation
    /// leaves a newer registration in place.
    pub fn apply_removal(&self, gear: &str, instance_id: Uuid, generation: u64) {
        let key = (gear.to_owned(), instance_id);
        let mut versions = self.versions.lock();
        let applied = match versions.get(&key) {
            Some(held) if held.generation > generation => return,
            Some(held) => !held.removed,
            None => false,
        };
        versions.insert(key, Version::tombstone(generation, Instant::now()));
        if applied {
            self.manager.deregister(gear, instance_id);
        }
    }

    /// Replace the read model with a backend snapshot whose scan started at
    /// `started`.
    ///
    /// A snapshot record is skipped when the replica holds a newer version or
    /// applied a write-through or feed record after the scan began, since the
    /// scan may predate that write. A local instance missing from the
    /// snapshot is removed and tombstoned unless it was written after the
    /// scan began. Records that violate pinned gRPC service ownership are
    /// skipped and treated as missing.
    pub fn replace_snapshot(&self, records: Vec<DirectoryRecord>, started: Instant) {
        let mut versions = self.versions.lock();
        let written_since_scan = |held: &Version| held.written_at.is_some_and(|at| at >= started);

        let mut accepted = Vec::with_capacity(records.len());
        let mut retained: HashSet<Key> = HashSet::new();
        for record in records {
            let key = (record.instance.gear.clone(), record.instance.instance_id);
            match versions.get(&key) {
                Some(held) if held.supersedes(&record) || written_since_scan(held) => {
                    retained.insert(key);
                }
                _ => accepted.push(record),
            }
        }
        for (key, held) in versions.iter() {
            if written_since_scan(held) {
                retained.insert(key.clone());
            }
        }

        let rejected: HashSet<Key> = self
            .manager
            .replace_all(
                accepted
                    .iter()
                    .map(|record| (Arc::clone(&record.instance), record.lease_expires_at))
                    .collect(),
                |gear, instance_id| retained.contains(&(gear.to_owned(), instance_id)),
            )
            .into_iter()
            .collect();

        let now = Instant::now();
        let mut loaded = HashSet::with_capacity(accepted.len());
        for record in &accepted {
            let key = (record.instance.gear.clone(), record.instance.instance_id);
            if !rejected.contains(&key) {
                versions.insert(key.clone(), Version::applied(record, None, now));
                loaded.insert(key);
            }
        }
        for (key, held) in versions.iter_mut() {
            if !held.removed && !loaded.contains(key) && !retained.contains(key) {
                *held = Version::tombstone(held.generation, now);
            }
        }
    }

    /// Garbage-collect the read model and the version map: sweep records
    /// whose lease lapsed past grace, fence their generations against late
    /// write-through results, and evict entries untouched for `ttl + grace`.
    pub fn sweep(&self, now: Instant) {
        let mut versions = self.versions.lock();
        for key in self.manager.sweep_expired(now) {
            if let Some(held) = versions.get_mut(&key)
                && !held.removed
            {
                held.fenced_at = Some(now);
                held.updated_at = now;
            }
        }
        let retention = self.manager.lease_ttl() + self.manager.lease_grace();
        versions.retain(|_, held| now.saturating_duration_since(held.updated_at) <= retention);
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.versions.lock().len()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};
    use toolkit::runtime::{Endpoint, GearInstance, InstanceState, ReportedState};

    fn record(gear: &str, id: Uuid, reported: ReportedState, revision: u64) -> DirectoryRecord {
        record_generation(gear, id, reported, revision, 1)
    }

    fn record_generation(
        gear: &str,
        id: Uuid,
        reported: ReportedState,
        revision: u64,
        generation: u64,
    ) -> DirectoryRecord {
        DirectoryRecord {
            instance: Arc::new(
                GearInstance::new(gear, id)
                    .with_rest_endpoint(Endpoint::http("127.0.0.1", 8080))
                    .with_reported_state(reported),
            ),
            generation,
            revision,
            lease_expires_at: SystemTime::now() + Duration::from_secs(60),
        }
    }

    fn replica() -> DirectoryReplica {
        DirectoryReplica::new(Arc::new(GearManager::new()))
    }

    fn state_of(replica: &DirectoryReplica, gear: &str) -> Option<InstanceState> {
        replica
            .manager()
            .instances_of(gear)
            .first()
            .map(|i| i.state())
    }

    /// Sweep with the clock moved past every lease and tombstone retention.
    fn sweep_after_lapse(replica: &DirectoryReplica) {
        replica.sweep(Instant::now() + Duration::from_secs(3600));
    }

    #[test]
    fn upsert_older_than_held_revision_is_dropped() {
        let replica = replica();
        let id = Uuid::new_v4();
        replica
            .apply_write(
                record("gear", id, ReportedState::Draining, 2),
                Instant::now(),
            )
            .unwrap();

        // The feed delivers the earlier write after this replica applied its own.
        replica
            .apply_change(record("gear", id, ReportedState::Serving, 1))
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Draining));

        replica
            .apply_change(record("gear", id, ReportedState::Serving, 3))
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Healthy));
    }

    #[test]
    fn write_racing_a_removal_is_dropped() {
        let replica = replica();
        let id = Uuid::new_v4();
        replica
            .apply_change(record("gear", id, ReportedState::Starting, 1))
            .unwrap();

        let started = Instant::now();
        replica.apply_removal("gear", id, 1);
        replica
            .apply_write(record("gear", id, ReportedState::Serving, 2), started)
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), None);

        // A write issued after the removal (re-registration) applies at once.
        replica
            .apply_write(
                record_generation("gear", id, ReportedState::Starting, 1, 2),
                Instant::now(),
            )
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Registered));
    }

    #[test]
    fn stale_write_cannot_overwrite_a_reregistration() {
        let replica = replica();
        let id = Uuid::new_v4();
        replica
            .apply_change(record("gear", id, ReportedState::Serving, 4))
            .unwrap();

        // Removed and re-registered elsewhere; the new generation restarts
        // its revisions.
        let started = Instant::now();
        replica.apply_removal("gear", id, 1);
        replica
            .apply_change(record_generation("gear", id, ReportedState::Starting, 1, 2))
            .unwrap();

        // This replica's write issued before the removal lands late.
        replica
            .apply_write(record("gear", id, ReportedState::Draining, 5), started)
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Registered));

        replica
            .apply_change(record_generation("gear", id, ReportedState::Serving, 2, 2))
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Healthy));
    }

    #[test]
    fn write_issued_before_a_local_expiry_cannot_revive_it() {
        let replica = DirectoryReplica::new(Arc::new(
            GearManager::new().with_lease_policy(Duration::from_secs(1), Duration::ZERO),
        ));
        let id = Uuid::new_v4();
        let mut lapsed = record("gear", id, ReportedState::Serving, 1);
        lapsed.lease_expires_at = SystemTime::now();
        replica.apply_change(lapsed).unwrap();
        let started = Instant::now();
        replica.sweep(Instant::now() + Duration::from_millis(10));
        assert!(replica.manager().instances_of("gear").is_empty());

        replica
            .apply_write(record("gear", id, ReportedState::Serving, 2), started)
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), None);

        // A renewal delivered by the feed still recovers it.
        replica
            .apply_change(record("gear", id, ReportedState::Serving, 2))
            .unwrap();
        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Healthy));
    }

    #[test]
    fn delayed_removal_does_not_remove_a_new_registration() {
        let replica = replica();
        let id = Uuid::new_v4();
        replica
            .apply_change(record_generation("gear", id, ReportedState::Serving, 1, 2))
            .unwrap();

        replica.apply_removal("gear", id, 1);

        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Healthy));
    }

    #[test]
    fn snapshot_does_not_overwrite_a_newer_acknowledged_write() {
        let replica = replica();
        let id = Uuid::new_v4();
        let old = record("gear", id, ReportedState::Starting, 1);
        replica.apply_change(old.clone()).unwrap();
        let scan_started = Instant::now();

        replica
            .apply_write(
                record("gear", id, ReportedState::Draining, 2),
                Instant::now(),
            )
            .unwrap();
        replica.replace_snapshot(vec![old], scan_started);

        assert_eq!(state_of(&replica, "gear"), Some(InstanceState::Draining));
    }

    #[test]
    fn snapshot_keeps_newer_records_removes_missing_and_skips_pinned_violations() {
        let replica = replica();
        replica.manager().set_grpc_service_owners(HashMap::from([(
            "pinned.Service".to_owned(),
            "owner".to_owned(),
        )]));
        let kept = Uuid::new_v4();
        let missing = Uuid::new_v4();
        replica
            .apply_change(record("gear", kept, ReportedState::Serving, 5))
            .unwrap();
        replica
            .apply_change(record("gear", missing, ReportedState::Serving, 1))
            .unwrap();

        // The held revision of `kept` is newer than the snapshot's, so the
        // local copy (with its REST endpoint) survives. A squatter on a pinned
        // name must not keep the rest of the snapshot from loading.
        let older = DirectoryRecord {
            instance: Arc::new(
                GearInstance::new("gear", kept).with_reported_state(ReportedState::Draining),
            ),
            ..record("gear", kept, ReportedState::Draining, 1)
        };
        let squatter = DirectoryRecord {
            instance: Arc::new(
                GearInstance::new("squatter", Uuid::new_v4())
                    .with_grpc_service("pinned.Service", Endpoint::http("127.0.0.1", 9000))
                    .with_reported_state(ReportedState::Serving),
            ),
            ..record("squatter", Uuid::new_v4(), ReportedState::Serving, 1)
        };
        replica.replace_snapshot(vec![older, squatter], Instant::now());

        let instances = replica.manager().instances_of("gear");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance_id, kept);
        assert_eq!(
            instances[0].rest_endpoint,
            Some(Endpoint::http("127.0.0.1", 8080))
        );
        assert_eq!(instances[0].state(), InstanceState::Healthy);
        assert!(replica.manager().instances_of("squatter").is_empty());

        // The missing record is tombstoned: a late feed item cannot revive it.
        replica
            .apply_change(record("gear", missing, ReportedState::Serving, 1))
            .unwrap();
        assert_eq!(replica.manager().instances_of("gear").len(), 1);
    }

    #[test]
    fn late_record_after_tombstone_eviction_stays_unobservable() {
        let replica = DirectoryReplica::new(Arc::new(
            GearManager::new().with_lease_policy(Duration::from_secs(1), Duration::from_secs(2)),
        ));
        let id = Uuid::new_v4();
        replica.apply_removal("gear", id, 1);
        replica.sweep(Instant::now() + Duration::from_secs(5));
        assert_eq!(replica.tracked(), 0);

        // A write issued before the removal arrives long after: its lease
        // lapsed with the tombstone, so it cannot bring the instance back.
        let mut late = record("gear", id, ReportedState::Serving, 1);
        late.lease_expires_at = SystemTime::now() - Duration::from_secs(5);
        replica.apply_change(late).unwrap();
        assert_eq!(state_of(&replica, "gear"), None);
    }

    #[test]
    fn tombstones_are_evicted_after_ttl_plus_grace() {
        let replica = replica();
        for _ in 0..3 {
            let id = Uuid::new_v4();
            replica
                .apply_change(record("gear", id, ReportedState::Serving, 1))
                .unwrap();
            replica.apply_removal("gear", id, 1);
        }
        replica.apply_removal("gear", Uuid::new_v4(), 7);
        replica.sweep(Instant::now());
        assert_eq!(replica.tracked(), 4);

        sweep_after_lapse(&replica);
        assert_eq!(replica.tracked(), 0);
    }
}
