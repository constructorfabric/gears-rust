//! The cluster wiring builder, per-profile backend bindings, and lifecycle
//! handle (DESIGN §3.7).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::{
    ClusterCacheBackend, ClusterError, ClusterProfile, DistributedLockBackend,
    LeaderElectionBackend, SD_POLL_INTERVAL_MS_OPTION, ServiceDiscoveryBackend, StopHook,
    deregister_cache_backend, deregister_leader_election_backend, deregister_lock_backend,
    deregister_service_discovery_backend, register_cache_backend, register_leader_election_backend,
    register_lock_backend, register_service_discovery_backend,
};

use crate::defaults::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend, ShutdownRevoke,
};
use toolkit::client_hub::ClientHub;

use crate::config::{BackendBinding, ClusterConfig, DEFAULT_PROVIDER, ProfileConfig};
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
    /// Whether an auto-filled [`CasBasedLeaderElectionBackend`] may be built over
    /// an eventually-consistent cache, via
    /// [`ProfileBackends::allow_weak_leader_election`]. Ignored when
    /// `leader_election` is explicitly bound to a native backend.
    allow_weak_leader_election: bool,
    /// Whether an auto-filled [`CasBasedDistributedLockBackend`] may be built over
    /// an eventually-consistent cache, via [`ProfileBackends::allow_weak_lock`].
    /// Ignored when `lock` is explicitly bound to a native backend.
    allow_weak_lock: bool,
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
            allow_weak_leader_election: false,
            allow_weak_lock: false,
        }
    }

    /// Permits the auto-filled leader-election default to be built over an
    /// eventually-consistent cache, routing it through
    /// [`CasBasedLeaderElectionBackend::new_allow_weak_consistency`] instead of the
    /// default-safe `new`.
    ///
    /// Takes no argument on purpose: calling it *is* the acknowledgement, mirroring
    /// the SDK constructor it selects, so there is no `false` to pass and no way to
    /// reach the weak path without naming it. Without this, a profile whose cache
    /// declares `EventuallyConsistent` fails
    /// [`build_and_start`](ClusterWiringBuilder::build_and_start) with
    /// `InvalidConfig` — which is the correct default (ADR-009), not a bug to work
    /// around.
    ///
    /// The config-driven equivalent is
    /// `leader_election: { provider: default, allow_weak_consistency: true }`.
    /// No-op when `leader_election` is bound to a native backend, which brings its
    /// own guarantees.
    #[must_use]
    pub fn allow_weak_leader_election(mut self) -> Self {
        self.allow_weak_leader_election = true;
        self
    }

    /// Permits the auto-filled lock default to be built over an
    /// eventually-consistent cache, routing it through
    /// [`CasBasedDistributedLockBackend::new_allow_weak_consistency`].
    ///
    /// The lock counterpart of
    /// [`allow_weak_leader_election`](Self::allow_weak_leader_election), and needed
    /// just as often: both CAS-based defaults share the same constructor guard, so a
    /// weak-cache profile that only permits the weak leader election still fails on
    /// the lock. The config-driven equivalent is
    /// `lock: { provider: default, allow_weak_consistency: true }`.
    ///
    /// A profile that can bind a *native* lock should prefer that — the Redis
    /// plugin's `SET NX PX` lease is a purpose-built primitive, whereas this flag
    /// only silences a guard over CAS on a cache that cannot support it.
    #[must_use]
    pub fn allow_weak_lock(mut self) -> Self {
        self.allow_weak_lock = true;
        self
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
        // Wiring is all-or-nothing, and on the failure path that means *shutting
        // down what already started* rather than merely not returning a handle.
        // Every `?` below can fire after one or more profiles have had real
        // backends built — a `build_cache_for_profile` opens a connection pool,
        // background tasks, and (for Redis) a second subscriber connection — and
        // those live behind stop hooks accumulated on `builder`. Dropping the
        // builder discards the hooks without running them, so a misconfigured
        // profile leaked every backend it managed to build before failing.
        //
        // Invisible for a long time, because the Postgres plugin's leak is a quiet
        // idle pool. The Redis plugin has an ADR-006 `Drop` guard, so the same
        // leak **panics in a debug build** — which is how this was found, by
        // `RD-SPEC-004`, whose whole subject is a profile that must fail startup
        // (DESIGN.md §3.13).
        let builder = match Self::wire_profiles(Self::builder(hub), config, providers).await {
            Ok(builder) => builder,
            Err((err, hooks)) => {
                run_stop_hooks_on_failed_wiring(hooks).await;
                return Err(err);
            }
        };
        match builder.build_and_start_returning_hooks() {
            Ok(handle) => Ok(handle),
            Err((err, hooks)) => {
                run_stop_hooks_on_failed_wiring(hooks).await;
                Err(err)
            }
        }
    }

    /// Wires every profile in `config` onto `builder`.
    ///
    /// Hands the accumulated [`StopHook`]s back alongside the error rather than
    /// letting them drop, so [`from_config`](Self::from_config) can shut down the
    /// backends earlier profiles already started. That is the only reason this is a
    /// separate function: the loop body is otherwise unchanged, and `?` inside it
    /// would discard exactly what the caller needs.
    async fn wire_profiles(
        mut builder: ClusterWiringBuilder,
        config: &ClusterConfig,
        providers: &ProviderRegistry,
    ) -> Result<ClusterWiringBuilder, (ClusterError, Vec<StopHook>)> {
        macro_rules! unwind_on_err {
            ($result:expr, $builder:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(err) => return Err((err, $builder.take_stop_hooks())),
                }
            };
        }

        for (name, profile) in &config.profiles {
            tracing::debug!(profile = %name, "wiring cluster profile from config");
            let (cache, cache_stop) = unwind_on_err!(
                build_cache_for_profile(name, profile, providers).await,
                builder
            );
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
            // `sd_poll_interval_ms`, cf. `SD_POLL_INTERVAL_MS_OPTION`); read it
            // generically here so a non-default cadence reaches
            // `with_prefix_watch_polling` instead of being silently forced to
            // the 5s default. A zero/absent value keeps the default.
            if let Some(interval_ms) = profile
                .cache
                .options
                .get(SD_POLL_INTERVAL_MS_OPTION)
                .and_then(serde_json::Value::as_u64)
                .filter(|&ms| ms > 0)
            {
                backends = backends.with_sd_poll_interval(Duration::from_millis(interval_ms));
            }

            // The three optional primitives, each either natively bound through
            // its own provider registry or riding the omit-default auto-wrap. One
            // helper apiece rather than three near-identical blocks inline: they
            // differ only in which registry they consult and which `with_*` setter
            // they call, and inlining all three put `from_config` over the
            // cognitive-complexity budget through sheer repetition.
            //
            // Each appends its plugin stop hook to `hooks` rather than pushing
            // straight onto the builder, so hook order stays visibly
            // cache-then-primitives here — `build_and_start` unwinds in reverse
            // push order, which is what makes the cache stop LAST (DESIGN §3.7).
            // Each binding appends its own plugin hook here, and a failure in a
            // later binding must unwind the earlier ones too — `bind_lock` failing
            // after `bind_leader_election` started a native leader backend has to
            // stop it. So the local hooks are handed to the builder *before* the
            // error is returned, which is what `take_stop_hooks` then collects.
            let mut hooks: Vec<StopHook> = Vec::new();
            let bound = async {
                let backends =
                    bind_leader_election(name, profile, providers, backends, &mut hooks).await?;
                let backends = bind_lock(name, profile, providers, backends, &mut hooks).await?;
                bind_service_discovery(name, profile, providers, backends, &mut hooks).await
            }
            .await;
            for stop in hooks {
                builder = builder.on_stop(move || async move { stop().await });
            }
            backends = unwind_on_err!(bound, builder);

            builder = builder.profile_named(name.clone(), backends);
        }
        Ok(builder)
    }
}

