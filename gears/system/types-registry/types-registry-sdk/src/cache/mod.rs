//! The client-side representation cache (SPEC §8.3, DESIGN "The client-side cache").
//!
//! # One batch read
//!
//! 1. Plan under the lock. A hit requires a fresh snapshot covering the projection
//!    and, for tenant reads, visibility confirmed to the same read context (subject,
//!    its type, home tenant and token scopes). `fresh` forces a fetch.
//! 2. Fetch remote keys in one `batchGet` outside the lock. A **pinned** read holds
//!    a snapshot covering the projection and sends its `ETag`. Other reads forward
//!    the caller's applicable validator.
//! 3. Verify the whole batch, merge fields with matching `ETags` and build the response.
//!    `Unchanged` confirms either the pinned snapshot ([`Verified::Confirmed`]) or
//!    the caller's validator ([`Verified::Matched`]). A caller match confirms a
//!    matching resident entry or drops a conflicting one.
//! 4. Commit identities and snapshots under the lock. A tenant read's `NotFound`
//!    revokes that read context's confirmation and fences its pending reads.
//!
//! # Why reads take tickets
//!
//! `ETags` cannot order revisions. A **ticket** records the event counter and the
//! key's **spelling** (GTS ID, UUID or known pair) before the fetch. A read is
//! **quiet** if no intervening event could concern that entity. Otherwise the event
//! **fences** the read. A fenced reply can drop a conflicting snapshot but cannot
//! fill or refresh one. For example, if the client observes a successful registration
//! while a read is in flight, the old reply reaches its caller without refilling the cache.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use gts::GtsId;
use tokio::time::Instant;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::contract::{PlatformTypesRegistryApi, TypesRegistryApi};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, DeleteEntitiesRequest,
    DeletionOperation, DeletionOutcome, Entity, EntityField, EntityIdentity, EntityKey,
    EntityLookup, FieldSelection, IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse,
    MAX_BATCH_GET_KEYS, Operation, Ownership, RegisterEntitiesRequest, RegistrationOperation,
    RegistrationOutcome, Validator,
};

mod identity;
mod metrics;
mod store;

use metrics::CacheMetrics;
use store::{ContextTicket, ReadContext, Store, Ticket, Usage, Weight};

/// Freshness and retention limits for Types Registry client caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSettings {
    /// `0` is meaningful: every snapshot read revalidates.
    pub freshness_window: Duration,
    /// Charges compact JSON documents (including selected `null`), GTS ID bytes,
    /// raw validator bytes and 256 bytes per entry. LRU eviction enforces the
    /// budget. Oversized entries bypass retention. Heap usage may be higher.
    pub max_size_bytes: usize,
    /// Maximum number of resident snapshots. Zero disables snapshot retention and,
    /// in a tenant cache, which uses the same limit for its visibility records,
    /// all local tenant answers.
    pub max_entries: usize,
    /// Identity LRU limit. Zero leaves resolution to fresh snapshots or the registry.
    /// A tenant cache answers a remembered pair only to a read context the registry
    /// showed it to within the window; without pairs it resolves only from a fresh
    /// read of a resident snapshot. A zero window disables local tenant resolution.
    pub max_identity_entries: usize,
}

impl CacheSettings {
    /// DESIGN's freshness window.
    pub const DEFAULT_FRESHNESS_WINDOW: Duration = Duration::from_secs(30);
    /// Default snapshot cache size limit (64 MiB).
    pub const DEFAULT_MAX_SIZE_BYTES: usize = 64 * 1024 * 1024;
    /// Default maximum number of resident snapshots.
    pub const DEFAULT_MAX_ENTRIES: usize = 16 * 1024;
    /// Default identity count limit (up to 16 MiB of identifier text).
    pub const DEFAULT_MAX_IDENTITY_ENTRIES: usize = 16 * 1024;
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            freshness_window: Self::DEFAULT_FRESHNESS_WINDOW,
            max_size_bytes: Self::DEFAULT_MAX_SIZE_BYTES,
            max_entries: Self::DEFAULT_MAX_ENTRIES,
            max_identity_entries: Self::DEFAULT_MAX_IDENTITY_ENTRIES,
        }
    }
}

/// A bounded per-client cache for platform reads and identity resolution.
///
/// One snapshot per entity accumulates fields under one validator. Covered reads
/// return the requested projection. Matching replies merge, while a new validator
/// replaces the snapshot.
///
/// [`batch_resolve_identities`](PlatformTypesRegistryApi::batch_resolve_identities)
/// checks a separate identity LRU without expiry, then fresh snapshots, then the
/// registry. Both indexes use registry-issued pairs. The cache checks the whole
/// reply against requested keys and known pairs before updating either store.
pub struct CachedPlatformTypesRegistry {
    inner: Arc<dyn PlatformTypesRegistryApi>,
    cache: ReadCache,
}

struct ReadCache {
    metrics: CacheMetrics,
    freshness_window: Duration,
    /// The store's byte bound, so that a merge too large to keep is never built.
    max_size_bytes: usize,
    /// Both stores share one lock. Each mutation preserves consistency, so recover poison.
    store: Mutex<Store>,
}

impl Drop for ReadCache {
    fn drop(&mut self) {
        let store = self.store.get_mut().unwrap_or_else(PoisonError::into_inner);
        self.metrics.update_usage(store.usage(), Usage::default());
    }
}

