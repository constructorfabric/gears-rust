// Created: 2026-06-03 by Constructor Tech
//! The public `ClusterCacheV1` facade — a thin, cloneable handle delegating to
//! the resolved `Arc<dyn ClusterCacheBackend>`.

use std::sync::Arc;

use toolkit::client_hub::ClientHub;

use crate::cache::backend::ClusterCacheBackend;
use crate::cache::resolver::CacheResolverBuilder;
use crate::cache::types::{CacheConsistency, CacheEntry, CacheFeatures, PutRequest, Ttl};
use crate::cache::watch::CacheWatch;
use crate::error::ClusterError;
use crate::restart::ResubscribeFuture;

/// The public cache facade. Construct via [`ClusterCacheV1::resolver`]; cloning
/// is cheap (an `Arc` bump).
///
/// Use [`scoped`](Self::scoped) to carve a composable sub-namespace: every key
/// (and watch/scan prefix) is auto-prefixed on the write path and stripped on
/// the read path (DESIGN §3.8).
#[derive(Clone)]
pub struct ClusterCacheV1 {
    inner: Arc<dyn ClusterCacheBackend>,
}

impl ClusterCacheV1 {
    /// Wraps a resolved backend. Crate-internal: consumers obtain a facade
    /// through the resolver.
    pub(crate) fn from_backend(inner: Arc<dyn ClusterCacheBackend>) -> Self {
        Self { inner }
    }

