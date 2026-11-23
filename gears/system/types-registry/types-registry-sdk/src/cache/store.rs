//! Snapshot LRU bounded by charged bytes and entry count. Both keys address
//! one snapshot holding fields read under one validator.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Weak};
use std::time::Duration;

use gts::GtsId;
use lru::LruCache;
use tokio::time::Instant;
use uuid::Uuid;

use super::identity::Identities;
use crate::models::{
    Entity, EntityField, EntityIdentity, EntityKey, FieldSelection, JsonDocument, Validator,
};

/// Who a tenant read is answered for: the subject, its type, home tenant and token
/// scopes. The registry may authorize each entity per subject, so a positive answer
/// to one read context proves nothing to another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReadContext {
    subject: Uuid,
    subject_type: Option<Arc<str>>,
    /// Visibility follows the home tenant's ancestor chain. A Context Tenant, once
    /// the SDK carries one, joins rather than replaces it.
    tenant: Uuid,
    /// As presented. Equivalent spellings stay distinct rather than assuming how
    /// the policy reads them.
    scopes: Arc<[String]>,
}

impl ReadContext {
    pub fn new(subject: Uuid, subject_type: Option<&str>, tenant: Uuid, scopes: &[String]) -> Self {
        Self {
            subject,
            subject_type: subject_type.map(Arc::from),
            tenant,
            scopes: scopes.into(),
        }
    }
}

/// That one entity is visible to one read context.
#[derive(Default)]
struct Confirmation {
    /// From a read: the snapshot version it confirmed. Serves reads and resolutions.
    content: Option<Proof>,
    /// From a read or a resolution: the remembered pair it confirmed. Serves resolutions.
    identity: Option<Proof>,
}

/// What the registry showed, held weakly, and when the request that saw it started.
struct Proof {
    version: Weak<()>,
    at: Instant,
}

impl Proof {
    /// Whether `current` is still what was shown, inside the window.
    fn live(&self, current: Option<&Arc<()>>, now: Instant, window: Duration) -> bool {
        current.is_some_and(|current| std::ptr::eq(self.version.as_ptr(), Arc::as_ptr(current)))
            && now.saturating_duration_since(self.at) < window
    }

    /// Record a showing of `version`. A later reply from an earlier request never
    /// moves the time back. Only one version is current, so another one is dead.
    fn renew(slot: &mut Option<Self>, version: &Arc<()>, at: Instant) {
        match slot {
            Some(proof) if std::ptr::eq(proof.version.as_ptr(), Arc::as_ptr(version)) => {
                proof.at = proof.at.max(at);
            }
            _ => {
                *slot = Some(Self {
                    version: Arc::downgrade(version),
                    at,
                });
            }
        }
    }
}

/// A resident entity: the fields read of it so far, all under `etag`.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Carries exactly the fields of `covers` that apply to its kind.
    pub snapshot: Arc<Entity>,
    /// The fields read under `etag`, the mandatory ones included. A read is answered
    /// from the entry only when these include every field it selects.
    pub covers: FieldSelection,
    pub etag: Validator,
    /// When the registry last confirmed `etag` as current.
    pub confirmed_at: Instant,
    pub weight: Weight,
    /// Confirmations hold it weakly, so a new validator retires them all.
    confirmed_version: Option<Arc<()>>,
}

/// Fixed charge for metadata and per-entry bookkeeping.
const ENTRY_ALLOWANCE_BYTES: usize = 256;

/// Snapshot weight before the validator charge. Keeping compact JSON sizes by
/// field lets merges reuse resident weights, including the four bytes for `null`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Weight {
    /// GTS ID bytes plus the fixed allowance.
    fixed: usize,
    /// Compact JSON bytes of each document field the snapshot carries.
    documents: BTreeMap<EntityField, usize>,
}