/// How one distinct key of a batch is answered.
enum Plan {
    /// From the cache, inside the window: the entry covers the read.
    Local {
        snapshot: Arc<Entity>,
        etag: Validator,
        caller: Option<Validator>,
    },
    /// From the registry, conditionally on `sent`: the entry's validator when the entry
    /// covers the read, or else the caller's own.
    Remote {
        caller: Option<Validator>,
        /// The entry when the read started, whether or not it covers the read.
        resident: Option<Resident>,
        /// Whether `resident` covers the read, so that `sent` is its validator.
        pinned: bool,
        sent: Option<Validator>,
        /// This read's ticket, taken with its lookup.
        ticket: Ticket,
        /// Whether the entity was known global when the read started, by its snapshot
        /// or its remembered pair, which either may lose before the answer arrives.
        global: bool,
    },
}

/// A snapshot to store, the fields it holds and its measured weight.
struct Kept {
    snapshot: Arc<Entity>,
    covers: FieldSelection,
    weight: Weight,
}

/// What an entry held when a read started.
struct Resident {
    snapshot: Arc<Entity>,
    covers: FieldSelection,
    etag: Validator,
    weight: Weight,
}

impl Resident {
    fn of(entry: &store::Entry) -> Self {
        Self {
            snapshot: Arc::clone(&entry.snapshot),
            covers: entry.covers.clone(),
            etag: entry.etag.clone(),
            weight: entry.weight.clone(),
        }
    }
}

/// A registry read of `key`, pinned to `resident` when it covers the selection, with
/// a ticket taken under the lookup lock.
fn remote(
    store: &mut Store,
    key: &EntityKey,
    caller: Option<Validator>,
    resident: Option<Resident>,
    selection: &FieldSelection,
    held: &mut Vec<Ticket>,
) -> Plan {
    let pinned = resident
        .as_ref()
        .is_some_and(|r| r.covers.is_superset(selection));
    let global = resident
        .as_ref()
        .is_some_and(|r| r.snapshot.ownership == Ownership::Global)
        || store.key_known_global(key);
    let ticket = store.ticket(key);
    held.push(ticket.clone());
    Plan::Remote {
        global,
        ticket,
        sent: match &resident {
            Some(r) if pinned => Some(r.etag.clone()),
            _ => caller.clone(),
        },
        caller,
        resident,
        pinned,
    }
}

/// The tickets one batch read holds, released when it completes, fails or is dropped.
struct Tickets<'a> {
    store: &'a Mutex<Store>,
    held: Vec<Ticket>,
    context: Option<ContextTicket>,
}

impl Drop for Tickets<'_> {
    fn drop(&mut self) {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        for ticket in &self.held {
            store.release(ticket);
        }
        if let Some(ticket) = &self.context {
            store.release_context_ticket(ticket);
        }
    }
}

/// Resolution answers by requested key.
type Resolutions = HashMap<EntityKey, Option<EntityIdentity>>;

/// A registry answer checked against what was sent, ready to commit without further work.
enum Verified {
    Found {
        /// The answer as read: exactly the selected fields.
        snapshot: Arc<Entity>,
        etag: Validator,
        /// The snapshot to retain. `None` keeps the existing entry if the merge is oversized.
        keep: Option<Kept>,
    },
    /// `Unchanged` to the entry's validator: the pinned entry is current.
    Confirmed,
    /// `Unchanged` to the caller's validator: the caller's snapshot is current.
    Matched(Validator),
    NotFound,
}

impl std::fmt::Debug for CachedPlatformTypesRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedPlatformTypesRegistry")
            .field("freshness_window", &self.cache.freshness_window)
            .finish_non_exhaustive()
    }
}

impl CachedPlatformTypesRegistry {
    /// Wrap `inner` in a bounded cache. Share the wrapper to share cached reads.
    #[must_use]
    pub fn new(inner: Arc<dyn PlatformTypesRegistryApi>, settings: CacheSettings) -> Self {
        Self {
            inner,
            cache: ReadCache::new(settings, false),
        }
    }

    #[cfg(test)]
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.cache.store()
    }

    /// The identity store's answer for `key`, promoting it as a resolution would.
    #[cfg(test)]
    fn identity(&self, key: &EntityKey) -> Option<EntityIdentity> {
        self.store().identities.get(key)
    }

    async fn read(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.cache
            .read(request, None, |request| {
                self.inner.batch_get_entities(ctx, request)
            })
            .await
    }
}

