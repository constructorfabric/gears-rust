//! The client-side representation cache (SPEC §8.3, DESIGN "The client-side cache").

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
    MAX_BATCH_GET_KEYS, Operation, RegisterEntitiesRequest, RegistrationOperation,
    RegistrationOutcome, Validator,
};

mod identity;
mod store;

use store::{Store, TenantTicket, TenantViewer, Ticket, Weight};

/// Freshness and retention limits for Types Registry client caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSettings {
    /// `0` is meaningful: every snapshot read revalidates.
    pub freshness_window: Duration,
    /// Maximum total bytes of compact JSON snapshots and their raw validators.
    /// The cache evicts least-recently-used snapshots.
    pub max_size_bytes: usize,
    /// Maximum number of resident snapshots. Zero disables snapshot retention.
    /// A tenant cache uses the same limit for its separate visibility records.
    pub max_entries: usize,
    /// Maximum number of identities retained for the platform `resolve_*` helpers.
    /// Each identity holds a Registry Reference and a GTS identifier. With zero entries, a
    /// snapshot inside the freshness window can still answer a key, and other keys go to
    /// the registry.
    pub max_identity_entries: usize,
}

impl CacheSettings {
    /// DESIGN's freshness window.
    pub const DEFAULT_FRESHNESS_WINDOW: Duration = Duration::from_secs(30);
    /// Default snapshot cache size limit (64 MiB).
    pub const DEFAULT_MAX_SIZE_BYTES: usize = 64 * 1024 * 1024;
    /// Default maximum number of resident snapshots.
    pub const DEFAULT_MAX_ENTRIES: usize = 16 * 1024;
    /// 16 384 identities: a few MB at typical identifier lengths, 16 MB of identifier
    /// text at the most.
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

/// A [`PlatformTypesRegistryApi`] that serves batch reads from a bounded, per-client
/// cache and forwards everything else.
///
/// One entry per entity holds every field read of it under one validator, which names
/// the entity's state under every projection. A read the entry covers is answered from
/// it, projected to exactly the fields asked; any other goes to the registry, and its
/// answer is merged into the entry under the same validator or replaces it under a
/// new one.
///
/// Beside the entries, a separate bounded identity store keeps the canonical identifier
/// of each entity a read found, for
/// [`batch_resolve_identities`](PlatformTypesRegistryApi::batch_resolve_identities) and
/// so the `resolve_*` helpers: known identities are answered from it with no window, and
/// only the others are read, as a batch read under the mandatory fields.
///
/// An identifier and a Registry Reference are one entity only because the registry said
/// so: nothing derives one from the other. A key finds an entry by the spelling its
/// resident snapshot carries, and every answer must name the key asked and agree with
/// every pair the cache knows — resident or remembered, and within the batch — or the
/// whole read fails as a protocol fault and changes nothing.
pub struct CachedPlatformTypesRegistry {
    inner: Arc<dyn PlatformTypesRegistryApi>,
    cache: ReadCache,
}

struct ReadCache {
    freshness_window: Duration,
    /// The store's byte bound, so that a merge too large to keep is never built.
    max_size_bytes: usize,
    /// The snapshot store and the identity store it holds. Every critical section is a
    /// lookup or a few inserts and removals that leave both consistent at each step, so a
    /// poisoned lock is recovered.
    store: Mutex<Store>,
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

/// The tickets one batch read holds, released when it completes, fails or is dropped.
struct Tickets<'a> {
    store: &'a Mutex<Store>,
    held: Vec<Ticket>,
    tenant: Option<TenantTicket>,
}

impl Drop for Tickets<'_> {
    fn drop(&mut self) {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        for ticket in &self.held {
            store.release(ticket);
        }
        if let Some(ticket) = self.tenant {
            store.release_tenant_ticket(ticket);
        }
    }
}

