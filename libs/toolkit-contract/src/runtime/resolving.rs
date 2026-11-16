//! Directory-resolving transport, shared by REST and gRPC.
//!
//! [`DirectoryResolvingClient`] is a thin, self-healing wrapper around a
//! generated `<Trait>RestClient` or `<Trait>GrpcClient`. Instead of binding to
//! a single static base URL at construction time, it resolves the provider's
//! endpoint from the service directory on every call and builds a client per
//! endpoint, reusing it while that endpoint keeps coming back.
//!
//! This is what makes the consumer side tolerant of *eventual readiness* and
//! runtime churn:
//! - **Not ready yet** — the provider hasn't registered → resolution yields
//!   `None` → calls fail with [`TransportError::Unresolved`] (a transient
//!   error that maps to `service_unavailable`), never a panic.
//! - **Provider moved / pod replaced** — the directory returns a new endpoint
//!   → the wrapper builds a client for it on the next call.
//! - **Several instances** — the directory rotates between them; each keeps
//!   its client (and its HTTP/2 connection and circuit breaker) instead of
//!   being rebuilt on every rotation.
//! - **Endpoint failing** — an attempt the circuit breaker classifies as a
//!   backend failure drops the resolver's cached answer (at most once per
//!   second per endpoint), so the next call asks the directory again rather
//!   than waiting out the cache TTL. The breaker for that endpoint opens if
//!   the failures go on.
//! - **Provider vanished** — all instances evicted → resolution yields `None`
//!   again → `Unresolved`; the wrapper recovers automatically once a live
//!   instance reappears.
//!
//! The `Arc<dyn Trait>` registered in the `ClientHub` is the long-lived
//! resolving wrapper, so the hub entry is wired once and never replaced; all
//! churn handling lives inside this transport.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::runtime::breaker::EndpointFailureHook;
use crate::runtime::config::ClientConfig;
use crate::runtime::transport_error::TransportError;
use crate::wiring::ClientTuning;

/// Endpoints whose clients are kept at once. Covers the instances a directory
/// round-robins between; the least recently built one goes first.
const MAX_ENDPOINTS: usize = 16;

/// An endpoint failure invalidates the resolver's answer at most this often,
/// so a dead endpoint under load does not turn every call into a directory
/// round-trip.
const INVALIDATE_DEBOUNCE: Duration = Duration::from_secs(1);

/// Directory-lookup failure (the directory backend itself could not answer —
/// e.g. the gRPC directory is unreachable), as opposed to a successful lookup
/// that found no live instance (`Ok(None)`). Keeping the two distinct lets
/// callers and observability tell a not-ready provider apart from a directory
/// outage.
#[derive(Debug, thiserror::Error)]
#[error("directory lookup for gear `{gear}` failed: {source}")]
pub struct ResolveError {
    /// Gear name (or gRPC service name) whose lookup failed.
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

/// What a resolving client looks up.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResolveTarget {
    /// The REST endpoint of a gear.
    Rest {
        /// Providing gear.
        gear: String,
    },
    /// A gRPC service. The directory keys gRPC by service name
    /// (`package.Service`), which carries the contract's major version.
    Grpc {
        /// Providing gear, for logs, errors and readiness.
        gear: String,
        /// Fully qualified gRPC service name.
        service: String,
    },
}

impl ResolveTarget {
    /// REST endpoint of `gear`.
    pub fn rest(gear: impl Into<String>) -> Self {
        Self::Rest { gear: gear.into() }
    }

    /// gRPC `service` provided by `gear`.
    pub fn grpc(gear: impl Into<String>, service: impl Into<String>) -> Self {
        Self::Grpc {
            gear: gear.into(),
            service: service.into(),
        }
    }

    /// The providing gear.
    #[must_use]
    pub fn gear(&self) -> &str {
        match self {
            Self::Rest { gear } | Self::Grpc { gear, .. } => gear,
        }
    }
}

/// Resolves a logical gear name or gRPC service name to a live base endpoint
/// URI.
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
/// TTL) rather than performing a network round-trip per call, and drop the
/// memoized answer in [`invalidate`](Self::invalidate).
#[async_trait]
pub trait EndpointResolver: Send + Sync {
    /// Resolve `gear` to a base endpoint URI (e.g. `http://billing:8080`).
    ///
    /// # Errors
    /// Returns [`ResolveError`] only when the directory backend could not be
    /// queried; a successful query with no live instance is `Ok(None)`.
    async fn resolve_endpoint(&self, gear: &str) -> Result<Option<String>, ResolveError>;

