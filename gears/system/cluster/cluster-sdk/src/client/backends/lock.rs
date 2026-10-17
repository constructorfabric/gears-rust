//! The remote distributed-lock backend — DESIGN.md
//!
//! Unary throughout: the lease is a record in the backing store and the token is
//! the whole authority over it, so there is nothing to subscribe to and nothing
//! replica-local to address (§5.8.1, invariant I7). A `renew` issued here lands
//! correctly on a replica that never saw the acquire.
//!
//! # The token cannot live in the guard, so a task holds it
//!
//! [`LockGuard`] is `{ name, commands }` with both fields private and
//! [`LockGuard::channel`] as its only public constructor, so an acquisition spawns
//! a pump whose closure owns the token and the guard addresses it over the channel
//! (§12.11, §12.17). Widening `LockGuard` with an opaque lease field would remove
//! the task, but it would change a frozen consumer-facing type for one backend's
//! benefit.
//!
//! The cost is one tokio task per **held** lock, client-side. The in-process
//! backends already pay it, so it is not a regression — but §7.2.5 says it
//! plainly, and so does this: a consumer holding many concurrent locks holds many
//! pumps.
//!
//! # Who the owner is
//!
//! The server mints it. [`acquire`](DistributedLockBackend::acquire) takes an
//! `owner` argument because an in-process backend is told who is asking; over the
//! wire the *transport caller* is the identity, and the serving gear derives
//! `{caller}/{nonce}` from it and ignores anything the client might claim (§4.6,
//! and `cluster/src/api/grpc/lock.rs`). A client-supplied owner would be
//! forgeable, which is the whole reason it is not honoured. The argument is
//! therefore advisory here and the minted token carries the real owner — which is
//! what every later operation is predicated on, so the semantics still hold end to
//! end.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::{RemoteProfile, provider};
use crate::client::backends::cache::duration_ms;
use crate::client::remote::{LockStub, decode};
use crate::convert::{LeaseContext, from_lease_status, from_status};
use crate::descriptors::DescriptorCache;
use crate::dto;
use crate::error::ClusterError;
use crate::grpc::stubs::lock as stubs;
use crate::lease::LeaseToken;
use crate::lock::{DistributedLockBackend, LockFeatures, LockGuard, LockRequest};

/// The guard command buffer.
///
/// One, matching §12.11 and the in-process backends: a consumer awaits each
/// `renew` before issuing the next, so a deeper buffer would only queue commands
/// that cannot be concurrent anyway.
const COMMAND_BUFFER: usize = 1;