/// A registry answer checked against what was sent, ready to commit without further work.
enum Verified {
    Found {
        /// The answer as read: exactly the selected fields.
        snapshot: Arc<Entity>,
        etag: Validator,
        /// What the entry is to hold, or `None` when the answer and what the entry held
        /// of the same state are too large together: the entry then stays as it is.
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
    /// Wraps `inner` in a new cache of its own: every call builds a separate store, so
    /// share one instance between consumers that should share cached reads. `settings`
    /// sets its freshness window and retention limits.
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
        if tenant {
            store.track_tenant_visibility();
        } else {
            store.remember_identities(settings.max_identity_entries);
        }
        Self {
            freshness_window: settings.freshness_window,
            max_size_bytes: settings.max_size_bytes,
            store: Mutex::new(store),
        }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A batch read through the cache.
    async fn read<F>(
        &self,
        request: BatchGetEntitiesRequest,
        viewer: Option<TenantViewer>,
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
        let (plans, tickets) = self.plan(&request, &selection, now, viewer);
        let remote: Vec<BatchGetItem> = plans
            .iter()
            .filter_map(|(key, plan)| match plan {
                Plan::Local { .. } => None,
                Plan::Remote { sent, .. } => Some(BatchGetItem::for_cache_read(
                    key.clone(),
                    sent.clone(),
                    &selection,
                )),
            })
            .collect();
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
        self.settle(&plans, &verified, now, tickets.tenant)?;
        drop(tickets);
        Ok(response)
    }

    /// Commit snapshots only after all pairs have passed the shared identity checks.
    fn settle(
        &self,
        plans: &[(EntityKey, Plan)],
        verified: &[Option<Verified>],
        sent_at: Instant,
        tenant: Option<TenantTicket>,
    ) -> Result<(), CanonicalError> {
        let mut store = self.store();
        let found = plans
            .iter()
            .zip(verified)
            .filter_map(|((_, plan), answer)| identified(plan, answer.as_ref()))
            .map(|snapshot| (&snapshot.gts_id, snapshot.gts_uuid));
        Self::learn_identities(&mut store, &[], found)?;
        commit(&mut store, plans, verified, sent_at, tenant);
        Ok(())
    }

    /// Validate the complete result before changing LRU membership or recency. `held` are
    /// identities the caller answers beside these: an answer contradicting one fails like
    /// any other contradiction, though both stores may have lost it while the request was
    /// in flight.
    fn learn_identities<'a>(
        store: &mut Store,
        held: &[&'a EntityIdentity],
        found: impl Iterator<Item = (&'a GtsId, Uuid)> + Clone,
    ) -> Result<(), CanonicalError> {
        // Pairs the caller holds beside this read are part of the batch: checked against,
        // not learned again.
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
        for (gts_id, gts_uuid) in found {
            store.identities.learn(gts_id, gts_uuid);
        }
        Ok(())
    }

    /// Every distinct key this cache can resolve without the registry: from the identity
    /// store, which no window bounds, or else from a resident snapshot confirmed inside the
    /// window. The others, in first-asked order, are left for the registry.
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

    /// Every distinct key a resident snapshot answers for `viewer`: one whose visibility
    /// the registry confirmed for this viewer inside the window, under its current epoch.
    /// The others, in first-asked order, are left for the registry.
    fn visible_identities(
        &self,
        keys: Vec<EntityKey>,
        viewer: TenantViewer,
        now: Instant,
    ) -> (HashMap<EntityKey, Option<EntityIdentity>>, Vec<EntityKey>) {
        let mut store = self.store();
        let mut answers = HashMap::with_capacity(keys.len());
        let mut unknown = Vec::new();
        for key in keys {
            if answers.contains_key(&key) || unknown.contains(&key) {
                continue;
            }
            // Only a visible entry is marked used: a viewer refused it leaves its recency.
            let entry = if store.tenant_visible(&key, viewer, now, self.freshness_window) {
                store.get(&key)
            } else {
                None
            };
            match entry {
                Some(entry) => {
                    let identity = EntityIdentity {
                        gts_id: entry.snapshot.gts_id.clone(),
                        gts_uuid: entry.snapshot.gts_uuid,
                    };
                    answers.insert(key, Some(identity));
                }
                None => unknown.push(key),
            }
        }
        (answers, unknown)
    }

    /// Decides, per distinct key, whether the cache answers it or the registry does, and
    /// with which condition. A repeated key keeps its first mention, as the registry does.
    /// Each key sent to the registry takes a ticket in the same critical section as its
    /// lookup, so a mutation observed after the lookup fences its answer.
    fn plan(
        &self,
        request: &BatchGetEntitiesRequest,
        selection: &FieldSelection,
        now: Instant,
        viewer: Option<TenantViewer>,
    ) -> (Vec<(EntityKey, Plan)>, Tickets<'_>) {
        // Declared before the guard, so that an unwind drops the guard first and the
        // tickets' release can take the lock.
        let mut tickets = Tickets {
            store: &self.store,
            held: Vec::new(),
            tenant: None,
        };
        let mut store = self.store();
        let mut seen = HashSet::new();
        let mut plans = Vec::with_capacity(request.items.len());
        for item in &request.items {
            if !seen.insert(&item.key) {
                continue;
            }
            // Only a condition for this key that covers the read is the caller's.
            let caller = item.if_none_match_for(&request.projection).cloned();
            // Only an entry whose snapshot carries the key's own spelling answers,
            // conditions or merges it.
            let visible = viewer.is_none_or(|viewer| {
                store.tenant_visible(&item.key, viewer, now, self.freshness_window)
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
                    let resident = entry.map(|e| Resident {
                        snapshot: Arc::clone(&e.snapshot),
                        covers: e.covers.clone(),
                        etag: e.etag.clone(),
                        weight: e.weight.clone(),
                    });
                    let pinned = resident
                        .as_ref()
                        .is_some_and(|r| r.covers.is_superset(selection));
                    let ticket = store.ticket(&item.key);
                    tickets.held.push(ticket.clone());
                    Plan::Remote {
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
            };
            plans.push((item.key.clone(), plan));
        }
        if let Some(viewer) = viewer
            && !tickets.held.is_empty()
        {
            tickets.tenant = Some(store.tenant_ticket(viewer));
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

    /// Invalidates each entity by the key an operation or a forwarded read named it with,
    /// and by the counterpart a resident snapshot carries; with the key alone it fences
    /// every read in flight that knew only its own key, since the two may be one entity.
    fn invalidate(&self, keys: &[EntityKey]) {
        if keys.is_empty() {
            return;
        }
        let mut store = self.cache.store();
        for key in keys {
            store.invalidate(key, None);
        }
    }
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

/// Records verified answers whose identities were checked. Only map operations run under
/// the lock. An answer to a read during which anything else was recorded for its entity
/// changes nothing (the late-fill guard); the caller still receives it.
fn commit(
    store: &mut Store,
    plans: &[(EntityKey, Plan)],
    verified: &[Option<Verified>],
    sent_at: Instant,
    tenant: Option<TenantTicket>,
) {
    // Judged before any of this batch is applied: its keys were ticketed together, so
    // what one of them records is not news to another.
    let quiet: Vec<bool> = plans
        .iter()
        .map(|(_, plan)| match plan {
            Plan::Remote { ticket, .. } => store.quiet_since(ticket),
            Plan::Local { .. } => false,
        })
        .collect();
    let confirmed_viewer = tenant
        .filter(|ticket| store.tenant_ticket_current(*ticket))
        .map(|ticket| ticket.viewer);
    for (((asked, plan), verified), quiet) in plans.iter().zip(verified).zip(quiet) {
        let (Plan::Remote { sent, resident, .. }, Some(verified)) = (plan, verified) else {
            continue;
        };
        if tenant.is_some() && matches!(verified, Verified::NotFound) {
            continue;
        }
        let pair = stated(plan, verified);
        // The entry's validator when an entry carries the spelling asked.
        let current = store.peek(asked).map(|entry| entry.etag.clone());
        if !quiet {
            // Something was recorded for this entity while the read was in flight — an
            // observed mutation, or another read's answer — and nothing orders states,
            // so this answer fills nothing. Unless it agrees with what is resident, one
            // of the two is stale: drop the entity's entries and let the next read refetch.
            let agrees = match verified {
                Verified::Found { etag, .. } | Verified::Matched(etag) => {
                    current.as_ref() == Some(etag)
                }
                // The registry confirmed the entry this read held: still agreement
                // only if that is what is resident now.
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
        // Quiet: nothing outside this batch was recorded since the lookup. Another key of
        // this batch naming the same entity may have just been applied, so decisions are
        // taken against the entry as it is now, not as the lookup saw it.
        match verified {
            Verified::Found { etag, keep, .. } => {
                let same = current.as_ref() == Some(etag);
                match keep {
                    // Oversize policy: under the same validator, an answer too large to
                    // keep (with what the entry holds, or alone) leaves the entry as it
                    // is, confirmed, rather than losing a smaller valid holding.
                    Some(kept) if !same || store.fits(&kept.weight, etag) => {
                        if !same && current.is_some()
                            || sent.as_ref().is_some_and(|held| held != etag)
                        {
                            // A validator this read held is no longer current: a new
                            // state, so reads of the old one started earlier must not
                            // fill. An answer too large to keep still drops the stale
                            // entry.
                            store.invalidate(asked, pair);
                        }
                        store.insert(
                            Arc::clone(&kept.snapshot),
                            kept.covers.clone(),
                            kept.weight.clone(),
                            etag.clone(),
                            sent_at,
                        );
                    }
                    _ if same => store.confirm(asked, etag, sent_at),
                    // The union was not built for a state that is no longer resident:
                    // whatever is resident now is another state, and stale.
                    _ => store.invalidate(asked, pair),
                }
            }
            Verified::Confirmed => {
                if let Some(ours) = resident {
                    store.confirm(asked, &ours.etag, sent_at);
                }
            }
            // The caller's validator is current: so is the entry, if it holds the same
            // state; otherwise the entry is stale.
            Verified::Matched(etag) => match current {
                Some(held) if held == *etag => store.confirm(asked, etag, sent_at),
                Some(_) => store.invalidate(asked, pair),
                None => {}
            },
            Verified::NotFound => store.invalidate(asked, pair),
        }
        if let Some(viewer) = confirmed_viewer
            && let Some(etag) = match verified {
                Verified::Found { etag, .. } | Verified::Matched(etag) => Some(etag),
                Verified::Confirmed => sent.as_ref(),
                Verified::NotFound => None,
            }
        {
            store.confirm_tenant(asked, viewer, etag, sent_at);
        }
        store.record(asked, pair);
    }
    if let Some(ticket) = tenant {
        let absent = plans.iter().zip(verified).filter_map(|((key, _), answer)| {
            matches!(answer, Some(Verified::NotFound)).then_some(key)
        });
        store.revoke_tenant_reads(ticket.viewer, absent);
    }
}

/// Checks every answer against the request that was sent before any of it is cached:
/// one answer per sent key and no other, the asked entity in a consistent shape, and
/// `Unchanged` only to the exact validator sent. A `Found` under the entry's validator is
/// merged with what the entry holds, outside the lock.
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
                    // One state: the entry's other fields stay beside the answer, if the
                    // two fit together. Judged on measured weights, so a union too large to
                    // keep is never built; the entry then keeps what it already holds.
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

/// The caller's response, built outside the lock and before anything is committed:
/// exactly the selected fields, and `Unchanged` only where the caller itself sent the
/// matching validator. Every pairing `verify` can produce is listed; any other is a
/// broken invariant and fails the read rather than posing as an absent entity.
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

/// Whether the snapshot carries exactly the fields `selection` asks for that apply to its
/// kind, as the registry answers them. A selected JSON `null` is present.
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

    /// Known identities from the identity store, which no window bounds, then from a
    /// resident snapshot inside the window; every other key resolved by the registry. A
    /// batch the registry refuses for its size is sent whole.
    async fn batch_resolve_identities(
        &self,
        ctx: &PlatformSecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        if keys.is_empty() || keys.len() > MAX_BATCH_GET_KEYS {
            return self.inner.batch_resolve_identities(ctx, keys).await;
        }
        let (mut answers, unknown) = self.cache.known_identities(keys, Instant::now());
        if unknown.is_empty() {
            return Ok(answers);
        }
        let fetched = self
            .inner
            .batch_resolve_identities(ctx, unknown.clone())
            .await?;
        let fetched = crate::ext::checked_resolutions(&unknown, fetched)?;
        // The identities answered so far are held while the rest are resolved: an answer
        // contradicting one of them fails the resolution, and is learned nowhere.
        let held: Vec<&EntityIdentity> = answers.values().flatten().collect();
        ReadCache::learn_identities(
            &mut self.cache.store(),
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
        ReadCache::learn_identities(
            &mut self.cache.store(),
            &[],
            page.items
                .iter()
                .map(|entity| (&entity.gts_id, entity.gts_uuid)),
        )?;
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

/// Tenant reads over one snapshot cache. A tenant can reuse a snapshot after the
/// registry has confirmed visibility for that tenant within the freshness window.
pub struct CachedTypesRegistry {
    inner: Arc<dyn TypesRegistryApi>,
    cache: ReadCache,
    binding: TenantCacheBinding,
}

impl std::fmt::Debug for CachedTypesRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let binding = match self.binding {
            TenantCacheBinding::TrustedLocal => "trusted_local",
            #[cfg(feature = "rest-client")]
            TenantCacheBinding::Bearer => "bearer",
        };
        f.debug_struct("CachedTypesRegistry")
            .field("freshness_window", &self.cache.freshness_window)
            .field("binding", &binding)
            .finish_non_exhaustive()
    }
}

enum TenantCacheBinding {
    TrustedLocal,
    #[cfg(feature = "rest-client")]
    Bearer,
}

impl CachedTypesRegistry {
    /// Cache a trusted in-process client whose visibility is tenant-wide: a confirmation
    /// is shared by every caller of the tenant. Use only with a client whose visibility
    /// does not depend on the caller's credential; a REST client takes
    /// `new_rest`.
    #[must_use]
    pub fn new_local(inner: Arc<dyn TypesRegistryApi>, settings: CacheSettings) -> Self {
        Self {
            inner,
            cache: ReadCache::new(settings, true),
            binding: TenantCacheBinding::TrustedLocal,
        }
    }

    /// Cache a REST client that forwards the caller's bearer to the registry.
    #[cfg(feature = "rest-client")]
    #[must_use]
    pub fn new_rest(inner: Arc<dyn TypesRegistryApi>, settings: CacheSettings) -> Self {
        Self {
            inner,
            cache: ReadCache::new(settings, true),
            binding: TenantCacheBinding::Bearer,
        }
    }

    fn viewer(&self, ctx: &SecurityContext) -> Option<TenantViewer> {
        if ctx.is_anonymous() {
            return None;
        }
        let credential = match self.binding {
            TenantCacheBinding::TrustedLocal => None,
            #[cfg(feature = "rest-client")]
            TenantCacheBinding::Bearer => {
                use secrecy::ExposeSecret as _;
                use toolkit_security::crypto::hash::Sha256;

                let bearer = ctx.bearer_token()?;
                Some(Sha256::digest(bearer.expose_secret().as_bytes()))
            }
        };
        Some(TenantViewer::new(ctx.subject_tenant_id(), credential))
    }

    fn revoke_if_refused<T>(
        &self,
        viewer: Option<TenantViewer>,
        result: &Result<T, CanonicalError>,
    ) {
        if let Some(viewer) = viewer
            && matches!(
                result,
                Err(CanonicalError::Unauthenticated { .. }
                    | CanonicalError::PermissionDenied { .. })
            )
        {
            self.cache.store().revoke_viewer(viewer);
        }
    }
}

#[async_trait]
impl TypesRegistryApi for CachedTypesRegistry {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let Some(viewer) = self.viewer(ctx) else {
            return self.inner.batch_get_entities(ctx, request).await;
        };
        self.cache
            .read(request, Some(viewer), |request| async {
                let result = self.inner.batch_get_entities(ctx, request).await;
                self.revoke_if_refused(Some(viewer), &result);
                result
            })
            .await
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let result = self.inner.list_entities(ctx, request).await;
        self.revoke_if_refused(self.viewer(ctx), &result);
        result
    }

    /// A resident snapshot answers a key only under a fresh visibility confirmation for
    /// this viewer; every other key is resolved by the registry for this caller. An answer
    /// confirms nothing — it carries no validator — but `not_found` withdraws this viewer's
    /// confirmation of the key, and fences its confirmations in flight, as a batch read's
    /// absence does; other viewers' confirmations and the snapshot stay.
    async fn batch_resolve_identities(
        &self,
        ctx: &SecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        let viewer = self.viewer(ctx);
        let Some(seen_by) = viewer.filter(|_| !keys.is_empty() && keys.len() <= MAX_BATCH_GET_KEYS)
        else {
            let result = self.inner.batch_resolve_identities(ctx, keys).await;
            self.revoke_if_refused(viewer, &result);
            return result;
        };
        let (mut answers, unknown) = self.cache.visible_identities(keys, seen_by, Instant::now());
        if unknown.is_empty() {
            return Ok(answers);
        }
        let result = self
            .inner
            .batch_resolve_identities(ctx, unknown.clone())
            .await;
        self.revoke_if_refused(viewer, &result);
        let fetched = crate::ext::checked_resolutions(&unknown, result?)?;
        {
            let mut store = self.cache.store();
            // Checked against the snapshots answered beside it and every resident one,
            // as a batch read's answers are; a tenant cache learns no identity.
            let held: Vec<&EntityIdentity> = answers.values().flatten().collect();
            ReadCache::learn_identities(
                &mut store,
                &held,
                fetched
                    .values()
                    .flatten()
                    .map(|identity| (&identity.gts_id, identity.gts_uuid)),
            )?;
            let absent = fetched
                .iter()
                .filter_map(|(key, identity)| identity.is_none().then_some(key));
            store.revoke_tenant_reads(seen_by, absent);
        }
        answers.extend(fetched);
        Ok(answers)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
