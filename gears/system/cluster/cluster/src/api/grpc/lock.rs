// Created: 2026-08-12 by Constructor Tech
//! The distributed-lock service (DESIGN-DEPLOYABLE-GEAR §6.5, §12.6).
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

use cluster_sdk::dto;
use cluster_sdk::grpc::stubs::lock as stubs;
use cluster_sdk::lease::LeaseToken;
use tonic::{Request, Response, Status};

use super::{ServiceContext, millis};

/// The distributed-lock primitive, served over the wire.
#[derive(Debug, Clone)]
pub struct DistributedLockService {
    ctx: ServiceContext,
}

impl DistributedLockService {
    /// Builds the service over the shared [`ServiceContext`].
    #[must_use]
    pub fn new(ctx: ServiceContext) -> Self {
        Self { ctx }
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
        let (caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        // Insert-or-steal-if-lapsed. The backend bumps `fence` on every steal, so
        // a previous holder's token can never match again — which is what fences
        // a stale holder without this service remembering one (§5.8.1).
        let token = bound
            .lock
            .acquire(&req.name, &caller.mint_owner(), millis(req.ttl_ms))
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(acquired(token)))
    }

    async fn lock(
        &self,
        request: Request<stubs::LockRequest>,
    ) -> Result<Response<stubs::LockAcquired>, Status> {
        let (caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
        let req = request.into_inner();

        let token = bound
            .lock
            .acquire_waiting(
                &req.name,
                &caller.mint_owner(),
                millis(req.ttl_ms),
                millis(req.timeout_ms),
            )
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(acquired(token)))
    }

    async fn renew(
        &self,
        request: Request<stubs::LeaseRef>,
    ) -> Result<Response<stubs::RenewResponse>, Status> {
        let (caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
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
        let ttl = lease
            .ttl_ms
            .ok_or_else(|| Status::invalid_argument("a lock renewal must carry `ttl_ms`"))?;

        bound
            .lock
            .renew(&token, millis(ttl))
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(self.renew_ack()))
    }

    async fn release(
        &self,
        request: Request<stubs::LeaseRef>,
    ) -> Result<Response<stubs::ReleaseResponse>, Status> {
        let (caller, bound) = self
            .ctx
            .authorize(&request, &request.get_ref().profile)
            .await?;
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

/// The minted lease, on the wire.
fn acquired(token: LeaseToken) -> stubs::LockAcquired {
    stubs::LockAcquired::from(dto::LockAcquired {
        token: dto::LeaseToken::from(token),
    })
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod lock_tests;