/// A blocking wait as the wire's whole milliseconds, rounded **up**.
///
/// Rounding down, as [`duration_ms`] does, would turn the sub-millisecond
/// remainder at the end of [`acquire_waiting`](DistributedLockBackend::acquire_waiting)'s
/// budget into a `timeout_ms: 0` re-issue that the server answers with an
/// immediate `LockTimeout`, so the loop would spin RPCs until the remainder closed.
/// Rounding up asks for at least the remaining budget, so the wait that comes back
/// lands past the caller's deadline and ends the loop. Only a caller's own zero
/// timeout goes out as zero.
fn wait_ms(value: Duration) -> u64 {
    u64::try_from(value.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// [`DistributedLockBackend`] over the wire (§12.11).
#[derive(Debug, Clone)]
pub struct RemoteLockBackend {
    stub: LockStub,
    profile: RemoteProfile,
}

impl RemoteLockBackend {
    /// Binds a handle to `profile` over `stub`.
    pub fn new(stub: LockStub, profile: &str, descriptors: Arc<DescriptorCache>) -> Self {
        Self {
            stub,
            profile: RemoteProfile::new(profile, descriptors),
        }
    }

    /// The cached lock descriptor, if one has been fetched.
    fn describe(&self) -> Option<dto::LockDescriptor> {
        self.profile.descriptor().map(|profile| profile.lock)
    }

    fn stub(&self) -> LockStub {
        self.stub.clone()
    }

    /// One blocking `Lock` RPC waiting up to `timeout`. The server may clamp the
    /// wait; [`acquire_waiting`](DistributedLockBackend::acquire_waiting) owns the
    /// re-issue loop that makes a clamp invisible to the caller.
    async fn lock_once(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let request = stubs::LockRequest::from(dto::LockRequest {
            profile: self.profile.name(),
            name: name.to_owned(),
            ttl_ms: duration_ms(ttl),
            timeout_ms: wait_ms(timeout),
            client_request_id: None,
        });
        let response = self
            .stub()
            .lock(request)
            .await
            .map_err(|status| from_status(&status))?;
        Ok(decode::<dto::LockAcquired, _>(response.into_inner())?
            .token
            .into())
    }

    /// The lease reference every token-keyed operation carries.
    fn lease_ref(&self, token: &LeaseToken, ttl: Option<Duration>) -> stubs::LeaseRef {
        stubs::LeaseRef::from(dto::LeaseRef {
            profile: self.profile.name(),
            token: dto::LeaseToken::from(token.clone()),
            ttl_ms: ttl.map(duration_ms),
            client_request_id: None,
        })
    }

    /// Wraps an acquired lease in the [`LockGuard`] the trait returns, spawning
    /// the pump that holds its token.
    fn guard(&self, token: LeaseToken) -> LockGuard {
        let (mut commands, guard) = LockGuard::channel(token.name.clone(), COMMAND_BUFFER);
        let backend = self.clone();
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                match command {
                    LockRequest::Renew { new_ttl, responder } => {
                        responder.respond(backend.renew(&token, new_ttl).await);
                    }
                    LockRequest::Release { responder } => {
                        responder.respond(backend.release(&token).await);
                        // Release consumes the guard, so no further command can
                        // arrive and the pump has nothing left to hold.
                        return;
                    }
                }
            }
            // The channel closed without a release: the consumer dropped the
            // guard. No I/O — the lease lapses at its own deadline, exactly as it
            // does in-process (`lock/guard.rs`'s module docs). Issuing a release
            // here would turn a dropped guard into a network call the consumer
            // never asked for, and it would do it after the consumer stopped
            // caring about the result.
        });
        guard
    }
}

#[async_trait]
impl DistributedLockBackend for RemoteLockBackend {
    /// From the descriptor cache (§5.5). An unfetched descriptor declares no
    /// linearizability, so a `require(Linearizable)` fails rather than being
    /// falsely satisfied.
    fn features(&self) -> LockFeatures {
        self.describe()
            .map_or_else(|| LockFeatures::new(false), |lock| lock.features.into())
    }

