//! Regenerate the committed `cluster.*.v1` `.proto` files from the contract IR and
//! the gRPC binding IR.
//!
//! ```text
//! cargo run -p cf-gears-cluster-sdk --features grpc-client --example gen_grpc_proto
//! ```
//!
//! Both inputs are generated: `#[toolkit::contract]` emits the contract IR,
//! `#[toolkit::grpc_contract]` emits the binding IR, and `schemars` emits the type
//! schemas. Nothing here decides a wire shape — it only writes out what those three
//! already imply, with `proto.lock.toml` pinning every field number so a
//! regeneration can add fields but never move one (invariant I12).
//!
//! The output is committed, and `build.rs` compiles it. That split is deliberate:
//! a normal build needs no contract machinery, and a contract change that has not
//! been regenerated shows up as a reviewable diff here rather than as a silent
//! wire change.
//!
//! **One lockfile, four packages.** Each contract projects into its own package
//! (see `grpc.rs` for why), and a message reachable from two contracts — `LeaseRef`,
//! `LeaseToken`, `RenewResponse`, `WireError` — must carry the same field numbers in
//! both. Sharing one lockfile across the four generations is what guarantees that.

use std::fs;
use std::path::PathBuf;

use cluster_sdk::contract::{
    cluster_cache_api_ir, cluster_profile_api_ir, distributed_lock_api_ir, leader_election_api_ir,
};
use cluster_sdk::grpc::{
    cluster_cache_api_grpc_binding, cluster_profile_api_grpc_binding,
    distributed_lock_api_grpc_binding, leader_election_api_grpc_binding,
};
use toolkit_contract::ir::contract::ContractIr;
use toolkit_contract::ir::grpc::GrpcBindingIr;
use toolkit_contract_protogen::{ProtoLockfile, generate_proto_file};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lock_path = manifest.join("proto.lock.toml");
    let mut lock = ProtoLockfile::load(&lock_path)?;

    let contracts: [(ContractIr, GrpcBindingIr, &str); 4] = [
        (
            cluster_cache_api_ir(),
            cluster_cache_api_grpc_binding(),
            "cluster/cache/v1/cache.proto",
        ),
        (
            distributed_lock_api_ir(),
            distributed_lock_api_grpc_binding(),
            "cluster/lock/v1/lock.proto",
        ),
        (
            leader_election_api_ir(),
            leader_election_api_grpc_binding(),
            "cluster/leader/v1/leader.proto",
        ),
        (
            cluster_profile_api_ir(),
            cluster_profile_api_grpc_binding(),
            "cluster/profile/v1/profile.proto",
        ),
    ];

    for (contract, binding, relative) in contracts {
        let proto = generate_proto_file(&contract, &binding, &schemas(), &mut lock)?;
        let out = manifest.join("proto").join(relative);
        fs::create_dir_all(out.parent().ok_or("proto path has no parent")?)?;
        fs::write(&out, &proto)?;
        eprintln!("wrote {} ({} bytes)", out.display(), proto.len());
    }

    lock.save(&lock_path)?;
    eprintln!("updated {}", lock_path.display());
    Ok(())
}

/// Every named wire type. A DTO missing from here surfaces as protogen's
/// `unknown type reference`, which is the failure mode to want — the alternative
/// would be a silently truncated `.proto`.
fn schemas() -> Vec<(&'static str, schemars::Schema)> {
    use cluster_sdk::dto::{
        AwaitChangeRequest, CacheDescriptor, CacheWatchEventKind, CadRequest, CadResponse,
        CasRequest, CasResponse, ContainsRequest, ContainsResponse, DeleteRequest, DeleteResponse,
        DescribeProfilesRequest, DescribeProfilesResponse, GetRequest, GetResponse, JoinRequest,
        LeaderElectionDescriptor, LeaderJoined, LeaderWatchEventKind, LeaseRef, LeaseToken,
        LockAcquired, LockDescriptor, LockRequest, ProfileDescriptor, ProfileHealth,
        PutIfAbsentResponse, PutRequest, PutResponse, ReleaseResponse, RenewResponse,
        ResignResponse, ScanRequest, ScanResponse, TryLockRequest, WatchPrefixRequest,
        WatchRequest, WireCacheConsistency, WireCacheEntry, WireCacheFeatures, WireCacheWatchEvent,
        WireError, WireLeaderElectionFeatures, WireLeaderStatus, WireLeaderWatchEvent,
        WireLockFeatures,
    };
    use schemars::schema_for;

    macro_rules! schemas {
        ($($ty:ty),+ $(,)?) => {
            vec![ $((stringify!($ty), schema_for!($ty))),+ ]
        };
    }

    schemas!(
        // Shared
        LeaseToken,
        LeaseRef,
        WireError,
        WireCacheEntry,
        // Cache
        GetRequest,
        GetResponse,
        PutRequest,
        PutResponse,
        PutIfAbsentResponse,
        CasRequest,
        CasResponse,
        CadRequest,
        CadResponse,
        DeleteRequest,
        DeleteResponse,
        ContainsRequest,
        ContainsResponse,
        ScanRequest,
        ScanResponse,
        WatchRequest,
        WatchPrefixRequest,
        WireCacheWatchEvent,
        CacheWatchEventKind,
        // Lock
        TryLockRequest,
        LockRequest,
        LockAcquired,
        RenewResponse,
        ReleaseResponse,
        // Leader election
        JoinRequest,
        LeaderJoined,
        WireLeaderStatus,
        ResignResponse,
        AwaitChangeRequest,
        WireLeaderWatchEvent,
        LeaderWatchEventKind,
        // Profile
        DescribeProfilesRequest,
        DescribeProfilesResponse,
        ProfileDescriptor,
        ProfileHealth,
        CacheDescriptor,
        LockDescriptor,
        LeaderElectionDescriptor,
        WireCacheConsistency,
        WireCacheFeatures,
        WireLockFeatures,
        WireLeaderElectionFeatures,
    )
}
