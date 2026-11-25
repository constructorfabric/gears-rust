//! Snapshot LRU bounded by charged bytes and entry count. Both keys address
//! an entity's snapshots, each holding fields read under one validator for one
//! [`Scope`]. Snapshots of one entity share equal documents, charged once.

use std::collections::hash_map::Entry as MapEntry;
use std::collections::{BTreeMap, HashMap, HashSet};
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
    Entity, EntityField, EntityIdentity, EntityKey, FieldSelection, JsonDocument, Ownership,
    Validator,
};

/// Who a tenant read is answered for: the subject, its type, home tenant and token
/// scopes. From P1 the registry scopes reads per subject by ownership constraints, so a
/// positive answer to one read context proves nothing to another.
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

    /// The tenant a read's representation is relative to: the home tenant until the
    /// SDK carries a Context Tenant, which then takes its place here.
    pub fn tenant(&self) -> Uuid {
        self.tenant
    }
}

/// The readers one snapshot answers. `ownership` — and from P1 availability — is
/// relative to the reader's Context Tenant, so a tenant-owned snapshot answers only the
/// tenant it was read for. A global snapshot, and every snapshot of the platform
/// cache, answers every reader. Ownership never changes, so an entity's snapshots are
/// all `Shared` or all `Tenant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Shared,
    Tenant(Uuid),
}

impl Scope {
    /// Where a snapshot read for `reader` (`None` on the platform plane) is kept.
    pub fn of(entity: &Entity, reader: Option<Uuid>) -> Self {
        match reader {
            Some(tenant) if entity.ownership != Ownership::Global => Self::Tenant(tenant),
            _ => Self::Shared,
        }
    }
}

/// The document fields, apart from the metadata, so that snapshots of one entity held
/// for different tenants share equal documents.
const DOCUMENTS: [EntityField; 4] = [
    EntityField::Content,
    EntityField::ResolvedSchema,
    EntityField::EffectiveTraits,
    EntityField::EffectiveTraitsSchema,
];

/// A snapshot as held: metadata, with every document `None`, and the documents apart.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    meta: Arc<Entity>,
    /// By position in [`DOCUMENTS`]; a selected JSON `null` is `Some`.
    documents: [Option<Arc<JsonDocument>>; 4],
}

impl Held {
    pub fn new(mut entity: Entity) -> Self {
        // A new document field must be held apart.
        let Entity {
            content,
            resolved_schema,
            effective_traits,
            effective_traits_schema,
            ..
        } = &mut entity;
        let documents = [
            content.take(),
            resolved_schema.take(),
            effective_traits.take(),
            effective_traits_schema.take(),
        ]
        .map(|document| document.map(Arc::new));
        Self {
            meta: Arc::new(entity),
            documents,
        }
    }

    /// The held documents, by position in the document fields.
    #[cfg(test)]
    pub fn documents(&self) -> &[Option<Arc<JsonDocument>>; 4] {
        &self.documents
    }

    /// Identity, kind, lifecycle, ownership and the non-document fields; its documents
    /// are `None` whatever is held.
    pub fn meta(&self) -> &Entity {
        &self.meta
    }

    /// The snapshot as a read selecting `selection` is answered: every other field
    /// dropped. Holds at least the applicable fields of `selection`.
    pub fn project(&self, selection: &FieldSelection) -> Entity {
        let keep = |field: EntityField| selection.contains(field);
        let document = |at: usize| {
            self.documents[at]
                .as_deref()
                .filter(|_| keep(DOCUMENTS[at]))
                .cloned()
        };
        Entity {
            gts_id: self.meta.gts_id.clone(),
            gts_uuid: self.meta.gts_uuid,
            kind: self.meta.kind,
            lifecycle_status: self.meta.lifecycle_status,
            ownership: self.meta.ownership,
            origin: self.meta.origin.filter(|_| keep(EntityField::Origin)),
            content: document(0),
            resolved_schema: document(1),
            effective_traits: document(2),
            effective_traits_schema: document(3),
            provenance: self
                .meta
                .provenance
                .filter(|_| keep(EntityField::Provenance)),
        }
    }

