// Created: 2026-08-12 by Constructor Tech
//! The four gRPC service impls (DESIGN-DEPLOYABLE-GEAR §6.1, §12.6, item `S1`).
//!
//! Hand-written over the generated `*_server` traits, and **that is the sanctioned
//! permanent pattern, not interim glue**: gRPC server codegen is out of scope
//! platform-wide, so these four impls are where the wire meets the backends and
//! they will not be replaced by a generator later.
//!
//! # The five steps, in the same order in every method
//!
//! 1. **Resolve the caller** from platform-plane metadata ([`identity`], §4.6).
//! 2. **Take the request apart** — proto message to SDK types.
//! 3. **Dispatch on `profile`** through the [`ProfileRegistry`](crate::ProfileRegistry)
//!    (§5.2). An unbound profile is `ProfileNotBound`, which maps to `NotFound`.
//! 4. **Call the backend** — the real `Arc`, with no wrapper interposed
//!    (invariant I14).
//! 5. **Map the outcome** — `ClusterError` to `Status` through
//!    [`cluster_sdk::to_status`], the one codec (§6.9).
//!
//! Steps 1 and 5 are the same code in all four services; steps 2–4 are the
//! service's own. Nothing here reaches for a contract type: a contract change that
//! forces an edit in this module means the `*Api` trait boundary leaked, and that
//! is the finding (§6.1, `H3`).
//!
//! # Every service captures the registry, never a backend
//!
//! The gear's services are collected in the framework's phase 6 and its backends
//! exist only after phase 7 (§4.2), so a service that captured a backend could not
//! be built at all. Capturing [`ProfileRegistry`](crate::ProfileRegistry) is what
//! makes the ordering work, and it is also what makes the in-flight window
//! answerable: a request arriving before `start` publishes resolves to
//! `ProfileNotBound`, which is the correct answer from the frozen error model
//! (invariant I3). `S3` depends on this property.
//!
//! # There is no server-side lease state
//!
//! The lock service holds nothing between calls, because the lease is the backing
//! store's record and the token is the whole authority (§5.8.1). What the leader
//! service does hold is a **subscription** table ([`subscriptions`]) — and a
//! subscription is not a lease: dropping one revokes no leadership, which is the
//! property `S2`'s exit criterion asserts and §5.4 requires.

pub mod cache;
pub mod identity;
pub mod leader;
pub mod lock;
pub mod profile;
pub mod subscriptions;
pub mod sweep;

#[cfg(test)]
mod test_harness;

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::{ClusterError, dto};
use tonic::{Request, Status};

pub use cache::CacheService;
pub use identity::{Caller, CallerAuthentication, CallerResolver};
pub use leader::LeaderElectionService;
pub use lock::DistributedLockService;
pub use profile::ClusterProfileService;
pub use subscriptions::{ElectionSubscriptions, SubscriptionId, SweepReport};
pub use sweep::{
    SUBSCRIPTIONS_ACTIVE, SUBSCRIPTIONS_REAPED, SWEEP_GRACE_MULTIPLIER, SWEEP_INTERVAL,
    SubscriptionMetrics, spawn_subscription_sweep, sweep_grace, sweep_once,
};

use crate::domain::registry::{BoundProfile, ProfileRegistry};

/// What every service impl is built from.
///
/// One value, cloned into all four, so caller resolution and profile dispatch are
/// configured once. Both fields are `Arc`s: a service is `Clone`-cheap because
/// tonic's generated server wraps it in an `Arc` and clones per connection.
#[derive(Debug, Clone)]
pub struct ServiceContext {
    profiles: Arc<ProfileRegistry>,
    callers: CallerResolver,
}

impl ServiceContext {
    /// Builds the shared context the four services capture.
    #[must_use]
    pub fn new(profiles: Arc<ProfileRegistry>, callers: CallerResolver) -> Self {
        Self { profiles, callers }
    }

    /// The bound profile set — read on every request, never replaced here.
    #[must_use]
    pub fn profiles(&self) -> &Arc<ProfileRegistry> {
        &self.profiles
    }

    /// Steps 1 and 3 together, in the order §12.6 fixes them: **identify the
    /// caller before dispatching the profile.**
    ///
    /// The order is a disclosure decision, not a style one. Resolving the profile
    /// first would let an unauthenticated caller distinguish a bound profile from
    /// an unbound one by the code that comes back, which is a free inventory of
    /// the deployment's configuration.
    ///
    /// # Errors
    /// The caller-resolution statuses of [`CallerResolver::resolve`], or the
    /// `NotFound`-mapped `ProfileNotBound` for a profile this process has not
    /// bound.
    async fn authorize<T>(
        &self,
        request: &Request<T>,
        profile: &str,
    ) -> Result<(Caller, Arc<BoundProfile>), Status> {
        let caller = self.callers.resolve(request.metadata()).await?;
        let bound = self.profiles.resolve(profile).map_err(|error| {
            // The typed error carries an interned name, so a profile that was
            // never bound in this process reports `<unknown>` there (invariant
            // I3). The name that actually arrived belongs in the log, which is
            // unbounded-cardinality territory and therefore never a metric label
            // (invariant I15).
            tracing::debug!(
                requested_profile = profile,
                caller = caller.name(),
                "cluster: rejecting a request for an unbound profile"
            );
            cluster_sdk::to_status(error)
        })?;
        Ok((caller, bound))
    }

    /// Caller resolution alone, for the one request that names no profile.
    ///
    /// # Errors
    /// The caller-resolution statuses of [`CallerResolver::resolve`].
    async fn authorize_only<T>(&self, request: &Request<T>) -> Result<Caller, Status> {
        self.callers.resolve(request.metadata()).await
    }
}

/// Milliseconds off the wire become a [`Duration`].
///
/// Total by construction: every duration on the wire is a `u64` of milliseconds,
/// and `Duration::from_millis` accepts the whole range.
fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// The terminal error a watch stream carries in-band, as `Closed(err)` (§6.8).
///
/// It travels **inside** the stream rather than as the stream's `Status` because a
/// consumer's `RestartingWatch` branches on the typed `ClusterError`'s
/// retryability, and a bare status code cannot express the
/// `Shutdown`-versus-`ConnectionLost` distinction that decides whether it
/// resubscribes (§6.9).
fn wire_error(error: ClusterError) -> dto::WireError {
    dto::WireError::from(cluster_sdk::ClusterWireError::from(error))
}
