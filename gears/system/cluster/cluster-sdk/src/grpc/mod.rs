//! The gRPC projection of the `cluster.v1` contract — DESIGN.md
//!
//! Nothing here is hand-rolled. `#[toolkit::grpc_contract]` generates the binding
//! IR and the client; `toolkit-contract-protogen` generates the `.proto` from that
//! binding plus the contract IR; `tonic-prost-build` compiles the `.proto` into
//! prost messages and both the `*_client` and `*_server` traits. What is
//! deliberately **not** generated is the gear's four service implementations —
//! gRPC server codegen is out of scope platform-wide, so those are hand-written by
//! design (item `S1`), not interim glue.
//!
//! # One package per contract, and why it is not one shared `cluster.v1`
//!
//! §12.1 writes `cluster.v1` throughout, and that reads as one package. It cannot
//! be, and the reason is mechanical: `generate_proto_file` takes **one** contract
//! and **one** binding, so it emits one service per file, and it emits every
//! message that service reaches. Two files declaring the same package and both
//! defining `LeaseRef` — as the lock and leader contracts both do — are duplicate
//! symbols, and `protoc` rejects them.
//!
//! So each contract gets its own package: `cluster.cache.v1`, `cluster.lock.v1`,
//! `cluster.leader.v1`, `cluster.profile.v1`. The consequence to know about is that
//! a message shared by two contracts becomes two *distinct* proto types, and
//! therefore two distinct prost types — so a `LeaseToken` crossing from the lock
//! stubs to the leader stubs needs a conversion. The DTO layer is the single
//! source of truth on the Rust side, which keeps that conversion trivial
//! and keeps it out of the service impls.
//!
//! **This does not weaken §6.11's versioning rule.** "Additive-only within
//! `cluster.v1`" is about the wire contract, and `proto.lock.toml` is what enforces
//! it — a single lockfile shared by all four generations, so a message appearing in
//! two packages carries identical field numbers in both.
//!
//! # `#[retryable]` appears nowhere
//!
//! Not on any method, and least of all on an acquisition. A retried `try_lock`,
//! `lock` or `join` whose first response was lost comes back as contention or
//! another leader — against the caller's *own* lease. That is a silent wrong
//! answer, not an error (§6.10). `no_acquisition_is_retryable` asserts it against the
//! generated binding rather than trusting this comment.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

use crate::contract::{ClusterCacheApi, ClusterProfileApi, DistributedLockApi, LeaderElectionApi};
use crate::dto::{
    AwaitChangeRequest, CadRequest, CadResponse, CasRequest, CasResponse, ContainsRequest,
    ContainsResponse, DeleteRequest, DeleteResponse, DescribeProfilesRequest,
    DescribeProfilesResponse, GetRequest, GetResponse, JoinRequest, LeaderJoined, LeaseRef,
    LockAcquired, LockRequest, PutIfAbsentResponse, PutRequest, PutResponse, ReleaseResponse,
    RenewResponse, ResignResponse, ScanRequest, ScanResponse, TryLockRequest, WatchPrefixRequest,
    WatchRequest, WireCacheWatchEvent, WireLeaderWatchEvent,
};

mod cross_package;

/// Prost messages and the `*_client` / `*_server` traits, compiled from the
/// committed `.proto` files by `build.rs`.
///
/// One module per contract package. The `*_server` traits here are what item `S1`
/// implements by hand.
pub mod stubs {
    /// `cluster.cache.v1`
    #[allow(clippy::all, clippy::pedantic, clippy::nursery, warnings)]
    pub mod cache {
        tonic::include_proto!("cluster.cache.v1");
    }
    /// `cluster.lock.v1`
    #[allow(clippy::all, clippy::pedantic, clippy::nursery, warnings)]
    pub mod lock {
        tonic::include_proto!("cluster.lock.v1");
    }
    /// `cluster.leader.v1`
    #[allow(clippy::all, clippy::pedantic, clippy::nursery, warnings)]
    pub mod leader {
        tonic::include_proto!("cluster.leader.v1");
    }
    /// `cluster.profile.v1`
    #[allow(clippy::all, clippy::pedantic, clippy::nursery, warnings)]
    pub mod profile {
        tonic::include_proto!("cluster.profile.v1");
    }
}

/// gRPC projection of [`ClusterCacheApi`].
#[toolkit::grpc_contract(
    package = "cluster.cache.v1",
    service = "ClusterCacheApi",
    stubs_module = "crate::grpc::stubs::cache"
)]
pub trait ClusterCacheApiGrpc: ClusterCacheApi {
    #[rpc(name = "Get")]
    #[idempotency_level(NoSideEffects)]
    #[retryable]
    async fn get(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: GetRequest,
    ) -> Result<GetResponse, CanonicalError>;

    #[rpc(name = "Put")]
    #[idempotency_level(Idempotent)]
    #[retryable]
    async fn put(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: PutRequest,
    ) -> Result<PutResponse, CanonicalError>;

    /// **No `#[retryable]`.** A retry after a lost response returns "key existed",
    /// which the lock and election paths read as "someone else won" (§6.10).
    #[rpc(name = "PutIfAbsent")]
    #[idempotency_level(NotIdempotent)]
    async fn put_if_absent(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: PutRequest,
    ) -> Result<PutIfAbsentResponse, CanonicalError>;