    /// `fresh`, read under `selection`, with the fields of `resident` it did not select.
    /// Both must be one state, read under one validator for one scope.
    pub fn merged(fresh: &Self, selection: &FieldSelection, resident: &Self) -> Self {
        let take = |field: EntityField| !selection.contains(field);
        let mut meta = (*fresh.meta).clone();
        // A new non-document field needs an explicit merge rule.
        let Entity {
            gts_id: _,
            gts_uuid: _,
            kind: _,
            lifecycle_status: _,
            ownership: _,
            origin,
            content: _,
            resolved_schema: _,
            effective_traits: _,
            effective_traits_schema: _,
            provenance,
        } = &mut meta;
        if take(EntityField::Origin) {
            *origin = resident.meta.origin;
        }
        if take(EntityField::Provenance) {
            *provenance = resident.meta.provenance;
        }
        let documents = std::array::from_fn(|at| {
            if take(DOCUMENTS[at]) {
                resident.documents[at].clone()
            } else {
                fresh.documents[at].clone()
            }
        });
        Self {
            meta: Arc::new(meta),
            documents,
        }
    }

    /// Hold a sibling's copy of each document that serializes, in the same field, to the
    /// same bytes as the one held here. Value equality is not enough: `serde_json` calls
    /// `-0.0` and `0.0` equal, and object key order may differ. Each distinct sibling
    /// document is compared at most once, and only when its measured size matches.
    fn share(&mut self, weight: &Weight, siblings: &[(&Self, &Weight)]) {
        for (at, field) in DOCUMENTS.into_iter().enumerate() {
            let Some(held) = self.documents[at].as_mut() else {
                continue;
            };
            let size = weight.documents.get(&field);
            let mut mine = None;
            let mut compared = HashSet::new();
            for (sibling, sibling_weight) in siblings {
                let Some(theirs) = &sibling.documents[at] else {
                    continue;
                };
                if Arc::ptr_eq(held, theirs) {
                    break;
                }
                if !compared.insert(Arc::as_ptr(theirs).addr())
                    || sibling_weight.documents.get(&field) != size
                {
                    continue;
                }
                let mine = mine.get_or_insert_with(|| serde_json::to_vec(&**held).ok());
                if mine.is_some() && *mine == serde_json::to_vec(&**theirs).ok() {
                    *held = Arc::clone(theirs);
                    break;
                }
            }
        }
    }