    /// Resolve a gRPC `service` (`package.Service`) to the base URI of a
    /// channel serving it. The default resolves nothing, so a resolver that
    /// predates gRPC consumers keeps them unresolved rather than misrouted.
    ///
    /// # Errors
    /// As [`resolve_endpoint`](Self::resolve_endpoint).
    async fn resolve_grpc_endpoint(&self, _service: &str) -> Result<Option<String>, ResolveError> {
        Ok(None)
    }

    /// Resolve `target` through the method for its transport.
    ///
    /// # Errors
    /// As [`resolve_endpoint`](Self::resolve_endpoint).
    async fn resolve_target(&self, target: &ResolveTarget) -> Result<Option<String>, ResolveError> {
        match target {
            ResolveTarget::Rest { gear } => self.resolve_endpoint(gear).await,
            ResolveTarget::Grpc { service, .. } => self.resolve_grpc_endpoint(service).await,
        }
    }

    /// Forget any memoized answer for `target`: its endpoint just failed. The
    /// default does nothing (a resolver that does not memoize).
    fn invalidate(&self, _target: &ResolveTarget) {}
}

type ClientBuilder<C> = dyn Fn(ClientConfig) -> Result<C, TransportError> + Send + Sync;

/// Self-healing transport that resolves its endpoint from the directory.
///
/// Generic over the concrete generated client `C` (e.g. `PaymentApiRestClient`
/// or `PaymentApiGrpcClient`). The generated `<Trait>RestResolvingClient` /
/// `<Trait>GrpcResolvingClient` wraps one of these and delegates each trait
/// method through [`DirectoryResolvingClient::resolved`].
pub struct DirectoryResolvingClient<C> {
    resolver: Arc<dyn EndpointResolver>,
    target: ResolveTarget,
    tuning: ClientTuning,
    build: Box<ClientBuilder<C>>,
    /// Clients by endpoint, most recently built first.
    cache: RwLock<Vec<(String, Arc<C>)>>,
    /// Serializes the build step. Without this, N callers that concurrently
    /// observe the same absent cache entry (first call, or immediately after
    /// an endpoint change) would each construct a redundant client — only the
    /// guard is held, never across an `.await` (the directory lookup has
    /// already completed by the time this lock is taken; `build` itself is
    /// synchronous).
    build_lock: std::sync::Mutex<()>,
}

