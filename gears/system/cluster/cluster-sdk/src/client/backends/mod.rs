// Created: 2026-08-13 by Constructor Tech
//! The three remote backend handles — DESIGN-DEPLOYABLE-GEAR §3.1, §12.10–12.12.
//!
//! §3.1 cuts the process boundary at the three **backend** traits, and this is
//! the far side of that cut: one type per primitive, each implementing the same
//! trait a plugin implements, each dispatching over the shared gRPC channel. A
//! consumer never learns any of this — it holds an `Arc<dyn ClusterCacheBackend>`
//! and cannot tell a `RemoteCacheBackend` from a `PostgresCacheBackend`, which is
//! what makes one consumer source file behave identically in both deployment
//! profiles (invariant I1).
//!
//! # Nothing here is nameable from outside this crate
//!
//! Every type is `pub(crate)`, produced only by
//! [`RemoteClusterClient`](crate::client::remote::RemoteClusterClient)'s factory
//! methods and handed back as `Arc<dyn _Backend>` (invariant I4). That is not
//! tidiness: a consumer that could name `RemoteCacheBackend` could branch on the
//! deployment profile, which is exactly the profile transparency the design is
//! built to keep.
//!
//! # What the wire cannot carry, and what carries instead
//!
//! | Trait shape | Wire shape | Bridged by |
//! |---|---|---|
//! | `consistency()` / `features()` / `provider_name()` — synchronous | one `DescribeProfiles` call | the [descriptor cache](crate::descriptors), read synchronously (§5.5) |
//! | `scan_prefix` — an unbounded `Vec` | paginated | the client loops pages (§6.4) |
//! | `watch` — a `CacheWatch` channel | a server-push stream | one pump task per watch (§6.8) |
//! | `try_lock` — a `LockGuard` whose fields are private | a lease token | the pump's closure holds the token (§12.11, §12.17) |
//! | `elect` — a `LeaderWatch` | `join` plus a subscription | a renewal-and-subscription pump (§12.12) |

use std::sync::Arc;

use crate::descriptors::DescriptorCache;
use crate::dto::ProfileDescriptor;

mod cache;
mod leader;
mod lock;

pub use cache::RemoteCacheBackend;
pub use leader::RemoteLeaderElectionBackend;
pub use lock::RemoteLockBackend;

/// The provider a backend reports before its descriptor has been fetched.
///
/// It reaches an operator through
/// [`ClusterError::CapabilityNotMet`](crate::error::ClusterError::CapabilityNotMet)'s
/// `provider` field, so it says what is true rather than guessing a name: this
/// process has not yet learned which backend serves the profile. `K4` awaits the
/// descriptor before validating a requirement, so a consumer sees this only when
/// that fetch failed — and then `K5`'s readiness contributor is what reports the
/// real problem.
const UNDESCRIBED_PROVIDER: &str = "unknown";

/// What every remote backend handle holds: the profile it addresses and the
/// shared descriptor cache its synchronous accessors read.
///
/// The profile is an [`Arc<str>`] rather than an interned `&'static str`: a
/// backend handle is built per `resolve()` from a name that is already validated
/// server-side, and interning is reserved for names that must reach the frozen
/// error model (§5.2). Every request has to render it into an owned `String`
/// anyway, so the `Arc` costs nothing beyond the handle itself.
#[derive(Debug, Clone)]
pub struct RemoteProfile {
    profile: Arc<str>,
    descriptors: Arc<DescriptorCache>,
}

impl RemoteProfile {
    /// Binds a handle to `profile`, sharing `descriptors` with its siblings.
    fn new(profile: &str, descriptors: Arc<DescriptorCache>) -> Self {
        Self {
            profile: Arc::from(profile),
            descriptors,
        }
    }

    /// The profile name, as every request carries it.
    fn name(&self) -> String {
        self.profile.to_string()
    }

    /// The cached descriptor, if `DescribeProfiles` has answered for this profile.
    fn descriptor(&self) -> Option<ProfileDescriptor> {
        self.descriptors.get(&self.profile)
    }
}

/// Promotes a descriptor's provider name to the `&'static str` the backend traits
/// return.
///
/// Interning is what makes a `String` from the wire fit a `&'static str` return
/// type, and it is bounded here for the same reason it is bounded in the server's
/// registry: provider names are the finite set of linked plugins, not request
/// input (§5.2).
fn provider(name: Option<String>) -> &'static str {
    name.map_or(UNDESCRIBED_PROVIDER, |name| crate::intern::intern(&name))
}