    /// The **server-side** provider, for the same reason as the cache's (§5.5).
    fn provider_name(&self) -> &'static str {
        provider(self.describe().map(|lock| lock.provider))
    }

    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        // Built over `acquire` rather than duplicating the RPC, exactly as the
        // cache-backed default builds its guard over its own lease method: one
        // acquisition path, so the guard and the token half cannot diverge.
        let token = self.acquire(name, "", ttl).await?;
        Ok(self.guard(token))
    }

    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        let token = self.acquire_waiting(name, "", ttl, timeout).await?;
        Ok(self.guard(token))
    }

    /// `owner` is advisory — the server mints the real one. See the [module
    /// docs](self).
    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let _server_mints_the_owner = owner;
        let request = stubs::TryLockRequest::from(dto::TryLockRequest {
            profile: self.profile.name(),
            name: name.to_owned(),
            ttl_ms: duration_ms(ttl),
            client_request_id: None,
        });
        let response = self
            .stub()
            .try_lock(request)
            .await
            .map_err(|status| from_status(&status))?;
        Ok(decode::<dto::LockAcquired, _>(response.into_inner())?
            .token
            .into())
    }

    /// The server does the waiting, and that is not delegation for tidiness: a
    /// lease that *lapses* writes nothing, so no event announces it, and only a
    /// waiter that can see the incumbent's deadline can cap its wait by it. The
    /// backends behind the wire do; a wait re-implemented here could not, so it
    /// would sleep past a lease it could have taken (§6.5).
    ///
    /// This is also why the client sets no RPC deadline: one shorter than
    /// `timeout` would sever an acquisition the server was about to grant.
    ///
    /// It waits up to the caller's **whole** `timeout`, re-issuing `Lock` as needed.
    /// The server clamps any single blocking wait (the gear's `MAX_LOCK_WAIT`) so
    /// that no wire caller can park a polling task for an unbounded time. A clamped
    /// wait comes back as `LockTimeout` while the caller still has budget. This loop
    /// re-issues with whatever budget remains, so a Profile-3 `lock(name, ttl, 10min)`
    /// waits the full ten minutes, as it does in-process (invariant I1). A
    /// `LockTimeout` that arrives once the caller's own deadline has passed is the
    /// genuine timeout. Its `waited` is the **sum of the server-measured waits**, so
    /// it stays populated server-side (§6.9) and covers the whole wait, not just the
    /// last RPC. Every other outcome ends the wait, the backpressure refusal
    /// included.
    ///
    /// `owner` is advisory — the server mints the real one. See the [module
    /// docs](self).
    async fn acquire_waiting(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let _server_mints_the_owner = owner;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut waited_total = Duration::ZERO;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.lock_once(name, ttl, remaining).await {
                Err(ClusterError::LockTimeout { name, waited }) => {
                    waited_total = waited_total.saturating_add(waited);
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ClusterError::LockTimeout {
                            name,
                            waited: waited_total,
                        });
                    }
                }
                outcome => return outcome,
            }
        }
    }

    /// # Errors
    /// [`ClusterError::LockExpired`] when the token matches no live lease — which
    /// covers expired, stolen and never-yours alike, deliberately
    /// indistinguishable (§5.8.1). A bare canonical `NotFound` from an
    /// intermediary maps to the same verdict through [`LeaseContext::LockRenew`].
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let request = self.lease_ref(token, Some(ttl));
        match self.stub().renew(request).await {
            Ok(_ack) => Ok(()),
            Err(status) => Err(from_lease_status(
                &status,
                LeaseContext::LockRenew { name: &token.name },
            )
            .unwrap_or_else(|| ClusterError::LockExpired {
                name: token.name.clone(),
            })),
        }
    }

    /// **Absence is `Ok`** (§6.10): a retried release, or one bearing a fenced-out
    /// token, deletes nothing and succeeds. That is what
    /// [`LeaseContext::LeaseRelease`] expresses — it is the one context in which
    /// the codec answers `None`, and `None` here means there was nothing to
    /// report.
    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let request = self.lease_ref(token, None);
        match self.stub().release(request).await {
            Ok(_ack) => Ok(()),
            Err(status) => match from_lease_status(&status, LeaseContext::LeaseRelease) {
                Some(error) => Err(error),
                None => Ok(()),
            },
        }
    }

    /// Left at the trait's `Ok(())` default, for the same reason as the cache
    /// handle's: this backend owns no resource the serving gear's readiness check
    /// is asking about.
    async fn probe(&self) -> Result<(), ClusterError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::wait_ms;

    #[test]
    fn a_blocking_wait_rounds_up_to_the_next_whole_millisecond() {
        assert_eq!(
            wait_ms(Duration::ZERO),
            0,
            "only a zero budget asks for zero"
        );
        assert_eq!(
            wait_ms(Duration::from_micros(1)),
            1,
            "a sub-ms remainder must not go out as a zero-length wait"
        );
        assert_eq!(wait_ms(Duration::from_micros(999)), 1);
        assert_eq!(wait_ms(Duration::from_millis(1)), 1, "whole ms are exact");
        assert_eq!(wait_ms(Duration::from_micros(1_001)), 2);
        assert_eq!(wait_ms(Duration::from_millis(350)), 350);
        assert_eq!(wait_ms(Duration::MAX), u64::MAX, "saturates, never wraps");
    }
}
