//! Runtime profile state — the bound-profile set the wiring produces
//! (DESIGN.md).
//!
//! Today a profile is transient: [`ClusterWiring::from_config`](crate::ClusterWiring::from_config) reads it, builds
//! the primitive `Arc`s, registers them in the hub and forgets everything else.
//! The hub is a type-keyed map — it can answer "give me the cache backend for
//! `cluster:event-broker`" but not "what profiles exist", "which provider serves
//! this one" or "is this instance shared". Serving remote clients needs all of
//! it, so the wiring now also returns a [`BoundProfile`] per profile: the same
//! three backend `Arc`s the hub holds, plus the two things the hub cannot
//! answer.
//!
//! - Its [`ProfileDescriptor`] — provider identity and declared characteristics,
//!   which lets a remote client answer `consistency()` / `features()` /
//!   `provider_name()` synchronously (§5.5).
//! - Which backend *instances* it is built from ([`ProfileInstanceRefs`], §5.3).
//!
//! [`ProfileRegistry`] is the index that set is published into: created empty in
//! the gear's `init`, populated by its `start`, and read on every request by both
//! `LocalClusterClient` and the wire services. One profile-to-backend dispatch
//! mechanism serves both deployment profiles, so it is load-bearing in
//! an embedded process and not only behind a transport (§5.2).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use arc_swap::ArcSwap;
use cluster_sdk::{
    ClusterCacheBackend, ClusterError, DistributedLockBackend, LeaderElectionBackend,
    ProfileDescriptor, ProfileHealth, intern, intern_existing,
};

/// The identity of one backend instance, so two profiles built from the same
/// instance are observably sharing it (§5.3).
///
/// The identity is the instance's address. It is stable for as long as the
/// instance lives, and a [`BoundProfile`] holds a strong `Arc` to every instance
/// it names, so an id cannot go stale while it is reachable. The address is only
/// ever compared and printed — never dereferenced.
///
/// Nothing deduplicates instances yet: each profile's bindings are built for
/// that profile alone, so distinct profiles report distinct ids even when they
/// name the same DSN. The `BackendInstanceCache` (§5.3) is what makes two
/// profiles on one DSN report **one** id here, and it needs no change to this
/// type to do it — which is the point of recording identity rather than
/// configuration here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceId(usize);

impl InstanceId {
    /// The identity of the instance behind `backend`.
    ///
    /// Generic over the trait object so one implementation serves all three
    /// primitives; the pointer is cast to a thin one, which discards the vtable
    /// and leaves the instance address that two `Arc`s to the same instance
    /// share.
    #[must_use]
    pub fn of<T: ?Sized>(backend: &Arc<T>) -> Self {
        Self(Arc::as_ptr(backend).cast::<()>().addr())
    }
}

/// Which backend instance serves each primitive of one profile (§5.3).
///
/// Equality between two profiles' ids for a primitive means they are served by
/// one instance — one pool, one reaper, one `StopHook`. Within a single profile
/// the ids normally differ: an auto-filled SDK default is a distinct instance
/// layered *over* the profile's cache instance, not the cache instance itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileInstanceRefs {
    /// The instance serving this profile's cache.
    pub cache: InstanceId,
    /// The instance serving this profile's leader election — the SDK default
    /// over `cache` unless a native backend was bound.
    pub leader_election: InstanceId,
    /// The instance serving this profile's distributed lock — the SDK default
    /// over `cache` unless a native backend was bound.
    pub lock: InstanceId,
}

