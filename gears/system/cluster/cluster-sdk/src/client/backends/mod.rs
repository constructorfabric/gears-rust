//! The three remote backend handles — DESIGN.md
//!
//! §3.1 cuts the process boundary at the three **backend** traits, and this is
//! the far side of that cut: one type per primitive, each implementing the same
//! trait a plugin implements, each dispatching over the shared gRPC channel. A
//! consumer never learns any of this — it holds an `Arc<dyn ClusterCacheBackend>`
//! and cannot tell a `RemoteCacheBackend` from a `PostgresCache`, which is
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

/// Resolves a descriptor's provider name to the `&'static str` the backend traits
/// return.
///
/// An already-interned lookup rather than a promotion: a `String` from the wire
/// must fit a `&'static str` return type without being leaked, for the same
/// reason the server's registry looks up rather than promotes — provider names
/// are the finite set of linked plugins, not request input (§5.2), so a
/// legitimate one was interned at wiring time and resolves here, and anything
/// else falls back rather than growing the table.
fn provider(name: Option<String>) -> &'static str {
    // Wire-facing: a descriptor's provider name is looked up, never promoted. A
    // peer that returned a fresh name per response would otherwise grow the
    // leaked intern table without bound (invariant I15). A name from the finite
    // set of linked plugins was interned locally at wiring time and resolves
    // here; anything else falls back rather than leaking.
    name.and_then(|name| crate::intern::intern_existing(&name))
        .unwrap_or(UNDESCRIBED_PROVIDER)
}
