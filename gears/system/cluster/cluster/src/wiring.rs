//! The cluster wiring builder, per-profile backend bindings, and lifecycle
//! handle (DESIGN §3.7).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::{
    ClusterCacheBackend, ClusterError, ClusterProfile, DistributedLockBackend,
    LeaderElectionBackend, ServiceDiscoveryBackend, StopHook, deregister_cache_backend,
    deregister_leader_election_backend, deregister_lock_backend,
    deregister_service_discovery_backend, register_cache_backend, register_leader_election_backend,
    register_lock_backend, register_service_discovery_backend,
};

use crate::defaults::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend, ShutdownRevoke,
};
use toolkit::client_hub::ClientHub;

use crate::config::{ClusterConfig, ProfileConfig};
use crate::provider::ProviderRegistry;

/// The per-primitive backend bindings for one profile.
///
/// `cache` is required; each of the other three primitives may be bound to its
/// own backend (`cpt-cf-clst-fr-routing-per-primitive`) or left `None`, in which
/// case [`ClusterWiringBuilder::build_and_start`] auto-fills it with the SDK
/// default backend over `cache` (`cpt-cf-clst-fr-routing-omit-default`).
pub struct ProfileBackends {
    cache: Arc<dyn ClusterCacheBackend>,
    leader_election: Option<Arc<dyn LeaderElectionBackend>>,
    lock: Option<Arc<dyn DistributedLockBackend>>,
    service_discovery: Option<Arc<dyn ServiceDiscoveryBackend>>,
    /// Operator-configured poll cadence for the auto-wrapped
    /// [`CacheBasedServiceDiscoveryBackend`]'s `PollingPrefixWatch` polyfill,
    /// used only when `service_discovery` is left to the SDK default over a
    /// cache without native prefix watch. `None` keeps `DEFAULT_PREFIX_WATCH_POLL`
    /// (PGR-D3).
    sd_poll_interval: Option<Duration>,
}

impl ProfileBackends {
    /// Binds a profile to `cache`, leaving the other three primitives to the SDK
    /// defaults unless overridden with the `with_*` methods.
    #[must_use]
    pub fn new(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        Self {
            cache,
            leader_election: None,
            lock: None,
            service_discovery: None,
            sd_poll_interval: None,
        }
    }

    /// Sets the service-discovery poll interval honoured when the SD primitive is
    /// auto-wrapped over a non-native-prefix-watch cache (PGR-D3). Ignored when a
    /// native `service_discovery` backend is bound.
    #[must_use]
    pub fn with_sd_poll_interval(mut self, interval: Duration) -> Self {
        self.sd_poll_interval = Some(interval);
        self
    }

    /// Binds a native leader-election backend, overriding the SDK default.
    #[must_use]
    pub fn with_leader_election(mut self, backend: Arc<dyn LeaderElectionBackend>) -> Self {
        self.leader_election = Some(backend);
        self
    }

    /// Binds a native distributed-lock backend, overriding the SDK default.
    #[must_use]
    pub fn with_lock(mut self, backend: Arc<dyn DistributedLockBackend>) -> Self {
        self.lock = Some(backend);
        self
    }

    /// Binds a native service-discovery backend, overriding the SDK default.
    #[must_use]
    pub fn with_service_discovery(mut self, backend: Arc<dyn ServiceDiscoveryBackend>) -> Self {
        self.service_discovery = Some(backend);
        self
    }
}

/// The four resolved backends for one profile, ready to register.
struct ResolvedProfile {
    name: String,
    cache: Arc<dyn ClusterCacheBackend>,
    leader_election: Arc<dyn LeaderElectionBackend>,
    lock: Arc<dyn DistributedLockBackend>,
    service_discovery: Arc<dyn ServiceDiscoveryBackend>,
}

/// Entry point for wiring the cluster gear.
pub struct ClusterWiring;

impl ClusterWiring {
    /// Returns a builder that registers backends into `hub`.
    ///
    /// `hub` is taken as a shared [`Arc`] (rather than a borrow) so the returned
    /// [`ClusterHandle`] can outlive the call and deregister at
    /// [`stop`](ClusterHandle::stop) time.
    pub fn builder(hub: Arc<ClientHub>) -> ClusterWiringBuilder {
        ClusterWiringBuilder {
            hub,
            profiles: Vec::new(),
            stop_hooks: Vec::new(),
        }
    }