/// One wired profile, as the wiring resolved it (§5.2).
///
/// The three `Arc`s are the *real* backends, the same ones registered in the hub
/// under `cluster:{name}` — no wrapper is interposed, so dispatching through
/// this set costs a profile lookup and nothing else (invariant I14).
///
/// The set is returned by [`ClusterWiring::from_config`](crate::ClusterWiring::from_config) and holds strong `Arc`s
/// to every instance a profile is built from, which keeps those
/// instances alive for as long as a profile references them (§5.3).
pub struct BoundProfile {
    /// The profile name, as registered in the hub scope `cluster:{name}`.
    pub name: String,
    /// The profile's cache backend.
    pub cache: Arc<dyn ClusterCacheBackend>,
    /// The profile's leader-election backend, native or the SDK default over
    /// `cache`.
    pub leader_election: Arc<dyn LeaderElectionBackend>,
    /// The profile's distributed-lock backend, native or the SDK default over
    /// `cache`.
    pub lock: Arc<dyn DistributedLockBackend>,
    /// Provider identity plus declared consistency and features per primitive,
    /// computed at wiring time from the real backends (§5.5).
    ///
    /// **Its `health` is the wiring-time value and must not be served**: health
    /// changes after wiring, so read [`descriptor`](Self::descriptor) instead,
    /// which overlays the live state. The field is named for what it is so the
    /// distinction cannot be missed at a call site.
    pub wired_descriptor: ProfileDescriptor,
    /// Which backend instances this profile is built from (§5.3).
    pub instances: ProfileInstanceRefs,
    /// The profile's live health, republished by the composite readiness
    /// healthcheck after every probe round (§4.4).
    ///
    /// It is a cell here rather than a value inside the published snapshot for
    /// one reason: **health must move without bumping the registry
    /// `generation`.** A client reads `generation` to detect that the profile
    /// *set* changed under it (§5.6), so routing a flapping backend through
    /// [`ProfileRegistry::publish`] would look like continuous reconfiguration
    /// and invalidate every cached descriptor on every flap. It is also why a
    /// client re-reads descriptors on a short unconditional poll rather than only
    /// on a generation change (§5.5).
    ///
    /// Stored as a byte because [`ProfileHealth`] is not itself atomic; the codec
    /// is local to this module and deliberately unrelated to the wire numbering.
    health: AtomicU8,
}

/// [`BoundProfile::health`]'s encoding. Local to this module and **not** the
/// wire's numbering — the proto enum reserves 0 for `_UNSPECIFIED`, a question
/// that does not arise for a value this process wrote itself.
const HEALTH_SERVING: u8 = 0;
/// See [`HEALTH_SERVING`].
const HEALTH_DEGRADED: u8 = 1;

impl BoundProfile {
    /// Assembles a bound profile, seeding its live health from the health
    /// `descriptor` was wired with.
    ///
    /// A constructor rather than a struct literal because
    /// [`health`](Self::health) is private: the seed has exactly one correct
    /// source (the wiring's verdict) and nothing else may set it.
    #[must_use]
    pub fn new(
        name: String,
        cache: Arc<dyn ClusterCacheBackend>,
        leader_election: Arc<dyn LeaderElectionBackend>,
        lock: Arc<dyn DistributedLockBackend>,
        wired_descriptor: ProfileDescriptor,
        instances: ProfileInstanceRefs,
    ) -> Self {
        let health = AtomicU8::new(encode_health(wired_descriptor.health));
        Self {
            name,
            cache,
            leader_election,
            lock,
            wired_descriptor,
            instances,
            health,
        }
    }

    /// The profile's current health.
    #[must_use]
    pub fn health(&self) -> ProfileHealth {
        match self.health.load(Ordering::Relaxed) {
            HEALTH_SERVING => ProfileHealth::Serving,
            // Fail safe on a byte this module did not write: an unknown health
            // pulls consumers out of rotation rather than keeping them in, the
            // same reading the wire gives an unspecified value.
            _ => ProfileHealth::Degraded,
        }
    }

    /// Records the profile's health, returning the previous value so a caller can
    /// log only the transitions.
    ///
    /// `Relaxed` on both sides is sufficient: this is a single independent flag
    /// with no ordering relationship to other memory, read by descriptor serving
    /// and written by the readiness healthcheck, and a reader that observes the
    /// previous value simply reports one poll's worth of stale health — which is
    /// the accuracy the 10 s client-side refresh already assumes (§4.4).
    pub fn set_health(&self, health: ProfileHealth) -> ProfileHealth {
        let previous = self.health.swap(encode_health(health), Ordering::Relaxed);
        match previous {
            HEALTH_SERVING => ProfileHealth::Serving,
            _ => ProfileHealth::Degraded,
        }
    }