/// Runs the stop hooks of a wiring that failed partway, newest first.
///
/// Reverse push order, the same order [`ClusterHandle::stop`] uses, so a
/// primitive layered on a cache is stopped before the cache it rides. Failures
/// are not propagated: the caller already has an error to report, and it is the
/// *configuration* error an operator needs to see rather than whatever a
/// best-effort teardown then said.
async fn run_stop_hooks_on_failed_wiring(hooks: Vec<StopHook>) {
    if hooks.is_empty() {
        return;
    }
    tracing::debug!(
        hooks = hooks.len(),
        "cluster wiring failed after starting backends; stopping them before reporting"
    );
    for hook in hooks.into_iter().rev() {
        hook().await;
    }
}

/// Binds a profile's `leader_election`, honouring the reserved
/// [`DEFAULT_PROVIDER`] sentinel.
///
/// The sentinel is checked *before* the registry lookup, so the reserved name can
/// never be shadowed by a plugin that registers a provider called `default`. It
/// contributes no backend and no stop hook — [`resolve_profile_backends`] still
/// supplies the SDK default and owns its shutdown-revoke seam; all the binding does
/// is carry the operator's `allow_weak_consistency` acknowledgement to it.
async fn bind_leader_election(
    name: &str,
    profile: &ProfileConfig,
    providers: &ProviderRegistry,
    mut backends: ProfileBackends,
    hooks: &mut Vec<StopHook>,
) -> Result<ProfileBackends, ClusterError> {
    let Some(binding) = &profile.leader_election else {
        return Ok(backends);
    };
    if binding.provider == DEFAULT_PROVIDER {
        if guarded_default_options(name, "leader_election", binding)?.allow_weak_consistency {
            backends = backends.allow_weak_leader_election();
        }
        return Ok(backends);
    }
    let provider = providers
        .leader_election_provider(&binding.provider)
        .ok_or_else(|| ClusterError::InvalidConfig {
            reason: format!(
                "profile `{name}`: unknown leader_election provider `{}`",
                binding.provider
            ),
        })?;
    let (backend, stop) = provider.build_leader_election(&binding.options).await?;
    hooks.push(stop);
    Ok(backends.with_leader_election(backend))
}