impl Weight {
    /// The snapshot's bytes; `usize::MAX` when any part could not be measured.
    pub fn total(&self) -> usize {
        self.documents
            .values()
            .fold(self.fixed, |sum, bytes| sum.saturating_add(*bytes))
    }

    /// Total resident bytes, including the raw validator needed for revalidation.
    pub fn bytes_with_validator(&self, etag: &Validator) -> usize {
        self.total().saturating_add(etag.as_bytes().len())
    }

    /// Whether a snapshot of this weight can be retained under `bound` at all.
    pub fn fits(&self, bound: usize, etag: &Validator) -> bool {
        // `usize::MAX` is the unmeasurable charge: never retained, whatever the bound.
        let bytes = self.bytes_with_validator(etag);
        bytes != usize::MAX && bytes <= bound
    }

    /// Weight of [`merge`]'s result, with resident documents outside `selection`.
    pub fn merged(&self, selection: &FieldSelection, resident: &Self) -> Self {
        let mut documents = self.documents.clone();
        for (field, bytes) in &resident.documents {
            if !selection.contains(*field) {
                documents.insert(*field, *bytes);
            }
        }
        Self {
            fixed: self.fixed,
            documents,
        }
    }
}

pub fn weight(snapshot: &Entity) -> Weight {
    // A new field must be assigned a charge.
    let Entity {
        gts_id,
        gts_uuid: _,
        kind: _,
        lifecycle_status: _,
        origin: _,
        content,
        resolved_schema,
        effective_traits,
        effective_traits_schema,
        provenance: _,
    } = snapshot;
    let documents = [
        (EntityField::Content, content),
        (EntityField::ResolvedSchema, resolved_schema),
        (EntityField::EffectiveTraits, effective_traits),
        (EntityField::EffectiveTraitsSchema, effective_traits_schema),
    ]
    .into_iter()
    .filter_map(|(field, document)| Some((field, json_len(document.as_ref()?))))
    .collect();
    Weight {
        fixed: gts_id.as_ref().len().saturating_add(ENTRY_ALLOWANCE_BYTES),
        documents,
    }
}

/// `snapshot` as a read selecting `selection` is answered: every other field dropped.
/// `snapshot` carries at least the applicable fields of `selection`.
pub fn project(snapshot: &Entity, selection: &FieldSelection) -> Entity {
    let keep = |field: EntityField| selection.contains(field);
    Entity {
        gts_id: snapshot.gts_id.clone(),
        gts_uuid: snapshot.gts_uuid,
        kind: snapshot.kind,
        lifecycle_status: snapshot.lifecycle_status,
        origin: snapshot.origin.filter(|_| keep(EntityField::Origin)),
        content: kept(snapshot.content.as_ref(), keep(EntityField::Content)),
        resolved_schema: kept(
            snapshot.resolved_schema.as_ref(),
            keep(EntityField::ResolvedSchema),
        ),
        effective_traits: kept(
            snapshot.effective_traits.as_ref(),
            keep(EntityField::EffectiveTraits),
        ),
        effective_traits_schema: kept(
            snapshot.effective_traits_schema.as_ref(),
            keep(EntityField::EffectiveTraitsSchema),
        ),
        provenance: snapshot
            .provenance
            .filter(|_| keep(EntityField::Provenance)),
    }
}

/// Clones a document only when it is kept.
fn kept(document: Option<&JsonDocument>, keep: bool) -> Option<JsonDocument> {
    document.filter(|_| keep).cloned()
}

