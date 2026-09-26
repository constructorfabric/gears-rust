//! The distributed-lock service (DESIGN.md).
//!
//! Four unary methods, no streaming, and — the property everything else rests on —
//! **no server-side lease state**. The lease is the backing store's record; the
//! token is the whole authority over it (§5.8.1). This service translates a token
//! into a predicate and lets the backend execute it.
//!
//! # Three things these handlers do not do
//!
//! Look the lease up, check ownership *of the record*, or maintain a deadline for
//! a sweep. The row predicate does the first two and the store holds the third,
//! which is precisely what makes a second replica ordinary (§5.8) and a restart of
//! this gear harmless (§5.8.2, invariant I7). A `renew` therefore lands correctly
//! on a replica that never saw the acquire.
//!
//! # The one check that *is* this service's
//!
//! The backend's lease methods are token-only by design (§5.8.1's normative
//! table), so verifying that the **transport caller** is the token's owner is the
//! serving gear's authorization decision (§4.6) and it lives here. What a failed
//! check returns differs by operation and neither answer leaks:
//!
//! | Operation | Foreign token | Why that answer |
//! |---|---|---|
//! | `renew` | `LockExpired` | Identical to a token matching nothing, so `renew` cannot be used to discover that a live lease exists under another owner |
//! | `release` | `Ok`, having done nothing | Identical to releasing an absent record, which §6.10 makes idempotent by absence. "An unauthorized release is an `Ok` that does nothing" (§12.6) |
//!
//! # Blocking `Lock` is the backend's wait, not this service's
//!
//! [`acquire_waiting`](cluster_sdk::DistributedLockBackend::acquire_waiting) does
//! the waiting. That is not delegation for tidiness: a lease that *lapses* writes
//! nothing, so no watch event announces it, and every waiter has to cap its wait
//! by the incumbent's observed deadline. The backends do — both cache-backed
//! defaults compute it, and the Postgres lock bounds it with its release-NOTIFY
//! heartbeat — and a wait re-implemented here would not, so it would sleep past a
//! lease it could have taken.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cluster_sdk::dto;
use cluster_sdk::grpc::stubs::lock as stubs;
use cluster_sdk::lease::LeaseToken;
use tokio::sync::Semaphore;
use tonic::{Request, Response, Status};

use super::{ServiceContext, checked_ttl, millis};

/// The largest blocking-acquire wait this service honours from the wire.
///
/// **Deliberately far below the lease-TTL / fence-retention ceiling**, unlike the
/// TTL clamp ([`checked_ttl`](super::checked_ttl)). A blocking `Lock` waiter is not
/// an idle task: the backend re-runs its acquire probe on a jittered delay capped
/// at its heartbeat (the Redis lock polls `SET NX` + `PTTL` at ~4–8 round trips a
/// second; §"Blocking `Lock`" below), so a parked waiter is *sustained* command-pool
/// load for its whole wait. Reusing the one-hour fence-retention ceiling here let a
/// single wire caller pin that load for an hour; capping the wait keeps each parked
/// task bounded. A caller that needs to wait longer re-issues `Lock` — which is also
/// its chance to observe a `stop()` or a deadline — rather than parking one task for
/// an hour. The [per-profile waiter cap](MAX_CONCURRENT_LOCK_WAITERS) bounds the
/// *count* of such tasks; this bounds each one's *duration*.
///
/// Tunable: a conservative default, not a tuned SLA. Raise or lower it per the
/// deployment's Redis-pool headroom and lock-contention profile.
const MAX_LOCK_WAIT: Duration = Duration::from_mins(2);

/// The most concurrent blocking `Lock` waiters this service admits **per profile** at
/// once. Beyond it, a `Lock` RPC for that profile is refused with
/// [`Status::resource_exhausted`] rather than parking yet another poller.
///
/// The per-waiter cost is bounded by [`MAX_LOCK_WAIT`]; this bounds their *count*,
/// so the standing command-pool load from a profile's blocking waiters is bounded
/// (≈ this × the per-waiter poll rate) no matter how contended one of its hot names
/// becomes. A refused caller sees a standard gRPC `RESOURCE_EXHAUSTED` (retryable
/// with backoff), the correct backpressure signal — not a lock outcome, so it is not
/// confused with contention or a lapse.
///
/// **Per profile, not service-wide**: one busy profile (tenant) exhausting its own
/// waiters cannot refuse another profile's `Lock` RPCs — availability is isolated
/// along the profile boundary the gear already routes on. A finer per-name cap is a
/// possible further refinement if one name within a profile must not starve its
/// siblings; the profile boundary is the one that matters for tenant isolation.
///
/// Tunable: a conservative default. Size it against the Redis command-pool capacity.
const MAX_CONCURRENT_LOCK_WAITERS: usize = 256;

