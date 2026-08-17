// Created: 2026-08-12 by Constructor Tech
//! The `cluster.v1` contract traits — DESIGN-DEPLOYABLE-GEAR §6.2, §12.1.
//!
//! **The wire mirrors the backend traits, not the facades.** Everything the
//! facades do locally — resolution, `scoped()`, cached `status()`, `is_leader()`,
//! `auto_restart`, the polling prefix-watch polyfill — stays above the §3.1 seam
//! and never reaches the wire (§6.3).
//!
//! Four contracts: one per coordination primitive, plus the profile/admin one
//! that makes the backends' synchronous accessors answerable remotely (§5.5).
//!
//! # The two-trait split is load-bearing
//!
//! `ContractKind::from_suffix` classifies a contract by its trait-name suffix, and
//! `is_remote_capable()` returns true for **`Api` and `Backend` alike**. Cluster's
//! plugin-facing traits are named [`ClusterCacheBackend`](crate::ClusterCacheBackend),
//! [`DistributedLockBackend`](crate::DistributedLockBackend) and
//! [`LeaderElectionBackend`](crate::LeaderElectionBackend). Annotating *those* with
//! `#[toolkit::contract]` plus a projection — rather than the separate `*Api`
//! traits here — would classify them remote-capable and push a security-context
//! parameter onto the trait **every plugin implements**, breaking
//! `cpt-cf-clst-nfr-plugin-stability` (invariant I11) for no benefit. `*Backend`
//! stays local and serde-free; `*Api` carries the wire contract (§6.2, ADR-011).
//!
//! # Every method takes a platform-plane security context, via `#[secctx]`
//!
//! `cpt-cf-binding-constraint-security-context` requires a security-plane context
//! as the first non-`self` argument of every method on a remote-capable contract.
//! Cluster takes the **platform-plane** form: cluster is platform-plane
//! infrastructure, and the context carries no tenant.
//!
//! The parameter is marked with the explicit `#[secctx]` attribute rather than
//! relying on the `ctx:`-name heuristic, and that is **required, not stylistic**.
//! `#[toolkit::contract]`'s heuristic matches a parameter named `ctx` whose type
//! path ends in the segment `SecurityContext` (`parse.rs`), and
//! `PlatformSecurityContext` does not — the idents are compared whole. Without the
//! attribute the context would be classified `FieldRole::Wire` and protogen would
//! put it *on the wire*, since it filters on exactly that role. The attribute is
//! consumed by the macro and never appears on the emitted trait.
//!
//! It costs nothing on the wire either way: the role filters it out of the
//! generated schema, so the credential still travels in gRPC metadata and resolves
//! server-side as §4.6 describes. It is a signature requirement, not a payload one.
//!
//! # Every method returns a named DTO
//!
//! Including the ones whose backend counterpart returns `()` or `bool` — see the
//! [`dto`](crate::dto) module docs for why the merged protogen requires it and why
//! invariant I12 wants it.
//!
//! # Annotations
//!
//! `#[idempotency(..)]` on every method feeds the IR (§6.10). The classification
//! is not cosmetic: **no acquisition method is idempotent**, because a retried
//! `try_lock` or `join` whose first response was lost reports contention against
//! the caller's *own* lease — a silent wrong answer rather than an error. The
//! `#[retryable]` marker that actually licenses a generated client to retry lives
//! on the gRPC projection (item `C2`), and must never appear on an acquisition.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::dto::{
    AwaitChangeRequest, CacheWatchEventDto, CadRequest, CadResponse, CasRequest, CasResponse,
    ContainsRequest, ContainsResponse, DeleteRequest, DeleteResponse, DescribeProfilesRequest,
    DescribeProfilesResponse, GetRequest, GetResponse, JoinRequest, LeaderJoined,
    LeaderWatchEventDto, LeaseRef, LockAcquired, LockRequest, PutIfAbsentResponse, PutRequest,
    PutResponse, ReleaseResponse, RenewResponse, ResignResponse, ScanRequest, ScanResponse,
    TryLockRequest, WatchPrefixRequest, WatchRequest,
};

/// The cache primitive over the wire.
#[toolkit::contract(gear = "cluster", version = "v1")]
pub trait ClusterCacheApi: Send + Sync {
    /// Read one key. A missing key is `Ok` with no entry, never an error.
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(SafeRead)]
    async fn get(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: GetRequest,
    ) -> Result<GetResponse, CanonicalError>;

    /// Store a value, overwriting if present.
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(IdempotentWrite)]
    async fn put(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: PutRequest,
    ) -> Result<PutResponse, CanonicalError>;

    /// Create a key only if absent.
    ///
    /// `NonIdempotentWrite`: a retry after a lost response returns "key existed",
    /// which the lock and election paths read as "someone else won" (§6.10).
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(NonIdempotentWrite)]
    async fn put_if_absent(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: PutRequest,
    ) -> Result<PutIfAbsentResponse, CanonicalError>;

    /// Version-guarded write. A retried CAS conflicts against its own successful
    /// write, so it is never auto-retried (§6.10).
    ///
    /// # Errors
    /// `cas_conflict` when the expected version no longer matches, or another
    /// `cluster.v1` code (§6.9).
    #[idempotency(NonIdempotentWrite)]
    async fn compare_and_swap(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: CasRequest,
    ) -> Result<CasResponse, CanonicalError>;

    /// Value-guarded delete.
    ///
    /// On the wire even though the backend trait defaults it: that default is a
    /// non-atomic `get`-then-`delete`, which is a real race over a network, and the
    /// CAS-based lock and leader release depend on this being atomic (§6.3).
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(IdempotentWrite)]
    async fn compare_and_delete(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: CadRequest,
    ) -> Result<CadResponse, CanonicalError>;

    /// Remove a key.
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(IdempotentWrite)]
    async fn delete(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: DeleteRequest,
    ) -> Result<DeleteResponse, CanonicalError>;

    /// Existence check.
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(SafeRead)]
    async fn contains(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: ContainsRequest,
    ) -> Result<ContainsResponse, CanonicalError>;

    /// Enumerate keys under a prefix, one page at a time. The backend trait's
    /// whole-`Vec` shape is reassembled client-side by looping pages (§6.4).
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(SafeRead)]
    async fn scan_prefix(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: ScanRequest,
    ) -> Result<ScanResponse, CanonicalError>;

    /// Watch one exact key. Server-push; the stream carries no RPC timeout (§6.10).
    #[idempotency(SafeRead)]
    #[streaming]
    fn watch(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: WatchRequest,
    ) -> Result<CacheWatchEventDto, CanonicalError>;

    /// Watch a key prefix. Server-push; the stream carries no RPC timeout (§6.10).
    #[idempotency(SafeRead)]
    #[streaming]
    fn watch_prefix(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: WatchPrefixRequest,
    ) -> Result<CacheWatchEventDto, CanonicalError>;
}

