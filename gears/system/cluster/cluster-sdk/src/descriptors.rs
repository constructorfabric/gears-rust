//! The client-side descriptor cache — DESIGN.md
//!
//! A remote backend's `consistency()`, `features()` and `provider_name()` are
//! **synchronous** accessors on a trait that must stay stable for plugins
//! (invariant I11), and none of them can do I/O. What they answer with is the
//! profile's [`ProfileDescriptor`], fetched once by `DescribeProfiles` and held
//! here. That is the whole reason the profile contract exists (§5.5).
//!
//! # What `generation` is for
//!
//! Every response carries the server's `ProfileRegistry` generation, which the
//! server bumps on every publish of the profile *set* (§5.2). A client reads it
//! to notice the set changed under it (§5.6). It is deliberately **not** a health
//! counter: health moves without republishing, so a client also
//! re-reads on a short unconditional poll rather than only on a generation change
//! (§4.4, §5.5).
//!
//! # Why the generation is recorded but never gates a populate
//!
//! `generation` counts publishes **within one server process**:
//! `ProfileRegistry::new()` starts at 0 and `publish` bumps by one, and a fresh
//! pod starts again from 0. It is not a cluster-wide epoch, so it does not order
//! two responses that came from two *different* pods — and a consumer's channel
//! reaches a different pod on every reconnect.
//!
//! Dropping a response whose generation is lower therefore cannot mean "stale".
//! Across a rolling restart it means the opposite: the gear's `stop` publishes
//! the empty set at generation+1 while still serving (§4.8), so a consumer that
//! polled a draining pod holds a *higher* generation than every healthy pod can
//! ever answer with, and would discard all of them for the life of its process.
//!
//! What is left to defend against is the same-connection out-of-order case: two
//! `DescribeProfiles` calls in flight at once, completing in the other order,
//! with a publish in between. That is bounded and self-clearing — the cache is
//! replaced *wholesale* by a set the server really served, and the contributor's
//! unconditional 10 s poll (§5.5) corrects it within one interval. A bounded
//! staleness window is the right trade against an unbounded wedge, so the
//! populate is last-write-wins and a generation that moves backwards is
//! **logged** rather than acted on — which §5.6 prescribes for a
//! generation mismatch.
//!
//! # Why a `RwLock` and not an `ArcSwap`
//!
//! The registry on the server side is read on the request path and uses
//! [`ArcSwap`](https://docs.rs/arc-swap) for it. This one is not: it is read by
//! the sync accessors, which a consumer calls at resolution and when branching on
//! a capability, never per cache operation. A `RwLock` read is ample there and
//! costs the SDK no additional dependency.

use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock};

use crate::dto::ProfileDescriptor;

/// One immutable view of the descriptors this client has fetched.
#[derive(Debug, Default)]
struct Descriptors {
    /// The `ProfileRegistry` generation these descriptors were read from, or 0
    /// when nothing has been fetched yet. The server's first publish is
    /// generation 1, so 0 is unambiguously "never populated" — the same bit the
    /// gear's readiness check reads (§5.2).
    generation: u64,
    /// Descriptors by profile name. `BTreeMap` for the same reason the server's
    /// snapshot uses one: the set is small, config-derived, and worth enumerating
    /// in a stable order for diagnostics.
    profiles: BTreeMap<String, ProfileDescriptor>,
}

/// The per-process cache of [`ProfileDescriptor`]s behind a
/// [`RemoteClusterClient`](crate::client::remote::RemoteClusterClient).
///
/// Shared by the client and every backend handle it produces, so one
/// `DescribeProfiles` call serves all three primitives of every profile.
#[derive(Debug, Default)]
pub struct DescriptorCache {
    inner: RwLock<Descriptors>,
}

impl DescriptorCache {
    /// An empty cache at generation 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The descriptor for `profile`, if one has been fetched.
    ///
    /// Clones rather than handing out a guard: the accessors that read it are
    /// synchronous methods on a backend, and holding a lock guard across their
    /// return would put a `RwLock` read guard in a caller's hands.
    pub fn get(&self, profile: &str) -> Option<ProfileDescriptor> {
        self.read().profiles.get(profile).cloned()
    }

    /// Replaces the cached set with `profiles` as read at `generation`.
    ///
    /// **Wholesale, not a merge**: `DescribeProfiles` with an empty filter
    /// answers with the server's entire bound set, so a profile absent from the
    /// response is a profile the server no longer binds. Merging would keep a
    /// removed profile answering `consistency()` forever (§5.6 phase C).
    ///
    /// **Last-write-wins**, and never dropped for a lower `generation` — see the
    /// [module docs](self) for why a per-process counter cannot order responses
    /// from two pods. A generation that moves backwards is reported at `warn`
    /// with both values (§5.6) and the response is applied regardless: the peer
    /// that answered it is the one this client is now talking to.
    pub fn populate(&self, generation: u64, profiles: Vec<ProfileDescriptor>) {
        let mut guard = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if generation < guard.generation {
            tracing::warn!(
                held = guard.generation,
                answered = generation,
                "cluster: the profile registry generation moved backwards, which means this \
                 client is talking to a different cluster pod than the one it last read; \
                 adopting the answering pod's profile set"
            );
        }
        guard.generation = generation;
        guard.profiles = profiles
            .into_iter()
            .map(|descriptor| {
                // Ingestion is the one bounded seam where a provider name is
                // promoted into the interned set — mirroring the server, which
                // interns its configured names once at registry build and looks
                // them up thereafter (`registry.rs`). The wire-facing accessors
                // (`backends::provider`) then *look up* rather than promote (M2),
                // so a provider name only ever enters the leaked table here, from
                // the authoritative descriptor set the server serves and this
                // cache replaces wholesale — never from a per-request error.
                for provider in [
                    descriptor.cache.provider.as_str(),
                    descriptor.lock.provider.as_str(),
                    descriptor.leader_election.provider.as_str(),
                ] {
                    let interned = crate::intern::intern(provider);
                    debug_assert_eq!(interned, provider);
                }
                (descriptor.name.clone(), descriptor)
            })
            .collect();
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Descriptors> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[path = "descriptors_tests.rs"]
mod descriptors_tests;