/// `fresh`, read under `selection`, with the fields of `resident` it did not select.
/// Both must be one state, read under one validator.
pub fn merge(mut fresh: Entity, selection: &FieldSelection, resident: &Entity) -> Entity {
    let take = |field: EntityField| !selection.contains(field);
    // A new field needs an explicit merge rule.
    let Entity {
        gts_id: _,
        gts_uuid: _,
        kind: _,
        lifecycle_status: _,
        origin,
        content,
        resolved_schema,
        effective_traits,
        effective_traits_schema,
        provenance,
    } = &mut fresh;
    if take(EntityField::Origin) {
        *origin = resident.origin;
    }
    if take(EntityField::Content) {
        content.clone_from(&resident.content);
    }
    if take(EntityField::ResolvedSchema) {
        resolved_schema.clone_from(&resident.resolved_schema);
    }
    if take(EntityField::EffectiveTraits) {
        effective_traits.clone_from(&resident.effective_traits);
    }
    if take(EntityField::EffectiveTraitsSchema) {
        effective_traits_schema.clone_from(&resident.effective_traits_schema);
    }
    if take(EntityField::Provenance) {
        *provenance = resident.provenance;
    }
    fresh
}

/// The fields of both selections.
pub fn union(a: &FieldSelection, b: &FieldSelection) -> FieldSelection {
    let fields: Vec<EntityField> = a.fields().chain(b.fields()).collect();
    FieldSelection::with(&fields)
}

/// Compact JSON bytes. Charge failures as oversized.
fn json_len(document: &JsonDocument) -> usize {
    let mut counter = ByteCounter(0);
    match serde_json::to_writer(&mut counter, document) {
        Ok(()) => counter.0,
        Err(_) => usize::MAX,
    }
}

struct ByteCounter(usize);

impl io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// In-flight reads and the last event under one key. Tickets keep the slot alive.
#[derive(Debug, Default)]
struct Slot {
    tickets: usize,
    /// The store's event count when something was last recorded under this spelling.
    last_event: u64,
}

/// How a read or an event names its one entity: the key alone, when nothing here knows
/// its counterpart, or the pair the registry stated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spelling {
    Key(EntityKey),
    Pair(GtsId, Uuid),
}

impl Spelling {
    fn parts(&self) -> (Option<&GtsId>, Option<Uuid>) {
        match self {
            Self::Key(EntityKey::GtsId(id)) => (Some(id), None),
            Self::Key(EntityKey::GtsUuid(uuid)) => (None, Some(*uuid)),
            Self::Pair(id, uuid) => (Some(id), Some(*uuid)),
        }
    }
}

/// Fences visibility confirmation for a read context while its request is in flight.
#[derive(Clone)]
pub struct ContextTicket {
    pub context: ReadContext,
    seen: u64,
}

fn release_slot<K: Eq + std::hash::Hash>(slots: &mut HashMap<K, Slot>, name: &K) {
    if let Some(slot) = slots.get_mut(name) {
        slot.tickets = slot.tickets.saturating_sub(1);
        if slot.tickets == 0 {
            slots.remove(name);
        }
    }
}

/// Keys and event count pinned at read start. An event under an unknown
/// counterpart can fence a one-sided read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    spelling: Spelling,
    seen: u64,
}

/// Snapshot LRU with per-key read tickets. Registry-issued pairs index resident
/// snapshots and let either key fence in-flight reads.
pub struct Store {
    entries: LruCache<Uuid, Entry>,
    /// Validate both stores atomically while keeping their recency independent.
    pub identities: Identities,
    /// Keyed by (entity, read context).
    confirmations: Option<LruCache<(Uuid, ReadContext), Confirmation>>,
    /// A revocation fences requests for this read context, including requests under
    /// an alias the cache may only learn after the answer arrives.
    contexts_in_flight: HashMap<ReadContext, Slot>,
    /// The identifier of every resident snapshot, to the Registry Reference it carries.
    aliases: HashMap<GtsId, Uuid>,
    by_id: HashMap<GtsId, Slot>,
    by_uuid: HashMap<Uuid, Slot>,
    /// Events recorded so far, across entities: orders events independently of the clock.
    events: u64,
    /// Last event under a key with no known counterpart of the other form.
    /// Only one-sided reads of that other form may alias it.
    unresolved_by_id: u64,
    unresolved_by_uuid: u64,
    used: usize,
    bound: usize,
    max_entries: usize,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Usage {
    pub(super) snapshots: usize,
    pub(super) identities: usize,
    pub(super) snapshot_bytes: usize,
}

impl Store {
    pub fn new(bound: usize, max_entries: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            identities: Identities::new(0),
            confirmations: None,
            contexts_in_flight: HashMap::new(),
            aliases: HashMap::new(),
            by_id: HashMap::new(),
            by_uuid: HashMap::new(),
            events: 0,
            unresolved_by_id: 0,
            unresolved_by_uuid: 0,
            used: 0,
            bound,
            max_entries,
        }
    }

