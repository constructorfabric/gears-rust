// Created: 2026-06-10 by Constructor Tech
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
    /// Wraps `inner` with the effective `prefix` (already validated and
    /// separator-terminated by [`scope::validated_prefix`]).
    pub fn new(inner: Arc<dyn ClusterCacheBackend>, prefix: String) -> Self {
        Self { inner, prefix }
    }

    /// Wraps a backend-facing [`CacheWatch`] in a read-path forwarding task that
    /// strips `prefix` from every event key before handing it to the consumer.
    /// The task ends — dropping its sender — when the inner watch ends or the
    /// consumer drops the returned watch.
    fn strip_watch(prefix: String, mut inner: CacheWatch) -> CacheWatch {
        let (tx, watch) = CacheWatch::channel(FORWARD_BUFFER);
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
}

#[cfg(test)]
#[path = "scoped_tests.rs"]
mod scoped_tests;