    /// The descriptor to serve a client: what wiring resolved, with the live
    /// health overlaid (§5.5).
    ///
    /// This is the only correct way to answer `DescribeProfiles`;
    /// [`wired_descriptor`](Self::wired_descriptor) carries a health value frozen
    /// at wiring time.
    #[must_use]
    pub fn descriptor(&self) -> ProfileDescriptor {
        ProfileDescriptor {
            health: self.health(),
            ..self.wired_descriptor.clone()
        }
    }
}

/// Encodes `health` for [`BoundProfile::health`]. Matched exhaustively so a new
/// [`ProfileHealth`] variant fails the build here rather than silently encoding
/// as degraded.
fn encode_health(health: ProfileHealth) -> u8 {
    match health {
        ProfileHealth::Serving => HEALTH_SERVING,
        ProfileHealth::Degraded => HEALTH_DEGRADED,
    }
}

impl fmt::Debug for BoundProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The three backends are trait objects and so cannot be `Debug`. Name
        // the concrete backend behind each instead, which a diagnostic
        // reader wants from them anyway.
        f.debug_struct("BoundProfile")
            .field("name", &self.name)
            .field("cache", &self.cache.provider_name())
            .field("leader_election", &self.leader_election.provider_name())
            .field("lock", &self.lock.provider_name())
            // The live descriptor, not the wired one: a reader debugging a
            // degraded profile wants the health it is actually serving.
            .field("descriptor", &self.descriptor())
            .field("instances", &self.instances)
            // Non-exhaustive because every field is rendered through a derived
            // view rather than raw: the three backends as their provider names,
            // and `wired_descriptor` + `health` as the one live descriptor they
            // combine into. Listing the raw fields as well would print each twice.
            .finish_non_exhaustive()
    }
}

/// The name reported for a profile that has never been bound in this process.
///
/// [`ClusterError::ProfileNotBound`] carries a `&'static str` and the error model
/// is frozen (invariant I3), so a name is reportable only if it has been interned
/// — and interning is bounded to names that were actually registered (see
/// [`ProfileRegistry::publish`]). A name that was never registered is therefore
/// not echoed into the typed error; the request that carried it names it in the
/// span and the log instead, which is the same cardinality split invariant I15
/// draws for lock and election names.
const UNKNOWN_PROFILE: &str = "<unknown>";

/// One immutable view of the bound profile set.
///
/// Handed out whole by [`ProfileRegistry::snapshot`] so a caller that needs to
/// enumerate profiles — `DescribeProfiles`, the admin route, the readiness
/// aggregate — reads a consistent set rather than several racing lookups, and
/// can report the `generation` it read.
pub struct RegistrySnapshot {
    /// Incremented on every swap, so a client can detect that the server's
    /// profile set changed under it (§5.6).
    pub generation: u64,
    /// The bound profiles, keyed by their **interned** names.
    ///
    /// The key is `&'static str` rather than `String` because that is what
    /// [`ClusterError::ProfileNotBound`] needs and the map is already the
    /// bounded, config-derived set of names the interning rule permits. A hit
    /// therefore costs no interning at all, and lookups still take a plain
    /// `&str`.
    pub profiles: BTreeMap<&'static str, Arc<BoundProfile>>,
}

