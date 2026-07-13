//! Unit tests for [`InMemoryCache`](super::InMemoryCache).
//!
//! Kept in a sibling `_tests.rs` file per the `de1101_tests_in_separate_files`
//! repo lint. Linked into `cache.rs` via `#[path = "cache_tests.rs"] mod tests;`,
//! so the gear sees `cache.rs` as `super`.

use super::*;
use serde_json::json;
use std::thread::sleep;
use toolkit_gts::GTS_ID_PREFIX;
use toolkit_gts::gts_id;
use types_registry_sdk::GtsTypeId;

fn make_type_schema(type_id: &str) -> Arc<GtsTypeSchema> {
    Arc::new(GtsTypeSchema::try_new(GtsTypeId::new(type_id), json!({}), None, None).unwrap())
}

#[test]
fn test_get_miss_returns_none() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    assert!(cache.get(gts_id!("cf.y.z.t.v1~")).is_none());
}

#[test]
fn test_put_then_get_returns_value() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    let schema = make_type_schema(gts_id!("cf.y.z.t.v1~"));
    cache.put(gts_id!("cf.y.z.t.v1~").to_owned(), Arc::clone(&schema));
    let hit = cache.get(gts_id!("cf.y.z.t.v1~")).unwrap();
    assert!(Arc::ptr_eq(&hit, &schema));
}

#[test]
fn test_invalidate_removes_entry() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    let schema = make_type_schema(gts_id!("cf.y.z.t.v1~"));
    cache.put(gts_id!("cf.y.z.t.v1~").to_owned(), schema);
    cache.invalidate(gts_id!("cf.y.z.t.v1~"));
    assert!(cache.get(gts_id!("cf.y.z.t.v1~")).is_none());
}

#[test]
fn test_clear_drops_all() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    cache.put("a~".to_owned(), make_type_schema(gts_id!("cf.y.z.a.v1~")));
    cache.put("b~".to_owned(), make_type_schema(gts_id!("cf.y.z.b.v1~")));
    assert_eq!(cache.len(), 2);
    cache.clear();
    assert!(cache.is_empty());
}

#[test]
fn test_lru_eviction_at_capacity() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().with_capacity(2).without_ttl(),
    );
    cache.put("a".to_owned(), make_type_schema(gts_id!("cf.y.z.a.v1~")));
    cache.put("b".to_owned(), make_type_schema(gts_id!("cf.y.z.b.v1~")));
    cache.put("c".to_owned(), make_type_schema(gts_id!("cf.y.z.c.v1~")));
    // a was the least recently used → evicted.
    assert!(cache.get("a").is_none());
    assert!(cache.get("b").is_some());
    assert!(cache.get("c").is_some());
}

#[test]
fn test_ttl_expiry() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().with_ttl(Duration::from_millis(50)),
    );
    cache.put("a".to_owned(), make_type_schema(gts_id!("cf.y.z.a.v1~")));
    assert!(cache.get("a").is_some());
    sleep(Duration::from_millis(80));
    assert!(cache.get("a").is_none());
}

#[test]
fn test_no_ttl_keeps_entry() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().without_ttl().with_capacity(8),
    );
    cache.put("a".to_owned(), make_type_schema(gts_id!("cf.y.z.a.v1~")));
    sleep(Duration::from_millis(20));
    assert!(cache.get("a").is_some());
}

#[test]
fn test_zero_capacity_clamped_to_one() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig {
            capacity: 0,
            ttl: None,
        });
    cache.put("a".to_owned(), make_type_schema(gts_id!("cf.y.z.a.v1~")));
    // capacity = 1 means second put evicts the first.
    cache.put("b".to_owned(), make_type_schema(gts_id!("cf.y.z.b.v1~")));
    assert!(cache.get("a").is_none());
    assert!(cache.get("b").is_some());
}

#[test]
fn test_default_configs() {
    let s = CacheConfig::type_schemas();
    assert_eq!(s.capacity, DEFAULT_CACHE_CAPACITY);
    assert_eq!(s.ttl, Some(DEFAULT_CACHE_TTL));

    let i = CacheConfig::instances();
    assert_eq!(i.capacity, DEFAULT_CACHE_CAPACITY);
    assert_eq!(i.ttl, Some(DEFAULT_CACHE_TTL));
}

// ── Cache::get_many ──────────────────────────────────────────────────

