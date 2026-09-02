//! [`LocalClusterClient`] — Profile 1's half of the process seam
//! (DESIGN.md).
//!
//! §3.1 cuts the process boundary at the three backend traits and puts exactly
//! one [`ClusterClient`] per process on that seam: a *factory* for those
//! backends. This is the embedded implementation of it — the one the gear's
//! `start` registers under `dyn ClusterClient`, and the one a co-located
//! consumer's `resolve()` therefore finds (§11.2, §12.11's table).
//!
//! # Why it lives in the gear crate
//!
//! It dispatches through the [`ProfileRegistry`], which is gear state (§3.4,
//! §12.4). Putting it in `cluster-sdk` would make the SDK depend on the gear,
//! which is exactly backwards: the SDK is what a *consumer* links, and a
//! consumer in Profile 3 links no gear at all. This is where every other gear's
//! local implementation lives.
//!
//! # Why it interposes nothing
//!
//! The three factory methods hand back the **real** backend `Arc` out of the
//! registry — the same instance the hub holds under `cluster:{profile}` and the
//! same one the wire services dispatch to. A call is one
//! [`ArcSwap::load`](arc_swap::ArcSwap::load) plus one `BTreeMap` lookup plus an
//! `Arc` clone, so routing Profile 1 through this trait costs what resolving a
//! scoped hub entry always cost and adds no per-operation indirection
//! (invariant I14). A wrapper here would be invisible in a review and would tax
//! every cache operation in every embedded process, so the absence is
//! asserted by pointer equality rather than described.
//!
//! # It is not conditional
//!
//! The gear registers this unconditionally and knows nothing about which
//! deployment profile it is in. In Profile 3 the gear is alone in its pod, so
//! nothing resolves against it locally and the registration is inert rather than
//! wrong. Whether any consumer finds it is a property of what the binary linked
//! (§11.2).

use std::sync::Arc;

use async_trait::async_trait;
use cluster_sdk::{
    ClusterCacheBackend, ClusterClient, ClusterError, DistributedLockBackend,
    LeaderElectionBackend, ProfileDescriptor,
};

use crate::domain::registry::ProfileRegistry;

/// [`ClusterClient`] over the gear's own [`ProfileRegistry`] — the whole of
/// Profile 1's half of the seam (§12.4).
///
/// Holds the registry rather than a snapshot of it, so a client handed out
/// before `start` (or kept across a re-publish) follows the profile set instead
/// of pinning the set that existed when it was built. Every method resolves
/// against whatever is published at call time; before the first publish that is
/// nothing, and every method answers [`ClusterError::ProfileNotBound`] — the
/// correct answer, and one the frozen error model already has a variant for
/// (invariant I3).
#[derive(Debug)]
pub struct LocalClusterClient {
    profiles: Arc<ProfileRegistry>,
}

impl LocalClusterClient {
    /// A client dispatching through `profiles`.
    #[must_use]
    pub fn new(profiles: Arc<ProfileRegistry>) -> Self {
        Self { profiles }
    }
}

#[async_trait]
impl ClusterClient for LocalClusterClient {
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
        Ok(Arc::clone(&self.profiles.resolve(profile)?.cache))
    }

    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError> {
        Ok(Arc::clone(&self.profiles.resolve(profile)?.lock))
    }

    fn leader_election_backend(
        &self,
        profile: &str,
    ) -> Result<Arc<dyn LeaderElectionBackend>, ClusterError> {
        Ok(Arc::clone(&self.profiles.resolve(profile)?.leader_election))
    }

    /// Intrinsic locally, and so never I/O: the descriptor was computed at
    /// wiring time from the real backends' own `consistency()` / `features()` /
    /// `provider_name()`, and the live health is one relaxed atomic load
    /// (§4.7.1, §5.5).
    ///
    /// That is what makes `resolve()`'s bounded descriptor await a no-op in
    /// Profile 1: the future this returns is ready on its first poll, so the
    /// timeout `K4` wraps it in can never fire here (invariant I6 is about the
    /// remote half). The test asserts readiness-on-first-poll rather than
    /// asserting a duration.
    ///
    /// [`BoundProfile::descriptor`](crate::BoundProfile::descriptor), never the
    /// `wired_descriptor` field: the latter carries the health frozen at wiring
    /// time, and health moves without republishing the registry (§4.4). §12.4's
    /// sketch predates that split and clones the field directly — it would serve
    /// a permanently `Serving` profile.
    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError> {
        Ok(self.profiles.resolve(profile)?.descriptor())
    }
}

#[cfg(test)]
#[path = "local_client_tests.rs"]
mod local_client_tests;