/// Binds a profile's `lock`, honouring the reserved [`DEFAULT_PROVIDER`] sentinel.
/// See [`bind_leader_election`] for how the sentinel is handled.
async fn bind_lock(
    name: &str,
    profile: &ProfileConfig,
    providers: &ProviderRegistry,
    mut backends: ProfileBackends,
    hooks: &mut Vec<StopHook>,
) -> Result<ProfileBackends, ClusterError> {
    let Some(binding) = &profile.lock else {
        return Ok(backends);
    };
    if binding.provider == DEFAULT_PROVIDER {
        if guarded_default_options(name, "lock", binding)?.allow_weak_consistency {
            backends = backends.allow_weak_lock();
        }
        return Ok(backends);
    }
    let provider =
        providers
            .lock_provider(&binding.provider)
            .ok_or_else(|| ClusterError::InvalidConfig {
                reason: format!(
                    "profile `{name}`: unknown lock provider `{}`",
                    binding.provider
                ),
            })?;
    let (backend, stop) = provider.build_lock(&binding.options).await?;
    hooks.push(stop);
    Ok(backends.with_lock(backend))
}

/// Binds a profile's `service_discovery`.
///
/// [`DEFAULT_PROVIDER`] is accepted here for symmetry — the sentinel means the same
/// thing for all three optional primitives — but carries no options, because
/// [`CacheBasedServiceDiscoveryBackend`] has no consistency guard to waive.
async fn bind_service_discovery(
    name: &str,
    profile: &ProfileConfig,
    providers: &ProviderRegistry,
    backends: ProfileBackends,
    hooks: &mut Vec<StopHook>,
) -> Result<ProfileBackends, ClusterError> {
    let Some(binding) = &profile.service_discovery else {
        return Ok(backends);
    };
    if binding.provider == DEFAULT_PROVIDER {
        reject_options_on_unguarded_default(name, "service_discovery", binding)?;
        return Ok(backends);
    }
    let provider = providers
        .service_discovery_provider(&binding.provider)
        .ok_or_else(|| ClusterError::InvalidConfig {
            reason: format!(
                "profile `{name}`: unknown service_discovery provider `{}`",
                binding.provider
            ),
        })?;
    let (backend, stop) = provider.build_service_discovery(&binding.options).await?;
    hooks.push(stop);
    Ok(backends.with_service_discovery(backend))
}