/// The distributed lock over the wire — unary throughout, against a store-owned
/// lease (§5.8.1, §6.5).
///
/// Every operation after the acquire is predicated on a
/// [`LeaseToken`](crate::dto::LeaseToken) rather than addressed by a handle, which
/// is what lets **any replica answer any lease operation** (invariant I7). There is
/// no session to look up, so nothing here is replica-local.
#[toolkit::contract(gear = "cluster", version = "v1")]
pub trait DistributedLockApi: Send + Sync {
    /// Insert-or-steal-if-expired, bumping `fence`.
    ///
    /// **Not retryable.** A lost response on a successful acquire would come back
    /// as `lock_contended` against the caller's own lease (§6.10).
    ///
    /// # Errors
    /// `lock_contended` when the lock is held, or another `cluster.v1` code (§6.9).
    #[idempotency(NonIdempotentWrite)]
    async fn try_lock(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: TryLockRequest,
    ) -> Result<LockAcquired, CanonicalError>;

    /// The same, but the server waits up to `timeout_ms` before giving up.
    /// Not retryable, for the same reason as [`try_lock`](Self::try_lock).
    ///
    /// # Errors
    /// `lock_timeout` on expiry, or another `cluster.v1` code (§6.9).
    #[idempotency(NonIdempotentWrite)]
    async fn lock(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LockRequest,
    ) -> Result<LockAcquired, CanonicalError>;

    /// Conditional write on `(name, owner, fence, deadline > now())`.
    ///
    /// Retry-safe — renewing twice is harmless — but absence is **not** `Ok` here:
    /// the caller has to learn it lost the lease, so a token matching nothing is
    /// `lock_expired`. This is the one lease operation where idempotency stops at
    /// the wire (§6.10).
    ///
    /// # Errors
    /// `lock_expired` when the token matches no live lease (§6.9).
    #[idempotency(IdempotentWrite)]
    async fn renew(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<RenewResponse, CanonicalError>;

    /// The same predicate, delete.
    ///
    /// **Idempotent by absence**: a token that matches nothing has already
    /// achieved what the caller wanted, so it is `Ok`. That is also what keeps the
    /// token unprobeable — both answers are the same `Ok` (§5.8.1, §6.10).
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(IdempotentWrite)]
    async fn release(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<ReleaseResponse, CanonicalError>;
}

/// Leader election over the wire — the lock's shape plus one subscription
/// (§6.6).
#[toolkit::contract(gear = "cluster", version = "v1")]
pub trait LeaderElectionApi: Send + Sync {
    /// Enrol in a named election, minting a lease.
    ///
    /// **Not retryable**: a retried `join` reports another leader when the caller
    /// *is* the leader (§6.10).
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(NonIdempotentWrite)]
    async fn join(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: JoinRequest,
    ) -> Result<LeaderJoined, CanonicalError>;

    /// Renew the claim. Absence means this instance no longer holds it — the
    /// client's event pump turns that into `Status(Lost)` and keeps the
    /// subscription open rather than surfacing an error (§6.6).
    ///
    /// # Errors
    /// `lock_expired` when the token matches no live claim (§6.9).
    #[idempotency(IdempotentWrite)]
    async fn renew(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<RenewResponse, CanonicalError>;

    /// Step down explicitly. Idempotent by absence, exactly as `release` is.
    ///
    /// # Errors
    /// A `CanonicalError` carrying a `cluster.v1` code (§6.9).
    #[idempotency(IdempotentWrite)]
    async fn resign(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<ResignResponse, CanonicalError>;

    /// Follow one election's transitions.
    ///
    /// Keyed by `election_id`, which addresses a **subscription** rather than a
    /// lease — the one piece of replica-local state, and therefore the one
    /// operation that can report its replica going away while the lease itself is
    /// untouched (§5.8.1, §6.9).
    #[idempotency(SafeRead)]
    #[streaming]
    fn await_change(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: AwaitChangeRequest,
    ) -> Result<LeaderWatchEventDto, CanonicalError>;
}

/// Profile discovery — the one call that makes a remote backend's *synchronous*
/// `consistency()` / `features()` / `provider_name()` answerable (§5.5).
#[toolkit::contract(gear = "cluster", version = "v1")]
pub trait ClusterProfileApi: Send + Sync {
    /// Describe the bound profiles, or a named subset.
    ///
    /// # Errors
    /// `profile_not_bound` when a named profile is not bound, or another
    /// `cluster.v1` code (§6.9).
    #[idempotency(SafeRead)]
    async fn describe_profiles(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: DescribeProfilesRequest,
    ) -> Result<DescribeProfilesResponse, CanonicalError>;
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod contract_tests;
