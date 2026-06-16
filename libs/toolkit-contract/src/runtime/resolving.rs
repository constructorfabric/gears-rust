//! Directory-resolving REST transport.
//!
//! [`DirectoryResolvingClient`] is a thin, self-healing wrapper around a
//! generated `<Trait>RestClient`. Instead of binding to a single static base
//! URL at construction time, it resolves the provider's endpoint from the
//! service directory on every call and rebuilds the underlying client only
//! when the resolved endpoint changes.
//!
//! This is what makes the consumer side tolerant of *eventual readiness* and
//! runtime churn:
//! - **Not ready yet** — the provider hasn't registered → resolution yields
//!   `None` → calls fail with [`TransportError::Unresolved`] (a transient
//!   error that maps to `service_unavailable`), never a panic.
//! - **Provider moved / pod replaced** — the directory returns a new endpoint
//!   → the wrapper rebuilds the client against it on the next call.
//! - **Provider vanished** — all instances evicted → resolution yields `None`
//!   again → `Unresolved`; the wrapper recovers automatically once a live
//!   instance reappears.
//!
//! The `Arc<dyn Trait>` registered in the `ClientHub` is the long-lived
//! resolving wrapper, so the hub entry is wired once and never replaced; all
//! churn handling lives inside this transport.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tracing::{debug, warn};

use crate::runtime::config::ClientConfig;
use crate::runtime::transport_error::TransportError;
use crate::wiring::ClientTuning;

/// Directory-lookup failure (the directory backend itself could not answer —
/// e.g. the gRPC directory is unreachable), as opposed to a successful lookup
/// that found no live instance (`Ok(None)`). Keeping the two distinct lets
/// callers and observability tell a not-ready provider apart from a directory
/// outage.
#[derive(Debug, thiserror::Error)]
#[error("directory lookup for gear `{gear}` failed: {source}")]
pub struct ResolveError {
    /// Gear name whose lookup failed.
    pub gear: String,
    /// Underlying directory transport/backend error.
    #[source]
    pub source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl ResolveError {
    /// Build a [`ResolveError`] from any boxable source error.
    pub fn new<E>(gear: impl Into<String>, source: E) -> Self
    where
        E: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        Self {
            gear: gear.into(),
            source: source.into(),
        }
    }
}

/// Resolves a logical gear name to a live base endpoint URI.
///
/// Kept as a minimal trait here so `toolkit-contract` does not depend on the
/// service-directory SDK. The host layer (`toolkit`) provides an adapter over
/// its `DirectoryClient`; tests can implement it directly.
///
/// Return contract:
/// - `Ok(Some(uri))` — a live instance was resolved.
/// - `Ok(None)` — the lookup succeeded but no live instance is registered
///   (provider not ready yet, or every instance evicted).
/// - `Err(ResolveError)` — the directory backend itself failed to answer.
///
/// Both `Ok(None)` and `Err(..)` surface to the caller as a retryable
/// [`TransportError::Unresolved`]; the distinction exists for logging/metrics.
///
/// [`DirectoryResolvingClient`] calls this **on every request** so provider
/// churn is observed immediately. Implementations whose lookup is expensive
/// (e.g. an out-of-process gRPC directory) SHOULD memoize internally (short
/// TTL) rather than performing a network round-trip per call.
#[async_trait]
pub trait EndpointResolver: Send + Sync {
    /// Resolve `gear` to a base endpoint URI (e.g. `http://billing:8080`).
    ///
    /// # Errors
    /// Returns [`ResolveError`] only when the directory backend could not be
    /// queried; a successful query with no live instance is `Ok(None)`.
    async fn resolve_endpoint(&self, gear: &str) -> Result<Option<String>, ResolveError>;
}

type ClientBuilder<C> = dyn Fn(ClientConfig) -> Result<C, TransportError> + Send + Sync;

/// Self-healing REST transport that resolves its endpoint from the directory.
///
/// Generic over the concrete generated client `C` (e.g. `PaymentApiRestClient`).
/// The generated `<Trait>ResolvingClient` wraps one of these and delegates each
/// trait method through [`DirectoryResolvingClient::resolved`].
pub struct DirectoryResolvingClient<C> {
    resolver: Arc<dyn EndpointResolver>,
    from_gear: String,
    tuning: ClientTuning,
    build: Box<ClientBuilder<C>>,
    /// Cached `(endpoint, client)` reused while the resolved endpoint is stable.
    cache: RwLock<Option<(String, Arc<C>)>>,
}