    /// Builds the wiring from operator [`ClusterConfig`], instantiating each
    /// profile's cache backend through the matching provider in `providers` and
    /// letting the omit-default auto-wrap supply the other three primitives.
    ///
    /// Each provider's shutdown hook is owned by the returned [`ClusterHandle`]
    /// and awaited on [`stop`](ClusterHandle::stop).
    ///
    /// # Errors
    /// - [`ClusterError::InvalidConfig`] if a profile names an unregistered
    ///   provider for any primitive, or if a provider rejects its options.
    /// - Propagates [`ClusterError`] from provider construction, the SDK default
    ///   backends (consistency guard), and backend registration (invalid name).
    pub async fn from_config(
        hub: Arc<ClientHub>,
        config: &ClusterConfig,
        providers: &ProviderRegistry,
    ) -> Result<ClusterHandle, ClusterError> {
        let mut builder = Self::builder(hub);
        for (name, profile) in &config.profiles {
            tracing::debug!(profile = %name, "wiring cluster profile from config");
            let (cache, cache_stop) = build_cache_for_profile(name, profile, providers).await?;
            // Pushed immediately, so it matches the cache's actual start-order
            // position (first). `build_and_start` runs `stop_hooks` in reverse push
            // order, so pushing here — before the leader/lock/sd hooks below — means
            // the cache stops LAST, after every primitive layered on top of it for
            // this profile (true reverse-start order, DESIGN §3.7).
            builder = builder.on_stop(move || async move { cache_stop().await });

            let mut backends = ProfileBackends::new(Arc::clone(&cache));

            // Honour an operator-configured service-discovery poll cadence for
            // the omit-default auto-wrap (PGR-D3). The interval lives in the
            // cache provider's own options (e.g. the Postgres plugin's
            // `sd_poll_interval_ms`); read it generically here so a non-default
            // cadence reaches `with_prefix_watch_polling` instead of being
            // silently forced to the 5s default. A zero/absent value keeps the
            // default.
            if let Some(interval_ms) = profile
                .cache
                .options
                .get("sd_poll_interval_ms")
                .and_then(serde_json::Value::as_u64)
                .filter(|&ms| ms > 0)
            {
                backends = backends.with_sd_poll_interval(Duration::from_millis(interval_ms));
            }

            if let Some(binding) = &profile.leader_election {
                let provider = providers
                    .leader_election_provider(&binding.provider)
                    .ok_or_else(|| ClusterError::InvalidConfig {
                        reason: format!(
                            "profile `{name}`: unknown leader_election provider `{}`",
                            binding.provider
                        ),
                    })?;
                let (backend, stop) = provider.build_leader_election(&binding.options).await?;
                backends = backends.with_leader_election(backend);
                builder = builder.on_stop(move || async move { stop().await });
            }

            if let Some(binding) = &profile.lock {
                let provider = providers.lock_provider(&binding.provider).ok_or_else(|| {
                    ClusterError::InvalidConfig {
                        reason: format!(
                            "profile `{name}`: unknown lock provider `{}`",
                            binding.provider
                        ),
                    }
                })?;
                let (backend, stop) = provider.build_lock(&binding.options).await?;
                backends = backends.with_lock(backend);
                builder = builder.on_stop(move || async move { stop().await });
            }

            if let Some(binding) = &profile.service_discovery {
                let provider = providers
                    .service_discovery_provider(&binding.provider)
                    .ok_or_else(|| ClusterError::InvalidConfig {
                        reason: format!(
                            "profile `{name}`: unknown service_discovery provider `{}`",
                            binding.provider
                        ),
                    })?;
                let (backend, stop) = provider.build_service_discovery(&binding.options).await?;
                backends = backends.with_service_discovery(backend);
                builder = builder.on_stop(move || async move { stop().await });
            }

            builder = builder.profile_named(name.clone(), backends);
        }
        builder.build_and_start()
    }
}

async fn build_cache_for_profile(
    name: &str,
    profile: &ProfileConfig,
    providers: &ProviderRegistry,
) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
    let provider = providers
        .cache_provider(&profile.cache.provider)
        .ok_or_else(|| ClusterError::InvalidConfig {
            reason: format!(
                "profile `{name}`: unknown cache provider `{}`",
                profile.cache.provider
            ),
        })?;
    provider.build_cache(&profile.cache.options).await
}