impl ReadCache {
    fn new(settings: CacheSettings, tenant: bool) -> Self {
        let mut store = Store::new(settings.max_size_bytes, settings.max_entries);
        store.remember_identities(settings.max_identity_entries);
        if tenant {
            store.track_visibility();
        }
        Self {
            metrics: CacheMetrics::from_global(if tenant { "tenant" } else { "platform" }),
            freshness_window: settings.freshness_window,
            max_size_bytes: settings.max_size_bytes,
            store: Mutex::new(store),
        }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records net occupancy changes under the same lock as the mutation.
    fn update_store<T>(&self, update: impl FnOnce(&mut Store) -> T) -> T {
        let mut store = self.store();
        let before = store.usage();
        let result = update(&mut store);
        self.metrics.update_usage(before, store.usage());
        result
    }

    /// A batch read through the cache.
    async fn read<F>(
        &self,
        request: BatchGetEntitiesRequest,
        context: Option<&ReadContext>,
        fetch: impl FnOnce(BatchGetEntitiesRequest) -> F,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError>
    where
        F: Future<Output = Result<BatchGetEntitiesResponse, CanonicalError>>,
    {
        if request.items.is_empty() || request.items.len() > MAX_BATCH_GET_KEYS {
            // Refused for its size whatever it names.
            return fetch(request).await;
        }
        let now = Instant::now();
        let selection = request.projection.normalized();
        let (plans, tickets) = self.plan(&request, &selection, now, context);
        let mut revalidations = 0;
        let remote: Vec<BatchGetItem> = plans
            .iter()
            .filter_map(|(key, plan)| match plan {
                Plan::Local { .. } => None,
                Plan::Remote { pinned, sent, .. } => {
                    revalidations += usize::from(*pinned);
                    Some(BatchGetItem::for_cache_read(
                        key.clone(),
                        sent.clone(),
                        &selection,
                    ))
                }
            })
            .collect();
        // Classify each planned key, including lookups whose registry call later fails.
        self.metrics.lookups(
            "batch_get",
            &[
                ("hit", plans.len() - remote.len()),
                ("miss", remote.len() - revalidations),
                ("revalidate", revalidations),
            ],
        );
        let answers = if remote.is_empty() {
            HashMap::new()
        } else {
            let inner = BatchGetEntitiesRequest {
                items: remote,
                projection: request.projection,
                fresh: request.fresh,
            };
            fetch(inner).await?.0
        };
        let verified = verify(&plans, &selection, self.max_size_bytes, answers)?;
        let response = respond(&plans, &selection, &verified)?;
        self.settle(&plans, &verified, now, tickets.context.as_ref())?;
        drop(tickets);
        Ok(response)
    }

    /// Commit snapshots only after all pairs have passed the shared identity checks.
    fn settle(
        &self,
        plans: &[(EntityKey, Plan)],
        verified: &[Option<Verified>],
        sent_at: Instant,
        context: Option<&ContextTicket>,
    ) -> Result<(), CanonicalError> {
        let found = plans
            .iter()
            .zip(verified)
            .filter_map(|((_, plan), answer)| identified(plan, answer.as_ref()))
            .map(|snapshot| (&snapshot.gts_id, snapshot.gts_uuid));
        let classes = plans
            .iter()
            .zip(verified)
            .filter_map(|((_, plan), answer)| identified(plan, answer.as_ref()))
            .map(|snapshot| (snapshot.gts_uuid, snapshot.ownership == Ownership::Global));
        // The pair a read's ticket started with may be evicted by now; it still binds both keys.
        for ((_, plan), answer) in plans.iter().zip(verified) {
            if let (Plan::Remote { ticket, .. }, Some(Verified::Found { snapshot, .. })) =
                (plan, answer)
                && let Some((gts_id, gts_uuid)) = ticket.pair()
                && ((snapshot.gts_id == *gts_id) != (snapshot.gts_uuid == gts_uuid))
            {
                return Err(CanonicalError::internal(format!(
                    "the registry returned a conflicting identity {} ({}): the read started from {gts_id} ({gts_uuid})",
                    snapshot.gts_id, snapshot.gts_uuid
                ))
                .create());
            }
        }
        // The snapshot a read started from may be evicted by now; it still holds the class.
        for ((_, plan), answer) in plans.iter().zip(verified) {
            if let (
                Plan::Remote {
                    resident: Some(held),
                    ..
                },
                Some(Verified::Found { snapshot, .. }),
            ) = (plan, answer)
                && held.snapshot.gts_uuid == snapshot.gts_uuid
                && (held.snapshot.ownership == Ownership::Global)
                    != (snapshot.ownership == Ownership::Global)
            {
                return Err(CanonicalError::internal(format!(
                    "the registry returned a conflicting ownership for {}",
                    snapshot.gts_uuid
                ))
                .create());
            }
        }
        let (identities, snapshots) = self.update_store(|store| {
            check_classes(store, classes.clone())?;
            let identities = Self::learn_identities(store, &[], found)?;
            for (gts_uuid, global) in classes {
                store.identities.classify(gts_uuid, global);
            }
            let snapshots = commit(store, plans, verified, sent_at, context);
            Ok::<_, CanonicalError>((identities, snapshots))
        })?;
        self.metrics.evictions("identity", identities);
        self.metrics.evictions("snapshot", snapshots);
        Ok(())
    }

    /// Learns identities from a resolution or discovery reply and records any LRU pressure.
    fn learn_from_reply<'a>(
        &self,
        held: &[&'a EntityIdentity],
        found: impl Iterator<Item = (&'a GtsId, Uuid)> + Clone,
    ) -> Result<(), CanonicalError> {
        let evictions = self.update_store(|store| Self::learn_identities(store, held, found))?;
        self.metrics.evictions("identity", evictions);
        Ok(())
    }

    /// Learns the pairs of a whole, consistent discovery page in page order.
    fn learn_page(&self, page: &ListEntitiesResponse) -> Result<(), CanonicalError> {
        if let Some(entity) = page
            .items
            .iter()
            .find(|entity| !crate::ext::consistent(entity))
        {
            return Err(CanonicalError::internal(format!(
                "the registry returned an inconsistent discovery item for {}",
                entity.gts_id
            ))
            .create());
        }
        let evictions = self.update_store(|store| {
            let classes = page
                .items
                .iter()
                .map(|entity| (entity.gts_uuid, entity.ownership == Ownership::Global));
            check_classes(store, classes.clone())?;
            let evictions = Self::learn_identities(
                store,
                &[],
                page.items
                    .iter()
                    .map(|entity| (&entity.gts_id, entity.gts_uuid)),
            )?;
            for (gts_uuid, global) in classes {
                store.identities.classify(gts_uuid, global);
            }
            Ok::<_, CanonicalError>(evictions)
        })?;
        self.metrics.evictions("identity", evictions);
        Ok(())
    }

    /// Validate before learning or changing recency. `held` pairs remain part of
    /// the batch even if they were evicted while the registry call was in flight.
    fn learn_identities<'a>(
        store: &mut Store,
        held: &[&'a EntityIdentity],
        found: impl Iterator<Item = (&'a GtsId, Uuid)> + Clone,
    ) -> Result<u64, CanonicalError> {
        // Validate against held pairs without relearning them.
        let mut by_id: HashMap<&GtsId, Uuid> = held
            .iter()
            .map(|known| (&known.gts_id, known.gts_uuid))
            .collect();
        let mut by_uuid: HashMap<Uuid, &GtsId> = held
            .iter()
            .map(|known| (known.gts_uuid, &known.gts_id))
            .collect();
        for (gts_id, gts_uuid) in found.clone() {
            let known_uuid = *by_id.entry(gts_id).or_insert(gts_uuid);
            let known_id = *by_uuid.entry(gts_uuid).or_insert(gts_id);
            let within = if known_uuid != gts_uuid {
                Some(EntityKey::GtsUuid(known_uuid))
            } else if known_id != gts_id {
                Some(EntityKey::GtsId(known_id.clone()))
            } else {
                None
            };
            let conflict = within
                .map(|known| (known, "this batch"))
                .or_else(|| {
                    store
                        .conflict(gts_id, gts_uuid)
                        .map(|known| (known, "a resident snapshot"))
                })
                .or_else(|| {
                    store
                        .identities
                        .conflict(gts_id, gts_uuid)
                        .map(|known| (known, "the identity store"))
                });
            if let Some((known, source)) = conflict {
                return Err(CanonicalError::internal(format!(
                    "the registry returned a conflicting identity {gts_id} ({gts_uuid}): \
                     {source} pairs one of them with {known}"
                ))
                .create());
            }
        }
        Ok(found
            .map(|(gts_id, gts_uuid)| u64::from(store.identities.learn(gts_id, gts_uuid)))
            .sum())
    }

    /// Resolve distinct keys from the identity LRU or fresh snapshots.
    /// Return misses in request order.
    fn known_identities(
        &self,
        keys: Vec<EntityKey>,
        now: Instant,
    ) -> (HashMap<EntityKey, Option<EntityIdentity>>, Vec<EntityKey>) {
        let mut store = self.store();
        let mut answers = HashMap::with_capacity(keys.len());
        let mut unknown = Vec::new();
        for key in keys {
            if answers.contains_key(&key) || unknown.contains(&key) {
                continue;
            }
            let known = store.identities.get(&key).or_else(|| {
                store
                    .get(&key)
                    .filter(|entry| now.duration_since(entry.confirmed_at) < self.freshness_window)
                    .map(|entry| EntityIdentity {
                        gts_id: entry.snapshot.gts_id.clone(),
                        gts_uuid: entry.snapshot.gts_uuid,
                    })
            });
            match known {
                Some(identity) => {
                    answers.insert(key, Some(identity));
                }
                None => unknown.push(key),
            }
        }
        (answers, unknown)
    }

    /// Resolve distinct keys the read context was shown within the window. Return
    /// misses in request order and, if any, a ticket fencing their confirmation.
    fn visible_identities(
        &self,
        keys: Vec<EntityKey>,
        context: &ReadContext,
        now: Instant,
    ) -> (Resolutions, Vec<EntityKey>, Tickets<'_>) {
        let mut tickets = Tickets {
            store: &self.store,
            held: Vec::new(),
            context: None,
        };
        let mut store = self.store();
        let mut answers = HashMap::with_capacity(keys.len());
        let mut unknown = Vec::new();
        for key in keys {
            if answers.contains_key(&key) || unknown.contains(&key) {
                continue;
            }
            // Any authenticated reader may see a global entity; others need their own proof.
            let identity = if store.key_known_global(&key) {
                store.resolvable_global(&key)
            } else {
                store.resolvable(&key, context, now, self.freshness_window)
            };
            match identity {
                Some(identity) => {
                    answers.insert(key, Some(identity));
                }
                None => unknown.push(key),
            }
        }
        if !unknown.is_empty() {
            tickets.context = Some(store.context_ticket(context));
        }
        (answers, unknown, tickets)
    }

    /// Plan distinct keys in first-mention order. Take remote read tickets under
    /// the lookup lock so intervening mutations can fence their replies.
    fn plan(
        &self,
        request: &BatchGetEntitiesRequest,
        selection: &FieldSelection,
        now: Instant,
        context: Option<&ReadContext>,
    ) -> (Vec<(EntityKey, Plan)>, Tickets<'_>) {
        // Drop the guard before tickets reacquire the lock during unwind.
        let mut tickets = Tickets {
            store: &self.store,
            held: Vec::new(),
            context: None,
        };
        let mut store = self.store();
        let mut seen = HashSet::new();
        let mut plans = Vec::with_capacity(request.items.len());
        for item in &request.items {
            if !seen.insert(&item.key) {
                continue;
            }
            // The caller's validator must cover this key and projection.
            let caller = item.if_none_match_for(&request.projection).cloned();
            // Use only snapshots that carry the requested key.
            // Any authenticated reader may see a global snapshot; others need their
            // own confirmation.
            let global = store
                .peek(&item.key)
                .is_some_and(|entry| entry.snapshot.ownership == Ownership::Global);
            let visible = global
                || context.is_none_or(|context| {
                    store.visible(&item.key, context, now, self.freshness_window)
                });
            let entry = if visible {
                store.get(&item.key)
            } else {
                store.peek(&item.key)
            };
            let plan = match entry {
                Some(entry)
                    if visible
                        && entry.covers.is_superset(selection)
                        && !request.fresh
                        && now.duration_since(entry.confirmed_at) < self.freshness_window =>
                {
                    Plan::Local {
                        snapshot: Arc::clone(&entry.snapshot),
                        etag: entry.etag.clone(),
                        caller,
                    }
                }
                entry => {
                    let resident = entry.map(Resident::of);
                    remote(
                        &mut store,
                        &item.key,
                        caller,
                        resident,
                        selection,
                        &mut tickets.held,
                    )
                }
            };
            plans.push((item.key.clone(), plan));
        }
        if let Some(context) = context
            && !tickets.held.is_empty()
        {
            tickets.context = Some(store.context_ticket(context));
        }
        (plans, tickets)
    }
}

impl CachedPlatformTypesRegistry {
    /// Invalidates every entity a terminal successful registration committed.
    fn observe_registration(&self, operation: &RegistrationOperation) {
        let committed: Vec<EntityKey> = operation
            .items
            .iter()
            .filter(|item| matches!(item.outcome, RegistrationOutcome::Succeeded { .. }))
            .map(|item| EntityKey::GtsId(item.gts_id.clone()))
            .collect();
        self.invalidate(&committed);
    }

    /// Invalidates every entity a terminal successful deletion committed.
    fn observe_deletion(&self, operation: &DeletionOperation) {
        let committed: Vec<EntityKey> = operation
            .items
            .iter()
            .filter(|item| matches!(item.outcome, DeletionOutcome::Succeeded { .. }))
            .map(|item| item.entity_key.clone())
            .collect();
        self.invalidate(&committed);
    }

    /// Invalidate the known pair or fence one-sided reads if the counterpart is unknown.
    fn invalidate(&self, keys: &[EntityKey]) {
        if keys.is_empty() {
            return;
        }
        self.cache.update_store(|store| {
            for key in keys {
                store.invalidate(key, None);
            }
        });
    }
}

/// Refuse a reply whose ownership contradicts itself or what the cache holds: the
/// ownership of an admitted entity never changes.
fn check_classes(
    store: &Store,
    classes: impl Iterator<Item = (Uuid, bool)>,
) -> Result<(), CanonicalError> {
    let mut seen: HashMap<Uuid, bool> = HashMap::new();
    for (gts_uuid, global) in classes {
        let in_reply = *seen.entry(gts_uuid).or_insert(global) != global;
        if in_reply || store.class_conflict(gts_uuid, global) {
            return Err(CanonicalError::internal(format!(
                "the registry returned a conflicting ownership for {gts_uuid}"
            ))
            .create());
        }
    }
    Ok(())
}

/// The snapshot whose identity a checked answer establishes: the answer's own, the entry it
/// confirmed, or the entry inside the window.
fn identified<'a>(plan: &'a Plan, verified: Option<&'a Verified>) -> Option<&'a Arc<Entity>> {
    match (plan, verified) {
        (Plan::Local { snapshot, .. }, _)
        | (_, Some(Verified::Found { snapshot, .. }))
        | (
            Plan::Remote {
                resident: Some(Resident { snapshot, .. }),
                ..
            },
            Some(Verified::Confirmed),
        ) => Some(snapshot),
        _ => None,
    }
}

/// The pair an answer states for its entity: the answer's own, or else that of the entry
/// the read started from.
fn stated<'a>(plan: &'a Plan, verified: &'a Verified) -> Option<(&'a GtsId, Uuid)> {
    let entity = match (plan, verified) {
        (_, Verified::Found { snapshot, .. }) => snapshot,
        (
            Plan::Remote {
                resident: Some(r), ..
            },
            _,
        ) => &r.snapshot,
        _ => return None,
    };
    Some((&entity.gts_id, entity.gts_uuid))
}

/// Apply checked replies under the store lock. An intervening event prevents
/// a fill, while the caller still receives its reply.
fn commit(
    store: &mut Store,
    plans: &[(EntityKey, Plan)],
    verified: &[Option<Verified>],
    sent_at: Instant,
    context: Option<&ContextTicket>,
) -> u64 {
    // Judge all tickets before applying any reply from this batch.
    let quiet: Vec<bool> = plans
        .iter()
        .map(|(_, plan)| match plan {
            Plan::Remote { ticket, .. } => store.quiet_since(ticket),
            Plan::Local { .. } => false,
        })
        .collect();
    let confirmed_for = context
        .filter(|ticket| store.context_ticket_current(ticket))
        .map(|ticket| ticket.context.clone());
    let mut evictions = 0;
    for (((asked, plan), verified), quiet) in plans.iter().zip(verified).zip(quiet) {
        let (
            Plan::Remote {
                sent,
                resident,
                ticket,
                global: planned_global,
                ..
            },
            Some(verified),
        ) = (plan, verified)
        else {
            continue;
        };
        // A tenant read's absence may be its own visibility, so the shared snapshot
        // stays — unless it is global: every authenticated reader sees a global entity,
        // so its absence is everyone's.
        // What the plan held counts as much as what is resident now: an eviction in
        // flight must not turn a global absence back into a private one.
        let global = *planned_global || store.key_known_global(asked);
        let absent = matches!(verified, Verified::NotFound);
        if context.is_some() && absent && !global {
            continue;
        }
        // An absence with no snapshot to name the pair fences both keys of the pair the
        // read started with.
        let pair = stated(plan, verified).or_else(|| absent.then(|| ticket.pair()).flatten());
        // The entry's validator when an entry carries the spelling asked.
        let current = store.peek(asked).map(|entry| entry.etag.clone());
        if !quiet {
            // An intervening event blocks a fill. Validators cannot order revisions, so
            // discard a conflicting resident snapshot and let the next read refetch.
            let agrees = match verified {
                Verified::Found { etag, .. } | Verified::Matched(etag) => {
                    current.as_ref() == Some(etag)
                }
                // A confirmation agrees only while its pinned validator is resident.
                Verified::Confirmed => resident
                    .as_ref()
                    .is_some_and(|ours| current.as_ref() == Some(&ours.etag)),
                Verified::NotFound => current.is_none(),
            };
            if !agrees {
                store.invalidate(asked, pair);
            }
            continue;
        }
        // Use current residency because an earlier reply in this batch may name
        // the same entity under another key.
        match verified {
            Verified::Found { etag, keep, .. } => {
                let same = current.as_ref() == Some(etag);
                match keep {
                    // An oversized merge retains the smaller snapshot under the same validator.
                    Some(kept) if !same || store.fits(&kept.weight, etag) => {
                        if !same && current.is_some()
                            || sent.as_ref().is_some_and(|held| held != etag)
                        {
                            // A new validator fences older fills and drops the stale snapshot even
                            // if the reply is oversized.
                            store.invalidate(asked, pair);
                        }
                        evictions += store.insert(
                            Arc::clone(&kept.snapshot),
                            kept.covers.clone(),
                            kept.weight.clone(),
                            etag.clone(),
                            sent_at,
                        );
                    }
                    _ if same => store.confirm(asked, etag, sent_at),
                    // The merge cannot be kept under the now-resident validator.
                    _ => store.invalidate(asked, pair),
                }
            }
            Verified::Confirmed => {
                if let Some(ours) = resident {
                    store.confirm(asked, &ours.etag, sent_at);
                }
            }
            // Confirm a matching resident validator and invalidate a conflicting one.
            Verified::Matched(etag) => match current {
                Some(held) if held == *etag => store.confirm(asked, etag, sent_at),
                Some(_) => store.invalidate(asked, pair),
                None => {}
            },
            Verified::NotFound => store.invalidate(asked, pair),
        }
        if let Some(confirmed) = &confirmed_for
            && let Some(etag) = match verified {
                Verified::Found { etag, .. } | Verified::Matched(etag) => Some(etag),
                Verified::Confirmed => sent.as_ref(),
                Verified::NotFound => None,
            }
        {
            store.confirm_visible(asked, confirmed, etag, sent_at);
        }
        // A checked answer shows its pair even when its snapshot is not kept or was
        // evicted in flight. A caller-only `Unchanged` checks no pair.
        if let Some(confirmed) = &confirmed_for
            && let Some(snapshot) = identified(plan, Some(verified))
        {
            store.confirm_resolved(snapshot.gts_uuid, confirmed, sent_at);
        }
        store.record(asked, pair);
    }
    if let Some(ticket) = context {
        let absent = plans.iter().zip(verified).filter_map(|((key, _), answer)| {
            matches!(answer, Some(Verified::NotFound)).then_some(key)
        });
        store.revoke_visible(&ticket.context, absent);
    }
    evictions
}

/// Validate all keys, entity identities, field coverage and exact sent validators
/// before caching. Merge matching snapshots outside the lock.
fn verify(
    plans: &[(EntityKey, Plan)],
    selection: &FieldSelection,
    max_size_bytes: usize,
    mut answers: HashMap<EntityKey, EntityLookup>,
) -> Result<Vec<Option<Verified>>, CanonicalError> {
    let mut verified = Vec::with_capacity(plans.len());
    for (key, plan) in plans {
        let Plan::Remote {
            resident,
            pinned,
            sent,
            ..
        } = plan
        else {
            verified.push(None);
            continue;
        };
        let answer = match answers.remove(key) {
            None => return Err(protocol_fault(key, "no answer")),
            Some(EntityLookup::Found { entity, etag }) => {
                if !key.names(&entity) || !crate::ext::consistent(&entity) {
                    return Err(protocol_fault(key, "another or an inconsistent entity"));
                }
                // The entry the read started from is a pair the registry stated before.
                if resident.as_ref().is_some_and(|r| {
                    r.snapshot.gts_id != entity.gts_id || r.snapshot.gts_uuid != entity.gts_uuid
                }) {
                    return Err(protocol_fault(
                        key,
                        "an identity contradicting one already known",
                    ));
                }
                if !shaped(&entity, selection) {
                    return Err(protocol_fault(key, "fields other than those selected"));
                }
                // Measured once, here: a merge adds the entry's measured fields to it.
                let measured = store::weight(&entity);
                let snapshot = Arc::new(*entity);
                let keep = match resident {
                    // Keep compatible fields only if their combined measured weight fits.
                    Some(r) if r.etag == etag && !selection.is_superset(&r.covers) => {
                        let weight = measured.merged(selection, &r.weight);
                        weight.fits(max_size_bytes, &etag).then(|| Kept {
                            snapshot: Arc::new(store::merge(
                                (*snapshot).clone(),
                                selection,
                                &r.snapshot,
                            )),
                            covers: store::union(&r.covers, selection),
                            weight,
                        })
                    }
                    _ => Some(Kept {
                        snapshot: Arc::clone(&snapshot),
                        covers: selection.clone(),
                        weight: measured,
                    }),
                };
                Verified::Found {
                    snapshot,
                    etag,
                    keep,
                }
            }
            Some(EntityLookup::Unchanged { etag }) if sent.as_ref() == Some(&etag) => {
                if *pinned {
                    Verified::Confirmed
                } else {
                    Verified::Matched(etag)
                }
            }
            Some(EntityLookup::Unchanged { .. }) => {
                return Err(protocol_fault(key, "unchanged to a validator not sent"));
            }
            Some(EntityLookup::NotFound) => Verified::NotFound,
        };
        verified.push(Some(answer));
    }
    if let Some(key) = answers.keys().next() {
        return Err(protocol_fault(key, "an answer for a key not asked"));
    }
    Ok(verified)
}

fn protocol_fault(key: &EntityKey, what: &str) -> CanonicalError {
    CanonicalError::internal(format!(
        "the registry answered the batch read of {key} with {what}"
    ))
    .create()
}

/// Build the response outside the lock before committing. Return only selected
/// fields and match only the caller's validator. Unhandled pairs are protocol faults.
fn respond(
    plans: &[(EntityKey, Plan)],
    selection: &FieldSelection,
    verified: &[Option<Verified>],
) -> Result<BatchGetEntitiesResponse, CanonicalError> {
    let mut out = HashMap::with_capacity(plans.len());
    for ((key, plan), verified) in plans.iter().zip(verified) {
        let lookup = match (plan, verified) {
            (
                Plan::Local {
                    snapshot,
                    etag,
                    caller,
                },
                None,
            )
            | (
                Plan::Remote {
                    caller,
                    resident: Some(Resident { snapshot, etag, .. }),
                    pinned: true,
                    ..
                },
                Some(Verified::Confirmed),
            ) => answer(etag, caller.as_ref(), || {
                store::project(snapshot, selection)
            }),
            (Plan::Remote { caller, .. }, Some(Verified::Found { snapshot, etag, .. })) => {
                answer(etag, caller.as_ref(), || (**snapshot).clone())
            }
            (Plan::Remote { pinned: false, .. }, Some(Verified::Matched(etag))) => {
                EntityLookup::Unchanged { etag: etag.clone() }
            }
            (Plan::Remote { .. }, Some(Verified::NotFound)) => EntityLookup::NotFound,
            _ => {
                return Err(CanonicalError::internal(format!(
                    "the client cache paired its plan for {key} with a verified answer it \
                     cannot produce"
                ))
                .create());
            }
        };
        out.insert(key.clone(), lookup);
    }
    Ok(BatchGetEntitiesResponse(out))
}

/// `Unchanged` when the caller holds `etag`, else the entity, built only then.
fn answer(
    etag: &Validator,
    caller: Option<&Validator>,
    entity: impl FnOnce() -> Entity,
) -> EntityLookup {
    if caller == Some(etag) {
        EntityLookup::Unchanged { etag: etag.clone() }
    } else {
        EntityLookup::Found {
            entity: Box::new(entity()),
            etag: etag.clone(),
        }
    }
}

/// Require exactly the selected fields applicable to the kind, including JSON `null`.
fn shaped(entity: &Entity, selection: &FieldSelection) -> bool {
    EntityField::ALL.into_iter().all(|field| {
        entity.has(field) == (field.applies_to(entity.kind) && selection.contains(field))
    })
}

#[async_trait]
impl PlatformTypesRegistryApi for CachedPlatformTypesRegistry {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.read(ctx, request).await
    }

    /// Resolve from remembered pairs, then fresh snapshots, then the registry.
    /// Forward invalid batch sizes intact.
    async fn batch_resolve_identities(
        &self,
        ctx: &PlatformSecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        if keys.is_empty() || keys.len() > MAX_BATCH_GET_KEYS {
            return self.inner.batch_resolve_identities(ctx, keys).await;
        }
        let (mut answers, unknown) = self.cache.known_identities(keys, Instant::now());
        self.cache.metrics.resolutions(answers.len(), unknown.len());
        if unknown.is_empty() {
            return Ok(answers);
        }
        let fetched = self
            .inner
            .batch_resolve_identities(ctx, unknown.clone())
            .await?;
        let fetched = crate::ext::checked_resolutions(&unknown, fetched)?;
        // Keep local pairs pinned while resolving misses to detect contradictions.
        let held: Vec<&EntityIdentity> = answers.values().flatten().collect();
        self.cache.learn_from_reply(
            &held,
            fetched
                .values()
                .flatten()
                .map(|identity| (&identity.gts_id, identity.gts_uuid)),
        )?;
        answers.extend(fetched);
        Ok(answers)
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let page = self.inner.list_entities(ctx, request).await?;
        self.cache.learn_page(&page)?;
        Ok(page)
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        let operation = self.inner.register_entities(ctx, key, request).await?;
        self.observe_registration(&operation);
        Ok(operation)
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        let operation = self.inner.delete_entities(ctx, key, request).await?;
        self.observe_deletion(&operation);
        Ok(operation)
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        let operation = self.inner.get_operation(ctx, operation_id).await?;
        match &operation {
            Operation::Registration(registration) => self.observe_registration(registration),
            Operation::Deletion(deletion) => self.observe_deletion(deletion),
        }
        Ok(operation)
    }
}

/// Local or REST cache with one visibility confirmation per entity and read context:
/// the subject, its type, home tenant and token scopes. From P1 the registry scopes
/// reads per subject by ownership constraints the cache cannot apply, so confirmations are never
/// shared between read contexts.
/// Snapshots and the identity LRU are shared; a pair resolves locally only for a
/// read context that a read or resolution showed it to within the window, unless it
/// is known global: every authenticated reader may see a global entity, so a fresh
/// global snapshot serves every authenticated context. Anonymous calls bypass the
/// cache. REST misses require the caller's bearer.
pub struct CachedTypesRegistry {
    inner: Arc<dyn TypesRegistryApi>,
    cache: ReadCache,
}

impl std::fmt::Debug for CachedTypesRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedTypesRegistry")
            .field("freshness_window", &self.cache.freshness_window)
            .finish_non_exhaustive()
    }
}