impl<C: Send + Sync + 'static> DirectoryResolvingClient<C> {
    /// Construct a client resolving the REST endpoint of `from_gear`.
    ///
    /// - `resolver` — resolves the provider gear name to a live endpoint.
    /// - `from_gear` — the logical provider gear name to resolve.
    /// - `tuning` — timeout/retry/reconnect/circuit-breaker settings applied to
    ///   each built client.
    /// - `build` — constructs the concrete client `C` from a [`ClientConfig`]
    ///   (the generated `C::new(config)`, mapping its error into
    ///   [`TransportError`]).
    pub fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
        build: impl Fn(ClientConfig) -> Result<C, TransportError> + Send + Sync + 'static,
    ) -> Self {
        Self::for_target(resolver, ResolveTarget::rest(from_gear), tuning, build)
    }

    /// Construct a client resolving `target`. See [`Self::new`].
    pub fn for_target(
        resolver: Arc<dyn EndpointResolver>,
        target: ResolveTarget,
        tuning: ClientTuning,
        build: impl Fn(ClientConfig) -> Result<C, TransportError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            resolver,
            target,
            tuning,
            build: Box::new(build),
            cache: RwLock::new(Vec::new()),
            build_lock: std::sync::Mutex::new(()),
        }
    }

    /// The logical provider gear name this client resolves against.
    #[must_use]
    pub fn from_gear(&self) -> &str {
        self.target.gear()
    }

    /// What this client resolves.
    #[must_use]
    pub fn target(&self) -> &ResolveTarget {
        &self.target
    }

    /// Resolve (or reuse) the underlying client for the current endpoint.
    ///
    /// Re-resolves the directory on every call so a moved or vanished provider
    /// is observed immediately; the built client is cached per endpoint.
    /// Returns [`TransportError::Unresolved`] when no live instance is
    /// registered.
    ///
    /// # Errors
    /// - [`TransportError::Unresolved`] if the directory has no live endpoint.
    /// - Whatever `build` returns if constructing the client fails.
    pub async fn resolved(&self) -> Result<Arc<C>, TransportError> {
        let gear = self.target.gear();
        let uri = match self.resolver.resolve_target(&self.target).await {
            Ok(Some(uri)) => uri,
            Ok(None) => {
                debug!(gear, target = ?self.target, "no live instance registered; provider not ready");
                self.clear();
                return Err(TransportError::unresolved(gear));
            }
            Err(err) => {
                warn!(gear, target = ?self.target, error = %err, "directory lookup failed");
                self.clear();
                return Err(TransportError::unresolved(gear));
            }
        };

        // Fast path: a client for this endpoint already exists.
        if let Some(client) = self.cached_for(&uri) {
            return Ok(client);
        }

        // First call for this endpoint: build and cache. Single-flight via
        // `build_lock` so a thundering herd of concurrent callers that all
        // missed the fast path above builds the client ONCE, not N times.
        let _build_guard = self
            .build_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Re-check under the lock: another caller may have just finished
        // building for this exact endpoint while we were waiting for it.
        if let Some(client) = self.cached_for(&uri) {
            return Ok(client);
        }
        debug!(gear, endpoint = %uri, "building client for resolved endpoint");
        let cfg = self.config_for(&uri);
        let client = Arc::new((self.build)(cfg)?);
        if let Ok(mut w) = self.cache.write() {
            w.insert(0, (uri, Arc::clone(&client)));
            w.truncate(MAX_ENDPOINTS);
        }
        Ok(client)
    }

    /// The config for a client of `uri`: the tuning, a circuit breaker of its
    /// own, and the hook that makes an endpoint failure re-resolve.
    fn config_for(&self, uri: &str) -> ClientConfig {
        let name = format!("{}@{uri}", self.target.gear());
        let breaker = self.tuning.circuit_breaker_for(&name);
        let invalidator = Invalidator {
            resolver: Arc::clone(&self.resolver),
            target: self.target.clone(),
            last: parking_lot::Mutex::new(None),
        };
        self.tuning
            .apply_to(uri)
            .with_circuit_breaker(breaker)
            .with_on_endpoint_failure(EndpointFailureHook::new(move || invalidator.fire()))
    }

    /// Returns the cached client built for `uri`, if any.
    fn cached_for(&self, uri: &str) -> Option<Arc<C>> {
        let r = self.cache.read().ok()?;
        r.iter()
            .find(|(cached, _)| cached == uri)
            .map(|(_, client)| Arc::clone(client))
    }

    /// Drop every cached client so a now-absent or moved provider isn't
    /// masked on the next call.
    fn clear(&self) {
        if let Ok(mut w) = self.cache.write() {
            w.clear();
        }
    }
}

/// Drops the resolver's answer for one endpoint's target, debounced.
struct Invalidator {
    resolver: Arc<dyn EndpointResolver>,
    target: ResolveTarget,
    last: parking_lot::Mutex<Option<Instant>>,
}