impl<C: Send + Sync + 'static> DirectoryResolvingClient<C> {
    /// Construct a resolving client.
    ///
    /// - `resolver` — resolves the provider gear name to a live endpoint.
    /// - `from_gear` — the logical provider gear name to resolve.
    /// - `tuning` — timeout/retry/reconnect overrides applied to each built client.
    /// - `build` — constructs the concrete client `C` from a [`ClientConfig`]
    ///   (the generated `C::new(config)`, mapping its error into
    ///   [`TransportError`]).
    pub fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
        build: impl Fn(ClientConfig) -> Result<C, TransportError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            resolver,
            from_gear: from_gear.into(),
            tuning,
            build: Box::new(build),
            cache: RwLock::new(None),
        }
    }

    /// The logical provider gear name this client resolves against.
    #[must_use]
    pub fn from_gear(&self) -> &str {
        &self.from_gear
    }

    /// Resolve (or reuse) the underlying client for the current endpoint.
    ///
    /// Re-resolves the directory on every call so a moved or vanished provider
    /// is observed immediately; the built client is cached and reused while the
    /// endpoint is unchanged. Returns [`TransportError::Unresolved`] when no
    /// live instance is registered.
    ///
    /// # Errors
    /// - [`TransportError::Unresolved`] if the directory has no live endpoint.
    /// - Whatever `build` returns if constructing the client fails.
    pub async fn resolved(&self) -> Result<Arc<C>, TransportError> {
        let uri = match self.resolver.resolve_endpoint(&self.from_gear).await {
            Ok(Some(uri)) => uri,
            Ok(None) => {
                debug!(gear = %self.from_gear, "no live instance registered; provider not ready");
                self.invalidate();
                return Err(TransportError::unresolved(&self.from_gear));
            }
            Err(err) => {
                warn!(gear = %self.from_gear, error = %err, "directory lookup failed");
                self.invalidate();
                return Err(TransportError::unresolved(&self.from_gear));
            }
        };

        // Fast path: endpoint unchanged → reuse the cached client.
        if let Ok(r) = self.cache.read() {
            if let Some((cached_uri, client)) = r.as_ref() {
                if *cached_uri == uri {
                    return Ok(Arc::clone(client));
                }
            }
        }

        // First call, or the endpoint changed: build and cache.
        debug!(gear = %self.from_gear, endpoint = %uri, "building REST client for resolved endpoint");
        let cfg = self.tuning.apply_to(&uri);
        let client = Arc::new((self.build)(cfg)?);
        if let Ok(mut w) = self.cache.write() {
            *w = Some((uri, Arc::clone(&client)));
        }
        Ok(client)
    }

    /// Drop any cached client so a now-absent or moved provider isn't masked
    /// on the next call.
    fn invalidate(&self) {
        if let Ok(mut w) = self.cache.write() {
            *w = None;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One scripted resolver outcome.
    #[derive(Clone)]
    enum Step {
        /// `Ok(Some(uri))`.
        Found(String),
        /// `Ok(None)` — looked up, nothing registered.
        Empty,
        /// `Err(..)` — directory backend failure.
        Fail,
    }

    /// Resolver returning a scripted sequence of outcomes.
    struct ScriptResolver {
        steps: Vec<Step>,
        idx: AtomicUsize,
    }

    #[async_trait]
    impl EndpointResolver for ScriptResolver {
        async fn resolve_endpoint(&self, gear: &str) -> Result<Option<String>, ResolveError> {
            let i = self.idx.fetch_add(1, Ordering::SeqCst);
            match self.steps.get(i).cloned() {
                Some(Step::Found(uri)) => Ok(Some(uri)),
                Some(Step::Empty) | None => Ok(None),
                Some(Step::Fail) => Err(ResolveError::new(gear, "directory down")),
            }
        }
    }

    /// Dummy "client" carrying the endpoint it was built against.
    #[derive(Debug)]
    struct DummyClient {
        base_url: String,
    }

    /// Build a resolving client with a **per-instance** build counter (returned
    /// alongside) so parallel tests don't race on a shared global.
    fn resolving(steps: Vec<Step>) -> (DirectoryResolvingClient<DummyClient>, Arc<AtomicUsize>) {
        let builds = Arc::new(AtomicUsize::new(0));
        let builds_for_closure = Arc::clone(&builds);
        let client = DirectoryResolvingClient::new(
            Arc::new(ScriptResolver {
                steps,
                idx: AtomicUsize::new(0),
            }),
            "billing",
            ClientTuning::default(),
            move |cfg| {
                builds_for_closure.fetch_add(1, Ordering::SeqCst);
                Ok(DummyClient {
                    base_url: cfg.base_url,
                })
            },
        );
        (client, builds)
    }

    #[tokio::test]
    async fn unresolved_when_no_endpoint() {
        let (c, _builds) = resolving(vec![Step::Empty]);
        let err = c.resolved().await.unwrap_err();
        assert!(matches!(err, TransportError::Unresolved { .. }));
    }

    #[tokio::test]
    async fn directory_failure_surfaces_as_unresolved() {
        let (c, builds) = resolving(vec![Step::Fail]);
        let err = c.resolved().await.unwrap_err();
        assert!(matches!(err, TransportError::Unresolved { .. }));
        assert_eq!(builds.load(Ordering::SeqCst), 0, "must not build a client on directory failure");
    }

    #[tokio::test]
    async fn caches_while_endpoint_stable_and_rebuilds_on_change() {
        let (c, builds) = resolving(vec![
            Step::Found("http://a:8080".into()),
            Step::Found("http://a:8080".into()),
            Step::Found("http://b:9090".into()),
        ]);

        let c1 = c.resolved().await.unwrap();
        assert_eq!(c1.base_url, "http://a:8080");
        let c2 = c.resolved().await.unwrap();
        assert_eq!(c2.base_url, "http://a:8080");
        assert_eq!(builds.load(Ordering::SeqCst), 1, "stable endpoint reuses client");

        let c3 = c.resolved().await.unwrap();
        assert_eq!(c3.base_url, "http://b:9090");
        assert_eq!(builds.load(Ordering::SeqCst), 2, "endpoint change rebuilds client");
    }

    #[tokio::test]
    async fn recovers_after_provider_vanishes_and_returns() {
        let (c, _builds) = resolving(vec![
            Step::Found("http://a:8080".into()),
            Step::Empty,
            Step::Found("http://c:7070".into()),
        ]);

        assert_eq!(c.resolved().await.unwrap().base_url, "http://a:8080");
        // Provider vanished → Unresolved, stale cache dropped.
        assert!(matches!(
            c.resolved().await.unwrap_err(),
            TransportError::Unresolved { .. }
        ));
        // New instance appears → resolves again, self-healed.
        assert_eq!(c.resolved().await.unwrap().base_url, "http://c:7070");
    }
}
