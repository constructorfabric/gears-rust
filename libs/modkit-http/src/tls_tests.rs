use super::*;
use std::sync::atomic::Ordering;

/// Test that native root certs are cached after the first load.
///
/// NOTE: This test verifies "at most one load" rather than "exactly one load"
/// because `LOAD_COUNT` is a global atomic shared across all tests. If another
/// test (or parallel test) calls `native_root_certs()` before this test runs,
/// the cache will already be initialized and `final_count - initial_count`
/// will be 0. The assertion handles this correctly.
#[test]
fn test_native_roots_cached() {
    // Capture count before our calls (may be non-zero if cache already initialized)
    let initial_count = LOAD_COUNT.load(Ordering::SeqCst);

    // First call - loads if not cached, otherwise uses existing cache
    let result1 = native_root_certs();

    // Second call should use cache
    let result2 = native_root_certs();

    // Third call should also use cache
    let result3 = native_root_certs();

    // Verify loader was called at most once more than initial (0 if already cached, 1 if we triggered the load)
    let final_count = LOAD_COUNT.load(Ordering::SeqCst);
    assert!(
        final_count <= initial_count + 1,
        "loader should run at most once, but ran {} times since test start",
        final_count - initial_count
    );

    // Results should be consistent (same slice pointer)
    assert_eq!(result1.len(), result2.len());
    assert_eq!(result2.len(), result3.len());
    assert!(std::ptr::eq(result1, result2), "should return same slice");
    assert!(std::ptr::eq(result2, result3), "should return same slice");
}

#[test]
fn test_native_roots_client_config() {
    // Building client config succeeds if native roots are available
    // (which they should be on most CI/dev systems)
    // On systems without native certs, this returns Err (expected behavior)
    let result = native_roots_client_config();

    // Log the result for debugging in CI
    match &result {
        Ok(_) => tracing::debug!("native_roots_client_config succeeded"),
        Err(e) => {
            tracing::debug!(error = %e, "native_roots_client_config failed (expected on minimal containers)");
        }
    }

    // We don't assert success because CI containers may not have OS certs.
    // The important thing is it doesn't panic.
}
