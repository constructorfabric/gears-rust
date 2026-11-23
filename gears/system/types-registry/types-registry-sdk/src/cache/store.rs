//! The cache's bounded store: one entry per entity, holding the fields read
//! of it so far under one validator, evicted least-recently-used by the bytes it holds,
//! and found by either spelling a resident snapshot carries.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Weak};
use std::time::Duration;

use gts::GtsId;
use lru::LruCache;
use tokio::time::Instant;
use uuid::Uuid;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::identity::Identities;
use crate::models::{
    Entity, EntityField, EntityKey, EntityKind, FieldSelection, JsonDocument, LifecycleStatus,
    Origin, Validator,
};

/// Visibility follows the tenant. Remote confirmations also bind the bearer that was
/// presented to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TenantViewer {
    tenant: Uuid,
    credential: Option<[u8; 32]>,
}

impl TenantViewer {
    pub const fn new(tenant: Uuid, credential: Option<[u8; 32]>) -> Self {
        Self { tenant, credential }
    }
}

struct TenantConfirmation {
    version: Weak<()>,
    viewer_epoch: u64,
    read_at: Instant,
}

struct TenantVisibility {
    confirmations: LruCache<(Uuid, TenantViewer), TenantConfirmation>,
    /// Bounded separately so a credential refusal can revoke every confirmation in O(1).
    epochs: LruCache<TenantViewer, u64>,
    last_epoch: u64,
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
    tenant_version: Option<Arc<()>>,
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

    /// Total resident payload bytes, including the validator needed for revalidation.
    pub fn bytes_with_validator(&self, etag: &Validator) -> usize {
        self.total().saturating_add(etag.as_bytes().len())
    }