    /// Each held document with its charge, by `weight`.
    fn charged<'a>(&'a self, weight: &'a Weight) -> impl Iterator<Item = (usize, usize)> + 'a {
        self.documents
            .iter()
            .zip(DOCUMENTS)
            .filter_map(move |(document, field)| {
                let document = document.as_ref()?;
                Some((
                    Arc::as_ptr(document).addr(),
                    weight.documents.get(&field).copied().unwrap_or(0),
                ))
            })
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

/// A resident snapshot: the fields read of an entity so far for one scope, all under `etag`.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Carries exactly the fields of `covers` that apply to its kind.
    pub snapshot: Held,
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

    /// The bytes charged per snapshot whatever its documents share: identifier,
    /// allowance and validator.
    fn own(&self, etag: &Validator) -> usize {
        self.fixed.saturating_add(etag.as_bytes().len())
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
        ownership: _,
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

impl Ticket {
    /// The pair the read named when it started, if one was known.
    pub fn pair(&self) -> Option<(&GtsId, Uuid)> {
        match &self.spelling {
            Spelling::Pair(gts_id, gts_uuid) => Some((gts_id, *gts_uuid)),
            Spelling::Key(_) => None,
        }
    }
}

/// Snapshot LRU with per-key read tickets. Registry-issued pairs index resident
/// snapshots and let either key fence in-flight reads.
pub struct Store {
    entries: LruCache<(Uuid, Scope), Entry>,
    /// The scopes each resident entity has a snapshot in.
    scopes: HashMap<Uuid, Vec<Scope>>,
    /// Held documents by address: how many snapshots hold each, and its charge.
    documents: HashMap<usize, (usize, usize)>,
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
            scopes: HashMap::new(),
            documents: HashMap::new(),
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

    /// The entity `key` spells, by a resident snapshot's identifier or by its UUID.
    fn resident(&self, key: &EntityKey) -> Option<Uuid> {
        let uuid = match key {
            EntityKey::GtsId(id) => *self.aliases.get(id)?,
            EntityKey::GtsUuid(uuid) => *uuid,
        };
        Some(uuid)
    }

    /// The resident snapshot of `gts_uuid` that answers `reader` (`None` on the platform
    /// plane): the shared one, else the one read for that tenant.
    fn slot(&self, gts_uuid: Uuid, reader: Option<Uuid>) -> Option<(Uuid, Scope)> {
        let scopes = self.scopes.get(&gts_uuid)?;
        if scopes.contains(&Scope::Shared) {
            return Some((gts_uuid, Scope::Shared));
        }
        let tenant = Scope::Tenant(reader?);
        scopes.contains(&tenant).then_some((gts_uuid, tenant))
    }

    /// Any resident snapshot of `gts_uuid`: all agree on its pair and its class.
    fn any(&self, gts_uuid: Uuid) -> Option<&Entry> {
        let scope = *self.scopes.get(&gts_uuid)?.first()?;
        self.entries.peek(&(gts_uuid, scope))
    }

    /// Remember up to `bound` identity pairs for resolution.
    pub fn remember_identities(&mut self, bound: usize) {
        self.identities = Identities::new(bound);
    }

    /// Limit confirmations independently of the number of readers and entities.
    pub fn track_visibility(&mut self) {
        self.confirmations = NonZeroUsize::new(self.max_entries).map(LruCache::sparse);
    }

    /// Whether the entity `gts_uuid` names is known to be global: from its resident
    /// snapshot, or from what an entity-carrying reply said of its remembered pair.
    pub fn known_global(&self, gts_uuid: Uuid) -> bool {
        self.any(gts_uuid).map_or_else(
            || self.identities.is_global(gts_uuid) == Some(true),
            |entry| entry.snapshot.meta().ownership == Ownership::Global,
        )
    }

    /// The remembered pair `key` names, without promoting it.
    pub fn remembered_pair(&self, key: &EntityKey) -> Option<(GtsId, Uuid)> {
        let uuid = self.identities.peek_uuid(key)?;
        Some((self.identities.peek_id(uuid)?.clone(), uuid))
    }

    /// Whether `key` names an entity known to be global (`Some(true)`) or tenant-owned,
    /// from any of its snapshots or its remembered pair.
    pub fn class(&self, key: &EntityKey) -> Option<bool> {
        let uuid = self.reference(key)?;
        self.any(uuid).map_or_else(
            || self.identities.is_global(uuid),
            |entry| Some(entry.snapshot.meta().ownership == Ownership::Global),
        )
    }

    /// Whether `key` names an entity known to be global.
    pub fn key_known_global(&self, key: &EntityKey) -> bool {
        self.reference(key)
            .is_some_and(|uuid| self.known_global(uuid))
    }

    /// Whether a reply saying the entity `gts_uuid` is `global` (or not) contradicts
    /// what its resident snapshot or remembered pair already says.
    pub fn class_conflict(&self, gts_uuid: Uuid, global: bool) -> bool {
        self.any(gts_uuid)
            .is_some_and(|entry| (entry.snapshot.meta().ownership == Ownership::Global) != global)
            || self
                .identities
                .is_global(gts_uuid)
                .is_some_and(|known| known != global)
    }

    /// The pair `key` names if it is known to be global: any authenticated reader may
    /// see a global entity.
    pub fn resolvable_global(&mut self, key: &EntityKey) -> Option<EntityIdentity> {
        let uuid = self.reference(key)?;
        if !self.known_global(uuid) {
            return None;
        }
        if let Some(entry) = self.entries.get(&(uuid, Scope::Shared)) {
            return Some(EntityIdentity {
                gts_id: entry.snapshot.meta().gts_id.clone(),
                gts_uuid: entry.snapshot.meta().gts_uuid,
            });
        }
        self.identities.get(&EntityKey::GtsUuid(uuid))
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
            .peek(&(uuid, Scope::Tenant(context.tenant)))
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
                .peek(&(uuid, Scope::Tenant(context.tenant)))
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
            let snapshot = self
                .entries
                .get(&(uuid, Scope::Tenant(context.tenant)))?
                .snapshot
                .meta();
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
        // A global entity is served by its class alone; its own proof would only take a
        // slot. A tenant-owned one is confirmed only in the snapshot read for this tenant.
        let Some(version) = self
            .entries
            .peek(&(uuid, Scope::Tenant(context.tenant)))
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
        if self.known_global(gts_uuid) {
            return;
        }
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

    /// The entry `key` spells that answers `reader`, marked most recently used.
    pub fn get(&mut self, key: &EntityKey, reader: Option<Uuid>) -> Option<&Entry> {
        let at = self.slot(self.resident(key)?, reader)?;
        self.entries.get(&at)
    }

    /// The entry `key` spells that answers `reader`, without marking it used.
    pub fn peek(&self, key: &EntityKey, reader: Option<Uuid>) -> Option<&Entry> {
        self.entries.peek(&self.slot(self.resident(key)?, reader)?)
    }

    /// Return a conflicting resident counterpart without changing recency.
    pub fn conflict(&self, gts_id: &GtsId, gts_uuid: Uuid) -> Option<EntityKey> {
        if let Some(known) = self.aliases.get(gts_id).filter(|known| **known != gts_uuid) {
            return Some(EntityKey::GtsUuid(*known));
        }
        self.any(gts_uuid)
            .filter(|entry| entry.snapshot.meta().gts_id != *gts_id)
            .map(|entry| EntityKey::GtsId(entry.snapshot.meta().gts_id.clone()))
    }

    /// Use the stated pair, then the resident pair, then `key` alone.
    fn spelling(&self, key: &EntityKey, stated: Option<(&GtsId, Uuid)>) -> Spelling {
        let resident = || {
            self.any(self.resident(key)?).map(|entry| {
                (
                    &entry.snapshot.meta().gts_id,
                    entry.snapshot.meta().gts_uuid,
                )
            })
        };
        if let Some((gts_id, gts_uuid)) = stated.or_else(resident) {
            return Spelling::Pair(gts_id.clone(), gts_uuid);
        }
        // A remembered pair names both keys even with no snapshot resident.
        match self.remembered_pair(key) {
            Some((gts_id, gts_uuid)) => Spelling::Pair(gts_id, gts_uuid),
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

    /// Replace the snapshot resident in `scope` and return the number of capacity
    /// evictions. An oversized replacement drops the stale entry. Measure `weight`
    /// before locking. Callers check [identity conflicts](Self::conflict) first.
    /// Insertion still drops a conflicting resident alias to preserve uniqueness.
    pub fn insert(
        &mut self,
        mut snapshot: Held,
        covers: FieldSelection,
        weight: Weight,
        etag: Validator,
        confirmed_at: Instant,
        scope: Scope,
    ) -> u64 {
        let uuid = snapshot.meta().gts_uuid;
        let key = (uuid, scope);
        if let Some(other) = self.aliases.get(&snapshot.meta().gts_id).copied()
            && other != uuid
        {
            self.remove_entity(other);
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
        let siblings: Vec<(&Held, &Weight)> = self
            .scopes
            .get(&uuid)
            .into_iter()
            .flatten()
            .filter_map(|other| self.entries.peek(&(uuid, *other)))
            .map(|entry| (&entry.snapshot, &entry.weight))
            .collect();
        snapshot.share(&weight, &siblings);
        let mut evictions = 0;
        // Recharged each round: an eviction may leave a shared document to this one.
        // `used <= bound` holds throughout, so the subtraction cannot underflow.
        while self.charge(&snapshot, &weight, &etag) > self.bound - self.used
            || self.entries.len() >= self.max_entries
        {
            let Some((evicted, entry)) = self.entries.pop_lru() else {
                break;
            };
            self.forget(&evicted, &entry);
            evictions += 1;
        }
        self.used += weight.own(&etag);
        for (address, bytes) in snapshot.charged(&weight) {
            match self.documents.entry(address) {
                MapEntry::Occupied(mut held) => held.get_mut().0 += 1,
                MapEntry::Vacant(first) => {
                    first.insert((1, bytes));
                    self.used += bytes;
                }
            }
        }
        self.aliases.insert(snapshot.meta().gts_id.clone(), uuid);
        self.scopes.entry(uuid).or_default().push(scope);
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

    /// What keeping `snapshot` would add: its own bytes and each document no resident
    /// snapshot already holds.
    fn charge(&self, snapshot: &Held, weight: &Weight, etag: &Validator) -> usize {
        snapshot
            .charged(weight)
            .filter(|(address, _)| !self.documents.contains_key(address))
            .fold(weight.own(etag), |sum, (_, bytes)| {
                sum.saturating_add(bytes)
            })
    }

    /// Advance confirmation time only for the matching validator of the snapshot that
    /// answers `reader`.
    pub fn confirm(
        &mut self,
        key: &EntityKey,
        etag: &Validator,
        at: Instant,
        reader: Option<Uuid>,
    ) {
        let Some(slot) = self.resident(key).and_then(|uuid| self.slot(uuid, reader)) else {
            return;
        };
        if let Some(entry) = self.entries.get_mut(&slot)
            && entry.etag == *etag
        {
            entry.confirmed_at = entry.confirmed_at.max(at);
        }
    }

    fn remove(&mut self, key: &(Uuid, Scope)) {
        if let Some(entry) = self.entries.pop(key) {
            self.forget(key, &entry);
        }
    }

    /// Drop every snapshot of the entity, whatever it was read for.
    fn remove_entity(&mut self, gts_uuid: Uuid) {
        for scope in self.scopes.get(&gts_uuid).cloned().unwrap_or_default() {
            self.remove(&(gts_uuid, scope));
        }
    }

    /// Drop the entity's snapshots other than the one that answers `reader`: its own
    /// already carries the change that retires them.
    pub fn retire_siblings(&mut self, key: &EntityKey, reader: Option<Uuid>) {
        let Some(uuid) = self.resident(key) else {
            return;
        };
        let keep = self.slot(uuid, reader).map(|(_, scope)| scope);
        for scope in self.scopes.get(&uuid).cloned().unwrap_or_default() {
            if Some(scope) != keep {
                self.remove(&(uuid, scope));
            }
        }
    }

    /// Drop the entity's snapshots and fence older reads under either known key, or
    /// under a possible unknown counterpart.
    pub fn invalidate(&mut self, key: &EntityKey, stated: Option<(&GtsId, Uuid)>) {
        let spelling = self.spelling(key, stated);
        if let (_, Some(uuid)) = spelling.parts() {
            self.remove_entity(uuid);
        }
        self.event(&spelling);
    }

    /// Accounts for an entry that left the store, its documents once no snapshot holds
    /// them, and its identifier once no snapshot of the entity remains.
    fn forget(&mut self, (gts_uuid, scope): &(Uuid, Scope), entry: &Entry) {
        self.used -= entry.weight.own(&entry.etag);
        for (address, _) in entry.snapshot.charged(&entry.weight) {
            if let Some((holders, bytes)) = self.documents.get_mut(&address) {
                *holders -= 1;
                if *holders == 0 {
                    self.used -= *bytes;
                    self.documents.remove(&address);
                }
            }
        }
        if let Some(scopes) = self.scopes.get_mut(gts_uuid) {
            scopes.retain(|resident| resident != scope);
            if !scopes.is_empty() {
                return;
            }
            self.scopes.remove(gts_uuid);
        }
        if self.aliases.get(&entry.snapshot.meta().gts_id) == Some(gts_uuid) {
            self.aliases.remove(&entry.snapshot.meta().gts_id);
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

    /// Any snapshot of the entity `key` spells, whatever it was read for.
    #[cfg(test)]
    pub fn peek_any(&self, key: &EntityKey) -> Option<&Entry> {
        self.any(self.resident(key)?)
    }

    /// The snapshots of the entity `key` spells, by scope.
    #[cfg(test)]
    pub fn scopes(&self, key: &EntityKey) -> Vec<Scope> {
        self.resident(key)
            .and_then(|uuid| self.scopes.get(&uuid).cloned())
            .unwrap_or_default()
    }

    /// Whether the store keeps any bookkeeping under the spelling `name`.
    #[cfg(test)]
    pub fn indexes(&self, name: &EntityKey) -> bool {
        match name {
            EntityKey::GtsId(id) => self.by_id.contains_key(id) || self.aliases.contains_key(id),
            EntityKey::GtsUuid(uuid) => {
                self.by_uuid.contains_key(uuid) || self.scopes.contains_key(uuid)
            }
        }
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