impl CachedTypesRegistry {
    /// Cache reads from `inner` using `settings`.
    ///
    /// `inner` must let every authenticated reader see every global entity, and gate
    /// `batch_get_entities` and `batch_resolve_identities` alike: a global snapshot
    /// read for one context serves all. The Types Registry local and REST clients
    /// qualify.
    #[must_use]
    pub fn new(inner: Arc<dyn TypesRegistryApi>, settings: CacheSettings) -> Self {
        Self {
            inner,
            cache: ReadCache::new(settings, true),
        }
    }

    fn context(ctx: &SecurityContext) -> Option<ReadContext> {
        (!ctx.is_anonymous()).then(|| {
            ReadContext::new(
                ctx.subject_id(),
                ctx.subject_type(),
                ctx.subject_tenant_id(),
                ctx.token_scopes(),
            )
        })
    }
}

#[async_trait]
impl TypesRegistryApi for CachedTypesRegistry {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let Some(context) = Self::context(ctx) else {
            return self.inner.batch_get_entities(ctx, request).await;
        };
        self.cache
            .read(request, Some(&context), |request| {
                self.inner.batch_get_entities(ctx, request)
            })
            .await
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let page = self.inner.list_entities(ctx, request).await?;
        // Discovery is a `list`: it teaches pairs but shows them to no read context.
        // Anonymous calls bypass the cache, so they teach nothing.
        if !ctx.is_anonymous() {
            self.cache.learn_page(&page)?;
        }
        Ok(page)
    }

    /// Resolve locally from a fresh read or resolution shown to this read context.
    /// A positive reply shows the pair to it, unless a revocation fenced the request.
    /// `not_found` revokes this read context's confirmation for the key and fences
    /// its in-flight confirmations, leaving the shared snapshot and pair available.
    async fn batch_resolve_identities(
        &self,
        ctx: &SecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        let Some(seen_by) =
            Self::context(ctx).filter(|_| !keys.is_empty() && keys.len() <= MAX_BATCH_GET_KEYS)
        else {
            return self.inner.batch_resolve_identities(ctx, keys).await;
        };
        let sent_at = Instant::now();
        let (mut answers, unknown, tickets) =
            self.cache.visible_identities(keys, &seen_by, sent_at);
        self.cache.metrics.resolutions(answers.len(), unknown.len());
        if unknown.is_empty() {
            return Ok(answers);
        }
        let fetched = self
            .inner
            .batch_resolve_identities(ctx, unknown.clone())
            .await?;
        let fetched = crate::ext::checked_resolutions(&unknown, fetched)?;
        let evictions = self.cache.update_store(|store| {
            // Validate against pinned and resident pairs.
            let held: Vec<&EntityIdentity> = answers.values().flatten().collect();
            let found = fetched
                .values()
                .flatten()
                .map(|identity| (&identity.gts_id, identity.gts_uuid));
            let evictions = ReadCache::learn_identities(store, &held, found.clone())?;
            if tickets
                .context
                .as_ref()
                .is_some_and(|ticket| store.context_ticket_current(ticket))
            {
                for (_, gts_uuid) in found {
                    store.confirm_resolved(gts_uuid, &seen_by, sent_at);
                }
            }
            let absent = fetched
                .iter()
                .filter_map(|(key, identity)| identity.is_none().then_some(key));
            store.revoke_visible(&seen_by, absent);
            Ok::<_, CanonicalError>(evictions)
        })?;
        drop(tickets);
        self.cache.metrics.evictions("identity", evictions);
        answers.extend(fetched);
        Ok(answers)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