    /// Whether a snapshot of this weight can be retained under `bound` at all.
    pub fn fits(&self, bound: usize, etag: &Validator) -> bool {
        // `usize::MAX` is the unmeasurable charge: never retained, whatever the bound.
        let bytes = self.bytes_with_validator(etag);
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

/// The reads in flight under one spelling of an entity — its identifier or its Registry
/// Reference — and when anything was last recorded under it. It lives while a read holds
/// a ticket on it, so the bookkeeping is bounded by reads in flight.
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

/// Fences visibility confirmation for a viewer while its request is in flight.
#[derive(Clone, Copy)]
pub struct TenantTicket {
    pub viewer: TenantViewer,
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

/// A read's view of its entity when it started: the spelling it is fenced under and the
/// store's event count then. A read that knew only its key is also fenced by any event
/// that knew only its own, since the two may be one entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    spelling: Spelling,
    seen: u64,
}

/// Entries bounded by the bytes they hold, found by either spelling, with the bookkeeping
/// that lets an invalidation under either one fence the reads that started before it.
///
/// An identifier resolves to the Registry Reference its resident snapshot carries, for
/// exactly as long as that snapshot stays resident: nothing derives one from the other.
pub struct Store {
    entries: LruCache<Uuid, Entry>,
    /// The identity store, under the same lock so that an answer's identities are checked
    /// against both and learned in one critical section. Neither store touches the
    /// other's recency.
    pub identities: Identities,
    tenant_reads: Option<TenantVisibility>,
    /// A revocation fences requests for this viewer, including requests under an alias
    /// the cache may only learn after the answer arrives.
    tenant_in_flight: HashMap<TenantViewer, Slot>,
    /// The identifier of every resident snapshot, to the Registry Reference it carries.
    aliases: HashMap<GtsId, Uuid>,
    by_id: HashMap<GtsId, Slot>,
    by_uuid: HashMap<Uuid, Slot>,
    /// Events recorded so far, across entities: orders events independently of the clock.
    events: u64,
    /// The last event recorded under an identifier, or a Registry Reference, whose
    /// counterpart was not known. Two distinct identifiers, or two distinct Registry
    /// References, are never one entity, so a read that knew only its key is fenced by
    /// its own slot and by these one-sided events of the other form only.
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
            tenant_reads: None,
            tenant_in_flight: HashMap::new(),
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

    /// Remember up to `bound` identities for the platform `resolve_*` helpers.
    pub fn remember_identities(&mut self, bound: usize) {
        self.identities = Identities::new(bound);
    }

    /// Limit confirmations independently of the number of tenants and entities.
    pub fn track_tenant_visibility(&mut self) {
        self.tenant_reads = NonZeroUsize::new(self.max_entries).map(|limit| TenantVisibility {
            confirmations: LruCache::sparse(limit),
            epochs: LruCache::sparse(limit),
            last_epoch: 0,
        });
    }

    pub fn tenant_visible(
        &mut self,
        key: &EntityKey,
        viewer: TenantViewer,
        now: Instant,
        window: Duration,
    ) -> bool {
        let Some(uuid) = self.resident(key) else {
            return false;
        };
        let Some(reads) = self.tenant_reads.as_mut() else {
            return false;
        };
        let version = self
            .entries
            .peek(&uuid)
            .and_then(|entry| entry.tenant_version.as_ref());
        let key = (uuid, viewer);
        let epoch = reads.epochs.peek(&viewer);
        let visible = reads.confirmations.peek(&key).is_some_and(|seen| {
            version.is_some_and(|version| std::ptr::eq(seen.version.as_ptr(), Arc::as_ptr(version)))
                && epoch == Some(&seen.viewer_epoch)
                && now.saturating_duration_since(seen.read_at) < window
        });
        if visible {
            reads.confirmations.promote(&key);
            reads.epochs.promote(&viewer);
        } else {
            reads.confirmations.pop(&key);
        }
        visible
    }

    pub fn confirm_tenant(
        &mut self,
        key: &EntityKey,
        viewer: TenantViewer,
        etag: &Validator,
        read_at: Instant,
    ) {
        let Some(uuid) = self.resident(key) else {
            return;
        };
        if let Some(entry) = self.entries.peek(&uuid)
            && entry.etag == *etag
            && let Some(version) = &entry.tenant_version
            && let Some(reads) = self.tenant_reads.as_mut()
        {
            let epoch = *reads.epochs.get_or_insert(viewer, || {
                reads.last_epoch += 1;
                reads.last_epoch
            });
            reads.confirmations.put(
                (uuid, viewer),
                TenantConfirmation {
                    version: Arc::downgrade(version),
                    viewer_epoch: epoch,
                    read_at,
                },
            );
        }
    }

    pub fn revoke_tenant_reads<'a>(
        &mut self,
        viewer: TenantViewer,
        keys: impl Iterator<Item = &'a EntityKey>,
    ) {
        let mut keys = keys.peekable();
        if keys.peek().is_some() {
            self.fence_tenant(viewer);
        }
        for key in keys {
            if let Some(uuid) = self.resident(key)
                && let Some(reads) = self.tenant_reads.as_mut()
            {
                reads.confirmations.pop(&(uuid, viewer));
            }
        }
    }

    pub fn revoke_viewer(&mut self, viewer: TenantViewer) {
        if let Some(reads) = self.tenant_reads.as_mut() {
            reads.epochs.pop(&viewer);
        }
        self.fence_tenant(viewer);
    }

    fn fence_tenant(&mut self, viewer: TenantViewer) {
        if let Some(slot) = self.tenant_in_flight.get_mut(&viewer) {
            slot.last_event += 1;
        }
    }

    pub fn tenant_ticket(&mut self, viewer: TenantViewer) -> TenantTicket {
        let slot = self.tenant_in_flight.entry(viewer).or_default();
        slot.tickets += 1;
        TenantTicket {
            viewer,
            seen: slot.last_event,
        }
    }

    pub fn release_tenant_ticket(&mut self, ticket: TenantTicket) {
        release_slot(&mut self.tenant_in_flight, &ticket.viewer);
    }

    /// Whether any read for `viewer` holds a ticket.
    #[cfg(test)]
    pub fn tenant_in_flight(&self, viewer: TenantViewer) -> bool {
        self.tenant_in_flight.contains_key(&viewer)
    }

    pub fn tenant_ticket_current(&self, ticket: TenantTicket) -> bool {
        self.tenant_in_flight
            .get(&ticket.viewer)
            .is_some_and(|slot| slot.last_event == ticket.seen)
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

    /// Whether `gts_id` and `gts_uuid` can be one entity beside every resident one: the
    /// resident counterpart of either spelling, when it is not the other one. Changes no
    /// recency.
    pub fn conflict(&self, gts_id: &GtsId, gts_uuid: Uuid) -> Option<EntityKey> {
        if let Some(known) = self.aliases.get(gts_id).filter(|known| **known != gts_uuid) {
            return Some(EntityKey::GtsUuid(*known));
        }
        self.entries
            .peek(&gts_uuid)
            .filter(|entry| entry.snapshot.gts_id != *gts_id)
            .map(|entry| EntityKey::GtsId(entry.snapshot.gts_id.clone()))
    }

    /// The entity `key` spells, by the pair stated for it, or else by the one a resident
    /// snapshot carries, or else by `key` alone.
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

    /// Registers a read of the entity `key` spells, about to go to the registry, under
    /// `key` and the counterpart a resident snapshot carries. Every ticket is
    /// [released](Self::release) exactly once.
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

    /// Whether nothing was recorded for the entity since `ticket` was taken: under neither
    /// spelling it knew, and, if it knew only its key, under no key of the other form known
    /// only by itself either — that may be the same entity; another key of its own form is
    /// not. A ticket keeps its slots, so the event counts they saw are never reused.
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

    /// Records that an answer for the entity `key` spells was taken into the store; `stated`
    /// is the pair the answer, or the entry the read started from, names. Reads in flight
    /// under either spelling are fenced, and with the key alone, so is every read that
    /// knew only its own key of the other form.
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

    /// Stores `snapshot`, holding `covers` under `etag`, as its entity's entry, replacing
    /// what was there. Returns the number of LRU entries evicted for capacity. A snapshot
    /// that does not [fit](Self::fits) is not retained, and the entry it would replace is
    /// dropped rather than left stale. `weight` is [`weight`] of `snapshot`, measured
    /// before taking the store's lock. The caller has checked that its identity has no
    /// [conflict](Self::conflict) with what is resident; a resident entity it contradicts is
    /// dropped all the same, so that no spelling ever finds two entities.
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
        let tenant_version = self.tenant_reads.as_ref().map(|_| {
            self.entries
                .peek(&key)
                .filter(|old| old.etag == etag)
                .and_then(|old| old.tenant_version.clone())
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
                tenant_version,
            },
        );
        evictions
    }

    /// Records that the registry confirmed `etag` for the entity `key` spells at `at`, if
    /// that is still the resident validator; a confirmation never moves the instant back.
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

    /// An observed mutation of the entity `key` spells — `stated` is its pair, where an
    /// answer stated one: its entry is dropped, and reads that started before it can no
    /// longer fill the store, under either spelling known here or, with the key alone,
    /// any read that knew only its own.
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