    /// Where the entry `key` spells is resident.
    fn resident(&self, key: &EntityKey) -> Option<Uuid> {
        let uuid = match key {
            EntityKey::GtsId(id) => *self.aliases.get(id)?,
            EntityKey::GtsUuid(uuid) => *uuid,
        };
        Some(uuid)
    }

    /// Remember up to `bound` identity pairs for resolution.
    pub fn remember_identities(&mut self, bound: usize) {
        self.identities = Identities::new(bound);
    }

    /// Limit confirmations independently of the number of readers and entities.
    pub fn track_visibility(&mut self) {
        self.confirmations = NonZeroUsize::new(self.max_entries).map(LruCache::sparse);
    }

    /// The Registry Reference `key` names, from a resident snapshot or a remembered
    /// pair, without changing recency.
    fn reference(&self, key: &EntityKey) -> Option<Uuid> {
        match key {
            EntityKey::GtsId(id) => self
                .aliases
                .get(id)
                .copied()
                .or_else(|| self.identities.peek_uuid(key)),
            EntityKey::GtsUuid(uuid) => Some(*uuid),
        }
    }

    /// Whether `context` holds a fresh read of the resident snapshot `key` spells.
    pub fn visible(
        &mut self,
        key: &EntityKey,
        context: &ReadContext,
        now: Instant,
        window: Duration,
    ) -> bool {
        let Some(uuid) = self.resident(key) else {
            return false;
        };
        let version = self
            .entries
            .peek(&uuid)
            .and_then(|entry| entry.confirmed_version.as_ref());
        let Some(confirmations) = self.confirmations.as_mut() else {
            return false;
        };
        let key = (uuid, context.clone());
        let Some(seen) = confirmations.peek_mut(&key) else {
            return false;
        };
        let visible = seen
            .content
            .as_ref()
            .is_some_and(|proof| proof.live(version, now, window));
        if visible {
            confirmations.promote(&key);
        } else {
            seen.content = None;
            if seen.identity.is_none() {
                confirmations.pop(&key);
            }
        }
        visible
    }

    /// The pair `key` names, if `context` holds a fresh read of the resident
    /// snapshot or a fresh showing of the remembered pair. Only a used store is
    /// marked used.
    pub fn resolvable(
        &mut self,
        key: &EntityKey,
        context: &ReadContext,
        now: Instant,
        window: Duration,
    ) -> Option<EntityIdentity> {
        let uuid = self.reference(key)?;
        let key = (uuid, context.clone());
        let (by_read, by_showing) = {
            let seen = self.confirmations.as_ref()?.peek(&key)?;
            let version = self
                .entries
                .peek(&uuid)
                .and_then(|entry| entry.confirmed_version.as_ref());
            let incarnation = self.identities.incarnation(uuid);
            (
                seen.content
                    .as_ref()
                    .is_some_and(|proof| proof.live(version, now, window)),
                seen.identity
                    .as_ref()
                    .is_some_and(|proof| proof.live(incarnation, now, window)),
            )
        };
        let confirmations = self.confirmations.as_mut()?;
        if !(by_read || by_showing) {
            confirmations.pop(&key);
            return None;
        }
        confirmations.promote(&key);
        if by_read {
            let snapshot = &self.entries.get(&uuid)?.snapshot;
            Some(EntityIdentity {
                gts_id: snapshot.gts_id.clone(),
                gts_uuid: snapshot.gts_uuid,
            })
        } else {
            self.identities.get(&EntityKey::GtsUuid(uuid))
        }
    }