    /// Static entry point: returns a fluent resolver bound to `hub`.
    pub fn resolver(hub: &ClientHub) -> CacheResolverBuilder<'_> {
        CacheResolverBuilder::new(hub)
    }

    /// Returns a sub-namespaced view of this cache: every key (and the prefix of
    /// `watch_prefix`/`scan_prefix`) is auto-prefixed with `prefix + "/"` on the
    /// write path and stripped on the read path (DESIGN §3.8). Scoping composes —
    /// `cache.scoped("a")?.scoped("b")?` makes the backend observe `"a/b/<key>"`.
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidName`] if `prefix` violates the scope-prefix
    /// rule: slash-separated segments of `[a-zA-Z0-9_-]` with no leading, trailing,
    /// or empty (doubled-slash) segments, max 255 chars.
    pub fn scoped(&self, prefix: &str) -> Result<Self, ClusterError> {
        let prefix = crate::scope::validated_prefix(prefix)?;
        Ok(Self::from_backend(Arc::new(
            crate::cache::ScopedCacheBackend::new(Arc::clone(&self.inner), prefix),
        )))
    }

    /// The bound backend's declared consistency class.
    #[must_use]
    pub fn consistency(&self) -> CacheConsistency {
        self.inner.consistency()
    }

    /// The bound backend's native capability flags.
    #[must_use]
    pub fn features(&self) -> CacheFeatures {
        self.inner.features()
    }

    /// Returns the versioned entry for `key`, or `None` if absent.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        crate::scope::validate_cache_key(key)?;
        self.inner.get(key).await
    }

    /// Stores `req.value` under `req.key`, incrementing the version; overwrites if
    /// present.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        crate::scope::validate_cache_key(req.key)?;
        self.inner.put(req).await
    }

    /// Removes `key`, returning whether it existed.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        crate::scope::validate_cache_key(key)?;
        self.inner.delete(key).await
    }

    /// Existence check for `key`.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        crate::scope::validate_cache_key(key)?;
        self.inner.contains(key).await
    }

    /// Atomically creates `req.key` only if absent.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn put_if_absent(
        &self,
        req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        crate::scope::validate_cache_key(req.key)?;
        self.inner.put_if_absent(req).await
    }

    /// Atomic version-based compare-and-swap.
    ///
    /// # Errors
    /// Returns [`ClusterError::CasConflict`] on version mismatch, or another
    /// [`ClusterError`] from the backend.
    pub async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        crate::scope::validate_cache_key(key)?;
        self.inner
            .compare_and_swap(key, expected_version, new_value, ttl)
            .await
    }

    /// Watches an exact key.
    ///
    /// The returned watch carries a resubscribe seam, so
    /// [`CacheWatch::auto_restart`] can transparently re-`watch` this key on a
    /// retryable terminal close.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the backend.
    pub async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        crate::scope::validate_cache_key(key)?;
        let mut watch = self.inner.watch(key).await?;
        install_exact_watch_seam(Arc::clone(&self.inner), key.to_owned(), &mut watch);
        Ok(watch)
    }

    /// Watches a key prefix.
    ///
    /// The returned watch carries a resubscribe seam (see [`watch`](Self::watch)).
    ///
    /// # Errors
    /// Returns [`ClusterError::Unsupported`] when the backend lacks native
    /// prefix-watch support, or another [`ClusterError`] from the backend.
    pub async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        let mut watch = self.inner.watch_prefix(prefix).await?;
        install_prefix_watch_seam(Arc::clone(&self.inner), prefix.to_owned(), &mut watch);
        Ok(watch)
    }

    /// Lists the keys currently present under `prefix`.
    ///
    /// # Errors
    /// Returns [`ClusterError::Unsupported`] when the backend lacks scan support,
    /// or another [`ClusterError`] from the backend.
    pub async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        self.inner.scan_prefix(prefix).await
    }

    /// Opt-in polling prefix watch: synthesizes `watch_prefix` semantics on a
    /// backend that declares no native support
    /// (`features().prefix_watch == false`), by polling
    /// [`scan_prefix`](Self::scan_prefix) + `get` every `interval` (DESIGN §3.12).
    ///
    /// This is **not** free — see [`PollingPrefixWatch`] for the cost and the
    /// recommendation to prefer a native-prefix-watch backend at scale. Dropping
    /// the returned [`CacheWatch`] stops the polling task. Pair with
    /// [`watch_prefix`](Self::watch_prefix) (native) when the backend supports it.
    ///
    /// A zero `interval` does not panic: the returned watch yields a single
    /// terminal [`CacheWatchEvent::Closed`](crate::cache::CacheWatchEvent::Closed)
    /// carrying [`ClusterError::InvalidConfig`] (non-retryable) — see
    /// [`PollingPrefixWatch::spawn`]. Disappeared keys are reported as
    /// [`CacheEvent::Deleted`](crate::cache::CacheEvent::Deleted), never `Expired`.
    #[must_use]
    pub fn watch_prefix_polling(&self, prefix: &str, interval: std::time::Duration) -> CacheWatch {
        let mut watch =
            crate::cache::PollingPrefixWatch::spawn(Arc::clone(&self.inner), prefix, interval);
        install_polling_watch_seam(
            Arc::clone(&self.inner),
            prefix.to_owned(),
            interval,
            &mut watch,
        );
        watch
    }
}

/// Installs a self-reinstalling resubscribe seam that re-runs `watch(key)` on
/// the bound backend. Each reconnected watch is re-seamed, so
/// [`CacheWatch::auto_restart`] reconnects *repeatedly* on successive retryable
/// closes, not just once. Capturing the backend (whose `async_trait` methods
/// return a concretely-`Send` boxed future) rather than the facade avoids a
/// `Send` auto-trait inference cycle.
fn install_exact_watch_seam(
    backend: Arc<dyn ClusterCacheBackend>,
    key: String,
    watch: &mut CacheWatch,
) {
    watch.set_resubscribe(move || -> ResubscribeFuture<CacheWatch> {
        let backend = Arc::clone(&backend);
        let key = key.clone();
        Box::pin(async move {
            let mut fresh = backend.watch(&key).await?;
            install_exact_watch_seam(Arc::clone(&backend), key, &mut fresh);
            Ok(fresh)
        })
    });
}

/// As [`install_exact_watch_seam`], but re-runs `watch_prefix(prefix)`.
fn install_prefix_watch_seam(
    backend: Arc<dyn ClusterCacheBackend>,
    prefix: String,
    watch: &mut CacheWatch,
) {
    watch.set_resubscribe(move || -> ResubscribeFuture<CacheWatch> {
        let backend = Arc::clone(&backend);
        let prefix = prefix.clone();
        Box::pin(async move {
            let mut fresh = backend.watch_prefix(&prefix).await?;
            install_prefix_watch_seam(Arc::clone(&backend), prefix, &mut fresh);
            Ok(fresh)
        })
    });
}

/// As [`install_exact_watch_seam`], but re-spawns the polling polyfill (which
/// can also surface a retryable backend error as `Closed`).
fn install_polling_watch_seam(
    backend: Arc<dyn ClusterCacheBackend>,
    prefix: String,
    interval: std::time::Duration,
    watch: &mut CacheWatch,
) {
    watch.set_resubscribe(move || -> ResubscribeFuture<CacheWatch> {
        let backend = Arc::clone(&backend);
        let prefix = prefix.clone();
        Box::pin(async move {
            let mut fresh =
                crate::cache::PollingPrefixWatch::spawn(Arc::clone(&backend), &prefix, interval);
            install_polling_watch_seam(Arc::clone(&backend), prefix, interval, &mut fresh);
            Ok(fresh)
        })
    });
}
