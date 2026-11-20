//! The client-side representation cache (SPEC §8.3, DESIGN "The client-side cache").

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, Condition,
    DeleteEntitiesRequest, DeletionOperation, DeletionOutcome, Entity, EntityField, EntityKey,
    EntityKind, EntityLookup, FieldSelection, IdempotencyKey, ListEntitiesRequest,
    ListEntitiesResponse, MAX_BATCH_GET_KEYS, Operation, RegisterEntitiesRequest,
    RegistrationOperation, RegistrationOutcome, Validator,
};

mod store;

use store::{EntryKey, Store, Ticket, Weight};

/// How long a confirmed entry is served without asking the registry again, and how
/// many bytes of snapshots the cache holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSettings {
    /// `0` is meaningful: every read revalidates.
    pub freshness_window: Duration,
    /// Bytes of cached snapshots, evicted least-recently-used.
    pub store_bound: usize,
}

impl CacheSettings {
    /// DESIGN's freshness window.
    pub const DEFAULT_FRESHNESS_WINDOW: Duration = Duration::from_secs(30);
    /// DESIGN's store bound: 64 MB.
    pub const DEFAULT_STORE_BOUND: usize = 64 * 1024 * 1024;
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            freshness_window: Self::DEFAULT_FRESHNESS_WINDOW,
            store_bound: Self::DEFAULT_STORE_BOUND,
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
pub struct CachedPlatformTypesRegistry {
    inner: Arc<dyn PlatformTypesRegistryApi>,
    freshness_window: Duration,
    /// The store's byte bound, so that a merge too large to keep is never built.
    store_bound: usize,
    /// Every critical section is a lookup or a few inserts and removals that leave the
    /// store consistent at each step, so a poisoned lock is recovered.
    store: Mutex<Store>,
}

/// The registry's bound on a key's identifier and on a validator (TR `MAX_KEY_LEN`, the
/// REST client's entity-tag bound). A request over it is the registry's to refuse.
const TOKEN_BOUND: usize = 1024;

/// Whether a validator stays within the bound on every transport. REST measures the
/// quoted entity-tag, so a validator two bytes short of the bound is already over it.
fn validator_fits(validator: &Validator) -> bool {
    validator.as_bytes().len() + 2 <= TOKEN_BOUND
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
        key: EntryKey,
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
    uuids: Vec<Uuid>,
}

impl Drop for Tickets<'_> {
    fn drop(&mut self) {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        for uuid in &self.uuids {
            store.release(*uuid);
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
            .field("freshness_window", &self.freshness_window)
            .finish_non_exhaustive()
    }
}

impl CachedPlatformTypesRegistry {
    /// Wraps `inner` in a new cache of its own: every call builds a separate store, so
    /// share one instance between consumers that should share cached reads. `settings`
    /// sets the freshness window and the byte budget of the stored snapshots.
    #[must_use]
    pub fn new(inner: Arc<dyn PlatformTypesRegistryApi>, settings: CacheSettings) -> Self {
        Self {
            inner,
            freshness_window: settings.freshness_window,
            store_bound: settings.store_bound,
            store: Mutex::new(Store::new(settings.store_bound)),
        }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
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
    ) -> (Vec<(EntityKey, Plan)>, Tickets<'_>) {
        // Declared before the guard, so that an unwind drops the guard first and the
        // tickets' release can take the lock.
        let mut tickets = Tickets {
            store: &self.store,
            uuids: Vec::new(),
        };
        let mut store = self.store();
        let mut seen = HashSet::new();
        let mut plans = Vec::with_capacity(request.items.len());
        for item in &request.items {
            if !seen.insert(&item.key) {
                continue;
            }
            let key = EntryKey::new(item.key.to_uuid());
            // Only a condition for this entity that covers the read is the caller's.
            let caller = item.validator_for(&request.projection).cloned();
            let plan = match store.get(&key) {
                Some(entry)
                    if entry.covers.is_superset(selection)
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
                    tickets.uuids.push(key.uuid);
                    Plan::Remote {
                        ticket: store.ticket(key.uuid),
                        sent: match &resident {
                            Some(r) if pinned => Some(r.etag.clone()),
                            _ => caller.clone(),
                        },
                        key,
                        caller,
                        resident,
                        pinned,
                    }
                }
            };
            plans.push((item.key.clone(), plan));
        }
        (plans, tickets)
    }

