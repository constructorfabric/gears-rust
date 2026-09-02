//! The four gRPC service impls (DESIGN.md, item `S1`).
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
pub use identity::{Caller, CallerResolver};
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
/// One value, cloned into all four, so profile dispatch is configured once. The
/// only field is an `Arc`: a service is `Clone`-cheap because tonic's generated
/// server wraps it in an `Arc` and clones per connection. Caller resolution needs
/// no state — [`CallerResolver::resolve`] reads the request extensions the
/// platform-plane layer stamped — so it is not held here.
#[derive(Debug, Clone)]
pub struct ServiceContext {
    profiles: Arc<ProfileRegistry>,
}

impl ServiceContext {
    /// Builds the shared context the four services capture.
    #[must_use]
    pub fn new(profiles: Arc<ProfileRegistry>) -> Self {
        Self { profiles }
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
    /// The `NotFound`-mapped `ProfileNotBound` for a profile this process has not
    /// bound. Caller resolution itself is a read of the extension the
    /// platform-plane layer stamped and cannot fail here (§4.6).
    fn authorize<T>(
        &self,
        request: &Request<T>,
        profile: &str,
    ) -> Result<(Caller, Arc<BoundProfile>), Status> {
        let caller = CallerResolver::resolve(request)?;
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
}

/// Milliseconds off the wire become a [`Duration`].
///
/// Total by construction: every duration on the wire is a `u64` of milliseconds,
/// and `Duration::from_millis` accepts the whole range.
fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// The largest lease TTL this service honours from the wire, whatever a caller
/// asks for (M3). Shared by the lock and election lease paths.
///
/// Drawn from the fence-retention default — the config default that bounds how
/// long a lease meaningfully lives: a lease TTL beyond the fence-retention window
/// is exactly what DESIGN §5.8.1 rules out (the fence would be reclaimed under a
/// still-live lease), so no legitimate caller sets one past it, while an
/// unauthenticated caller can no longer pass a `u64` that parks a lease for years.
/// The clamp is a ceiling, not a default: a caller asking for less gets exactly
/// what it asked for.
const MAX_LEASE_TTL: Duration = cluster_sdk::lease::FENCE_RETENTION_DEFAULT;

/// A lease TTL off the wire: rejected if zero, clamped to [`MAX_LEASE_TTL`] (M3).
///
/// A zero TTL is a lease that has already lapsed the instant it is taken, so it is
/// meaningless. Every lease-bearing wire path — lock `try_lock`/`lock`/`renew` and
/// election `join`/`renew` — routes its TTL through here, so the two primitives
/// agree on the same reject-and-clamp rule (M3, M9, invariant I1), and the
/// rejection ships through the `to_status` codec so the client reconstructs a
/// typed [`ClusterError::InvalidConfig`](cluster_sdk::ClusterError::InvalidConfig)
/// rather than an opaque `Provider` error.
fn checked_ttl(ttl_ms: u64) -> Result<Duration, Status> {
    if ttl_ms == 0 {
        return Err(cluster_sdk::to_status(
            cluster_sdk::ClusterError::InvalidConfig {
                reason: format!("ttl_ms must be > 0 (got {ttl_ms})"),
            },
        ));
    }
    Ok(millis(ttl_ms).min(MAX_LEASE_TTL))
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
