//! The per-primitive scoping wrapper for the cache (DESIGN §3.8).
//!
//! The cache is the only primitive with a read-path strip: its watch events
//! carry the affected `key`, so a forwarding task rewrites each event's key back
//! into the consumer's name space before delivery
//! (`cpt-cf-clst-algo-scoping-polyfill-prefix-translate`, `inst-pt-read`).

use std::sync::Arc;

use async_trait::async_trait;

use crate::cache::backend::ClusterCacheBackend;
use crate::cache::types::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, PutRequest, Ttl,
};
use crate::cache::watch::{CacheWatch, CacheWatchEvent};
use crate::error::ClusterError;
use crate::restart::ResubscribeFuture;
use crate::scope;

/// Per-watch in-flight buffer for the read-path forwarding task. Matches the
/// generous buffer the contract stubs use so a burst of mutations is not dropped
/// as `Lagged` by the strip layer itself.
const FORWARD_BUFFER: usize = 256;

/// A delegating [`ClusterCacheBackend`] that prepends a validated scope prefix to
/// every `key` (and to the `prefix` of `watch_prefix`/`scan_prefix`) on the write
/// path, and strips it from returned keys on the read path. Scoping composes by
/// stacking wrappers.
pub struct ScopedCacheBackend {
    inner: Arc<dyn ClusterCacheBackend>,
    prefix: String,
}

impl ScopedCacheBackend {
    /// Wraps `inner` with the effective `prefix` — already separator-terminated,
    /// and already validated: by [`scope::validated_prefix`] for a consumer
    /// scope, or by construction for a reserved one (see
    /// [`reserved_lease_cache`]).
    pub fn new(inner: Arc<dyn ClusterCacheBackend>, prefix: String) -> Self {
        Self { inner, prefix }
    }

    /// Wraps a backend-facing [`CacheWatch`] in a read-path forwarding task that
    /// strips `prefix` from every event key before handing it to the consumer.
    /// The task ends — dropping its sender — when the inner watch ends or the
    /// consumer drops the returned watch. The inner watch's two seams are carried
    /// across the layer first — see [`carry_seams_across`].
    fn strip_watch(prefix: String, mut inner: CacheWatch) -> CacheWatch {
        let (tx, mut watch) = CacheWatch::channel(FORWARD_BUFFER);
        carry_seams_across(&prefix, &inner, &mut watch);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // The consumer dropped the watch: stop forwarding promptly,
                    // even if the inner stream is idle and would never fail a `send`.
                    () = tx.closed() => return,
                    event = inner.recv() => {
                        let Some(event) = event else {
                            // Inner watch ended: drop our sender so the consumer's
                            // `recv()` ends too.
                            return;
                        };
                        let forwarded = match event {
                            CacheWatchEvent::Event(inner_event) => {
                                CacheWatchEvent::Event(strip_event(&prefix, inner_event))
                            }
                            // Lifecycle signals carry no key — forward unchanged.
                            other => other,
                        };
                        if tx.send(forwarded).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        watch
    }
}

/// Rewrites a watch event's key from the backend name space into the consumer's
/// by stripping `prefix`.
fn strip_event(prefix: &str, event: CacheEvent) -> CacheEvent {
    match event {
        CacheEvent::Changed { key } => CacheEvent::Changed {
            key: scope::strip(prefix, &key).to_owned(),
        },
        CacheEvent::Deleted { key } => CacheEvent::Deleted {
            key: scope::strip(prefix, &key).to_owned(),
        },
        CacheEvent::Expired { key } => CacheEvent::Expired {
            key: scope::strip(prefix, &key).to_owned(),
        },
    }
}

/// Carries the inner watch's two seams across the strip layer onto the watch the
/// consumer receives.
///
/// [`CacheWatch::channel`] hands back a *bare* watch — `resubscribe: None`,
/// `observability: None` — so without this the strip layer silently drops both.
/// The observability half is the one that bites: both plugins wrap their native
/// cache in [`InstrumentedCache`](crate::InstrumentedCache) at the bottom, so a
/// scoped view over a plugin backend is `Scoped(Instrumented(raw))` and the stamp
/// `InstrumentedCache::watch` applies is *below* this layer. Losing it means an
/// [`auto_restart`](CacheWatch::auto_restart)ed scoped watch reconnects but emits
/// no `cluster_watch_resets_total` / `cluster.watch.reset` — a metrics gap, not a
/// correctness one, and the reason it went unnoticed.
///
/// The resubscribe half is **re-wrapped, never copied**. A fresh watch from the
/// inner seam speaks the *backend's* name space, so handing it to the consumer
/// unchanged would make a reconnect start delivering prefixed keys — the strip
/// layer's whole job, undone by its own recovery path. Wrapping it in a fresh
/// `strip_watch` also re-arms this function, so the seam survives repeated
/// reconnects rather than only the first.
///
/// The facade re-installs its own seam *after* this runs
/// (`install_exact_watch_seam`), so on the facade path the resubscribe half is
/// overwritten by an equivalent one and only the observability half is new. On
/// the direct-backend path — which is how the cluster gear's default lease
/// backends hold a scoped cache — this is the only thing that installs either.
fn carry_seams_across(prefix: &str, inner: &CacheWatch, outer: &mut CacheWatch) {
    if let Some((provider, metrics)) = inner.observability_context() {
        outer.set_observability(provider, metrics);
    }
    if let Some(factory) = inner.resubscribe_factory() {
        let prefix = prefix.to_owned();
        outer.set_resubscribe(move || -> ResubscribeFuture<CacheWatch> {
            let factory = Arc::clone(&factory);
            let prefix = prefix.clone();
            Box::pin(async move {
                let fresh = factory().await?;
                Ok(ScopedCacheBackend::strip_watch(prefix, fresh))
            })
        });
    }
}

/// Opens the cluster gear's **reserved lease keyspace** over `cache`: the view
/// the cache-backed default lock and leader-election backends store their
/// [`LeaseRecord`](crate::LeaseRecord)s in.
///
/// This exists because sharing one cache handle between the cache API and the
/// lease backends made mutual exclusion *forgeable through the cache API*. The
/// lease keys were plain `lock/`/`election/` prefixes on the same keyspace the
/// cache RPC serves and [`LeaseRecord`](crate::LeaseRecord)'s layout is fixed and
/// documented, so `get("lock/x")` returned a decodable lease, `put` installed one
/// held by nobody, and `delete` reset the row so the fence restarted and a stale
/// token matched again. Authentication does not fix that: any *authenticated*
/// caller with cache access had lock and election write access.
///
/// [`RESERVED_LEASE_PREFIX`](crate::RESERVED_LEASE_PREFIX) carries
/// [`RESERVED_KEY_SIGIL`](crate::RESERVED_KEY_SIGIL), which the SDK's
/// `validate_cache_key` rejects — so the two keyspaces do not merely sit apart
/// by convention: nothing a consumer supplies *through the cache API* can name
/// anything inside this one, on either surface. In-process,
/// [`ClusterCacheV1`](crate::ClusterCacheV1) validates every key and refuses a
/// reserved prefix on `scan_prefix`/`watch_prefix`/`watch_prefix_polling`;
/// remotely, the gear's cache service refuses the sigil at its boundary and
/// filters reserved keys out of `scan_prefix` and the watch pump.
///
/// Code holding a raw [`ClusterCacheBackend`] is a different matter — the trait
/// validates nothing, by design, and
/// [`ClusterClient::cache_backend`](crate::ClusterClient::cache_backend) hands
/// one out. So the separation here is a property of the *API*, not of the
/// store: hand the *raw* cache to anything that legitimately serves consumer
/// keys, and this view to the lease backends alone.
///
/// Applied once, as the outermost wrapper over the profile's cache handle, so
/// every lease key carries the prefix exactly once — a fixed 7 bytes, against a
/// lease name the SDK already caps at 255 and a Postgres key column bounded at
/// 2048 (`limits::MAX_INDEXED_KEY_BYTES`, and the `CHECK` its migration
/// installs).
#[must_use]
pub fn reserved_lease_cache(cache: Arc<dyn ClusterCacheBackend>) -> Arc<dyn ClusterCacheBackend> {
    Arc::new(ScopedCacheBackend::new(
        cache,
        scope::RESERVED_LEASE_PREFIX.to_owned(),
    ))
}

#[async_trait]
impl ClusterCacheBackend for ScopedCacheBackend {
    fn consistency(&self) -> CacheConsistency {
        self.inner.consistency()
    }