    /// Records verified answers. Only map operations run under the lock. An answer to a
    /// read during which anything else was recorded for its entity changes nothing (the
    /// late-fill guard); the caller still receives it.
    fn commit(&self, plans: &[(EntityKey, Plan)], verified: &[Option<Verified>], sent_at: Instant) {
        let mut store = self.store();
        // Judged before any of this batch is applied: its keys were ticketed together, so
        // what one of them records is not news to another.
        let quiet: Vec<bool> = plans
            .iter()
            .map(|(_, plan)| match plan {
                Plan::Remote { key, ticket, .. } => store.quiet_since(key.uuid, *ticket),
                Plan::Local { .. } => false,
            })
            .collect();
        for (((_, plan), verified), quiet) in plans.iter().zip(verified).zip(quiet) {
            let (
                Plan::Remote {
                    key,
                    sent,
                    resident,
                    ..
                },
                Some(verified),
            ) = (plan, verified)
            else {
                continue;
            };
            if !quiet {
                // Something was recorded for this entity while the read was in flight — an
                // observed mutation, or another read's answer — and nothing orders states,
                // so this answer fills nothing. Unless it agrees with what is resident, one
                // of the two is stale: drop the entity's entries and let the next read refetch.
                let agrees = match verified {
                    Verified::Found { etag, .. } | Verified::Matched(etag) => {
                        store.peek(key).is_some_and(|entry| entry.etag == *etag)
                    }
                    // The registry confirmed the entry this read held: still agreement
                    // only if that is what is resident now.
                    Verified::Confirmed => resident.as_ref().is_some_and(|ours| {
                        store.peek(key).is_some_and(|entry| entry.etag == ours.etag)
                    }),
                    Verified::NotFound => false,
                };
                if !agrees {
                    store.invalidate(key.uuid);
                }
                continue;
            }
            // Quiet: nothing outside this batch was recorded since the lookup. Another key of
            // this batch naming the same entity may have just been applied, so decisions are
            // taken against the entry as it is now, not as the lookup saw it.
            let current = store.peek(key).map(|entry| entry.etag.clone());
            match verified {
                Verified::Found { etag, keep, .. } => {
                    let same = current.as_ref() == Some(etag);
                    match keep {
                        // Oversize policy: under the same validator, an answer too large to
                        // keep (with what the entry holds, or alone) leaves the entry as it
                        // is, confirmed, rather than losing a smaller valid holding.
                        Some(kept) if !same || store.fits(&kept.weight) => {
                            if !same && current.is_some()
                                || sent.as_ref().is_some_and(|held| held != etag)
                            {
                                // A validator this read held is no longer current: a new
                                // state, so reads of the old one started earlier must not
                                // fill. An answer too large to keep still drops the stale
                                // entry.
                                store.invalidate(key.uuid);
                            }
                            store.insert(
                                *key,
                                Arc::clone(&kept.snapshot),
                                kept.covers.clone(),
                                kept.weight.clone(),
                                etag.clone(),
                                sent_at,
                            );
                        }
                        _ if same => store.confirm(key, etag, sent_at),
                        // The union was not built for a state that is no longer resident:
                        // whatever is resident now is another state, and stale.
                        _ => store.invalidate(key.uuid),
                    }
                }
                Verified::Confirmed => {
                    if let Some(ours) = resident {
                        store.confirm(key, &ours.etag, sent_at);
                    }
                }
                // The caller's validator is current: so is the entry, if it holds the same
                // state; otherwise the entry is stale.
                Verified::Matched(etag) => match current {
                    Some(held) if held == *etag => store.confirm(key, etag, sent_at),
                    Some(_) => store.invalidate(key.uuid),
                    None => {}
                },
                Verified::NotFound => store.invalidate(key.uuid),
            }
            store.record(key.uuid);
        }
    }

    /// Invalidates every entity a terminal successful registration committed.
    fn observe_registration(&self, operation: &RegistrationOperation) {
        let committed: Vec<Uuid> = operation
            .items
            .iter()
            .filter(|item| matches!(item.outcome, RegistrationOutcome::Succeeded { .. }))
            .map(|item| item.gts_id.to_uuid())
            .collect();
        self.invalidate(&committed);
    }

    /// Invalidates every entity a terminal successful deletion committed.
    fn observe_deletion(&self, operation: &DeletionOperation) {
        let committed: Vec<Uuid> = operation
            .items
            .iter()
            .filter(|item| matches!(item.outcome, DeletionOutcome::Succeeded { .. }))
            .map(|item| item.entity_key.to_uuid())
            .collect();
        self.invalidate(&committed);
    }