    /// Record that a read started at `read_at` showed `context` the resident
    /// snapshot under `etag`, and the remembered pair with it.
    pub fn confirm_visible(
        &mut self,
        key: &EntityKey,
        context: &ReadContext,
        etag: &Validator,
        read_at: Instant,
    ) {
        let Some(uuid) = self.resident(key) else {
            return;
        };
        let Some(version) = self
            .entries
            .peek(&uuid)
            .filter(|entry| entry.etag == *etag)
            .and_then(|entry| entry.confirmed_version.clone())
        else {
            return;
        };
        let incarnation = self.identities.incarnation(uuid).cloned();
        let Some(confirmations) = self.confirmations.as_mut() else {
            return;
        };
        let seen = confirmations.get_or_insert_mut((uuid, context.clone()), Confirmation::default);
        Proof::renew(&mut seen.content, &version, read_at);
        if let Some(incarnation) = &incarnation {
            Proof::renew(&mut seen.identity, incarnation, read_at);
        }
    }

    /// Record that a resolution started at `read_at` showed `context` the
    /// remembered pair for `gts_uuid`. Leaves any read proof as it is.
    pub fn confirm_resolved(&mut self, gts_uuid: Uuid, context: &ReadContext, read_at: Instant) {
        let Some(incarnation) = self.identities.incarnation(gts_uuid).cloned() else {
            return;
        };
        let Some(confirmations) = self.confirmations.as_mut() else {
            return;
        };
        let seen =
            confirmations.get_or_insert_mut((gts_uuid, context.clone()), Confirmation::default);
        Proof::renew(&mut seen.identity, &incarnation, read_at);
    }

