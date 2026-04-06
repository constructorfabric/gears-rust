//! Lock-free router caching using arc-swap for read-mostly data
//!
//! This module provides atomic swapping of read-mostly structures like
//! Axum routers to eliminate lock contention on hot paths.

use arc_swap::ArcSwap;
use std::sync::Arc;

/// Lock-free cache for read-mostly router data
///
/// Uses `arc-swap` to provide atomic, lock-free access to router instances.
/// This is ideal for scenarios where the router is rebuilt infrequently
/// but accessed very frequently (typical in HTTP servers).
///
/// # Benefits
///
/// - **Zero lock contention**: Readers never block each other or writers
/// - **Cache-friendly**: Readers get direct Arc references with minimal indirection
/// - **Atomic updates**: Router swaps are atomic and consistent
/// - **Memory efficient**: Old routers are freed when no longer referenced
///
/// # Usage
///
/// ```rust,ignore
/// let cache = RouterCache::new(initial_router);
///
/// // Hot path: load router (no locks)
/// let router = cache.load();
/// // Use router for request handling...
///
/// // Cold path: rebuild and swap router
/// let new_router = build_new_router().await;
/// cache.store(new_router);
/// ```
pub struct RouterCache<T> {
    inner: ArcSwap<T>,
}

impl<T> RouterCache<T> {
    /// Create a new router cache with an initial value
    pub fn new(initial: T) -> Self {
        Self {
            inner: ArcSwap::from_pointee(initial),
        }
    }

    /// Load the current router instance
    ///
    /// This operation is lock-free and very fast - it returns an Arc
    /// pointing to the current router instance. Multiple readers can
    /// call this concurrently without any contention.
    ///
    /// # Returns
    ///
    /// Arc<T> pointing to the current router instance
    pub fn load(&self) -> Arc<T> {
        self.inner.load_full()
    }

    /// Atomically replace the router with a new instance
    ///
    /// This operation is atomic - all subsequent calls to `load()` will
    /// return the new router instance. Existing Arc references to the old
    /// router remain valid until they are dropped.
    ///
    /// # Arguments
    ///
    /// * `new_router` - The new router instance to store
    pub fn store(&self, new_router: T) {
        self.inner.store(Arc::new(new_router));
    }

    /// Get a reference to the current router without cloning the Arc
    ///
    /// This provides temporary access to the router without incrementing
    /// the reference count. Use this for brief operations where you don't
    /// need to hold onto the router reference.
    ///
    /// # Safety
    ///
    /// The returned reference is only valid for the duration of the closure.
    /// Do not store this reference or use it after the closure returns.
    #[allow(dead_code)]
    pub fn with_current<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let current = self.inner.load();
        f(&*current)
    }

    /// Conditionally update the router
    ///
    /// Only updates the router if the provided predicate returns true
    /// when called with the current router instance.
    ///
    /// # Arguments
    ///
    /// * `predicate` - Function that determines whether to update
    /// * `new_router` - New router instance to store if predicate returns true
    ///
    /// # Returns
    ///
    /// `true` if the router was updated, `false` otherwise
    #[allow(dead_code)]
    pub fn update_if<P>(&self, predicate: P, new_router: T) -> bool
    where
        P: FnOnce(&T) -> bool,
    {
        let current = self.inner.load();
        if predicate(&*current) {
            self.store(new_router);
            true
        } else {
            false
        }
    }

    /// Compare and swap the router
    ///
    /// Only updates if the current router matches the expected value.
    /// This is useful for optimistic updates where you want to ensure
    /// the router hasn't changed since you last read it.
    ///
    /// # Arguments
    ///
    /// * `expected` - The expected current router instance
    /// * `new_router` - New router instance to store if current matches expected
    ///
    /// # Returns
    ///
    /// `Ok(())` if the swap succeeded, `Err(current)` if it failed
    #[allow(dead_code, clippy::needless_pass_by_value)] // Arc is used for pointer comparison
    pub fn compare_and_swap(&self, expected: Arc<T>, new_router: T) -> Result<(), Arc<T>>
    where
        T: PartialEq,
    {
        let new_arc = Arc::new(new_router);
        let result = self.inner.compare_and_swap(&expected, new_arc);

        // The compare_and_swap returns the previous value, not a Result
        // If it matches our expected value, the swap succeeded
        if Arc::ptr_eq(&*result, &expected) {
            Ok(())
        } else {
            Err(result.clone())
        }
    }
}

impl<T> Clone for RouterCache<T> {
    fn clone(&self) -> Self {
        Self {
            inner: ArcSwap::new(self.inner.load_full()),
        }
    }
}

impl<T> Default for RouterCache<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> std::fmt::Debug for RouterCache<T>
where
    T: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current = self.inner.load();
        f.debug_struct("RouterCache")
            .field("current", &*current)
            .finish()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "router_cache_tests.rs"]
mod router_cache_tests;