/// The options a `provider: `[`DEFAULT_PROVIDER`] binding may carry, for a primitive
/// whose SDK default backend has a consistency guard (`leader_election` and `lock`).
///
/// Lives here rather than in [`crate::config`] because it is a wiring-internal
/// parsing detail: unlike every other binding option, these keys never reach a
/// provider, so nothing outside this module ever sees the parsed form. The
/// operator-facing half — what the flag means and where it is accepted — is
/// documented on [`DEFAULT_PROVIDER`].
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultBindingOptions {
    /// Route this primitive's SDK default through `new_allow_weak_consistency`
    /// instead of the default-safe `new`, accepting that an eventually-consistent
    /// cache may produce split-brain (dual leaders / dual lock holders) under
    /// partition (ADR-009). Default `false`.
    #[serde(default)]
    allow_weak_consistency: bool,
}

/// Parses a `provider: `[`DEFAULT_PROVIDER`] binding's options for one of the two
/// primitives whose SDK default carries a consistency guard.
///
/// `deny_unknown_fields` on [`DefaultBindingOptions`] does the work: these options
/// reach no provider, so this is the only layer that can catch an operator's typo,
/// and a silently-ignored `allow_weak_consistancy` would leave the profile failing
/// startup with an error that says nothing about the misspelling.
fn guarded_default_options(
    profile: &str,
    primitive: &str,
    binding: &BackendBinding,
) -> Result<DefaultBindingOptions, ClusterError> {
    serde_json::from_value(serde_json::Value::Object(binding.options.clone())).map_err(|err| {
        ClusterError::InvalidConfig {
            reason: format!(
                "profile `{profile}`: invalid options on `{primitive}: {{ provider: \
                 {DEFAULT_PROVIDER} }}`: {err}. The only option this binding accepts is \
                 `allow_weak_consistency: <bool>`"
            ),
        }
    })
}

/// Rejects any option on a `provider: `[`DEFAULT_PROVIDER`] binding for a primitive
/// whose SDK default has no consistency guard.
///
/// Hand-rolled rather than another `deny_unknown_fields` struct so the error can say
/// *why* `allow_weak_consistency` is refused here specifically — serde's bare
/// "unknown field" would read as a typo and send the operator looking for the right
/// spelling of something that has no meaning at this binding at all.
fn reject_options_on_unguarded_default(
    profile: &str,
    primitive: &str,
    binding: &BackendBinding,
) -> Result<(), ClusterError> {
    if binding.options.is_empty() {
        return Ok(());
    }
    let keys: Vec<&str> = binding.options.keys().map(String::as_str).collect();
    Err(ClusterError::InvalidConfig {
        reason: format!(
            "profile `{profile}`: `{primitive}: {{ provider: {DEFAULT_PROVIDER} }}` accepts no \
             options, but got {keys:?}. The SDK default service-discovery backend has no \
             consistency guard, so `allow_weak_consistency` is meaningless here — it belongs on \
             the `leader_election` and `lock` bindings, whose CAS-based defaults do have one"
        ),
    })
}