impl Invalidator {
    fn fire(&self) {
        let now = Instant::now();
        {
            let mut last = self.last.lock();
            if last.is_some_and(|t| now.saturating_duration_since(t) < INVALIDATE_DEBOUNCE) {
                return;
            }
            *last = Some(now);
        }
        self.resolver.invalidate(&self.target);
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

    impl ScriptResolver {
        fn next(&self, key: &str) -> Result<Option<String>, ResolveError> {
            let i = self.idx.fetch_add(1, Ordering::SeqCst);
            match self.steps.get(i).cloned() {
                Some(Step::Found(uri)) => Ok(Some(uri)),
                Some(Step::Empty) | None => Ok(None),
                Some(Step::Fail) => Err(ResolveError::new(key, "directory down")),
            }
        }
    }

    #[async_trait]
    impl EndpointResolver for ScriptResolver {
        async fn resolve_endpoint(&self, gear: &str) -> Result<Option<String>, ResolveError> {
            self.next(gear)
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
        assert_eq!(
            builds.load(Ordering::SeqCst),
            0,
            "must not build a client on directory failure"
        );
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
        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "stable endpoint reuses client"
        );

        let c3 = c.resolved().await.unwrap();
        assert_eq!(c3.base_url, "http://b:9090");
        assert_eq!(
            builds.load(Ordering::SeqCst),
            2,
            "endpoint change rebuilds client"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_resolves_build_client_only_once() {
        // Real OS-thread concurrency (not single-threaded cooperative
        // interleaving): N callers all resolving to the same first-seen
        // endpoint race to build. Without single-flight (`build_lock`), each
        // one that misses the cache before any write lands would construct
        // its own redundant client.
        const N: usize = 16;
        let (c, builds) = resolving(vec![Step::Found("http://a:8080".into()); N]);
        let c = Arc::new(c);

        let handles: Vec<_> = (0..N)
            .map(|_| {
                let c = Arc::clone(&c);
                tokio::spawn(async move { c.resolved().await.unwrap() })
            })
            .collect();

        for h in handles {
            let client = h.await.unwrap();
            assert_eq!(client.base_url, "http://a:8080");
        }
        assert_eq!(
            builds.load(Ordering::SeqCst),
            1,
            "single-flight: exactly one build for {N} concurrent resolves to the same endpoint"
        );
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

    #[tokio::test]
    async fn rotating_between_endpoints_reuses_each_client() {
        let (c, builds) = resolving(vec![
            Step::Found("http://a:8080".into()),
            Step::Found("http://b:8080".into()),
            Step::Found("http://a:8080".into()),
            Step::Found("http://b:8080".into()),
        ]);
        let first_a = c.resolved().await.unwrap();
        c.resolved().await.unwrap();
        let again_a = c.resolved().await.unwrap();
        c.resolved().await.unwrap();

        assert!(Arc::ptr_eq(&first_a, &again_a));
        assert_eq!(builds.load(Ordering::SeqCst), 2, "one build per endpoint");
    }

    /// Records what the resolving client asked for, and invalidations.
    #[derive(Default)]
    struct Recording {
        grpc: parking_lot::Mutex<Vec<String>>,
        invalidated: AtomicUsize,
    }

    #[async_trait]
    impl EndpointResolver for Recording {
        async fn resolve_endpoint(&self, _gear: &str) -> Result<Option<String>, ResolveError> {
            Ok(Some("http://rest:8080".into()))
        }

        async fn resolve_grpc_endpoint(
            &self,
            service: &str,
        ) -> Result<Option<String>, ResolveError> {
            self.grpc.lock().push(service.to_owned());
            Ok(Some("http://grpc:50051".into()))
        }

        fn invalidate(&self, _target: &ResolveTarget) {
            self.invalidated.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn capturing(
        resolver: Arc<Recording>,
        target: ResolveTarget,
    ) -> DirectoryResolvingClient<ClientConfig> {
        DirectoryResolvingClient::for_target(resolver, target, ClientTuning::default(), Ok)
    }

    #[tokio::test]
    async fn a_grpc_target_resolves_by_service_name() {
        let resolver = Arc::new(Recording::default());
        let c = capturing(
            Arc::clone(&resolver),
            ResolveTarget::grpc("payments", "payments.v1.PaymentApi"),
        );
        let cfg = c.resolved().await.unwrap();

        assert_eq!(cfg.base_url, "http://grpc:50051");
        assert_eq!(*resolver.grpc.lock(), ["payments.v1.PaymentApi"]);
        assert_eq!(c.from_gear(), "payments");
    }

    #[tokio::test]
    async fn each_endpoint_gets_its_own_breaker_by_default() {
        let resolver = Arc::new(Recording::default());
        let cfg = capturing(resolver, ResolveTarget::rest("payments"))
            .resolved()
            .await
            .unwrap();
        let breaker = cfg.circuit_breaker.as_ref().expect("on by default");
        assert!(format!("{breaker:?}").contains("payments@http://rest:8080"));
    }

    #[tokio::test]
    async fn a_disabled_breaker_is_not_attached() {
        let tuning = ClientTuning {
            circuit_breaker: Some(
                toolkit_resilience::circuit_breaker::CircuitBreakerConfig::disabled(),
            ),
            ..ClientTuning::default()
        };
        let c = DirectoryResolvingClient::for_target(
            Arc::new(Recording::default()),
            ResolveTarget::rest("payments"),
            tuning,
            Ok,
        );
        assert!(c.resolved().await.unwrap().circuit_breaker.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn an_endpoint_failure_invalidates_the_resolver_debounced() {
        let resolver = Arc::new(Recording::default());
        let cfg = capturing(Arc::clone(&resolver), ResolveTarget::rest("payments"))
            .resolved()
            .await
            .unwrap();
        let hook = cfg.on_endpoint_failure.clone().expect("hook installed");

        (hook.0)();
        (hook.0)();
        assert_eq!(resolver.invalidated.load(Ordering::SeqCst), 1);

        tokio::time::advance(INVALIDATE_DEBOUNCE).await;
        (hook.0)();
        assert_eq!(resolver.invalidated.load(Ordering::SeqCst), 2);
    }
}