    /// Revoke `context`'s confirmations of `keys` and fence its requests in flight.
    pub fn revoke_visible<'a>(
        &mut self,
        context: &ReadContext,
        keys: impl Iterator<Item = &'a EntityKey>,
    ) {
        let mut keys = keys.peekable();
        if keys.peek().is_none() {
            return;
        }
        if let Some(slot) = self.contexts_in_flight.get_mut(context) {
            slot.last_event += 1;
        }
        for key in keys {
            if let Some(uuid) = self.reference(key)
                && let Some(confirmations) = self.confirmations.as_mut()
            {
                confirmations.pop(&(uuid, context.clone()));
            }
        }
    }

    pub fn context_ticket(&mut self, context: &ReadContext) -> ContextTicket {
        let slot = self.contexts_in_flight.entry(context.clone()).or_default();
        slot.tickets += 1;
        ContextTicket {
            context: context.clone(),
            seen: slot.last_event,
        }
    }

    pub fn release_context_ticket(&mut self, ticket: &ContextTicket) {
        release_slot(&mut self.contexts_in_flight, &ticket.context);
    }

    pub fn context_ticket_current(&self, ticket: &ContextTicket) -> bool {
        self.contexts_in_flight
            .get(&ticket.context)
            .is_some_and(|slot| slot.last_event == ticket.seen)
    }

    /// Whether any read for `context` holds a ticket.
    #[cfg(test)]
    pub fn context_in_flight(&self, context: &ReadContext) -> bool {
        self.contexts_in_flight.contains_key(context)
    }

    /// The entry `key` spells, marked most recently used.
    pub fn get(&mut self, key: &EntityKey) -> Option<&Entry> {
        let at = self.resident(key)?;
        self.entries.get(&at)
    }

    /// The entry `key` spells, without marking it used.
    pub fn peek(&self, key: &EntityKey) -> Option<&Entry> {
        self.entries.peek(&self.resident(key)?)
    }

    /// Return a conflicting resident counterpart without changing recency.
    pub fn conflict(&self, gts_id: &GtsId, gts_uuid: Uuid) -> Option<EntityKey> {
        if let Some(known) = self.aliases.get(gts_id).filter(|known| **known != gts_uuid) {
            return Some(EntityKey::GtsUuid(*known));
        }
        self.entries
            .peek(&gts_uuid)
            .filter(|entry| entry.snapshot.gts_id != *gts_id)
            .map(|entry| EntityKey::GtsId(entry.snapshot.gts_id.clone()))
    }

    /// Use the stated pair, then the resident pair, then `key` alone.
    fn spelling(&self, key: &EntityKey, stated: Option<(&GtsId, Uuid)>) -> Spelling {
        let resident = || {
            self.peek(key)
                .map(|entry| (&entry.snapshot.gts_id, entry.snapshot.gts_uuid))
        };
        match stated.or_else(resident) {
            Some((gts_id, gts_uuid)) => Spelling::Pair(gts_id.clone(), gts_uuid),
            None => Spelling::Key(key.clone()),
        }
    }

    /// Pin `key` and any known counterpart before a registry read.
    /// [Release](Self::release) the ticket exactly once.
    pub fn ticket(&mut self, key: &EntityKey) -> Ticket {
        let spelling = self.spelling(key, None);
        let (id, uuid) = spelling.parts();
        if let Some(id) = id {
            self.by_id.entry(id.clone()).or_default().tickets += 1;
        }
        if let Some(uuid) = uuid {
            self.by_uuid.entry(uuid).or_default().tickets += 1;
        }
        Ticket {
            spelling,
            seen: self.events,
        }
    }

    pub fn release(&mut self, ticket: &Ticket) {
        let (id, uuid) = ticket.spelling.parts();
        if let Some(id) = id {
            release_slot(&mut self.by_id, id);
        }
        if let Some(uuid) = uuid {
            release_slot(&mut self.by_uuid, &uuid);
        }
    }

    /// Check pinned slots for intervening events. A one-sided ticket also
    /// checks events under unknown counterparts of the other key form.
    pub fn quiet_since(&self, ticket: &Ticket) -> bool {
        let quiet = |slot: Option<&Slot>| slot.is_some_and(|slot| slot.last_event <= ticket.seen);
        let other_form = match &ticket.spelling {
            Spelling::Pair(..) => 0,
            Spelling::Key(EntityKey::GtsId(_)) => self.unresolved_by_uuid,
            Spelling::Key(EntityKey::GtsUuid(_)) => self.unresolved_by_id,
        };
        let (id, uuid) = ticket.spelling.parts();
        other_form <= ticket.seen
            && id.is_none_or(|id| quiet(self.by_id.get(id)))
            && uuid.is_none_or(|uuid| quiet(self.by_uuid.get(&uuid)))
    }

    /// Record the answer's event under both known spellings. A one-sided event
    /// also fences one-sided reads under the other key form.
    pub fn record(&mut self, key: &EntityKey, stated: Option<(&GtsId, Uuid)>) {
        let spelling = self.spelling(key, stated);
        self.event(&spelling);
    }

    fn event(&mut self, spelling: &Spelling) {
        self.events += 1;
        let (id, uuid) = spelling.parts();
        if let Some(slot) = id.and_then(|id| self.by_id.get_mut(id)) {
            slot.last_event = self.events;
        }
        if let Some(slot) = uuid.and_then(|uuid| self.by_uuid.get_mut(&uuid)) {
            slot.last_event = self.events;
        }
        match spelling {
            Spelling::Key(EntityKey::GtsId(_)) => self.unresolved_by_id = self.events,
            Spelling::Key(EntityKey::GtsUuid(_)) => self.unresolved_by_uuid = self.events,
            Spelling::Pair(..) => {}
        }
    }

    /// Whether a snapshot of `weight` can be retained at all.
    pub fn fits(&self, weight: &Weight, etag: &Validator) -> bool {
        self.max_entries > 0 && weight.fits(self.bound, etag)
    }

    /// Replace the resident snapshot and return the number of capacity evictions.
    /// An oversized replacement drops the stale entry. Measure `weight` before locking.
    /// Callers check [identity conflicts](Self::conflict) first. Insertion still drops
    /// a conflicting resident alias to preserve uniqueness.
    pub fn insert(
        &mut self,
        snapshot: Arc<Entity>,
        covers: FieldSelection,
        weight: Weight,
        etag: Validator,
        confirmed_at: Instant,
    ) -> u64 {
        let key = snapshot.gts_uuid;
        if let Some(other) = self.aliases.get(&snapshot.gts_id).copied()
            && other != snapshot.gts_uuid
        {
            self.remove(&other);
        }
        let confirmed_version = self.confirmations.as_ref().map(|_| {
            self.entries
                .peek(&key)
                .filter(|old| old.etag == etag)
                .and_then(|old| old.confirmed_version.clone())
                .unwrap_or_default()
        });
        self.remove(&key);
        if !self.fits(&weight, &etag) {
            return 0;
        }
        let bytes = weight.bytes_with_validator(&etag);
        let mut evictions = 0;
        // `used <= bound` holds throughout, so the subtraction cannot underflow.
        while bytes > self.bound - self.used || self.entries.len() >= self.max_entries {
            let Some((evicted, entry)) = self.entries.pop_lru() else {
                break;
            };
            self.forget(&evicted, &entry);
            evictions += 1;
        }
        self.used += bytes;
        self.aliases
            .insert(snapshot.gts_id.clone(), snapshot.gts_uuid);
        self.entries.put(
            key,
            Entry {
                snapshot,
                covers,
                etag,
                confirmed_at,
                weight,
                confirmed_version,
            },
        );
        evictions
    }

    /// Advance confirmation time only for the matching resident validator.
    pub fn confirm(&mut self, key: &EntityKey, etag: &Validator, at: Instant) {
        let Some(resident) = self.resident(key) else {
            return;
        };
        if let Some(entry) = self.entries.get_mut(&resident)
            && entry.etag == *etag
        {
            entry.confirmed_at = entry.confirmed_at.max(at);
        }
    }

    fn remove(&mut self, key: &Uuid) {
        if let Some(entry) = self.entries.pop(key) {
            self.forget(key, &entry);
        }
    }

    /// Drop the snapshot and fence older reads under either known key, or under
    /// a possible unknown counterpart.
    pub fn invalidate(&mut self, key: &EntityKey, stated: Option<(&GtsId, Uuid)>) {
        let spelling = self.spelling(key, stated);
        if let (_, Some(uuid)) = spelling.parts() {
            self.remove(&uuid);
        }
        self.event(&spelling);
    }

    /// Accounts for an entry that left the store, and with it its identifier.
    fn forget(&mut self, key: &Uuid, entry: &Entry) {
        self.used -= entry.weight.bytes_with_validator(&entry.etag);
        if self.aliases.get(&entry.snapshot.gts_id) == Some(key) {
            self.aliases.remove(&entry.snapshot.gts_id);
        }
    }

    pub(super) fn usage(&self) -> Usage {
        Usage {
            snapshots: self.entries.len(),
            identities: self.identities.len(),
            snapshot_bytes: self.used,
        }
    }

    #[cfg(test)]
    pub fn used(&self) -> usize {
        self.used
    }

    /// Whether the store keeps any bookkeeping under the spelling `name`.
    #[cfg(test)]
    pub fn indexes(&self, name: &EntityKey) -> bool {
        match name {
            EntityKey::GtsId(id) => self.by_id.contains_key(id) || self.aliases.contains_key(id),
            EntityKey::GtsUuid(uuid) => {
                self.by_uuid.contains_key(uuid) || self.entries.contains(uuid)
            }
        }
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
