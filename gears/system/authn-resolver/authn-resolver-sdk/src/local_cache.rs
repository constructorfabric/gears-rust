//! Crate-private pieces shared by the local result caches
//! ([`ClientCredentialsProvider`](crate::ClientCredentialsProvider) and the
//! inbound-token authenticator). Not a general cache abstraction.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{Instrument as _, instrument::WithSubscriber as _};

use crate::error::AuthNResolverError;

/// Upper bound on how far ahead an entry deadline is tracked, so a far-future
/// `cache_until` cannot overflow `Instant` arithmetic.
pub const MAX_ENTRY_LIFETIME: Duration = Duration::from_hours(8760);

/// How many acquisitions one call waits for before giving up with
/// `ServiceUnavailable`, when each one's result is discarded before the call
/// collects it (fenced by invalidation or clear, or its credential expired).
/// Bounds the number of sequential acquisitions, not wall time.
pub const MAX_EVALUATIONS: u32 = 3;

/// Ids of local caches, shared by every cache type so a result's
/// [`CacheOrigin`](crate::models::CacheOrigin) never matches another cache.
static NEXT_CACHE_ID: AtomicU64 = AtomicU64::new(1);

/// A process-unique id for a new local cache.
pub fn next_cache_id() -> u64 {
    NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed)
}

/// When a delivered result stops being usable, captured at publication so a
/// backwards wall-clock jump cannot extend it.
#[derive(Clone, Copy)]
pub struct Deadline {
    /// Monotonic bound, immune to wall-clock jumps.
    pub monotonic: Instant,
    /// Wall-clock bound, so a suspended process cannot outlive it.
    pub absolute: OffsetDateTime,
}

impl Deadline {
    /// Whether both bounds are still ahead of `now` and `wall`.
    pub fn usable(self, now: Instant, wall: OffsetDateTime) -> bool {
        now < self.monotonic && wall < self.absolute
    }
}

/// The variant only: messages may carry plugin or `IdP` detail.
pub const fn error_kind(error: &AuthNResolverError) -> &'static str {
    match error {
        AuthNResolverError::Unauthorized(_) => "unauthorized",
        AuthNResolverError::NoPluginAvailable => "no_plugin_available",
        AuthNResolverError::ServiceUnavailable(_) => "service_unavailable",
        AuthNResolverError::TokenAcquisitionFailed(_) => "token_acquisition_failed",
        AuthNResolverError::Internal(_) => "internal",
    }
}

/// Copy of a resolver error for each waiter sharing one outcome
/// (`AuthNResolverError` is not `Clone`).
pub fn clone_error(error: &AuthNResolverError) -> AuthNResolverError {
    match error {
        AuthNResolverError::Unauthorized(m) => AuthNResolverError::Unauthorized(m.clone()),
        AuthNResolverError::NoPluginAvailable => AuthNResolverError::NoPluginAvailable,
        AuthNResolverError::ServiceUnavailable(m) => {
            AuthNResolverError::ServiceUnavailable(m.clone())
        }
        AuthNResolverError::TokenAcquisitionFailed(m) => {
            AuthNResolverError::TokenAcquisitionFailed(m.clone())
        }
        AuthNResolverError::Internal(m) => AuthNResolverError::Internal(m.clone()),
    }
}

/// Spawn work owned by a cache on behalf of the calling request.
///
/// Keeps the initiating caller's span and active subscriber. With no
/// subscriber yet, uses the ambient one when the task is polled: pinning
/// `NoSubscriber` would hide logging initialized later, and a scoped no-op can
/// cache a callsite as disabled during initialization.
pub fn spawn_in_caller_context<F>(work: F) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let work = work.in_current_span();
    let dispatcher = tracing::dispatcher::get_default(Clone::clone);
    if dispatcher.is::<tracing::subscriber::NoSubscriber>() {
        tokio::spawn(work)
    } else {
        tokio::spawn(work.with_subscriber(dispatcher))
    }
}