/// The distributed-lock primitive, served over the wire.
#[derive(Debug, Clone)]
pub struct DistributedLockService {
    ctx: ServiceContext,
    /// One [`Semaphore`] of [`MAX_CONCURRENT_LOCK_WAITERS`] permits **per profile**,
    /// created on first blocking `Lock` for that profile. Shared across the
    /// per-connection clones tonic makes (via the `Arc`), so a profile's cap is
    /// gear-wide; a permit is held for the duration of one blocking acquire. The map
    /// only ever gains keys for *bound* profiles — `authorize` rejects an unbound
    /// profile before the permit path — so it cannot grow without bound and needs no
    /// reclaim.
    waiters: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
}

impl DistributedLockService {
    /// Builds the service over the shared [`ServiceContext`].
    #[must_use]
    pub fn new(ctx: ServiceContext) -> Self {
        Self {
            ctx,
            waiters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The per-profile blocking-waiter [`Semaphore`], created on first use. The map
    /// lock is held only to get-or-create the `Arc`, never across the acquire itself.
    fn profile_waiters(&self, profile: &str) -> Arc<Semaphore> {
        // Poison is recoverable here: the guarded value is a plain map, so a prior
        // panic while holding it left no invariant broken — take the map and carry on
        // (the pattern the gear's own state mutex uses).
        let mut map = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            map.entry(profile.to_owned())
                .or_insert_with(|| Arc::new(Semaphore::new(MAX_CONCURRENT_LOCK_WAITERS))),
        )
    }

    /// The acknowledgement a `renew` answers with — the registry generation,
    /// §5.6's staleness detector.
    fn renew_ack(&self) -> stubs::RenewResponse {
        stubs::RenewResponse::from(dto::RenewResponse {
            generation: self.ctx.profiles().generation(),
        })
    }

    /// The acknowledgement a `release` answers with.
    ///
    /// It reports nothing about whether a record matched, and that emptiness is
    /// load-bearing: reporting it would let a caller use `release` to probe
    /// whether a token was ever valid, which §5.8.1 forbids.
    fn release_ack(&self) -> stubs::ReleaseResponse {
        stubs::ReleaseResponse::from(dto::ReleaseResponse {
            generation: self.ctx.profiles().generation(),
        })
    }
}

#[tonic::async_trait]
impl stubs::distributed_lock_api_server::DistributedLockApi for DistributedLockService {
    async fn try_lock(
        &self,
        request: Request<stubs::TryLockRequest>,
    ) -> Result<Response<stubs::LockAcquired>, Status> {
        let (caller, bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let req = request.into_inner();
        // H8: the facade validates the name client-side; on the wire the server
        // must, or Profile 3 accepts names Profile 1 rejects (invariant I1). The
        // scope-aware rule is required here: `.scoped()` composes `prefix/name`,
        // and a bare `validate_cluster_name` (no `/`) would reject every scoped
        // lock — only in Profile 3 — which is the parity break it exists to avoid.
        cluster_sdk::validate_scoped_cluster_name(&req.name).map_err(cluster_sdk::to_status)?;

        // Insert-or-steal-if-lapsed. The backend bumps `fence` on every steal, so
        // a previous holder's token can never match again — which fences
        // a stale holder without this service remembering one (§5.8.1).
        let token = bound
            .lock
            .acquire(&req.name, &caller.mint_owner(), checked_ttl(req.ttl_ms)?)
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(acquired(token)))
    }

