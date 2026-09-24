// Created: 2026-09-06 by Virtuozzo International GmbH
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
        fallback: json!(1),
        fallback_source: EffectiveSource::SchemaDefault,
        fallback_scope: None,
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
    cache.seed(entry("k", t));
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
    cache.seed(entry("k", t));
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
    cache.seed(entry("k", a));
    cache.seed(entry("k", b));
    cache.seed(entry("other", a));

    cache.invalidate("k", scope_class::CASCADING, Some(a));

    assert!(cache.get("k", a).is_none());
    assert!(cache.get("k", b).is_none(), "descendants re-resolve lazily");
    assert!(cache.get("other", a).is_some());
}

#[test]
fn a_local_change_evicts_only_the_named_scope() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    cache.seed(entry("k", a));
    cache.seed(entry("k", b));

    cache.invalidate("k", scope_class::LOCAL, Some(a));

    assert!(cache.get("k", a).is_none());
    assert!(cache.get("k", b).is_some());
}

#[test]
fn a_declaration_change_evicts_the_whole_key() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    cache.seed(entry("k", a));
    cache.seed(entry("k", b));
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
            cache.seed(Arc::new(tests_entry(key, tenant)));
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

#[test]
fn an_expired_entry_is_swept_by_the_next_store_not_only_by_its_own_lookup() {
    // A zero time-to-live: everything stored is stale by the next instruction.
    let cache = EffectiveCache::new(Duration::ZERO);
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    cache.seed(entry("one", a));
    assert_eq!(cache.len(), 1);

    // Nobody ever reads `a` again. Storing `b` still sweeps it: a cold entry
    // past its time-to-live does not wait for its own lookup to be evicted.
    cache.seed(entry("one", b));
    assert_eq!(cache.len(), 1, "the stale entry went with the store");
    assert!(cache.get("one", a).is_none());
}

#[test]
fn the_cache_holds_no_more_than_its_capacity_and_lets_the_oldest_stored_go_first() {
    let cache = EffectiveCache::bounded(Duration::from_secs(30), 3);
    assert_eq!(cache.max_entries(), 3);
    let t: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    for tenant in &t[..3] {
        cache.seed(entry("one", *tenant));
    }
    assert_eq!(cache.len(), 3);

    // Re-storing a held slot replaces in place and grows nothing.
    cache.seed(entry("one", t[1]));
    assert_eq!(cache.len(), 3);

    // A fourth slot displaces the oldest stored — the first — and nothing else.
    cache.seed(entry("one", t[3]));
    assert_eq!(cache.len(), 3);
    assert!(cache.get("one", t[0]).is_none(), "the oldest went first");
    for tenant in &t[1..] {
        assert!(cache.get("one", *tenant).is_some());
    }

    // An invalidated slot leaves a stale record behind. It is skipped, and the
    // next eviction still takes a held entry: the re-stored one, now the
    // oldest of those left.
    cache.invalidate("one", scope_class::LOCAL, Some(t[2]));
    assert_eq!(cache.len(), 2);
    cache.seed(entry("two", t[0]));
    cache.seed(entry("two", t[1]));
    assert_eq!(cache.len(), 3);
    assert!(
        cache.get("one", t[1]).is_none(),
        "oldest of the held entries"
    );
    assert!(cache.get("one", t[3]).is_some());
    assert!(cache.get("two", t[0]).is_some());
    assert!(cache.get("two", t[1]).is_some());
}

#[test]
fn the_default_bound_is_the_design_anchor() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    assert_eq!(cache.max_entries(), 500_000);
    assert_eq!(cache.max_entries(), EffectiveCache::DEFAULT_MAX_ENTRIES);
}

#[test]
fn a_store_whose_generation_an_invalidation_has_passed_is_dropped() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (t, other) = (Uuid::new_v4(), Uuid::new_v4());

    // Captured on the miss, before the read; the key is evicted in between.
    let seen = cache.generation("one", t);
    let seen_other = cache.generation("one", other);
    cache.invalidate_key("one");
    assert!(
        !cache.populate(entry("one", t), seen),
        "the read may predate the write"
    );
    assert!(cache.get("one", t).is_none());
    assert!(
        !cache.populate(entry("one", other), seen_other),
        "key-wide: every scope of the key"
    );

    // A subtree eviction moves the tenant's generation, and only that one's.
    let seen = cache.generation("two", t);
    let seen_other = cache.generation("two", other);
    cache.invalidate_subtree(&[t]);
    assert!(!cache.populate(entry("two", t), seen));
    assert!(
        cache.populate(entry("two", other), seen_other),
        "another tenant's read stands"
    );

    // Captured after the eviction, the store lands.
    let current = cache.generation("one", t);
    assert!(cache.populate(entry("one", t), current));
    assert!(cache.get("one", t).is_some());
}

#[test]
fn a_local_or_tenant_set_invalidation_also_drops_a_store_it_has_passed() {
    // The two paths the key-wide and subtree test does not drive: a local
    // write evicts one scope, a restriction change evicts a set of tenants.
    // Both move the whole key's generation — deliberately conservative: an
    // in-flight read of any scope of the key loses its store and re-resolves
    // on the next read, which costs a query and never serves a stale value.
    // Another key is not touched.
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (t, other) = (Uuid::new_v4(), Uuid::new_v4());

    let seen = cache.generation("local", t);
    let seen_sibling = cache.generation("local", other);
    let seen_unrelated = cache.generation("unrelated", t);
    cache.invalidate("local", scope_class::LOCAL, Some(t));
    assert!(
        !cache.populate(entry("local", t), seen),
        "the read of the written scope may predate the write"
    );
    assert!(
        !cache.populate(entry("local", other), seen_sibling),
        "a sibling scope of the same key re-resolves too"
    );
    assert!(
        cache.populate(entry("unrelated", t), seen_unrelated),
        "another key's read stands"
    );

    let seen = cache.generation("restricted", t);
    let seen_unrelated = cache.generation("unrelated-too", t);
    cache.invalidate_tenants("restricted", &[t]);
    assert!(
        !cache.populate(entry("restricted", t), seen),
        "a restriction change passes the named tenant's read"
    );
    assert!(
        cache.populate(entry("unrelated-too", t), seen_unrelated),
        "and leaves another key's read standing"
    );
}
