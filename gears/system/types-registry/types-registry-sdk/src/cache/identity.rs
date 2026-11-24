//! Positive-only LRU of registry-confirmed identity pairs. It has its own recency
//! and no validator, expiry or mutation invalidation. Deleted entities still
//! resolve in P0. A tenant cache answers a pair a read or discovery classified as
//! global to every authenticated reader, and any other pair only to a read context
//! that the registry showed it to.
//!
//! After purge (ADR-0013, deferred) or a logical registry change, recreate the
//! cached client or restart its consumer. `NotFound` alone leaves identities intact.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use gts::GtsId;
use lru::LruCache;
use uuid::Uuid;

use crate::models::{EntityIdentity, EntityKey};

/// Each confirmed pair has one LRU position. Both indexes share its parsed identifier.
pub struct Identities {
    by_uuid: Option<LruCache<Uuid, Record>>,
    by_id: HashMap<Arc<GtsId>, Uuid>,
}

struct Record {
    gts_id: Arc<GtsId>,
    /// Whether the entity is global, once a reply carrying the entity said so.
    /// A bare resolution leaves it unknown; a contradiction makes it unknown again.
    global: Option<bool>,
    /// Identifies this residency. Proofs hold it weakly, so eviction retires them
    /// even if the pair is learned again.
    incarnation: Arc<()>,
}

impl Identities {
    /// Retains up to `bound` identities, allocating as it fills.
    pub fn new(bound: usize) -> Self {
        Self {
            by_uuid: NonZeroUsize::new(bound).map(LruCache::sparse),
            by_id: HashMap::new(),
        }
    }

    /// Look up the exact key the caller supplied. A hit in either direction promotes the pair.
    pub fn get(&mut self, key: &EntityKey) -> Option<EntityIdentity> {
        let store = self.by_uuid.as_mut()?;
        let gts_uuid = match key {
            EntityKey::GtsId(id) => *self.by_id.get(id)?,
            EntityKey::GtsUuid(uuid) => *uuid,
        };
        Some(EntityIdentity {
            gts_id: store.get(&gts_uuid)?.gts_id.as_ref().clone(),
            gts_uuid,
        })
    }

    /// The Registry Reference `key` names, without promoting the pair.
    pub fn peek_uuid(&self, key: &EntityKey) -> Option<Uuid> {
        let store = self.by_uuid.as_ref()?;
        let gts_uuid = match key {
            EntityKey::GtsId(id) => *self.by_id.get(id)?,
            EntityKey::GtsUuid(uuid) => *uuid,
        };
        store.contains(&gts_uuid).then_some(gts_uuid)
    }

    /// Whether the pair for `gts_uuid` is known to be global, without promoting it.
    pub fn is_global(&self, gts_uuid: Uuid) -> Option<bool> {
        self.by_uuid.as_ref()?.peek(&gts_uuid)?.global
    }

    /// Record what an entity-carrying reply said of a resident pair's ownership.
    pub fn classify(&mut self, gts_uuid: Uuid, global: bool) {
        if let Some(record) = self
            .by_uuid
            .as_mut()
            .and_then(|store| store.peek_mut(&gts_uuid))
        {
            record.global = match record.global {
                None => Some(global),
                Some(known) if known == global => Some(known),
                Some(_) => None,
            };
        }
    }

    /// The remembered identifier for `gts_uuid`, without promoting it.
    pub fn peek_id(&self, gts_uuid: Uuid) -> Option<&GtsId> {
        Some(self.by_uuid.as_ref()?.peek(&gts_uuid)?.gts_id.as_ref())
    }

    /// The current residency of the pair for `gts_uuid`, without promoting it.
    pub fn incarnation(&self, gts_uuid: Uuid) -> Option<&Arc<()>> {
        Some(&self.by_uuid.as_ref()?.peek(&gts_uuid)?.incarnation)
    }

    /// Can this pair coexist with every mapping we still remember?
    pub fn agrees(&self, id: &GtsId, uuid: Uuid) -> bool {
        self.conflict(id, uuid).is_none()
    }

    /// Return a conflicting counterpart without changing recency.
    pub fn conflict(&self, id: &GtsId, uuid: Uuid) -> Option<EntityKey> {
        if let Some(known) = self.by_id.get(id).filter(|known| **known != uuid) {
            return Some(EntityKey::GtsUuid(*known));
        }
        self.by_uuid
            .as_ref()
            .and_then(|store| store.peek(&uuid))
            .filter(|known| known.gts_id.as_ref() != id)
            .map(|known| EntityKey::GtsId(known.gts_id.as_ref().clone()))
    }

    /// Number of resident pairs in the bounded identity store.
    pub(super) fn len(&self) -> usize {
        self.by_uuid.as_ref().map_or(0, LruCache::len)
    }

    /// Record a checked pair and report whether it displaced an LRU entry.
    pub fn learn(&mut self, gts_id: &GtsId, gts_uuid: Uuid) -> bool {
        debug_assert!(self.agrees(gts_id, gts_uuid));
        let Some(store) = self.by_uuid.as_mut() else {
            return false;
        };
        if store.get(&gts_uuid).is_some() {
            return false;
        }
        let id = Arc::new(gts_id.clone());
        let evicted = store.push(
            gts_uuid,
            Record {
                gts_id: Arc::clone(&id),
                global: None,
                incarnation: Arc::default(),
            },
        );
        if let Some((_, evicted)) = &evicted {
            self.by_id.remove(&evicted.gts_id);
        }
        self.by_id.insert(id, gts_uuid);
        evicted.is_some()
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod identity_tests;
