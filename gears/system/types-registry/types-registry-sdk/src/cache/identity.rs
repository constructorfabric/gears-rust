//! The cache's identity store: the canonical identifier of each registered entity the
//! client has read, by Registry Reference, for the platform `resolve_*` helpers.
//!
//! Positive only and permanent: an identity is learned from a checked registry answer
//! that found the entity, and in P0 nothing makes it wrong later — identity is permanent
//! and a deleted entity still resolves. So the store has no window, no validator, no
//! negative entries and no invalidation on an observed mutation. It is bounded by
//! entries, evicted least-recently-used, apart from the snapshot store: neither touches
//! the other's recency.
//!
//! P0 has no purge (SPEC §2, deferred), so nothing ever forgets an identity. ADR-0013's purge,
//! after which an identifier resolves to nothing, and a client switched to another
//! logical registry, will each need this store reset; a `NotFound` seen in passing is no
//! signal of either.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use gts::GtsId;
use lru::LruCache;
use uuid::Uuid;

use crate::models::{EntityIdentity, EntityKey};

/// Each confirmed pair has one LRU position. Both indexes share its parsed identifier.
pub struct Identities {
    by_uuid: Option<LruCache<Uuid, Arc<GtsId>>>,
    by_id: HashMap<Arc<GtsId>, Uuid>,
}

impl Identities {
    /// A store of at most `bound` identities, allocated as it fills rather than for the
    /// whole bound up front.
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
            gts_id: store.get(&gts_uuid)?.as_ref().clone(),
            gts_uuid,
        })
    }

    /// Can this pair coexist with every mapping we still remember?
    pub fn agrees(&self, id: &GtsId, uuid: Uuid) -> bool {
        self.conflict(id, uuid).is_none()
    }

    /// The remembered counterpart of either spelling, when it is not the other one.
    /// Changes no recency.
    pub fn conflict(&self, id: &GtsId, uuid: Uuid) -> Option<EntityKey> {
        if let Some(known) = self.by_id.get(id).filter(|known| **known != uuid) {
            return Some(EntityKey::GtsUuid(*known));
        }
        self.by_uuid
            .as_ref()
            .and_then(|store| store.peek(&uuid))
            .filter(|known| known.as_ref() != id)
            .map(|known| EntityKey::GtsId(known.as_ref().clone()))
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
        let evicted = store.push(gts_uuid, Arc::clone(&id));
        if let Some((_, evicted)) = &evicted {
            self.by_id.remove(evicted);
        }
        self.by_id.insert(id, gts_uuid);
        evicted.is_some()
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod identity_tests;
