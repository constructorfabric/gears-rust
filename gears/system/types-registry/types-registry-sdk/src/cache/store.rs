//! The cache's bounded store: one entry per entity and scope, holding the fields read
//! of it so far under one validator, evicted least-recently-used by the bytes it holds.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;

use lru::LruCache;
use tokio::time::Instant;
use uuid::Uuid;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::models::{
    Entity, EntityField, EntityKind, FieldSelection, JsonDocument, LifecycleStatus, Origin,
    Validator,
};

/// Who may see a representation. P0 has one value.
// ponytail: C7 — visibility context and Context Tenant are fixed for the platform plane
// in P0. P1 tenancy replaces this marker with the subject tenant and Context Tenant
// without reshaping the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Unrestricted visibility, no Context Tenant.
    Platform,
}

/// One cached entity as one scope sees it. Every projection of it shares the entry: a
/// validator names the entity's state, the same under every projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryKey {
    pub uuid: Uuid,
    pub scope: Scope,
}

impl EntryKey {
    pub fn new(uuid: Uuid) -> Self {
        Self {
            uuid,
            scope: Scope::Platform,
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
}

/// The bytes a snapshot is charged: its compact JSON as the REST plane writes an entity
/// (TR's `EntityDto`) — identity, kind and lifecycle always, each selected field otherwise,
/// a selected JSON `null` as `null`. Key order does not change the length. This is the
/// payload's size, not retained heap.
///
/// Kept by field, so that a merge adds up what was measured rather than serializing the
/// resident documents again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Weight {
    /// `"name":value` of the mandatory fields, summed.
    identity: usize,
    /// `"name":value` of each selectable field the snapshot carries.
    fields: BTreeMap<EntityField, usize>,
}

impl Weight {
    /// The whole object's bytes; `usize::MAX` when any part could not be measured.
    pub fn total(&self) -> usize {
        let members = EntityField::MANDATORY.len() + self.fields.len();
        self.fields
            .values()
            .fold(self.identity.saturating_add(2 + members - 1), |sum, f| {
                sum.saturating_add(*f)
            })
    }

    /// Whether a snapshot of this weight can be retained under `bound` at all.
    pub fn fits(&self, bound: usize) -> bool {
        // `usize::MAX` is the unmeasurable charge: never retained, whatever the bound.
        let bytes = self.total();
        bytes != usize::MAX && bytes <= bound
    }

