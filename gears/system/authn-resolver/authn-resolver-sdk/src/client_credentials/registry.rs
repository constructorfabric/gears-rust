//! Dynamic scope slots of a [`ClientCredentialsProvider`](super::ClientCredentialsProvider):
//! admission, pinning and eviction. Lock order: registry, then a slot's state.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::time::Instant;

use super::{CONFIGURED_SLOT, ClientCredentialsProviderError, Core, Handle, Shared, State};
use crate::models::RequestCacheControl;
use crate::scopes::ScopeSet;

#[derive(Default)]
pub(super) struct Registry {
    /// Set by shutdown and last-handle drop: no slot is admitted afterwards.
    pub(super) closed: bool,
    pub(super) slots: HashMap<ScopeSet, DynamicSlot>,
    /// Slot ids are never reused, so a tag cannot match a recreated slot.
    next_slot: u64,
    /// Use counter, advanced by every admission; unique, so LRU order has no ties.
    tick: u64,
}

pub(super) struct DynamicSlot {
    pub(super) shared: Arc<Shared>,
    last_used: u64,
}

/// An admitted call's hold on its slot; dynamic slots stay pinned until drop.
pub(super) struct Lease {
    pub(super) shared: Arc<Shared>,
    pinned: bool,
}

impl Lease {
    pub(super) fn configured(shared: &Arc<Shared>) -> Self {
        Self {
            shared: Arc::clone(shared),
            pinned: false,
        }
    }

    /// Must be called under the registry lock.
    fn pin(shared: &Arc<Shared>) -> Self {
        shared.pinned.fetch_add(1, Ordering::SeqCst);
        Self {
            shared: Arc::clone(shared),
            pinned: true,
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.pinned {
            self.shared.pinned.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Handle {
    /// Find or create the slot for `scopes` and pin it, atomically.
    pub(super) fn admit(&self, scopes: &ScopeSet) -> Result<Lease, ClientCredentialsProviderError> {
        if self.shared.scopes.as_ref() == Some(scopes) {
            return Ok(Lease::configured(&self.shared));
        }
        let core = &self.shared.core;
        let mut registry = self.registry.lock();
        if registry.closed {
            return Err(ClientCredentialsProviderError::ShutDown);
        }
        registry.tick += 1;
        let tick = registry.tick;
        if let Some(slot) = registry.slots.get_mut(scopes) {
            slot.last_used = tick;
            return Ok(Lease::pin(&slot.shared));
        }
        let max_entries = core.config.max_entries;
        let exhausted = ClientCredentialsProviderError::CapacityExhausted { max_entries };
        // The default slot always takes one entry.
        let dynamic_capacity = max_entries - 1;
        if dynamic_capacity == 0 {
            return Err(exhausted);
        }
        if registry.slots.len() >= dynamic_capacity {
            let victim = registry.victim(core).ok_or(exhausted)?;
            registry.slots.remove(&victim);
        }
        let slot = registry.next_slot;
        registry.next_slot += 1;
        // Even a first admission may follow an eviction that forgot a clear.
        let state = State {
            next_request: RequestCacheControl::refresh(),
            ..State::default()
        };
        let shared = Arc::new(Shared::new(
            Arc::clone(core),
            slot,
            Some(scopes.clone()),
            state,
        ));
        let lease = Lease::pin(&shared);
        registry.slots.insert(
            scopes.clone(),
            DynamicSlot {
                shared,
                last_used: tick,
            },
        );
        Ok(lease)
    }
}

impl Registry {
    pub(super) fn new() -> Self {
        Self {
            next_slot: CONFIGURED_SLOT + 1,
            ..Self::default()
        }
    }

    /// The slot to evict, if any is idle: no admitted call, no task, no
    /// flight and no pending backoff. Prefers slots without a usable entry,
    /// then the least recently used.
    fn victim(&self, core: &Core) -> Option<ScopeSet> {
        let now = Instant::now();
        let wall = (core.wall_clock)();
        self.slots
            .iter()
            .filter_map(|(scopes, slot)| {
                let shared = &slot.shared;
                if shared.pinned.load(Ordering::SeqCst) != 0 || *shared.running.borrow() != 0 {
                    return None;
                }
                let state = shared.state.lock();
                if state.flight.is_some() || state.in_backoff(now) {
                    return None;
                }
                let usable = state
                    .entry
                    .as_ref()
                    .is_some_and(|entry| entry.deadline().usable(now, wall));
                Some(((usable, slot.last_used), scopes))
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, scopes)| scopes.clone())
    }
}
