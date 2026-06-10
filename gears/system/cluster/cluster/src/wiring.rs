// Created: 2026-06-11 by Constructor Tech
//! The cluster wiring builder, per-profile backend bindings, and lifecycle
//! handle (DESIGN §3.7).

use std::future::Future;
use std::sync::Arc;

use cluster_sdk::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend, ClusterCacheBackend, ClusterError, ClusterProfile,
    DistributedLockBackend, LeaderElectionBackend, ServiceDiscoveryBackend, ShutdownRevoke,
    StopHook, deregister_cache_backend, deregister_leader_election_backend,
    deregister_lock_backend, deregister_service_discovery_backend, register_cache_backend,
    register_leader_election_backend, register_lock_backend, register_service_discovery_backend,
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
        }
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
    /// - [`ClusterError::InvalidConfig`] if a profile names an unregistered cache
    ///   `provider`, if it binds an explicit non-cache primitive (not yet
    ///   supported — omit it to use the SDK default over the cache), or if a
    ///   provider rejects its options.
    /// - Propagates [`ClusterError`] from provider construction, the SDK default
    ///   backends (consistency guard), and backend registration (invalid name).
    pub fn from_config(
        hub: Arc<ClientHub>,
        config: &ClusterConfig,
        providers: &ProviderRegistry,
    ) -> Result<ClusterHandle, ClusterError> {
        let mut builder = Self::builder(hub);
        for (name, profile) in &config.profiles {
            reject_unsupported_native_bindings(name, profile)?;
            let provider = providers
                .cache_provider(&profile.cache.provider)
                .ok_or_else(|| ClusterError::InvalidConfig {
                    reason: format!(
                        "profile `{name}`: unknown cache provider `{}`",
                        profile.cache.provider
                    ),
                })?;
            let (cache, stop) = provider.build_cache(&profile.cache.options)?;
            builder = builder
                .profile_named(name.clone(), ProfileBackends::new(cache))
                .on_stop(move || async move { stop().await });
        }
        builder.build_and_start()
    }
}

/// Rejects explicit non-cache bindings, which no provider can yet construct
/// natively. An operator must omit `leader_election` / `lock` /
/// `service_discovery` to get the SDK default over the cache; binding them
/// explicitly today would be silently ignored otherwise, so fail loudly instead.
fn reject_unsupported_native_bindings(
    name: &str,
    profile: &ProfileConfig,
) -> Result<(), ClusterError> {
    let explicit = [
        ("leader_election", profile.leader_election.is_some()),
        ("lock", profile.lock.is_some()),
        ("service_discovery", profile.service_discovery.is_some()),
    ]
    .into_iter()
    .find_map(|(primitive, bound)| bound.then_some(primitive));
    if let Some(primitive) = explicit {
        return Err(ClusterError::InvalidConfig {
            reason: format!(
                "profile `{name}`: explicit `{primitive}` binding is not yet supported; \
                 omit it to use the SDK default backend over the cache"
            ),
        });
    }
    Ok(())
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
        let mut resolved = Vec::with_capacity(self.profiles.len());
        // Default leader-election, lock, and service-discovery backends the
        // wiring itself creates expose a shutdown-revoke seam; collect them so
        // `ClusterHandle::stop` can revoke in-flight coordination before shutdown
        // completes (DESIGN §3.13). Native (explicitly-bound) backends are not
        // revoked here — they manage shutdown through their own plugin stop hook.
        let mut revokers: Vec<Arc<dyn ShutdownRevoke>> = Vec::new();
        for (name, backends) in self.profiles {
            // @cpt-begin:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-cache
            let cache = backends.cache;
            // @cpt-end:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-cache
            // @cpt-begin:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-wrap
            let leader_election: Arc<dyn LeaderElectionBackend> =
                if let Some(backend) = backends.leader_election {
                    backend
                } else {
                    // @cpt-begin:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-guard
                    // @cpt-begin:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-reject
                    let default = Arc::new(CasBasedLeaderElectionBackend::new(Arc::clone(&cache))?);
                    // @cpt-end:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-reject
                    // @cpt-end:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-guard
                    revokers.push(Arc::clone(&default) as Arc<dyn ShutdownRevoke>);
                    default as Arc<dyn LeaderElectionBackend>
                };
            let lock: Arc<dyn DistributedLockBackend> = if let Some(backend) = backends.lock {
                backend
            } else {
                let default = Arc::new(CasBasedDistributedLockBackend::new(Arc::clone(&cache))?);
                revokers.push(Arc::clone(&default) as Arc<dyn ShutdownRevoke>);
                default as Arc<dyn DistributedLockBackend>
            };
            let service_discovery: Arc<dyn ServiceDiscoveryBackend> = if let Some(backend) =
                backends.service_discovery
            {
                backend
            } else {
                let default = Arc::new(CacheBasedServiceDiscoveryBackend::new(Arc::clone(&cache)));
                revokers.push(Arc::clone(&default) as Arc<dyn ShutdownRevoke>);
                default as Arc<dyn ServiceDiscoveryBackend>
            };
            // @cpt-end:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-wrap
            // @cpt-begin:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-return
            resolved.push(ResolvedProfile {
                name,
                cache,
                leader_election,
                lock,
                service_discovery,
            });
            // @cpt-end:cpt-cf-clst-flow-sdk-default-backends-cache-only:p1:inst-co-return
        }

        // Phase 2 — register every primitive under the profile scope. A failure
        // partway (e.g. a later profile with an invalid name) must not leave
        // earlier profiles half-registered, so roll back everything registered
        // so far before propagating the error — the hub stays all-or-nothing.
        let mut registered: Vec<String> = Vec::with_capacity(resolved.len());
        for profile in resolved {
            let result = (|| {
                register_cache_backend(&self.hub, &profile.name, profile.cache)?;
                register_leader_election_backend(
                    &self.hub,
                    &profile.name,
                    profile.leader_election,
                )?;
                register_lock_backend(&self.hub, &profile.name, profile.lock)?;
                register_service_discovery_backend(
                    &self.hub,
                    &profile.name,
                    profile.service_discovery,
                )
            })();
            if let Err(err) = result {
                // Unwind the just-attempted profile and every prior one. Any
                // primitive of `profile.name` that did register is removed too;
                // deregister of an unregistered name is a harmless no-op.
                deregister_profile(&self.hub, &profile.name);
                for name in &registered {
                    deregister_profile(&self.hub, name);
                }
                return Err(err);
            }
            registered.push(profile.name);
        }

        Ok(ClusterHandle {
            hub: self.hub,
            registered,
            stop_hooks: self.stop_hooks,
            revokers,
        })
    }
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
    pub async fn stop(self) {
        for revoker in &self.revokers {
            revoker.revoke().await;
        }
        for name in &self.registered {
            deregister_profile(&self.hub, name);
        }
        for hook in self.stop_hooks.into_iter().rev() {
            hook().await;
        }
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