    /// This weight, of a snapshot read under `selection`, with the fields `resident`
    /// carries outside `selection` beside it: the weight of [`merge`]'s result.
    pub fn merged(&self, selection: &FieldSelection, resident: &Self) -> Self {
        let mut fields = self.fields.clone();
        for (field, bytes) in &resident.fields {
            if !selection.contains(*field) {
                fields.insert(*field, *bytes);
            }
        }
        Self {
            identity: self.identity,
            fields,
        }
    }
}

pub fn weight(snapshot: &Entity) -> Weight {
    let identity = [
        field("gts_id", str_len(snapshot.gts_id.as_ref())),
        field("gts_uuid", UUID_LEN),
        field(
            "kind",
            quoted(match snapshot.kind {
                EntityKind::TypeSchema => "type_schema",
                EntityKind::Instance => "instance",
            }),
        ),
        field(
            "lifecycle_status",
            quoted(match snapshot.lifecycle_status {
                LifecycleStatus::Active => "active",
                LifecycleStatus::Deleted => "deleted",
            }),
        ),
    ]
    .into_iter()
    .fold(0_usize, usize::saturating_add);
    let mut fields = BTreeMap::new();
    // Exhaustive, so that a new origin cannot go uncharged.
    match &snapshot.origin {
        Some(Origin::Managed {
            resource_version,
            created_at,
            updated_at,
        }) => {
            fields.insert(
                EntityField::Origin,
                field(
                    "origin",
                    object(&[
                        field("type", quoted("managed")),
                        field("resource_version", resource_version.to_string().len()),
                        field("created_at", timestamp_len(*created_at)),
                        field("updated_at", timestamp_len(*updated_at)),
                    ]),
                ),
            );
        }
        None => {}
    }
    for (name, selected, document) in [
        ("content", EntityField::Content, &snapshot.content),
        (
            "resolved_schema",
            EntityField::ResolvedSchema,
            &snapshot.resolved_schema,
        ),
        (
            "effective_traits",
            EntityField::EffectiveTraits,
            &snapshot.effective_traits,
        ),
        (
            "effective_traits_schema",
            EntityField::EffectiveTraitsSchema,
            &snapshot.effective_traits_schema,
        ),
    ] {
        if let Some(document) = document {
            fields.insert(selected, field(name, json_len(document)));
        }
    }
    if let Some(provenance) = &snapshot.provenance {
        let skipped = if provenance.compatibility_check_skipped {
            "true"
        } else {
            "false"
        };
        fields.insert(
            EntityField::Provenance,
            field(
                "provenance",
                object(&[field("compatibility_check_skipped", skipped.len())]),
            ),
        );
    }
    Weight { identity, fields }
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
    if take(EntityField::Origin) {
        fresh.origin = resident.origin;
    }
    if take(EntityField::Content) {
        fresh.content.clone_from(&resident.content);
    }
    if take(EntityField::ResolvedSchema) {
        fresh.resolved_schema.clone_from(&resident.resolved_schema);
    }
    if take(EntityField::EffectiveTraits) {
        fresh
            .effective_traits
            .clone_from(&resident.effective_traits);
    }
    if take(EntityField::EffectiveTraitsSchema) {
        fresh
            .effective_traits_schema
            .clone_from(&resident.effective_traits_schema);
    }
    if take(EntityField::Provenance) {
        fresh.provenance = resident.provenance;
    }
    fresh
}

/// The fields of both selections.
pub fn union(a: &FieldSelection, b: &FieldSelection) -> FieldSelection {
    let fields: Vec<EntityField> = a.fields().chain(b.fields()).collect();
    FieldSelection::with(&fields)
}

/// A hyphenated UUID, quoted.
const UUID_LEN: usize = 38;

/// `"name":value`.
fn field(name: &str, value: usize) -> usize {
    quoted(name).saturating_add(1).saturating_add(value)
}

/// `{a,b,...}`.
fn object(fields: &[usize]) -> usize {
    fields
        .iter()
        .fold(2 + fields.len().saturating_sub(1), |sum, f| {
            sum.saturating_add(*f)
        })
}

/// A string needing no escapes, quoted.
fn quoted(s: &str) -> usize {
    s.len() + 2
}

/// RFC 3339, quoted. A timestamp RFC 3339 cannot write cannot be serialized either; it
/// is charged as oversized so that its entity is never retained.
fn timestamp_len(at: OffsetDateTime) -> usize {
    at.format(&Rfc3339).map_or(usize::MAX, |s| quoted(&s))
}

fn str_len(s: &str) -> usize {
    counted(|w| serde_json::to_writer(w, s))
}

fn json_len(document: &JsonDocument) -> usize {
    counted(|w| serde_json::to_writer(w, document))
}

/// Bytes `write` produces. Writing to a counter cannot fail, and neither can a `Value`
/// or `str`; were it to, the value is charged as oversized so that it is never retained.
fn counted(write: impl FnOnce(&mut ByteCounter) -> serde_json::Result<()>) -> usize {
    let mut counter = ByteCounter(0);
    match write(&mut counter) {
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

/// What the store knows of one entity: its resident entries, the reads in flight
/// for it, and when anything was last recorded for it — an answer taken into the store or an
/// invalidation. It lives while either a representation is resident or a read holds a
/// ticket, so the bookkeeping is bounded by resident entities plus reads in flight.
#[derive(Debug, Default)]
struct Slot {
    keys: Vec<EntryKey>,
    tickets: usize,
    /// The store's event count when something was last recorded for this entity.
    last_event: u64,
}

/// A read's view of its entity when it started: the entity's last event then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    seen: u64,
}

impl Slot {
    fn unused(&self) -> bool {
        self.keys.is_empty() && self.tickets == 0
    }
}

/// Entries bounded by the bytes they hold, indexed by entity so that an invalidation
/// drops all of them and fences reads that started before it.
pub struct Store {
    entries: LruCache<EntryKey, Entry>,
    slots: HashMap<Uuid, Slot>,
    /// Events recorded so far, across entities: orders events independently of the clock.
    events: u64,
    used: usize,
    bound: usize,
}

impl Store {
    pub fn new(bound: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            slots: HashMap::new(),
            events: 0,
            used: 0,
            bound,
        }
    }

    /// The entry under `key`, marked most recently used.
    pub fn get(&mut self, key: &EntryKey) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// The entry under `key`, without marking it used.
    pub fn peek(&self, key: &EntryKey) -> Option<&Entry> {
        self.entries.peek(key)
    }

    /// Registers a read of the entity about to go to the registry. Every ticket is
    /// [released](Self::release) exactly once.
    pub fn ticket(&mut self, uuid: Uuid) -> Ticket {
        let slot = self.slots.entry(uuid).or_default();
        slot.tickets += 1;
        Ticket {
            seen: slot.last_event,
        }
    }

    pub fn release(&mut self, uuid: Uuid) {
        if let Some(slot) = self.slots.get_mut(&uuid) {
            slot.tickets = slot.tickets.saturating_sub(1);
            if slot.unused() {
                self.slots.remove(&uuid);
            }
        }
    }

    /// Whether nothing was recorded for the entity since `ticket` was taken. A ticket keeps
    /// its slot, so the event count it saw is never reused.
    pub fn quiet_since(&self, uuid: Uuid, ticket: Ticket) -> bool {
        self.slots
            .get(&uuid)
            .is_some_and(|slot| slot.last_event == ticket.seen)
    }

    /// Records that an answer for the entity was taken into the store.
    pub fn record(&mut self, uuid: Uuid) {
        self.events += 1;
        if let Some(slot) = self.slots.get_mut(&uuid) {
            slot.last_event = self.events;
        }
    }

    /// Whether a snapshot of `weight` can be retained at all.
    pub fn fits(&self, weight: &Weight) -> bool {
        weight.fits(self.bound)
    }

    /// Stores `snapshot`, holding `covers` under `etag`, as `key`'s entry, replacing what
    /// was there, and evicts the least recently used entries until it fits. A snapshot
    /// that does not [fit](Self::fits) is not retained, and the entry it would replace is
    /// dropped rather than left stale. `weight` is [`weight`] of `snapshot`, measured
    /// before taking the store's lock.
    pub fn insert(
        &mut self,
        key: EntryKey,
        snapshot: Arc<Entity>,
        covers: FieldSelection,
        weight: Weight,
        etag: Validator,
        confirmed_at: Instant,
    ) {
        self.remove(&key);
        if !self.fits(&weight) {
            return;
        }
        let bytes = weight.total();
        // `used <= bound` holds throughout, so the subtraction cannot underflow.
        while bytes > self.bound - self.used {
            let Some((evicted, entry)) = self.entries.pop_lru() else {
                break;
            };
            self.forget(&evicted, entry.weight.total());
        }
        self.used += bytes;
        self.slots.entry(key.uuid).or_default().keys.push(key);
        self.entries.put(
            key,
            Entry {
                snapshot,
                covers,
                etag,
                confirmed_at,
                weight,
            },
        );
    }

    /// Records that the registry confirmed `etag` for `key` at `at`, if that is still the
    /// resident validator; a confirmation never moves the instant back.
    pub fn confirm(&mut self, key: &EntryKey, etag: &Validator, at: Instant) {
        if let Some(entry) = self.entries.get_mut(key)
            && entry.etag == *etag
        {
            entry.confirmed_at = entry.confirmed_at.max(at);
        }
    }

    pub fn remove(&mut self, key: &EntryKey) {
        if let Some(entry) = self.entries.pop(key) {
            self.forget(key, entry.weight.total());
        }
    }

    /// An observed mutation of the entity: every entry of it is dropped, and
    /// reads that started before it can no longer fill the store. With no slot, nothing
    /// is resident and no read is in flight, so there is nothing to fence.
    pub fn invalidate(&mut self, uuid: Uuid) {
        self.events += 1;
        let Some(slot) = self.slots.get_mut(&uuid) else {
            return;
        };
        slot.last_event = self.events;
        for key in std::mem::take(&mut slot.keys) {
            if let Some(entry) = self.entries.pop(&key) {
                self.used -= entry.weight.total();
            }
        }
        if slot.unused() {
            self.slots.remove(&uuid);
        }
    }

    fn forget(&mut self, key: &EntryKey, weight: usize) {
        self.used -= weight;
        if let Some(slot) = self.slots.get_mut(&key.uuid) {
            slot.keys.retain(|k| k != key);
            if slot.unused() {
                self.slots.remove(&key.uuid);
            }
        }
    }

    #[cfg(test)]
    pub fn used(&self) -> usize {
        self.used
    }

    /// Whether the store keeps any bookkeeping for the entity.
    #[cfg(test)]
    pub fn indexes(&self, uuid: Uuid) -> bool {
        self.slots.contains_key(&uuid)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