#[test]
fn test_get_many_preserves_order_and_gaps() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    let a = make_type_schema(gts_id!("cf.y.z.a.v1~"));
    let c = make_type_schema(gts_id!("cf.y.z.c.v1~"));
    cache.put(gts_id!("cf.y.z.a.v1~").to_owned(), Arc::clone(&a));
    cache.put(gts_id!("cf.y.z.c.v1~").to_owned(), Arc::clone(&c));

    let results = cache.get_many(&[
        gts_id!("cf.y.z.a.v1~"),
        gts_id!("cf.y.z.b.v1~"), // not in cache
        gts_id!("cf.y.z.c.v1~"),
    ]);
    assert_eq!(results.len(), 3);
    assert!(Arc::ptr_eq(results[0].as_ref().unwrap(), &a));
    assert!(results[1].is_none());
    assert!(Arc::ptr_eq(results[2].as_ref().unwrap(), &c));
}

#[test]
fn test_get_many_empty_input() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    let results = cache.get_many(&[]);
    assert!(results.is_empty());
}

#[test]
fn test_get_many_all_misses() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    let results = cache.get_many(&[gts_id!("cf.y.z.a.v1~"), gts_id!("cf.y.z.b.v1~")]);
    assert_eq!(results.len(), 2);
    assert!(results[0].is_none());
    assert!(results[1].is_none());
}

#[test]
fn test_get_many_evicts_expired_entries() {
    // Mixed batch where some entries are TTL-expired and some are fresh.
    // Expired ones must surface as None and be evicted from the cache;
    // fresh ones surface as Some and stay.
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().with_ttl(Duration::from_millis(50)),
    );
    cache.put(
        gts_id!("cf.y.z.expired.v1~").to_owned(),
        make_type_schema(gts_id!("cf.y.z.expired.v1~")),
    );
    sleep(Duration::from_millis(80));
    // Fresh entry inserted AFTER the sleep — its TTL clock is reset.
    cache.put(
        gts_id!("cf.y.z.fresh.v1~").to_owned(),
        make_type_schema(gts_id!("cf.y.z.fresh.v1~")),
    );

    let results = cache.get_many(&[gts_id!("cf.y.z.expired.v1~"), gts_id!("cf.y.z.fresh.v1~")]);
    assert!(results[0].is_none());
    assert!(results[1].is_some());

    // Expired entry must be evicted as a side effect.
    assert!(cache.get(gts_id!("cf.y.z.expired.v1~")).is_none());
    assert!(cache.get(gts_id!("cf.y.z.fresh.v1~")).is_some());
    assert_eq!(cache.len(), 1);
}

// ── Cache::put_many ──────────────────────────────────────────────────

#[test]
fn test_put_many_inserts_all_entries() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    cache.put_many(vec![
        (
            gts_id!("cf.y.z.a.v1~").to_owned(),
            make_type_schema(gts_id!("cf.y.z.a.v1~")),
        ),
        (
            gts_id!("cf.y.z.b.v1~").to_owned(),
            make_type_schema(gts_id!("cf.y.z.b.v1~")),
        ),
        (
            gts_id!("cf.y.z.c.v1~").to_owned(),
            make_type_schema(gts_id!("cf.y.z.c.v1~")),
        ),
    ]);
    assert_eq!(cache.len(), 3);
    assert!(cache.get(gts_id!("cf.y.z.a.v1~")).is_some());
    assert!(cache.get(gts_id!("cf.y.z.b.v1~")).is_some());
    assert!(cache.get(gts_id!("cf.y.z.c.v1~")).is_some());
}

#[test]
fn test_put_many_replaces_existing() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    let v1 = make_type_schema(gts_id!("cf.y.z.a.v1~"));
    let v2 = make_type_schema(gts_id!("cf.y.z.a.v1~"));
    cache.put(gts_id!("cf.y.z.a.v1~").to_owned(), Arc::clone(&v1));
    cache.put_many(vec![(gts_id!("cf.y.z.a.v1~").to_owned(), Arc::clone(&v2))]);
    assert_eq!(cache.len(), 1);
    // The replacement won — same key now points to v2's Arc.
    assert!(Arc::ptr_eq(
        &cache.get(gts_id!("cf.y.z.a.v1~")).unwrap(),
        &v2
    ));
}

#[test]
fn test_put_many_empty_input_is_noop() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    cache.put_many(vec![]);
    assert!(cache.is_empty());
}