/// A fluent builder collecting per-profile backend bindings and plugin shutdown
/// hooks. Finish with [`build_and_start`](Self::build_and_start).
#[must_use = "a wiring builder registers nothing until `.build_and_start()` is called"]
pub struct ClusterWiringBuilder {
    hub: Arc<ClientHub>,
    profiles: Vec<(String, ProfileBackends)>,
    stop_hooks: Vec<StopHook>,
}

impl ClusterWiringBuilder {
    /// Binds `backends` to the typed profile `P`. The marker is passed by value
    /// (mirroring the SDK resolver builders' `profile(marker)`); only
    /// [`ClusterProfile::NAME`] is read — the profile string is never re-typed at
    /// this call site.
    pub fn profile<P: ClusterProfile>(mut self, _marker: P, backends: ProfileBackends) -> Self {
        self.profiles.push((P::NAME.to_owned(), backends));
        self
    }

    /// Binds `backends` to a profile named at runtime — the config-driven path
    /// ([`ClusterWiring::from_config`]) where the profile name comes from operator
    /// YAML rather than a [`ClusterProfile`] marker. The name is validated against
    /// the cluster name rule during [`build_and_start`](Self::build_and_start).
    pub fn profile_named(mut self, name: impl Into<String>, backends: ProfileBackends) -> Self {
        self.profiles.push((name.into(), backends));
        self
    }

    /// Registers a shutdown action — typically a wired plugin handle's `stop()`
    /// future — run once during [`ClusterHandle::stop`] after backends are
    /// deregistered.
    pub fn on_stop<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.stop_hooks.push(Box::new(move || Box::pin(hook())));
        self
    }

    /// Resolves every profile's four backends (auto-filling unbound primitives
    /// with the SDK defaults) and registers them in the hub under
    /// `cluster:{profile}`.
    ///
    /// Resolution happens before any hub mutation, so a failure to build a
    /// default backend cannot leave a partially-registered hub.
    ///
    /// # Errors
    /// - [`ClusterError::InvalidConfig`] if a default leader-election or lock
    ///   backend is auto-filled over a non-linearizable cache (their consistency
    ///   guard).
    /// - [`ClusterError::InvalidName`] if a profile name violates the cluster
    ///   name rule.
    pub fn build_and_start(self) -> Result<ClusterHandle, ClusterError> {
        // Phase 1 — resolve all backends (fallible) before touching the hub.
        // Default leader-election, lock, and service-discovery backends the
        // wiring itself creates expose a shutdown-revoke seam; collect them so
        // `ClusterHandle::stop` can revoke in-flight coordination before shutdown
        // completes (DESIGN §3.13). Native (explicitly-bound) backends are not
        // revoked here — they manage shutdown through their own plugin stop hook.
        let mut resolved = Vec::with_capacity(self.profiles.len());
        let mut revokers: Vec<Arc<dyn ShutdownRevoke>> = Vec::new();
        for (name, backends) in self.profiles {
            resolved.push(resolve_profile_backends(name, backends, &mut revokers)?);
        }

        // Phase 2 — register every primitive under the profile scope. A failure
        // partway (e.g. a later profile with an invalid name) must not leave
        // earlier profiles half-registered, so roll back everything registered
        // so far before propagating the error — the hub stays all-or-nothing.
        let mut registered: Vec<String> = Vec::with_capacity(resolved.len());
        for profile in resolved {
            let name = register_profile_or_rollback(&self.hub, profile, &registered)?;
            registered.push(name);
        }

        Ok(ClusterHandle {
            hub: self.hub,
            registered,
            stop_hooks: self.stop_hooks,
            revokers,
            stopped: false,
        })
    }
}

