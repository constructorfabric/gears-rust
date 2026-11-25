use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use toolkit::runtime::{GearInstance, GrpcServiceNameConflict};
use uuid::Uuid;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, PoisonError};

pub type DirectoryChangeStream = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = Result<DirectoryChange, DirectoryBackendError>> + Send>,
>;

/// Failure of a [`DirectoryBackend`] operation.
#[derive(Debug, thiserror::Error)]
pub enum DirectoryBackendError {
    /// The registration advertises a gRPC service name claimed by another
    /// gear. Returned to the caller as `ALREADY_EXISTS`.
    #[error(transparent)]
    Conflict(#[from] GrpcServiceNameConflict),
    /// The backend could not complete the operation. Logged, then returned
    /// to the caller as a generic "backend unavailable" error.
    #[cfg_attr(
        not(any(
            feature = "storage-k8s",
            feature = "storage-postgres",
            feature = "storage-redis"
        )),
        allow(dead_code, reason = "constructed by feature-gated cache storage")
    )]
    #[error("directory backend unavailable: {0:#}")]
    Unavailable(anyhow::Error),
}

/// A complete instance record as stored by a [`DirectoryBackend`].
///
/// # Versioning
///
/// Replicas order the records of one `(gear, instance_id)` by
/// `(generation, revision)` and keep a tombstone for a removed generation, so
/// a backend MUST uphold both rules below:
///
/// - `generation` identifies one registration lifetime. It MUST strictly
///   increase each time the key is registered afresh — after
///   [`deregister`](DirectoryBackend::deregister), or once the previous lease
///   has lapsed past grace — and MUST never be reused for the key, even if
///   the store's own entry version restarts after a delete. A replica drops
///   every record at or below a removed generation, so a reused generation
///   is silently lost.
/// - `revision` MUST strictly increase with every write within one
///   generation. Revisions are compared only within a generation; a new
///   generation may restart them.
#[derive(Clone, Debug)]
pub struct DirectoryRecord {
    pub instance: Arc<GearInstance>,
    pub generation: u64,
    pub revision: u64,
    pub lease_expires_at: SystemTime,
}

#[derive(Clone, Debug)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the production backend that emits these changes is implemented separately"
    )
)]
pub enum DirectoryChange {
    Upsert(DirectoryRecord),
    Renew(DirectoryRecord),
    Remove {
        gear: String,
        instance_id: Uuid,
        generation: u64,
    },
}

/// Persistence and replication contract owned by the service-discovery gear.
///
/// The backend is the authority for a registration's content, gRPC
/// service-name claims and versions; every replica keeps a local read model
/// and orders what it applies by [`DirectoryRecord`] versions. Each write
/// below MUST be atomic with respect to every other write to the same
/// `(gear, instance_id)`. Service-name ownership is coordinated with
/// per-name compare-and-swap reservations: records are not advertised until
/// all claims are secured and revalidated, and readers expose a service only
/// when its current claim names that record's gear. A crash may leave an
/// orphan claim, which the backend janitor reclaims after confirming no live
/// record backs it.
#[async_trait]
pub trait DirectoryBackend: Send + Sync {
    /// Register or re-register `instance`. Implementations MUST:
    ///
    /// 1. **Discard a lapsed record.** A stored record whose lease expired
    ///    `grace` or more ago is treated as absent.
    /// 2. **Merge partial registrations.** One process registers an instance
    ///    from two places (the gRPC hub with its services, the host with its
    ///    REST endpoint and `OpenAPI` hash), each writing only its own half.
    ///    An absent `control`, `rest_endpoint`, `openapi_spec_hash` or
    ///    `reported_state`, and an empty `grpc_services` or `labels`, keep the
    ///    stored value; `version` is always replaced. A new record without
    ///    `reported_state` stores `Starting`. Replacing instead of merging
    ///    makes the two halves overwrite each other.
    /// 3. **Enforce first-registration gRPC service-name claims.** If any
    ///    record of a *different* gear whose lease has not lapsed past `grace`
    ///    advertises one of the merged record's gRPC service names, write
    ///    nothing and return [`DirectoryBackendError::Conflict`] with
    ///    `recoverable: true`. Reserve each new name with an atomic per-key
    ///    compare-and-swap, then write the record and revalidate every claim
    ///    before reporting success. Readers filter services against current
    ///    claims, so competing registrations cannot both advertise a name.
    ///    On conflict, release only reservations acquired by this call.
    /// 4. **Assign versions** per [`DirectoryRecord`]: keep the stored
    ///    generation for a live record, start a new one otherwise, and bump
    ///    the revision.
    /// 5. Set `lease_expires_at` to now + `ttl`, and publish the stored
    ///    record as [`DirectoryChange::Upsert`].
    ///
    /// Returns the stored record.
    async fn register(
        &self,
        instance: Arc<GearInstance>,
        ttl: Duration,
        grace: Duration,
    ) -> Result<DirectoryRecord, DirectoryBackendError>;

