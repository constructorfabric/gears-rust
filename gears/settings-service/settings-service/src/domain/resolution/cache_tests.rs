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
fn a_global_change_evicts_every_tenants_copy_of_the_key_and_nothing_else() {
    // A global setting is read from the root's row for every tenant but cached
    // under the tenant that asked, and a write can only land at the root: the
    // root's slot alone would leave every other tenant serving the old value.
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let (root, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    for tenant in [root, a, b] {
        cache.seed(entry("flag", tenant));
    }
    cache.seed(entry("other", a));
    cache.invalidate("flag", scope_class::GLOBAL, Some(root));
    assert!(cache.get("flag", a).is_none());
    assert!(cache.get("flag", b).is_none());
    assert!(cache.get("flag", root).is_none());
    assert!(cache.get("other", a).is_some(), "another key is untouched");
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
fn many_keys_are_evicted_in_one_pass_and_each_generation_moves() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let tenants = [Uuid::new_v4(), Uuid::new_v4()];
    let named: Vec<String> = (0..500).map(|i| format!("named-{i}")).collect();
    for tenant in tenants {
        for key in &named {
            cache.seed(entry(key, tenant));
        }
        cache.seed(entry("kept", tenant));
    }
    let before = cache.generation("named-7", tenants[0]);
    cache.invalidate_keys(&named);
    assert_eq!(cache.len(), tenants.len(), "only the unnamed key stays");
    for tenant in tenants {
        assert!(cache.get("kept", tenant).is_some());
        assert!(cache.get("named-499", tenant).is_none());
    }
    assert_ne!(cache.generation("named-7", tenants[0]), before);
    // Nothing named, nothing touched.
    cache.invalidate_keys(&[]);
    assert_eq!(cache.len(), tenants.len());
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

#[test]
fn a_key_wide_eviction_leaves_every_other_keys_entries_and_reads_untouched() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let tenants = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    for tenant in tenants {
        for key in ["gone", "kept.one", "kept.two"] {
            cache.seed(entry(key, tenant));
        }
    }
    let kept: Vec<_> = tenants
        .iter()
        .flat_map(|t| {
            ["kept.one", "kept.two"].map(|k| {
                (
                    k,
                    *t,
                    cache.get(k, *t).expect("seeded"),
                    cache.generation(k, *t),
                )
            })
        })
        .collect();

    cache.invalidate_keys(&["gone".to_owned()]);

    assert_eq!(cache.len(), kept.len(), "only the named key's entries left");
    for tenant in tenants {
        assert!(cache.get("gone", tenant).is_none());
    }
    for (key, tenant, value, generation) in kept {
        let held = cache.get(key, tenant).expect("another key stays");
        assert!(Arc::ptr_eq(&held, &value), "the very entry, not a re-store");
        assert_eq!(
            cache.generation(key, tenant),
            generation,
            "and a read of it in flight may still store"
        );
    }
}

#[test]
fn a_key_wide_eviction_examines_the_entries_of_its_keys_only() {
    let cache = EffectiveCache::new(Duration::from_secs(30));
    let tenants: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    for i in 0..500 {
        for tenant in &tenants {
            cache.seed(entry(&format!("key-{i}"), *tenant));
        }
    }
    assert_eq!(cache.len(), 2000);

    let before = cache.eviction_stats();
    cache.invalidate_keys(&["key-7".to_owned(), "key-8".to_owned()]);
    let after = cache.eviction_stats();
    assert_eq!(
        after.visited - before.visited,
        2 * tenants.len(),
        "the two keys' entries, not the 2000 the cache holds"
    );
    assert_eq!(after.key_passes - before.key_passes, 1);
    assert_eq!(cache.len(), 2000 - 2 * tenants.len());

    // The same for the per-write key-wide eviction, and for a key not held.
    let before = cache.eviction_stats();
    cache.invalidate("key-9", scope_class::CASCADING, Some(tenants[0]));
    cache.invalidate_keys(&["never-held".to_owned()]);
    assert_eq!(
        cache.eviction_stats().visited - before.visited,
        tenants.len()
    );
    assert_eq!(cache.len(), 2000 - 3 * tenants.len());
}

#[test]
fn the_count_follows_every_way_an_entry_leaves() {
    let cache = EffectiveCache::bounded(Duration::from_secs(30), 6);
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    for key in ["k1", "k2", "k3"] {
        cache.seed(entry(key, a));
        cache.seed(entry(key, b));
    }
    cache.seed(entry("k1", a));
    assert_eq!(cache.len(), 6, "a re-store replaces in place");
    cache.seed(entry("k4", a));
    assert_eq!(
        cache.len(),
        6,
        "at capacity a new slot displaces the oldest"
    );
    assert!(
        cache.get("k1", b).is_none(),
        "the oldest still held: k1 at a was re-stored since"
    );
    cache.invalidate("k2", scope_class::LOCAL, Some(b));
    assert_eq!(cache.len(), 5);
    cache.invalidate_tenants("k3", &[a, b]);
    assert_eq!(cache.len(), 3);
    cache.invalidate_key("k1");
    assert_eq!(cache.len(), 2);
    cache.invalidate_subtree(&[a]);
    assert!(cache.is_empty());
}