/// Fills any primitive `backends` left unbound with its SDK default over
/// `backends.cache`, collecting each default's shutdown-revoke seam into
/// `revokers` (DESIGN §3.13). Explicitly-bound (native) primitives are passed
/// through untouched.
fn resolve_profile_backends(
    name: String,
    backends: ProfileBackends,
    revokers: &mut Vec<Arc<dyn ShutdownRevoke>>,
) -> Result<ResolvedProfile, ClusterError> {
    let cache = backends.cache;
    let leader_election: Arc<dyn LeaderElectionBackend> =
        if let Some(backend) = backends.leader_election {
            backend
        } else {
            let default = Arc::new(CasBasedLeaderElectionBackend::new(Arc::clone(&cache))?);
            revokers.push(Arc::clone(&default) as Arc<dyn ShutdownRevoke + Send + Sync>);
            default as Arc<dyn LeaderElectionBackend>
        };
    let lock: Arc<dyn DistributedLockBackend> = if let Some(backend) = backends.lock {
        backend
    } else {
        let default = Arc::new(CasBasedDistributedLockBackend::new(Arc::clone(&cache))?);
        revokers.push(Arc::clone(&default) as Arc<dyn ShutdownRevoke>);
        default as Arc<dyn DistributedLockBackend>
    };
    let service_discovery: Arc<dyn ServiceDiscoveryBackend> =
        if let Some(backend) = backends.service_discovery {
            backend
        } else {
            // Over a cache with no native prefix watch (`prefix_watch: false`,
            // e.g. the Postgres backend) the default SD backend drives its
            // topology watch through the `PollingPrefixWatch` polyfill. The
            // operator-configured cadence (e.g. the Postgres plugin's
            // `sd_poll_interval_ms`, surfaced through
            // [`ProfileBackends::with_sd_poll_interval`]) is threaded in here;
            // when unset it keeps `DEFAULT_PREFIX_WATCH_POLL` (5s) (PGR-D3).
            let mut sd = CacheBasedServiceDiscoveryBackend::new(Arc::clone(&cache));
            if let Some(interval) = backends.sd_poll_interval {
                sd = sd.with_prefix_watch_polling(interval);
            }
            let default = Arc::new(sd);
            revokers.push(Arc::clone(&default) as Arc<dyn ShutdownRevoke>);
            default as Arc<dyn ServiceDiscoveryBackend>
        };
    Ok(ResolvedProfile {
        name,
        cache,
        leader_election,
        lock,
        service_discovery,
    })
}

/// Registers `profile`'s four primitives in `hub`. On failure, deregisters
/// `profile` itself and every name in `registered` so the hub stays
/// all-or-nothing, logs a warning naming the failed profile and rollback
/// count, and returns the error. On success, logs registration and returns the
/// profile's name for the caller to add to `registered`.
fn register_profile_or_rollback(
    hub: &Arc<ClientHub>,
    profile: ResolvedProfile,
    registered: &[String],
) -> Result<String, ClusterError> {
    let result = (|| {
        register_cache_backend(hub, &profile.name, profile.cache)?;
        register_leader_election_backend(hub, &profile.name, profile.leader_election)?;
        register_lock_backend(hub, &profile.name, profile.lock)?;
        register_service_discovery_backend(hub, &profile.name, profile.service_discovery)
    })();
    let Err(err) = result else {
        tracing::info!(profile = %profile.name, "cluster profile registered");
        return Ok(profile.name);
    };
    tracing::warn!(
        profile = %profile.name,
        error = %err,
        rolled_back = registered.len(),
        "cluster profile registration failed; rolling back all registered profiles"
    );
    // Unwind the just-attempted profile and every prior one. Any primitive of
    // `profile.name` that did register is removed too; deregister of an
    // unregistered name is a harmless no-op.
    deregister_profile(hub, &profile.name);
    for name in registered {
        deregister_profile(hub, name);
    }
    Err(err)
}

/// The running cluster wiring. Backends are registered in the hub; consumers
/// resolve them with the SDK resolvers (e.g.
/// `ClusterCacheV1::resolver(handle.hub())`). Owns the wired plugins' shutdown.
pub struct ClusterHandle {
    hub: Arc<ClientHub>,
    registered: Vec<String>,
    stop_hooks: Vec<StopHook>,
    /// Shutdown-revoke seams for the wiring-created default leader-election,
    /// lock, and service-discovery backends, revoked first on
    /// [`stop`](ClusterHandle::stop).
    revokers: Vec<Arc<dyn ShutdownRevoke>>,
    /// Set by [`stop`](ClusterHandle::stop) so the [`Drop`] guard can tell a
    /// graceful shutdown apart from a forgotten one (ADR-006 §Confirmation).
    stopped: bool,
}