async fn build_cache_for_profile(
    name: &str,
    profile: &ProfileConfig,
    providers: &ProviderRegistry,
) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
    // The cache is the anchor every SDK default wraps, so there is no "default
    // cache" for the reserved name to resolve to. Caught here rather than left to
    // the registry lookup, which would answer the misleading "unknown cache
    // provider `default`" — misleading because `default` *is* a name the wiring
    // knows, just not one this binding can use.
    if profile.cache.provider == DEFAULT_PROVIDER {
        return Err(ClusterError::InvalidConfig {
            reason: format!(
                "profile `{name}`: `cache: {{ provider: {DEFAULT_PROVIDER} }}` is not valid — \
                 `{DEFAULT_PROVIDER}` selects the SDK default backend *over* a profile's cache, \
                 so the cache itself must name a real provider (e.g. `standalone`, `postgres`, \
                 `redis`)"
            ),
        });
    }
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

    /// Removes and returns every stop hook registered so far.
    ///
    /// Exists for one caller: [`ClusterWiring::from_config`]'s failure path, which
    /// has to shut down the backends already-wired profiles started rather than
    /// drop their hooks unrun (see the note there). Deliberately **not** public —
    /// a builder that has had its hooks taken will not stop what it started, which
    /// is only safe when the caller is about to run them itself.
    fn take_stop_hooks(&mut self) -> Vec<StopHook> {
        std::mem::take(&mut self.stop_hooks)
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
        self.build_and_start_returning_hooks()
            .map_err(|(err, hooks)| {
                // A direct builder caller registered these hooks itself and still
                // owns whatever they close over, so dropping them here preserves
                // the pre-existing contract. `from_config` — which built those
                // backends on the caller's behalf and is the only place that
                // *can* shut them down — uses the hook-returning form instead.
                drop(hooks);
                err
            })
    }

    /// [`build_and_start`](Self::build_and_start), handing the accumulated stop
    /// hooks back on failure instead of dropping them.
    ///
    /// The distinction matters only for [`ClusterWiring::from_config`], which
    /// starts every backend itself: a resolution failure there leaves real pools,
    /// tasks, and connections running behind these hooks, and the Redis plugin's
    /// ADR-006 `Drop` guard turns leaking them into a debug-build panic
    /// (DESIGN.md §3.13).
    ///
    /// # Errors
    /// Exactly [`build_and_start`](Self::build_and_start)'s errors, paired with the
    /// hooks the caller must now run.
    fn build_and_start_returning_hooks(
        mut self,
    ) -> Result<ClusterHandle, (ClusterError, Vec<StopHook>)> {
        macro_rules! unwind_on_err {
            ($result:expr, $self:expr) => {
                match $result {
                    Ok(value) => value,
                    Err(err) => return Err((err, $self.take_stop_hooks())),
                }
            };
        }

        // Phase 1 — resolve all backends (fallible) before touching the hub.
        // Default leader-election, lock, and service-discovery backends the
        // wiring itself creates expose a shutdown-revoke seam; collect them so
        // `ClusterHandle::stop` can revoke in-flight coordination before shutdown
        // completes (DESIGN §3.13). Native (explicitly-bound) backends are not
        // revoked here — they manage shutdown through their own plugin stop hook.
        let profiles = std::mem::take(&mut self.profiles);
        let mut resolved = Vec::with_capacity(profiles.len());
        let mut revokers: Vec<Arc<dyn ShutdownRevoke>> = Vec::new();
        for (name, backends) in profiles {
            resolved.push(unwind_on_err!(
                resolve_profile_backends(name, backends, &mut revokers),
                self
            ));
        }

        // Phase 2 — register every primitive under the profile scope. A failure
        // partway (e.g. a later profile with an invalid name) must not leave
        // earlier profiles half-registered, so roll back everything registered
        // so far before propagating the error — the hub stays all-or-nothing.
        let mut registered: Vec<String> = Vec::with_capacity(resolved.len());
        for profile in resolved {
            let name = unwind_on_err!(
                register_profile_or_rollback(&self.hub, profile, &registered),
                self
            );
            registered.push(name);
        }

        let stop_hooks = self.take_stop_hooks();
        Ok(ClusterHandle {
            hub: self.hub,
            registered,
            stop_hooks,
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
            // Both arms build the same backend; they differ only in whether the
            // consistency guard is enforced or explicitly waived
            // (`ProfileBackends::allow_weak_leader_election`, reached from YAML as
            // `leader_election: { provider: default, allow_weak_consistency: true }`).
            // The weak constructor is infallible and logs its own split-brain
            // warning, so the `?` lives on the guarded arm alone.
            let default = Arc::new(if backends.allow_weak_leader_election {
                CasBasedLeaderElectionBackend::new_allow_weak_consistency(Arc::clone(&cache))
            } else {
                CasBasedLeaderElectionBackend::new(Arc::clone(&cache))?
            });
            revokers.push(Arc::clone(&default) as Arc<dyn ShutdownRevoke + Send + Sync>);
            default as Arc<dyn LeaderElectionBackend>
        };
    let lock: Arc<dyn DistributedLockBackend> = if let Some(backend) = backends.lock {
        backend
    } else {
        // The lock default shares the leader default's constructor guard, so a
        // weak-cache profile has to waive both or neither — waiving only the
        // leader one moves the startup failure four lines down rather than
        // resolving it.
        let default = Arc::new(if backends.allow_weak_lock {
            CasBasedDistributedLockBackend::new_allow_weak_consistency(Arc::clone(&cache))
        } else {
            CasBasedDistributedLockBackend::new(Arc::clone(&cache))?
        });
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
