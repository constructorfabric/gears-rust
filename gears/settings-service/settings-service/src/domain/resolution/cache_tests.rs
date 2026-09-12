// Created: 2026-09-06 by Constructor Tech
//! The cache's contract: hit, miss, expiry, and the eviction shapes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use settings_service_sdk::EffectiveSource;
use uuid::Uuid;

use super::{EffectiveCache, tests_entry};
use crate::domain::resolution::{EffectiveValue, scope_class};

fn entry(key: &str, tenant: Uuid) -> Arc<EffectiveValue> {
    Arc::new(EffectiveValue {
        key: key.to_owned(),
        declaration_id: Uuid::nil(),
        scope: format!("/tenants/{tenant}"),
        tenant_id: tenant,
        value: json!(1),
        source: EffectiveSource::SchemaDefault,
        source_scope: None,
        traits: json!({}),
        trail: Vec::new(),
        data_classification: "public".to_owned(),
        domain_affinity: None,
        secret_backed: false,
        declaration_last_change_at: time::OffsetDateTime::UNIX_EPOCH,
        resolved_row_last_change_at: None,
        own_row: None,
    })
}

#[test]
fn a_populated_entry_is_served_and_an_unknown_one_is_a_miss() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let t = Uuid::new_v4();
    cache.populate(entry("k", t));
    assert!(cache.get("k", t).is_some());
    assert!(
        cache.get("k", Uuid::new_v4()).is_none(),
        "another scope is another entry"
    );
    assert!(cache.get("other", t).is_none());
}

#[test]
fn an_entry_older_than_the_ttl_is_a_miss_and_is_evicted() {
    let cache = EffectiveCache::new(Duration::ZERO);
    let t = Uuid::new_v4();
    cache.populate(entry("k", t));
    std::thread::sleep(Duration::from_millis(2));
    assert!(cache.get("k", t).is_none());
    assert!(
        cache.is_empty(),
        "the stale entry is gone, not merely skipped"
    );
}

#[test]
fn a_cascading_change_evicts_every_scope_of_the_key_and_nothing_else() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    cache.populate(entry("k", a));
    cache.populate(entry("k", b));
    cache.populate(entry("other", a));

    cache.invalidate("k", scope_class::CASCADING, Some(a));

    assert!(cache.get("k", a).is_none());
    assert!(cache.get("k", b).is_none(), "descendants re-resolve lazily");
    assert!(cache.get("other", a).is_some());
}

#[test]
fn a_local_change_evicts_only_the_named_scope() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    cache.populate(entry("k", a));
    cache.populate(entry("k", b));

    cache.invalidate("k", scope_class::LOCAL, Some(a));

    assert!(cache.get("k", a).is_none());
    assert!(cache.get("k", b).is_some());
}

#[test]
fn a_declaration_change_evicts_the_whole_key() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    cache.populate(entry("k", a));
    cache.populate(entry("k", b));
    cache.invalidate_key("k");
    assert!(cache.is_empty());
}

#[test]
fn a_hierarchy_change_evicts_the_affected_subtree_across_every_setting() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let moved = Uuid::new_v4();
    let below = Uuid::new_v4();
    let elsewhere = Uuid::new_v4();
    for tenant in [moved, below, elsewhere] {
        for key in ["one", "two"] {
            cache.populate(Arc::new(tests_entry(key, tenant)));
        }
    }
    assert_eq!(cache.len(), 6);

    // A re-parent changes what the moved tenant and everything under it
    // resolves, for every setting at once, with no value write involved.
    cache.invalidate_subtree(&[moved, below]);
    assert_eq!(cache.len(), 2);
    assert!(cache.get("one", elsewhere).is_some());
    assert!(cache.get("two", elsewhere).is_some());
    assert!(cache.get("one", moved).is_none());
    assert!(cache.get("two", below).is_none());

    // An empty subtree is not an instruction to evict everything.
    cache.invalidate_subtree(&[]);
    assert_eq!(cache.len(), 2);
}