/// Runtime, queryable view of every bound profile (§5.2).
///
/// The hub answers "give me the cache backend for `cluster:event-broker`"; this
/// answers "what profiles exist", "which provider serves this one" and "give me
/// the profile named in this request". It replaces nothing: hub registrations
/// stay, because the gear's own SDK-default backends resolve through them, and
/// this is the additional index the wire needs.
///
/// Reads are one [`ArcSwap::load`] and one `BTreeMap` lookup — no lock on the
/// request path, which keeps a 10k ops/s path inside a 5 ms budget — and
/// what comes back is the *real* backend `Arc`, with no wrapper interposed
/// (invariant I14).
pub struct ProfileRegistry {
    inner: ArcSwap<RegistrySnapshot>,
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileRegistry {
    /// An empty registry at generation 0, as created in the gear's `init`.
    ///
    /// The gear's services are collected before its `start` runs, so they capture
    /// this rather than a backend (§4.2's lifecycle constraint). Every request
    /// arriving before `publish` therefore resolves to
    /// [`ClusterError::ProfileNotBound`], which is the correct answer and needs no
    /// new error variant (invariant I3).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(RegistrySnapshot {
                generation: 0,
                profiles: BTreeMap::new(),
            }),
        }
    }

    /// Publishes `bound` as the current profile set, at the next generation.
    ///
    /// Called by the gear's `start` once the wiring returns (§4.11 item 7b). The
    /// swap is atomic: a request either sees the whole previous set or the whole
    /// new one.
    ///
    /// **Profile names are interned here**, which lets
    /// [`ClusterError::ProfileNotBound`] name a profile arriving in a request
    /// without widening the frozen error model (§5.2, invariant I3). Interning at
    /// registration is what makes the leak bounded — the set is drawn from
    /// operator configuration, not from request input.
    ///
    /// The generation is bumped under `rcu` rather than a load-then-store, so two
    /// concurrent publishes produce two generations instead of racing to the same
    /// one. `generation` is how a client detects that the profile set changed
    /// under it (§5.6), so a lost increment would leave it holding a stale
    /// descriptor set believing it current. The retry copies the profile map,
    /// which is a handful of `Arc`s on a control-plane path — never the request
    /// path.
    pub fn publish(&self, bound: Vec<Arc<BoundProfile>>) {
        let profiles: BTreeMap<&'static str, Arc<BoundProfile>> = bound
            .into_iter()
            .map(|profile| (intern(&profile.name), profile))
            .collect();
        self.inner.rcu(|current| RegistrySnapshot {
            generation: current.generation + 1,
            profiles: profiles.clone(),
        });
    }

    /// Publishes an empty set, at the next generation.
    ///
    /// Shutdown and profile removal both swap the snapshot *first*, so new
    /// requests resolve to [`ClusterError::ProfileNotBound`] before the backends
    /// they would have reached are torn down (§4.8, §5.6 phase C). It is the
    /// registry's counterpart to the wiring's hub deregistration.
    pub fn clear(&self) {
        self.publish(Vec::new());
    }

    /// The profile named in a request — the request path.
    ///
    /// One `ArcSwap` load plus one `BTreeMap` lookup, returning the real backends
    /// (invariant I14). This is also `LocalClusterClient`'s dispatch, so it is hot
    /// in an embedded process too, not only behind the wire (§5.2).
    ///
    /// # Errors
    /// [`ClusterError::ProfileNotBound`] if `profile` is not in the current
    /// snapshot — including every request that arrives before the first
    /// [`publish`](Self::publish).
    pub fn resolve(&self, profile: &str) -> Result<Arc<BoundProfile>, ClusterError> {
        self.inner
            .load()
            .profiles
            .get(profile)
            .cloned()
            .ok_or_else(|| ClusterError::ProfileNotBound {
                profile: intern_existing(profile).unwrap_or(UNKNOWN_PROFILE),
            })
    }

    /// The current snapshot, whole.
    #[must_use]
    pub fn snapshot(&self) -> Arc<RegistrySnapshot> {
        self.inner.load_full()
    }

    /// The current generation, without loading the whole snapshot.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.load().generation
    }
}

impl fmt::Debug for ProfileRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = self.inner.load();
        f.debug_struct("ProfileRegistry")
            .field("generation", &snapshot.generation)
            .field("profiles", &snapshot.profiles.keys())
            .finish()
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
