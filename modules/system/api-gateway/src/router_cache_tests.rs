use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
struct TestRouter {
    id: usize,
    name: String,
}

impl TestRouter {
    fn new(id: usize, name: &str) -> Self {
        Self {
            id,
            name: name.to_owned(),
        }
    }
}

#[test]
fn test_basic_load_store() {
    let initial = TestRouter::new(1, "initial");
    let cache = RouterCache::new(initial.clone());

    // Load initial router
    let loaded = cache.load();
    assert_eq!(*loaded, initial);

    // Store new router
    let new_router = TestRouter::new(2, "updated");
    cache.store(new_router.clone());

    // Load updated router
    let loaded = cache.load();
    assert_eq!(*loaded, new_router);
}

#[test]
fn test_concurrent_reads() {
    let initial = TestRouter::new(1, "concurrent_test");
    let cache = Arc::new(RouterCache::new(initial));

    let mut handles = vec![];

    // Spawn multiple readers
    for i in 0..10 {
        let cache_clone = Arc::clone(&cache);
        let handle = thread::spawn(move || {
            for _ in 0..100 {
                let router = cache_clone.load();
                assert_eq!(router.name, "concurrent_test");
                thread::sleep(Duration::from_micros(i * 10));
            }
        });
        handles.push(handle);
    }

    // Wait for all readers to complete
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_concurrent_read_write() {
    let initial = TestRouter::new(0, "router_0");
    let cache = Arc::new(RouterCache::new(initial));
    let update_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Spawn readers
    for _ in 0..5 {
        let cache_clone = Arc::clone(&cache);
        let handle = thread::spawn(move || {
            for _ in 0..50 {
                let router = cache_clone.load();
                // Router ID should be monotonically increasing
                assert!(router.name.starts_with("router_"));
                thread::sleep(Duration::from_micros(10));
            }
        });
        handles.push(handle);
    }

    // Spawn writers
    for _ in 0..2 {
        let cache_clone = Arc::clone(&cache);
        let count_clone = Arc::clone(&update_count);
        let handle = thread::spawn(move || {
            for _ in 0..10 {
                let id = count_clone.fetch_add(1, Ordering::SeqCst);
                let new_router = TestRouter::new(id, &format!("router_{id}"));
                cache_clone.store(new_router);
                thread::sleep(Duration::from_millis(1));
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify final state
    let final_router = cache.load();
    assert!(final_router.name.starts_with("router_"));
}

#[test]
fn test_with_current() {
    let initial = TestRouter::new(42, "test_with_current");
    let cache = RouterCache::new(initial);

    let result = cache.with_current(|router| format!("{}_{}", router.name, router.id));

    assert_eq!(result, "test_with_current_42");
}

#[test]
fn test_update_if() {
    let initial = TestRouter::new(1, "conditional");
    let cache = RouterCache::new(initial);

    // Update should succeed (id is 1)
    let new_router1 = TestRouter::new(2, "updated");
    let updated = cache.update_if(|r| r.id == 1, new_router1.clone());
    assert!(updated);

    let current = cache.load();
    assert_eq!(*current, new_router1);

    // Update should fail (id is now 2, not 5)
    let new_router2 = TestRouter::new(3, "should_not_update");
    let updated = cache.update_if(|r| r.id == 5, new_router2);
    assert!(!updated);

    let current = cache.load();
    assert_eq!(*current, new_router1); // Should still be new_router1
}

#[test]
fn test_compare_and_swap() {
    let initial = TestRouter::new(1, "cas_test");
    let cache = RouterCache::new(initial);

    let current = cache.load();
    let new_router = TestRouter::new(2, "cas_updated");

    // CAS should succeed with correct expected value
    let result = cache.compare_and_swap(current, new_router.clone());
    assert!(result.is_ok());

    let updated = cache.load();
    assert_eq!(*updated, new_router);

    // CAS should fail with wrong expected value
    let wrong_expected = Arc::new(TestRouter::new(99, "wrong"));
    let another_router = TestRouter::new(3, "cas_failed");
    let result = cache.compare_and_swap(wrong_expected, another_router);
    assert!(result.is_err());

    // Router should remain unchanged
    let current = cache.load();
    assert_eq!(*current, new_router);
}

#[test]
fn test_clone_and_debug() {
    let original = TestRouter::new(1, "clone_test");
    let cache1 = RouterCache::new(original.clone());
    let cache2 = cache1.clone();

    // Both caches should have the same initial value
    assert_eq!(*cache1.load(), *cache2.load());

    // Update one cache
    let new_router = TestRouter::new(2, "updated");
    cache1.store(new_router.clone());

    // Caches should now be independent
    assert_eq!(*cache1.load(), new_router);
    assert_eq!(*cache2.load(), original);

    // Debug should work
    let debug_str = format!("{cache1:?}");
    assert!(debug_str.contains("RouterCache"));
    assert!(debug_str.contains("updated"));
}