impl ClusterHandle {
    /// The hub the backends are registered in, for consumers to resolve against.
    #[must_use]
    pub fn hub(&self) -> &Arc<ClientHub> {
        &self.hub
    }

    /// The single shutdown entry point (DESIGN §3.7, §3.13).
    ///
    /// 1. **Revoke in-flight coordination first** (`cpt-cf-clst-fr-shutdown-revoke`):
    ///    every wiring-created default backend is revoked — an active leader
    ///    observes `Status(Lost)` then `Closed(Shutdown)`, an in-flight blocking
    ///    `lock()` waiter returns `Err(Shutdown)`, and an active service-discovery
    ///    watch observes `Closed(Shutdown)` — before this returns, so no consumer
    ///    can resume believing it still holds coordination state.
    /// 2. Deregister every registered backend — so later resolves report
    ///    [`ClusterError::ProfileNotBound`].
    /// 3. Run the plugin shutdown hooks in reverse-start order (DESIGN §3.7: last
    ///    started is stopped first). The standalone plugin's stop hook closes
    ///    active **cache** watches via the plugin's `StandaloneCache::shutdown`,
    ///    so a cache-watch consumer observes `Closed(Shutdown)` one phase after the
    ///    leader/lock/SD revocation — still within `stop()` (the chosen simplest
    ///    path; the slight ordering is intentional).
    ///
    /// No best-effort remote cleanup is attempted; TTL bounds any remaining
    /// cluster resources — held leader claims, locks, and service registrations
    /// all lapse via their backend TTL (`cpt-cf-clst-fr-shutdown-ttl-cleanup`).
    pub async fn stop(mut self) {
        tracing::info!(
            profiles = self.registered.len(),
            "stopping cluster wiring: revoking in-flight coordination"
        );
        for revoker in &self.revokers {
            revoker.revoke().await;
        }
        deregister_all(&self.hub, &self.registered);
        // `mem::take` rather than `into_iter` because `ClusterHandle` now owns a
        // `Drop` impl, and you cannot move a field out of a type that implements
        // `Drop`. Draining the hooks in place leaves an empty `Vec` behind.
        for hook in std::mem::take(&mut self.stop_hooks).into_iter().rev() {
            hook().await;
        }
        // Graceful shutdown completed — tell the `Drop` guard not to fire.
        self.stopped = true;
        tracing::info!("cluster wiring stopped");
    }
}

/// Deregisters every profile in `names`, logging each at `debug` (DESIGN §3.7).
fn deregister_all(hub: &Arc<ClientHub>, names: &[String]) {
    for name in names {
        tracing::debug!(profile = %name, "deregistering cluster profile");
        deregister_profile(hub, name);
    }
}

/// Diagnostic guard (ADR-006 §Confirmation): a [`ClusterHandle`] must be released
/// through [`stop`](ClusterHandle::stop). Dropping one without stopping leaks the
/// wired plugins' background tasks (cache TTL sweepers, leader-renewal loops), so
/// surface the bug loudly rather than silently — a debug-build panic, a
/// release-build warn-log. The [`std::thread::panicking`] guard skips the debug
/// panic during unwind so a forgotten handle dropped *while already panicking*
/// degrades to a warning instead of a double-panic process abort (ADR-002).
impl Drop for ClusterHandle {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if std::thread::panicking() {
            tracing::warn!(
                "ClusterHandle dropped during panic unwind without stop(); \
                 skipping debug panic to avoid double-panic abort"
            );
            return;
        }
        #[cfg(debug_assertions)]
        panic!("ClusterHandle dropped without stop() - programming error");
        #[cfg(not(debug_assertions))]
        tracing::warn!(
            "ClusterHandle dropped without stop() - programming error; \
             background tasks may leak"
        );
    }
}

/// Deregisters all four primitives bound under `cluster:{name}`. Deregistration
/// only fails on an invalid name, which cannot occur for a name that registered
/// successfully, and deregistering an unbound primitive is a harmless no-op — so
/// the presence reports are discarded.
fn deregister_profile(hub: &Arc<ClientHub>, name: &str) {
    deregister_cache_backend(hub, name).ok();
    deregister_leader_election_backend(hub, name).ok();
    deregister_lock_backend(hub, name).ok();
    deregister_service_discovery_backend(hub, name).ok();
}

#[cfg(test)]
#[path = "wiring_tests.rs"]
mod wiring_tests;