#[test]
fn test_put_many_respects_capacity() {
    // capacity=2, put_many of 3 entries — last two win, first is evicted.
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().with_capacity(2).without_ttl(),
    );
    cache.put_many(vec![
        (
            gts_id!("cf.y.z.a.v1~").to_owned(),
            make_type_schema(gts_id!("cf.y.z.a.v1~")),
        ),
        (
            gts_id!("cf.y.z.b.v1~").to_owned(),
            make_type_schema(gts_id!("cf.y.z.b.v1~")),
        ),
        (
            gts_id!("cf.y.z.c.v1~").to_owned(),
            make_type_schema(gts_id!("cf.y.z.c.v1~")),
        ),
    ]);
    assert_eq!(cache.len(), 2);
    assert!(cache.get(gts_id!("cf.y.z.a.v1~")).is_none()); // evicted (LRU)
    assert!(cache.get(gts_id!("cf.y.z.b.v1~")).is_some());
    assert!(cache.get(gts_id!("cf.y.z.c.v1~")).is_some());
}

// ── reverse `uuid → gts_id` index pruning ────────────────────────────────

#[test]
fn test_reverse_index_pruned_on_capacity_eviction() {
    // Regression for the previously-unbounded reverse index. With
    // capacity=2 and 5 distinct entries put in sequence, only the
    // most-recent 2 should remain reachable via `get_by_uuid`, and
    // the index size must track the LRU's.
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().with_capacity(2).without_ttl(),
    );
    let schemas: Vec<Arc<GtsTypeSchema>> = (0..5)
        .map(|i| make_type_schema(&format!("{GTS_ID_PREFIX}cf.y.z.s{i}.v1~")))
        .collect();
    for s in &schemas {
        cache.put(s.type_id.to_string(), Arc::clone(s));
    }
    // Old entries fully gone — both LRU and reverse index.
    for s in &schemas[..3] {
        assert!(
            cache.get_by_uuid(s.type_uuid).is_none(),
            "evicted entry still reachable by uuid"
        );
    }
    // Survivors reachable.
    for s in &schemas[3..] {
        assert!(cache.get_by_uuid(s.type_uuid).is_some());
    }
}

#[test]
fn test_reverse_index_pruned_on_invalidate() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> =
        InMemoryCache::<Arc<GtsTypeSchema>>::new(CacheConfig::type_schemas());
    let s = make_type_schema(gts_id!("cf.y.z.t.v1~"));
    let uuid = s.type_uuid;
    cache.put(s.type_id.to_string(), s);
    assert!(cache.get_by_uuid(uuid).is_some());
    cache.invalidate(gts_id!("cf.y.z.t.v1~"));
    // After invalidate, the reverse index must not retain a dangling
    // mapping that points at a now-evicted LRU entry.
    assert!(cache.get_by_uuid(uuid).is_none());
}

#[test]
fn test_reverse_index_pruned_on_retain() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().with_capacity(4).without_ttl(),
    );
    let kept = make_type_schema(gts_id!("cf.y.z.keep.v1~"));
    let first_dropped = make_type_schema(gts_id!("cf.y.z.drop_a.v1~"));
    let second_dropped = make_type_schema(gts_id!("cf.y.z.drop_b.v1~"));
    let kept_uuid = kept.type_uuid;
    let first_uuid = first_dropped.type_uuid;
    let second_uuid = second_dropped.type_uuid;
    cache.put(kept.type_id.to_string(), kept);
    cache.put(first_dropped.type_id.to_string(), first_dropped);
    cache.put(second_dropped.type_id.to_string(), second_dropped);

    // Drop everything containing "drop" in the type id.
    cache.retain(&|s: &Arc<GtsTypeSchema>| !s.type_id.as_ref().contains("drop"));

    assert!(cache.get_by_uuid(kept_uuid).is_some());
    assert!(cache.get_by_uuid(first_uuid).is_none());
    assert!(cache.get_by_uuid(second_uuid).is_none());
}

#[test]
fn test_reverse_index_pruned_on_ttl_expiry() {
    let cache: InMemoryCache<Arc<GtsTypeSchema>> = InMemoryCache::<Arc<GtsTypeSchema>>::new(
        CacheConfig::type_schemas().with_ttl(Duration::from_millis(50)),
    );
    let s = make_type_schema(gts_id!("cf.y.z.t.v1~"));
    let uuid = s.type_uuid;
    cache.put(s.type_id.to_string(), s);
    sleep(Duration::from_millis(80));
    // First lookup observes expiry and evicts the LRU entry; the
    // reverse mapping must drop in the same step so the second lookup
    // doesn't see a dangling pointer.
    assert!(cache.get(gts_id!("cf.y.z.t.v1~")).is_none());
    assert!(cache.get_by_uuid(uuid).is_none());
}
