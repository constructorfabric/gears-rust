// Created: 2026-09-06 by Virtuozzo International GmbH
//! The local effective-value cache.
//!
//! A copy of rows this gear already owns, keyed by setting key and scope, held
//! in process so the hot read never reaches the database. It is not shared
//! state: the database is the source of truth and the cache can be dropped at
//! any moment. Cross-replica convergence is the R2 `cache_invalidate`
//! broadcast; until then the time-to-live is the backstop.
//!
//! It is bounded. The hot working set of one instance is the design's sizing
//! anchor, not the cross-product of settings and tenants, so the cache holds
//! at most `max_entries` and, at capacity, lets the entry nearest to expiry
//! go first. A cold entry past its time-to-live is dropped by the next store
//! anywhere, not only by its own lookup.
//!
//! A store is a compare-and-swap. A read captures the cache's [`Generation`]
//! for its key and scope the moment it leaves empty-handed, before it touches
//! the database, and hands it back with the value it resolved; the store is
//! refused when an invalidation of the key or the scope has moved the
//! generation since, because what the read brought back may predate the write
//! that evicted the slot.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{EffectiveValue, scope_class};

struct Entry {
    stored_at: Instant,
    value: Arc<EffectiveValue>,
}

/// The entries, and the order they were stored in.
///
/// The queue is what bounds the map. Every store pushes its slot and instant;
/// eviction pops from the head — the oldest stored, which under one
/// time-to-live is the nearest to expiry. A record whose slot was replaced or
/// invalidated since no longer matches its entry's instant and is skipped when
/// it reaches the head, so no eviction ever searches.
struct Store {
    entries: HashMap<(String, Uuid), Entry>,
    order: VecDeque<(String, Uuid, Instant)>,
    /// How many invalidations have touched each key: a value write, a
    /// declaration change, an access change on the key.
    key_generations: HashMap<String, u64>,
    /// How many subtree evictions have touched each tenant: a hierarchy
    /// change, where no key is named at all.
    tenant_generations: HashMap<Uuid, u64>,
}

/// What a read captures before it goes to the database and hands back with
/// the value it resolved: the invalidations that had touched its key and its
/// scope by then. Opaque, compared for equality only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    key: u64,
    tenant: u64,
}

impl Store {
    fn generation(&self, key: &str, tenant: Uuid) -> Generation {
        Generation {
            key: self.key_generations.get(key).copied().unwrap_or(0),
            tenant: self.tenant_generations.get(&tenant).copied().unwrap_or(0),
        }
    }

    fn bump_key(&mut self, key: &str) {
        *self.key_generations.entry(key.to_owned()).or_insert(0) += 1;
    }

    fn bump_tenant(&mut self, tenant: Uuid) {
        *self.tenant_generations.entry(tenant).or_insert(0) += 1;
    }

    /// Drop the entries at the head that are past `ttl`.
    fn sweep_expired(&mut self, ttl: Duration) {
        while self
            .order
            .front()
            .is_some_and(|(_, _, stored_at)| stored_at.elapsed() > ttl)
        {
            if let Some((key, tenant, stored_at)) = self.order.pop_front() {
                self.remove_if_stored_at(&(key, tenant), stored_at);
            }
        }
    }

    /// Evict the oldest entry still held, skipping records that no longer
    /// describe one. Every held entry has the record of its latest store in
    /// the queue, so a non-empty map always yields one.
    fn evict_oldest(&mut self) {
        while let Some((key, tenant, stored_at)) = self.order.pop_front() {
            if self.remove_if_stored_at(&(key, tenant), stored_at) {
                return;
            }
        }
    }

    /// Remove the slot's entry when it is the one the record describes.
    fn remove_if_stored_at(&mut self, slot: &(String, Uuid), stored_at: Instant) -> bool {
        let held = self
            .entries
            .get(slot)
            .is_some_and(|entry| entry.stored_at == stored_at);
        if held {
            self.entries.remove(slot);
        }
        held
    }
}

/// The cache. This is the definition site of the time-to-live knob and of the
/// capacity; other components reference them rather than defining their own.
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-cache:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-cache-ttl:p1
pub struct EffectiveCache {
    ttl: Duration,
    max_entries: usize,
    store: Mutex<Store>,
}