    /// Blocking acquire off the wire. The wait itself is the backend's
    /// ([`acquire_waiting`](cluster_sdk::DistributedLockBackend::acquire_waiting)).
    ///
    /// # Waiter load
    ///
    /// Every wire caller that blocks here parks a task that *polls* the backing
    /// store until the lease is takeable or the timeout elapses — the Redis lock
    /// re-runs `SET NX` + `PTTL` on a jittered delay capped at its heartbeat (~4–8
    /// round trips a second) — so a blocking waiter is sustained command-pool load
    /// for its whole wait. Two bounds keep that finite: the per-waiter wait is
    /// clamped to [`MAX_LOCK_WAIT`] (well below the fence-retention hour), and the
    /// *number* of concurrent waiters is capped **per profile** by a
    /// [`Semaphore`] of [`MAX_CONCURRENT_LOCK_WAITERS`] permits, so one busy profile
    /// cannot starve another's `Lock` RPCs. A caller that finds its profile's cap full
    /// is refused with [`Status::resource_exhausted`] — standard gRPC backpressure,
    /// retryable with backoff — rather than being parked as one more poller. The
    /// permit is held only for this call's wait and released on return, success or
    /// failure.
    async fn lock(
        &self,
        request: Request<stubs::LockRequest>,
    ) -> Result<Response<stubs::LockAcquired>, Status> {
        let (caller, bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let req = request.into_inner();
        // H8, as `try_lock`.
        cluster_sdk::validate_scoped_cluster_name(&req.name).map_err(cluster_sdk::to_status)?;

        // Admit this waiter only if *this profile's* cap has room; otherwise refuse
        // with backpressure rather than parking another poller on the command pool.
        // Resolved after `authorize`, so the map only ever keys bound profiles. The
        // permit is held for the whole blocking acquire and dropped on return.
        let _permit = self
            .profile_waiters(&req.profile)
            .try_acquire_owned()
            .map_err(|_| {
                // The refusal returns before the backend is touched, so it never
                // reaches `record_lock`; surface it here (WARN) so an operator can see
                // a profile saturating its blocking-waiter cap. A well-behaved caller
                // backs off on `ResourceExhausted`, so volume tracks genuine
                // saturation. `profile`/`name` are the same low-cardinality attributes
                // the other lock signals carry.
                tracing::warn!(
                    profile = %req.profile,
                    lock = %req.name,
                    cap = MAX_CONCURRENT_LOCK_WAITERS,
                    "cluster.lock.waiters_exhausted: refusing a blocking Lock; this profile is at \
                     its concurrent-waiter cap"
                );
                Status::resource_exhausted(
                    "too many concurrent blocking Lock waiters for this profile; retry with backoff",
                )
            })?;

        let token = bound
            .lock
            .acquire_waiting(
                &req.name,
                &caller.mint_owner(),
                // M3: both durations clamped server-side so a wire caller cannot
                // hold a lock, or park this task waiting for one, past any
                // legitimate bound.
                checked_ttl(req.ttl_ms)?,
                clamped_timeout(req.timeout_ms),
            )
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(acquired(token)))
    }

    async fn renew(
        &self,
        request: Request<stubs::LeaseRef>,
    ) -> Result<Response<stubs::RenewResponse>, Status> {
        let (caller, bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let lease = dto::LeaseRef::from(request.into_inner());
        let token = LeaseToken::from(lease.token);

        if !caller.owns(&token) {
            // Indistinguishable from a token that matched nothing, on purpose.
            return Err(cluster_sdk::to_status(
                cluster_sdk::ClusterError::LockExpired { name: token.name },
            ));
        }

        // Renewal resets the lease to `ttl` from now rather than extending it by
        // `ttl`, matching `LockGuard::renew`'s existing contract. A renewal that
        // names no TTL cannot be answered: the backend has no "the previous one"
        // to reach for, since it stores a deadline, not a duration.
        let ttl = lease.ttl_ms.ok_or_else(|| {
            // Routed through the codec, not a bare `Status`, so the rejection
            // ships a problem trailer and the client reconstructs a typed
            // `InvalidConfig` rather than `Provider{Other}` (M9, one-codec
            // invariant at `api::grpc::mod`).
            cluster_sdk::to_status(cluster_sdk::ClusterError::InvalidConfig {
                reason: "a lock renewal must carry `ttl_ms`".to_owned(),
            })
        })?;

        bound
            .lock
            // M3: clamped as the acquire paths are, so a renewal cannot lift the
            // TTL back past the ceiling `try_lock`/`lock` enforce; and zero is
            // rejected here as it is on acquire.
            .renew(&token, checked_ttl(ttl)?)
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(self.renew_ack()))
    }

    async fn release(
        &self,
        request: Request<stubs::LeaseRef>,
    ) -> Result<Response<stubs::ReleaseResponse>, Status> {
        let (caller, bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let lease = dto::LeaseRef::from(request.into_inner());
        let token = LeaseToken::from(lease.token);

        // A foreign token releases nothing and says so with the same `Ok` an
        // absent record gets. The backend would leave another holder's record
        // untouched anyway; not calling it is what keeps the two answers
        // identical in timing as well as in shape.
        if caller.owns(&token) {
            bound
                .lock
                .release(&token)
                .await
                .map_err(cluster_sdk::to_status)?;
        }

        Ok(Response::new(self.release_ack()))
    }
}

/// A blocking-acquire wait off the wire, clamped to [`MAX_LOCK_WAIT`].
fn clamped_timeout(timeout_ms: u64) -> Duration {
    millis(timeout_ms).min(MAX_LOCK_WAIT)
}

/// The minted lease, on the wire.
fn acquired(token: LeaseToken) -> stubs::LockAcquired {
    stubs::LockAcquired::from(dto::LockAcquired {
        token: dto::LeaseToken::from(token),
    })
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod lock_tests;