    /// Renew the lease and update reported state atomically for one live
    /// registration. Implementations must not split this into separate writes.
    /// Returns `None`, writing nothing, when no record exists or its lease
    /// lapsed `grace` or more ago. Otherwise keeps every other field and the
    /// generation, bumps the revision, sets the lease to now + `ttl`, and
    /// publishes [`DirectoryChange::Renew`].
    async fn heartbeat(
        &self,
        gear: &str,
        instance_id: Uuid,
        reported: toolkit::runtime::ReportedState,
        ttl: Duration,
        grace: Duration,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError>;

    /// Remove the record, publishing [`DirectoryChange::Remove`] with its
    /// generation. Returns the removed record, if one was present.
    async fn deregister(
        &self,
        gear: &str,
        instance_id: Uuid,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError>;

    /// Load every record whose lease has not lapsed past `grace`.
    ///
    /// The result MUST reflect at least every change committed before this
    /// call. It need not be a point-in-time snapshot: the caller subscribes
    /// through [`changes`](Self::changes) first and orders the snapshot and
    /// the feed by record version.
    async fn load_all(
        &self,
        grace: Duration,
    ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError>;

    /// Reclaim orphaned cache keys and service-name claims. Backends without
    /// shared storage have nothing to sweep.
    async fn janitor(&self, _grace: Duration) -> Result<(), DirectoryBackendError> {
        Ok(())
    }

    /// Subscribe to changes. The subscription MUST be established before the
    /// returned future resolves, so it delivers every change committed after
    /// that point. Changes to one `(gear, instance_id)` MUST be delivered in
    /// commit order; changes to different keys may interleave. The feed may
    /// repeat a change the caller already holds; the caller de-duplicates by
    /// version. If the backend cannot deliver a change (for example, the
    /// subscriber lagged), it MUST yield an error rather than skip it; the
    /// caller then resubscribes and reloads.
    async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError>;
}

#[cfg(test)]
const CHANGE_FEED_CAPACITY: usize = 256;

#[cfg(test)]
#[derive(Default)]
struct State {
    records: HashMap<(String, Uuid), DirectoryRecord>,
    generations: HashMap<(String, Uuid), u64>,
    revision: u64,
}

#[cfg(test)]
impl State {
    fn store(
        &mut self,
        key: (String, Uuid),
        instance: GearInstance,
        generation: u64,
        lease_expires_at: SystemTime,
    ) -> DirectoryRecord {
        self.revision += 1;
        let record = DirectoryRecord {
            instance: Arc::new(instance),
            generation,
            revision: self.revision,
            lease_expires_at,
        };
        self.records.insert(key, record.clone());
        record
    }
}

#[cfg(test)]
/// Deterministic in-memory backend for service-discovery replica tests.
#[derive(Clone)]
pub struct MemoryDirectoryBackend {
    state: Arc<Mutex<State>>,
    changes: tokio::sync::broadcast::Sender<DirectoryChange>,
}

#[cfg(test)]
impl Default for MemoryDirectoryBackend {
    fn default() -> Self {
        let (changes, _) = tokio::sync::broadcast::channel(CHANGE_FEED_CAPACITY);
        Self {
            state: Arc::new(Mutex::new(State::default())),
            changes,
        }
    }
}

#[cfg(test)]
impl MemoryDirectoryBackend {
    pub fn set_lease_age(&self, gear: &str, instance_id: Uuid, age: Duration) {
        if let Some(record) = self.lock().records.get_mut(&(gear.to_owned(), instance_id)) {
            record.lease_expires_at = SystemTime::now() - age;
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish(&self, change: DirectoryChange) {
        if self.changes.send(change).is_err() {
            tracing::trace!("directory change published with no subscribers");
        }
    }

    fn merge(existing: Option<&GearInstance>, incoming: &GearInstance) -> GearInstance {
        let mut merged = GearInstance::new(incoming.gear.clone(), incoming.instance_id);
        merged.control = incoming
            .control
            .clone()
            .or_else(|| existing.and_then(|instance| instance.control.clone()));
        merged.grpc_services = if incoming.grpc_services.is_empty() {
            existing.map_or_else(HashMap::new, |instance| instance.grpc_services.clone())
        } else {
            incoming.grpc_services.clone()
        };
        merged.version = incoming.version.clone();
        merged.rest_endpoint = incoming
            .rest_endpoint
            .clone()
            .or_else(|| existing.and_then(|instance| instance.rest_endpoint.clone()));
        merged.openapi_spec_hash = incoming
            .openapi_spec_hash
            .clone()
            .or_else(|| existing.and_then(|instance| instance.openapi_spec_hash.clone()));
        merged.labels = if incoming.labels.is_empty() {
            existing.map_or_else(Default::default, |instance| instance.labels.clone())
        } else {
            incoming.labels.clone()
        };
        merged.reported_state = incoming
            .reported_state
            .or_else(|| existing.and_then(|instance| instance.reported_state));
        merged
    }

    fn check_claims(
        records: &HashMap<(String, Uuid), DirectoryRecord>,
        record: &GearInstance,
        grace: Duration,
    ) -> Result<(), GrpcServiceNameConflict> {
        let now = SystemTime::now();
        for entry in records.values().filter(|entry| {
            entry.instance.gear != record.gear && entry.lease_expires_at + grace > now
        }) {
            let other = &entry.instance;
            if let Some(name) = record
                .grpc_services
                .keys()
                .find(|name| other.grpc_services.contains_key(*name))
            {
                return Err(GrpcServiceNameConflict {
                    service_name: name.clone(),
                    owner: other.gear.clone(),
                    recoverable: true,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[async_trait]
impl DirectoryBackend for MemoryDirectoryBackend {
    async fn register(
        &self,
        instance: Arc<GearInstance>,
        ttl: Duration,
        grace: Duration,
    ) -> Result<DirectoryRecord, DirectoryBackendError> {
        let key = (instance.gear.clone(), instance.instance_id);
        let mut state = self.lock();
        let now = SystemTime::now();
        if state
            .records
            .get(&key)
            .is_some_and(|record| record.lease_expires_at + grace <= now)
        {
            state.records.remove(&key);
        }
        let existing = state.records.get(&key);
        let merged = Self::merge(existing.map(|record| record.instance.as_ref()), &instance);
        let reported = merged
            .reported_state
            .or_else(|| existing.and_then(|record| record.instance.reported_state))
            .unwrap_or_default();
        let merged = merged.with_reported_state(reported);
        Self::check_claims(&state.records, &merged, grace)?;
        let generation = if let Some(record) = state.records.get(&key) {
            record.generation
        } else {
            let generation = state.generations.get(&key).copied().unwrap_or(0) + 1;
            state.generations.insert(key.clone(), generation);
            generation
        };
        let record = state.store(key, merged, generation, SystemTime::now() + ttl);
        self.publish(DirectoryChange::Upsert(record.clone()));
        Ok(record)
    }

    async fn heartbeat(
        &self,
        gear: &str,
        instance_id: Uuid,
        reported: toolkit::runtime::ReportedState,
        ttl: Duration,
        grace: Duration,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
        let key = (gear.to_owned(), instance_id);
        let mut state = self.lock();
        let Some(existing) = state.records.get(&key) else {
            return Ok(None);
        };
        if existing.lease_expires_at + grace <= SystemTime::now() {
            return Ok(None);
        }
        let generation = existing.generation;
        // Heartbeat only updates reported state and lease; preserve all existing metadata
        // by merging with an empty incoming instance (None).
        let instance = Self::merge(
            Some(&existing.instance),
            &GearInstance::new(gear.to_owned(), instance_id),
        )
        .with_reported_state(reported);
        let record = state.store(key, instance, generation, SystemTime::now() + ttl);
        self.publish(DirectoryChange::Renew(record.clone()));
        Ok(Some(record))
    }

    async fn deregister(
        &self,
        gear: &str,
        instance_id: Uuid,
    ) -> Result<Option<DirectoryRecord>, DirectoryBackendError> {
        let mut state = self.lock();
        let removed = state.records.remove(&(gear.to_owned(), instance_id));
        if let Some(record) = &removed {
            self.publish(DirectoryChange::Remove {
                gear: gear.to_owned(),
                instance_id,
                generation: record.generation,
            });
        }
        Ok(removed)
    }

    async fn load_all(
        &self,
        grace: Duration,
    ) -> Result<Vec<DirectoryRecord>, DirectoryBackendError> {
        let now = SystemTime::now();
        Ok(self
            .lock()
            .records
            .values()
            .filter(|record| record.lease_expires_at + grace > now)
            .cloned()
            .collect())
    }

    async fn changes(&self) -> Result<DirectoryChangeStream, DirectoryBackendError> {
        let receiver = self.changes.subscribe();
        Ok(Box::pin(futures_util::stream::unfold(
            receiver,
            |mut receiver| async move {
                match receiver.recv().await {
                    Ok(change) => Some((Ok(change), receiver)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => Some((
                        Err(DirectoryBackendError::Unavailable(anyhow::anyhow!(
                            "directory change feed lagged by {count} changes"
                        ))),
                        receiver,
                    )),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                }
            },
        )))
    }
}