impl EffectiveCache {
    /// The design's sizing anchor for one instance's hot working set: what
    /// [`Self::new`] bounds a cache at, and what the `cache_max_entries` knob
    /// defaults to.
    pub const DEFAULT_MAX_ENTRIES: usize = 500_000;

    /// The design's backstop on staleness: how long a replica may serve a
    /// cached value after a missed invalidation. What the `cache_ttl_seconds`
    /// knob defaults to and may not exceed — a deployment shortens the
    /// backstop, it never widens it.
    pub const TTL_CEILING: Duration = Duration::from_secs(30);

    /// A cache whose entries expire `ttl` after being stored, bounded at the
    /// design's anchor.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self::bounded(ttl, Self::DEFAULT_MAX_ENTRIES)
    }

    /// A cache whose entries expire `ttl` after being stored and that holds
    /// at most `max_entries` of them. A zero is held to one here; the
    /// configuration refuses it before it gets this far.
    #[must_use]
    pub fn bounded(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries: max_entries.max(1),
            store: Mutex::new(Store {
                entries: HashMap::new(),
                order: VecDeque::new(),
                key_generations: HashMap::new(),
                tenant_generations: HashMap::new(),
            }),
        }
    }

    /// The configured time-to-live.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The most entries held at once.
    #[must_use]
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// The generation a read captures on its miss, before it touches the
    /// database, to hand back to [`Self::populate`] with what it resolved.
    #[must_use]
    pub fn generation(&self, key: &str, tenant: Uuid) -> Generation {
        self.lock().generation(key, tenant)
    }

    /// Store an entry under the current generation: what a test does to stand
    /// in for a read that just resolved.
    #[cfg(test)]
    pub fn seed(&self, value: Arc<EffectiveValue>) {
        let seen = self.generation(&value.key, value.tenant_id);
        self.populate(value, seen);
    }

    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Look up the entry for a key at a scope.
    ///
    /// An entry older than the time-to-live is evicted and reported as a miss,
    /// so a missed invalidation self-heals within that bound.
    #[must_use]
    pub fn get(&self, key: &str, tenant: Uuid) -> Option<Arc<EffectiveValue>> {
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-1
        let mut store = self.lock();
        let slot = (key.to_owned(), tenant);
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-1
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-2
        let entry = store.entries.get(&slot)?;
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-3
        if entry.stored_at.elapsed() > self.ttl {
            store.entries.remove(&slot);
            return None;
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-3
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-4
        Some(Arc::clone(&entry.value))
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-4
    }

    /// Store a resolved value with its source trace, if `seen` — the
    /// generation the read captured on its miss — is still current; the
    /// answer says whether it was stored.
    ///
    /// A refused store is a read overtaken by a write: what it brought back
    /// may predate the value that evicted the slot, and storing it would serve
    /// that for another time-to-live after a committed change. The next read
    /// resolves afresh.
    ///
    /// The store is also where the bound is kept: entries past the time-to-live
    /// at the head of the order go first, so a cold entry does not wait for its
    /// own lookup, and at capacity a new slot displaces the oldest stored — the
    /// nearest to expiry — so the map never exceeds `max_entries`. Re-storing a
    /// held slot replaces in place and grows nothing.
    pub fn populate(&self, value: Arc<EffectiveValue>, seen: Generation) -> bool {
        let mut store = self.lock();
        if store.generation(&value.key, value.tenant_id) != seen {
            return false;
        }
        store.sweep_expired(self.ttl);
        let slot = (value.key.clone(), value.tenant_id);
        if !store.entries.contains_key(&slot) && store.entries.len() >= self.max_entries {
            store.evict_oldest();
        }
        let stored_at = Instant::now();
        store.order.push_back((slot.0.clone(), slot.1, stored_at));
        store.entries.insert(slot, Entry { stored_at, value });
        true
    }

    /// Evict for a change to one declaration's value at one scope.
    ///
    /// A `cascading` declaration is evicted key-wide: an ancestor's change
    /// alters its descendants' effective values, and they re-resolve lazily on
    /// their next read. So is a `global` one: every tenant reads the root's
    /// row, cached under the tenant that asked, so the root's slot alone would
    /// leave the rest serving the old value. Eviction is local to this instance; converging peer
    /// replicas is the R2 broadcast and out of scope here.
    pub fn invalidate(&self, key: &str, declaration_scope_class: &str, tenant: Option<Uuid>) {
        let mut store = self.lock();
        // Counted before anything is removed: a read in flight for any scope
        // of this key must find its generation moved when it comes to store.
        store.bump_key(key);
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-2
        // A cascading change alters what descendants inherit; a global one is
        // the value every tenant reads, cached under each tenant that asked
        // though written only at the root. Either way every scope of the key
        // goes. Only a local value is its own scope's alone.
        if declaration_scope_class == scope_class::CASCADING
            || declaration_scope_class == scope_class::GLOBAL
            || tenant.is_none()
        {
            store.entries.retain(|(k, _), _| k != key);
            return;
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-1
        if let Some(tenant) = tenant {
            store.entries.remove(&(key.to_owned(), tenant));
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-1
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-5
        // Evicted locally only. Peer replicas converge on the R2
        // `cache_invalidate` broadcast; until then the time-to-live bounds
        // their staleness.
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-5
    }

    /// Evict a key's entries for the given tenants only: an access change on a
    /// tenant and its descendants, whatever the scope class.
    /// Evict every cached entry of the given tenants, whatever the setting.
    ///
    /// What a tenant hierarchy change costs: an effective value is a function
    /// of the ancestor chain, so a re-parent or a mid-chain insertion changes
    /// what a whole subtree resolves with no value write anywhere. The signal
    /// that would call this does not exist yet — the tenant resolver publishes
    /// no hierarchy event — so until it does the time-to-live is the only
    /// backstop and the post-re-parent staleness window equals it.
    // @cpt-dod:cpt-cf-settings-service-dod-value-resolution-hierarchy-invalidation:p1
    pub fn invalidate_subtree(&self, tenants: &[Uuid]) {
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-3
        if tenants.is_empty() {
            return;
        }
        // One membership set, built before the lock: the pass over the entries
        // is then linear in the cache, not in the cache times the subtree —
        // at the sizing anchors that difference is the lock held for
        // milliseconds rather than for the better part of a minute.
        let tenants: HashSet<Uuid> = tenants.iter().copied().collect();
        let mut store = self.lock();
        for tenant in &tenants {
            store.bump_tenant(*tenant);
        }
        // Every key, because which settings cascade is a property of each
        // declaration and the subtree's chain changed for all of them at once;
        // a non-cascading entry re-resolves to the same answer, so evicting it
        // costs a read and risks nothing.
        store
            .entries
            .retain(|(_, tenant), _| !tenants.contains(tenant));
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-3
    }

    pub fn invalidate_tenants(&self, key: &str, tenants: &[Uuid]) {
        let mut store = self.lock();
        store.bump_key(key);
        for tenant in tenants {
            store.entries.remove(&(key.to_owned(), *tenant));
        }
    }

    /// Evict every scope of a key: for a declaration change, whose default or
    /// traits alter every scope's effective value at once.
    pub fn invalidate_key(&self, key: &str) {
        self.invalidate(key, scope_class::CASCADING, None);
    }

    // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-4
    // A cached effective value of a cascading setting is a function of the
    // ancestor chain, so a tenant re-parent or a mid-chain insertion changes
    // the right answer with no value write involved. The tenant resolver
    // publishes no hierarchy-change signal today, so there is nothing to
    // subscribe to here: until it does, the time-to-live is the only backstop
    // and the post-re-parent staleness window equals it.
    // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-4

    /// How many entries are held, stale ones included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod cache_tests;

/// A minimal entry for tests elsewhere in the crate.
#[cfg(test)]
#[must_use]
pub fn tests_entry(key: &str, tenant: Uuid) -> EffectiveValue {
    EffectiveValue {
        key: key.to_owned(),
        declaration_id: Uuid::nil(),
        scope: format!("/tenants/{tenant}"),
        tenant_id: tenant,
        value: serde_json::json!(1),
        source: settings_service_sdk::EffectiveSource::SchemaDefault,
        source_scope: None,
        fallback: serde_json::json!(1),
        fallback_source: settings_service_sdk::EffectiveSource::SchemaDefault,
        fallback_scope: None,
        traits: serde_json::json!({}),
        trail: Vec::new(),
        data_classification: "public".to_owned(),
        domain_affinity: None,
        secret_backed: false,
        declaration_last_change_at: time::OffsetDateTime::UNIX_EPOCH,
        resolved_row_last_change_at: None,
        own_row: None,
    }
}
