// Created: 2026-06-18 by Constructor Tech
//! A telemetry-emitting [`ClusterCacheBackend`] decorator (ADR-004).
//!
//! [`InstrumentedCache`] wraps any cache backend and emits the contracted cache
//! signals — `cluster.cache.*` spans, `cluster_cache_*` metrics, and the
//! `cluster.provider.error` log event — using the stable names from the
//! [`observability`](crate::observability) module. It is the single emission
//! implementation every provider reuses, so the naming contract and the
//! cardinality rule hold structurally rather than per backend.
//!
//! It is a delegating backend in the same shape as
//! [`ScopedCacheBackend`](crate::cache) — it holds an
//! `Arc<dyn ClusterCacheBackend>` and forwards every method, adding telemetry
//! around the call. A backend is wrapped by its plugin, which supplies the
//! bounded `provider` label and the [`ClusterMetrics`] sink (so ADR-004's
//! "every plugin emits" holds while the emission code stays shared).
//!
//! Spans and log events go through `tracing`; only metrics flow through the
//! [`ClusterMetrics`] port. High-cardinality keys ride spans as attributes and
//! never become metric labels.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tracing::Instrument;

use crate::cache::types::{PutRequest, Ttl};
use crate::cache::{CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, ClusterCacheBackend};
use crate::error::ClusterError;
use crate::observability::{ClusterMetrics, ResourceId, emit_provider_error, result, spans};

/// A [`ClusterCacheBackend`] that emits the contracted cache observability
/// signals around a wrapped backend. See the [module docs](self).
pub struct InstrumentedCache {
    inner: Arc<dyn ClusterCacheBackend>,
    /// The bounded `provider` label, supplied by the wrapping plugin.
    provider: &'static str,
    metrics: Arc<dyn ClusterMetrics>,
}

impl InstrumentedCache {
    /// Wraps `inner`, labelling its emitted signals with `provider` and routing
    /// metrics through `metrics`.
    #[must_use]
    pub fn new(
        inner: Arc<dyn ClusterCacheBackend>,
        provider: &'static str,
        metrics: Arc<dyn ClusterMetrics>,
    ) -> Self {
        Self {
            inner,
            provider,
            metrics,
        }
    }

    /// Records the metric side of a finished op: duration + a bounded-`result`
    /// counter, plus a provider-error counter and `cluster.provider.error` log
    /// when the failure is a genuine backend error (not a normal outcome such as
    /// a CAS conflict).
    fn record<T>(
        &self,
        op: &'static str,
        key: &str,
        started: Instant,
        outcome: &Result<T, ClusterError>,
    ) {
        self.metrics
            .cache_op_duration(op, started.elapsed().as_secs_f64());
        self.metrics.cache_op(op, result::label(outcome));
        if let Err(err) = outcome {
            emit_provider_error(&*self.metrics, self.provider, op, ResourceId::Key(key), err);
        }
    }
}

#[async_trait]
impl ClusterCacheBackend for InstrumentedCache {
    fn consistency(&self) -> CacheConsistency {
        self.inner.consistency()
    }

    fn features(&self) -> CacheFeatures {
        self.inner.features()
    }

    fn provider_name(&self) -> &'static str {
        // Delegate so capability diagnostics still name the real backend.
        self.inner.provider_name()
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        let span = tracing::info_span!(spans::CACHE_GET, provider = %self.provider, key = %key);
        let started = Instant::now();
        let out = self.inner.get(key).instrument(span).await;
        self.record("get", key, started, &out);
        out
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        let key = req.key;
        let span = tracing::info_span!(spans::CACHE_PUT, provider = %self.provider, key = %key);
        let started = Instant::now();
        let out = self.inner.put(req).instrument(span).await;
        self.record("put", key, started, &out);
        out
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        let span = tracing::info_span!(spans::CACHE_DELETE, provider = %self.provider, key = %key);
        let started = Instant::now();
        let out = self.inner.delete(key).instrument(span).await;
        self.record("delete", key, started, &out);
        out
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        let span =
            tracing::info_span!(spans::CACHE_CONTAINS, provider = %self.provider, key = %key);
        let started = Instant::now();
        let out = self.inner.contains(key).instrument(span).await;
        self.record("contains", key, started, &out);
        out
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        let key = req.key;
        let span =
            tracing::info_span!(spans::CACHE_PUT_IF_ABSENT, provider = %self.provider, key = %key);
        let started = Instant::now();
        let out = self.inner.put_if_absent(req).instrument(span).await;
        self.record("put_if_absent", key, started, &out);
        out
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        let span = tracing::info_span!(
            spans::CACHE_COMPARE_AND_SWAP,
            provider = %self.provider,
            key = %key
        );
        let started = Instant::now();
        let out = self
            .inner
            .compare_and_swap(key, expected_version, new_value, ttl)
            .instrument(span)
            .await;
        self.record("compare_and_swap", key, started, &out);
        out
    }

    async fn compare_and_delete(
        &self,
        key: &str,
        expected_value: &[u8],
    ) -> Result<bool, ClusterError> {
        // No cataloged span (it is a backend-only op, not a facade operation),
        // but it is still a cache op for the metrics surface.
        let started = Instant::now();
        let out = self.inner.compare_and_delete(key, expected_value).await;
        self.record("compare_and_delete", key, started, &out);
        out
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        let span = tracing::info_span!(spans::CACHE_WATCH, provider = %self.provider, key = %key);
        let started = Instant::now();
        let mut out = self.inner.watch(key).instrument(span).await;
        self.record("watch", key, started, &out);
        // Stamp the watch so an `auto_restart`ed consumer emits the watch-reset
        // signals (`cluster_watch_resets_total` / `cluster.watch.reset`).
        if let Ok(watch) = &mut out {
            watch.set_observability(self.provider, Arc::clone(&self.metrics));
        }
        out
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        let span = tracing::info_span!(
            spans::CACHE_WATCH_PREFIX,
            provider = %self.provider,
            key = %prefix
        );
        let started = Instant::now();
        let mut out = self.inner.watch_prefix(prefix).instrument(span).await;
        self.record("watch_prefix", prefix, started, &out);
        if let Ok(watch) = &mut out {
            watch.set_observability(self.provider, Arc::clone(&self.metrics));
        }
        out
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        // No cataloged span; still recorded on the metrics surface.
        let started = Instant::now();
        let out = self.inner.scan_prefix(prefix).await;
        self.record("scan_prefix", prefix, started, &out);
        out
    }
}

#[cfg(test)]
#[path = "instrumented_cache_tests.rs"]
mod instrumented_cache_tests;