    fn features(&self) -> CacheFeatures {
        self.inner.features()
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.inner.get(&scope::apply(&self.prefix, key)).await
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        let scoped = scope::apply(&self.prefix, req.key);
        self.inner
            .put(PutRequest {
                key: &scoped,
                value: req.value,
                ttl: req.ttl,
            })
            .await
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.inner.delete(&scope::apply(&self.prefix, key)).await
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        self.inner.contains(&scope::apply(&self.prefix, key)).await
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        let scoped = scope::apply(&self.prefix, req.key);
        self.inner
            .put_if_absent(PutRequest {
                key: &scoped,
                value: req.value,
                ttl: req.ttl,
            })
            .await
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        self.inner
            .compare_and_swap(
                &scope::apply(&self.prefix, key),
                expected_version,
                new_value,
                ttl,
            )
            .await
    }

    async fn compare_and_swap_value(
        &self,
        key: &str,
        expected_value: &[u8],
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        self.inner
            .compare_and_swap_value(
                &scope::apply(&self.prefix, key),
                expected_value,
                new_value,
                ttl,
            )
            .await
    }

    async fn compare_and_delete(
        &self,
        key: &str,
        expected_value: &[u8],
    ) -> Result<bool, ClusterError> {
        self.inner
            .compare_and_delete(&scope::apply(&self.prefix, key), expected_value)
            .await
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        let inner = self.inner.watch(&scope::apply(&self.prefix, key)).await?;
        Ok(Self::strip_watch(self.prefix.clone(), inner))
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        let inner = self
            .inner
            .watch_prefix(&scope::apply(&self.prefix, prefix))
            .await?;
        Ok(Self::strip_watch(self.prefix.clone(), inner))
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        let keys = self
            .inner
            .scan_prefix(&scope::apply(&self.prefix, prefix))
            .await?;
        Ok(keys
            .into_iter()
            .map(|key| scope::strip(&self.prefix, &key).to_owned())
            .collect())
    }

    /// Forwarded unchanged: a probe carries no key, so there is nothing to scope.
    /// Forwarding at all is what keeps a scoped view from reporting the trait's
    /// `Ok(())` default over an unreachable backend.
    async fn probe(&self) -> Result<(), ClusterError> {
        self.inner.probe().await
    }
}

#[cfg(test)]
#[path = "scoped_tests.rs"]
mod scoped_tests;
