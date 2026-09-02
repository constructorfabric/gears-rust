//! [`ClusterClient`] — the one cluster object per process.
//!
//! DESIGN.md cuts the process boundary at the three backend
//! traits, and this is the object that sits on that seam: **a factory for those
//! backends, not a facade and not a transport detail.** It answers "give me the
//! backend for this profile" and nothing else.
//!
//! | Profile | Implementation | Registered by |
//! |---|---|---|
//! | 1 — embedded | `LocalClusterClient`, dispatching through the gear's `ProfileRegistry` | the cluster gear's `start` |
//! | 3 — deployed | `RemoteClusterClient`, holding the gRPC channel and the descriptor cache | the SDK's consumer registration |
//!
//! Exactly one `Arc<dyn ClusterClient>` is registered per process, local
//! winning over remote. That is the platform's ordinary one-dependency,
//! one-trait-object shape (`cpt-cf-fr-client-transparency`) applied to cluster
//! rather than reinvented for it; cluster's only variation is that its single
//! trait object is a *factory* for three backend traits rather than being the
//! consumer-facing API itself.
//!
//! # The profile is a request parameter, not a wiring parameter
//!
//! Nothing profile-specific is wired. Per-profile backends are derived from this
//! one object, and a remote implementation never learns which provider serves a
//! profile — that knowledge stays server-side, which keeps plugin
//! linkage a cluster-gear concern only (§3.1, §3.3).
//!
//! # Why the factory methods are synchronous
//!
//! They are sync and pure in **both** profiles, which keeps `resolve()`'s
//! only `await` the descriptor. Locally a factory call is one snapshot load plus
//! a map lookup returning the *real* backend `Arc` — no wrapper is interposed,
//! so Profile 1 keeps today's exact hot-path cost (§3.1, and invariant I14).
//! Remotely it constructs a `Remote*Backend`, which is an `Arc` clone plus an
//! interned profile name. Neither touches the network.
//!
//! [`ClusterClient::descriptor`] is the sole `async` member, because remotely it
//! needs I/O (§5.5).
//!
//! # Why this module is unfeatured
//!
//! Profile 1 needs the trait too: the cluster gear registers a local
//! implementation and the resolvers look it up, in a process where no gRPC
//! client is linked. Only the *remote* implementation sits behind
//! `grpc-client` (§3.4), so this module stays free of any transport dependency.

use std::sync::Arc;

use async_trait::async_trait;

/// The deployed half of the seam. Behind `grpc-client` because Profile 1 links
/// no transport (§3.4); the trait above it is unfeatured because Profile 1 needs
/// it.
/// The three remote backend handles the [`remote`] client produces (§3.1, §12.10-12.12).
///
/// Private, and that is invariant I4: a consumer holds `Arc<dyn ClusterCacheBackend>`
/// and must not be able to name — or branch on — the remote implementation. Grouped
/// under `client` because a `RemoteClusterClient` is the only thing that builds one;
/// the singular `cache::backend` / `leader::backend` / `lock::backend` modules are the
/// *plugin* traits, which is the opposite side of the same boundary.
#[cfg(feature = "grpc-client")]
mod backends;
#[cfg(feature = "grpc-client")]
pub mod remote;

use crate::cache::ClusterCacheBackend;
use crate::dto::ProfileDescriptor;
use crate::error::ClusterError;
use crate::leader::LeaderElectionBackend;
use crate::lock::DistributedLockBackend;

/// The one cluster object per process, registered under `dyn ClusterClient`.
///
/// See the [module documentation](self) for where this sits and why the factory
/// methods are synchronous.
///
/// No error variant is introduced for this seam: the error model is frozen
/// (§5.2, §6.10, and invariant I3), so an unbound profile is reported as
/// [`ClusterError::ProfileNotBound`] on every method here.
#[async_trait]
pub trait ClusterClient: Send + Sync {
    /// The cache backend serving `profile`.
    ///
    /// # Errors
    /// [`ClusterError::ProfileNotBound`] when `profile` has no cache binding.
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError>;

    /// The distributed-lock backend serving `profile`.
    ///
    /// # Errors
    /// [`ClusterError::ProfileNotBound`] when `profile` has no lock binding.
    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError>;

    /// The leader-election backend serving `profile`.
    ///
    /// # Errors
    /// [`ClusterError::ProfileNotBound`] when `profile` has no leader-election
    /// binding.
    fn leader_election_backend(
        &self,
        profile: &str,
    ) -> Result<Arc<dyn LeaderElectionBackend>, ClusterError>;

    /// The profile's descriptor — consistency, features, the server-side
    /// provider names and per-profile health.
    ///
    /// The only `async` member. Locally it resolves without I/O; remotely it is
    /// a `DescribeProfiles` call, cached thereafter and refreshed on a poll
    /// (§5.5). It is also the only thing `resolve()` may await, and it awaits it
    /// on a bounded timeout — never on cluster becoming reachable (§4.7.1, and
    /// invariant I6).
    ///
    /// # Errors
    /// [`ClusterError::ProfileNotBound`] when `profile` is not bound, or
    /// [`ClusterError::Provider`] when a remote descriptor fetch fails.
    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError>;

    /// Discards any cached descriptors so the next [`descriptor`](Self::descriptor)
    /// reads fresh state.
    ///
    /// **Defaulted to a no-op, and dyn-safe** (invariant I11's shape, applied to this
    /// trait): an implementation whose descriptor is computed live has nothing to
    /// invalidate. `LocalClusterClient` is exactly that — it reads
    /// `BoundProfile::descriptor()`, which overlays the live health cell on every
    /// call — so the default is correct for Profile 1 rather than merely tolerated.
    ///
    /// It exists because the remote client caches, deliberately and by design: the
    /// synchronous accessors on a backend handle have to answer without I/O, so a
    /// populated cache is never re-read. Health, however, moves without a
    /// configuration change (§4.4, §5.5) — so *something* has to be able to say "read
    /// it again", and the readiness contributor is the only caller that should
    /// (§4.7.1). Nothing on the request path calls this.
    ///
    /// # Errors
    /// Whatever the refresh reports. A failure is transient by construction — a
    /// cluster that cannot be reached now may be reachable next interval — so a
    /// caller logs it rather than treating it as a verdict.
    async fn refresh_descriptors(&self) -> Result<(), ClusterError> {
        Ok(())
    }
}

crate::assert_dyn_compatible!(ClusterClient);

#[cfg(test)]
mod client_tests;