    fn invalidate(&self, uuids: &[Uuid]) {
        if uuids.is_empty() {
            return;
        }
        let mut store = self.store();
        for uuid in uuids {
            store.invalidate(*uuid);
        }
    }
}

/// Checks every answer against the request that was sent before any of it is cached:
/// one answer per sent key and no other, the asked entity in a consistent shape, and
/// `Unchanged` only to the exact validator sent. A `Found` under the entry's validator is
/// merged with what the entry holds, outside the lock.
fn verify(
    plans: &[(EntityKey, Plan)],
    selection: &FieldSelection,
    store_bound: usize,
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
                if !names(&entity, key) || !crate::ext::consistent(&entity) {
                    return Err(protocol_fault(key, "another or an inconsistent entity"));
                }
                if !shaped(&entity, selection) {
                    return Err(protocol_fault(key, "fields other than those selected"));
                }
                if !validator_fits(&etag) {
                    return Err(protocol_fault(key, "a validator over the bound"));
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
                        weight.fits(store_bound).then(|| Kept {
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

/// Whether `entity` is the one `key` names, with a consistent identity.
fn names(entity: &Entity, key: &EntityKey) -> bool {
    let consistent = entity.gts_id.to_uuid() == entity.gts_uuid;
    consistent
        && match key {
            EntityKey::GtsId(id) => *id == entity.gts_id,
            EntityKey::GtsUuid(uuid) => *uuid == entity.gts_uuid,
        }
}

/// Whether the snapshot carries exactly the fields `selection` asks for that apply to its
/// kind, as the registry answers them: content and origin on either kind, the derived
/// documents and provenance on a Type Schema only. A selected JSON `null` is present.
fn shaped(entity: &Entity, selection: &FieldSelection) -> bool {
    let schema = entity.kind == EntityKind::TypeSchema;
    let expected = |field: EntityField, applicable: bool, present: bool| {
        present == (applicable && selection.contains(field))
    };
    expected(EntityField::Origin, true, entity.origin.is_some())
        && expected(EntityField::Content, true, entity.content.is_some())
        && expected(
            EntityField::ResolvedSchema,
            schema,
            entity.resolved_schema.is_some(),
        )
        && expected(
            EntityField::EffectiveTraits,
            schema,
            entity.effective_traits.is_some(),
        )
        && expected(
            EntityField::EffectiveTraitsSchema,
            schema,
            entity.effective_traits_schema.is_some(),
        )
        && expected(EntityField::Provenance, schema, entity.provenance.is_some())
}

/// A key or validator over the registry's bounds, whatever the cache holds: forwarded
/// intact, so the registry refuses it with its own error.
fn over_token_bound(request: &BatchGetEntitiesRequest) -> bool {
    request.items.iter().any(|item| {
        matches!(&item.key, EntityKey::GtsId(id) if id.as_ref().len() > TOKEN_BOUND)
            || item
                .validator_for(&request.projection)
                .is_some_and(|v| !validator_fits(v))
    })
}

#[async_trait]
impl PlatformTypesRegistryApi for CachedPlatformTypesRegistry {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        if request.items.is_empty() || request.items.len() > MAX_BATCH_GET_KEYS {
            // Refused for its size whatever it names.
            return self.inner.batch_get_entities(ctx, request).await;
        }
        if over_token_bound(&request) {
            // The registry decides; its answers are not checked against a plan, so a
            // successful one records nothing and drops what it may contradict.
            let named: Vec<Uuid> = request
                .items
                .iter()
                .map(|item| item.key.to_uuid())
                .collect();
            let response = self.inner.batch_get_entities(ctx, request).await?;
            self.invalidate(&named);
            return Ok(response);
        }
        let now = Instant::now();
        let selection = request.projection.normalized();
        let (plans, tickets) = self.plan(&request, &selection, now);
        let remote: Vec<BatchGetItem> = plans
            .iter()
            .filter_map(|(key, plan)| match plan {
                Plan::Local { .. } => None,
                Plan::Remote { sent, .. } => Some(BatchGetItem {
                    key: key.clone(),
                    // Covers exactly this read, so every transport sends it.
                    if_none_match: sent
                        .as_ref()
                        .map(|etag| Condition::new(key, etag.clone(), request.projection.clone())),
                }),
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
            self.inner.batch_get_entities(ctx, inner).await?.0
        };
        let verified = verify(&plans, &selection, self.store_bound, answers)?;
        let response = respond(&plans, &selection, &verified)?;
        self.commit(&plans, &verified, now);
        drop(tickets);
        Ok(response)
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.inner.list_entities(ctx, request).await
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

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