    /// **No `#[retryable]`.** A retried CAS conflicts against its own successful
    /// write (§6.10).
    #[rpc(name = "CompareAndSwap")]
    #[idempotency_level(NotIdempotent)]
    async fn compare_and_swap(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: CasRequest,
    ) -> Result<CasResponse, CanonicalError>;

    #[rpc(name = "CompareAndDelete")]
    #[idempotency_level(Idempotent)]
    #[retryable]
    async fn compare_and_delete(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: CadRequest,
    ) -> Result<CadResponse, CanonicalError>;

    #[rpc(name = "Delete")]
    #[idempotency_level(Idempotent)]
    #[retryable]
    async fn delete(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: DeleteRequest,
    ) -> Result<DeleteResponse, CanonicalError>;

    #[rpc(name = "Contains")]
    #[idempotency_level(NoSideEffects)]
    #[retryable]
    async fn contains(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: ContainsRequest,
    ) -> Result<ContainsResponse, CanonicalError>;

    #[rpc(name = "ScanPrefix")]
    #[idempotency_level(NoSideEffects)]
    #[retryable]
    async fn scan_prefix(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: ScanRequest,
    ) -> Result<ScanResponse, CanonicalError>;

    /// Server-push. Carries **no** RPC timeout, because §7.3 puts liveness on the
    /// consumer's own operations rather than on the transport ("the transport owes
    /// no keepalive"). Neither of this comment's former justifications held: no
    /// HTTP/2 keepalive is configured anywhere, and an RPC deadline does not sever
    /// a tonic 0.14 stream (measured). The rule stands regardless.
    #[rpc(name = "Watch")]
    #[idempotency_level(NoSideEffects)]
    #[streaming]
    fn watch(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: WatchRequest,
    ) -> Result<WireCacheWatchEvent, CanonicalError>;

    #[rpc(name = "WatchPrefix")]
    #[idempotency_level(NoSideEffects)]
    #[streaming]
    fn watch_prefix(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: WatchPrefixRequest,
    ) -> Result<WireCacheWatchEvent, CanonicalError>;
}

/// gRPC projection of [`DistributedLockApi`].
#[toolkit::grpc_contract(
    package = "cluster.lock.v1",
    service = "DistributedLockApi",
    stubs_module = "crate::grpc::stubs::lock"
)]
pub trait DistributedLockApiGrpc: DistributedLockApi {
    /// **Acquisition — never `#[retryable]`.** A lost response on a successful
    /// acquire comes back as `lock_contended` against the caller's own lease.
    #[rpc(name = "TryLock")]
    #[idempotency_level(NotIdempotent)]
    async fn try_lock(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: TryLockRequest,
    ) -> Result<LockAcquired, CanonicalError>;

    /// **Acquisition — never `#[retryable]`.**
    #[rpc(name = "Lock")]
    #[idempotency_level(NotIdempotent)]
    async fn lock(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LockRequest,
    ) -> Result<LockAcquired, CanonicalError>;

    #[rpc(name = "Renew")]
    #[idempotency_level(Idempotent)]
    #[retryable]
    async fn renew(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<RenewResponse, CanonicalError>;

    #[rpc(name = "Release")]
    #[idempotency_level(Idempotent)]
    #[retryable]
    async fn release(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<ReleaseResponse, CanonicalError>;
}

/// gRPC projection of [`LeaderElectionApi`].
#[toolkit::grpc_contract(
    package = "cluster.leader.v1",
    service = "LeaderElectionApi",
    stubs_module = "crate::grpc::stubs::leader"
)]
pub trait LeaderElectionApiGrpc: LeaderElectionApi {
    /// **Acquisition — never `#[retryable]`.** A retried `join` reports another
    /// leader when the caller *is* the leader (§6.10).
    #[rpc(name = "Join")]
    #[idempotency_level(NotIdempotent)]
    async fn join(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: JoinRequest,
    ) -> Result<LeaderJoined, CanonicalError>;

    #[rpc(name = "Renew")]
    #[idempotency_level(Idempotent)]
    #[retryable]
    async fn renew(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<RenewResponse, CanonicalError>;

    #[rpc(name = "Resign")]
    #[idempotency_level(Idempotent)]
    #[retryable]
    async fn resign(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: LeaseRef,
    ) -> Result<ResignResponse, CanonicalError>;

    /// Server-push, and likewise carries no RPC timeout (§6.10).
    #[rpc(name = "AwaitChange")]
    #[idempotency_level(NoSideEffects)]
    #[streaming]
    fn await_change(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: AwaitChangeRequest,
    ) -> Result<WireLeaderWatchEvent, CanonicalError>;
}

/// gRPC projection of [`ClusterProfileApi`].
#[toolkit::grpc_contract(
    package = "cluster.profile.v1",
    service = "ClusterProfileApi",
    stubs_module = "crate::grpc::stubs::profile"
)]
pub trait ClusterProfileApiGrpc: ClusterProfileApi {
    #[rpc(name = "DescribeProfiles")]
    #[idempotency_level(NoSideEffects)]
    #[retryable]
    async fn describe_profiles(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        req: DescribeProfilesRequest,
    ) -> Result<DescribeProfilesResponse, CanonicalError>;
}
