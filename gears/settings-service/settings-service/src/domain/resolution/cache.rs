// Created: 2026-09-06 by Constructor Tech
//! The local effective-value cache.
//!
//! A copy of rows this gear already owns, keyed by setting key and scope, held
//! in process so the hot read never reaches the database. It is not shared
//! state: the database is the source of truth and the cache can be dropped at
//! any moment. Cross-replica convergence is the R2 `cache_invalidate`
//! broadcast; until then the time-to-live is the backstop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{EffectiveValue, scope_class};

struct Entry {
    stored_at: Instant,
    value: Arc<EffectiveValue>,
}

/// The cache. This is the definition site of the time-to-live knob; other
/// components reference it rather than defining their own.
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-cache:p1
// @cpt-dod:cpt-cf-settings-service-dod-value-resolution-cache-ttl:p1
pub struct EffectiveCache {
    ttl: Duration,
    entries: Mutex<HashMap<(String, Uuid), Entry>>,
}

impl EffectiveCache {
    /// A cache whose entries expire `ttl` after being stored.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// The configured time-to-live.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Look up the entry for a key at a scope.
    ///
    /// An entry older than the time-to-live is evicted and reported as a miss,
    /// so a missed invalidation self-heals within that bound.
    #[must_use]
    pub fn get(&self, key: &str, tenant: Uuid) -> Option<Arc<EffectiveValue>> {
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-1
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = (key.to_owned(), tenant);
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-1
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-2
        let entry = entries.get(&slot)?;
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-3
        if entry.stored_at.elapsed() > self.ttl {
            entries.remove(&slot);
            return None;
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-3
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-4
        Some(Arc::clone(&entry.value))
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-read:p1:inst-vr-cache-4
    }

    /// Store a resolved value with its source trace.
    pub fn populate(&self, value: Arc<EffectiveValue>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.insert(
            (value.key.clone(), value.tenant_id),
            Entry {
                stored_at: Instant::now(),
                value,
            },
        );
    }

    /// Evict for a change to one declaration's value at one scope.
    ///
    /// A `cascading` declaration is evicted key-wide: an ancestor's change
    /// alters its descendants' effective values, and they re-resolve lazily on
    /// their next read. Eviction is local to this instance; converging peer
    /// replicas is the R2 broadcast and out of scope here.
    pub fn invalidate(&self, key: &str, declaration_scope_class: &str, tenant: Option<Uuid>) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-2
        if declaration_scope_class == scope_class::CASCADING || tenant.is_none() {
            entries.retain(|(k, _), _| k != key);
            return;
        }
        // @cpt-end:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-2
        // @cpt-begin:cpt-cf-settings-service-algo-value-resolution-cache-invalidate:p1:inst-vr-inv-1
        if let Some(tenant) = tenant {
            entries.remove(&(key.to_owned(), tenant));
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
    pub fn invalidate_tenants(&self, key: &str, tenants: &[Uuid]) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for tenant in tenants {
            entries.remove(&(key.to_owned(), *tenant));
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
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
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
