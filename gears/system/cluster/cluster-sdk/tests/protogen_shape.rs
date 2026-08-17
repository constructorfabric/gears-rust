// Created: 2026-08-12 by Constructor Tech
//! Does `toolkit-contract-protogen` digest the `cluster.v1` contract?
//!
//! This is the test that keeps the DTO layer projectable. Four of protogen's rules
//! are easy to violate by writing perfectly ordinary Rust, and each produces either
//! a hard rejection or — worse — a silently terrible encoding:
//!
//! | Rule | What violating it looks like |
//! |---|---|
//! | Method output must be `TypeRef::Named` | `-> Result<bool, _>` is `PrimitiveMethodReturn`; `-> Result<(), _>` has no mapping |
//! | A message must have **at least one field** | an empty response struct is `object without properties` |
//! | A `oneof` variant payload must be a `$ref` to a named message | a struct variant is `unknown primitive type "object"` |
//! | A byte field must publish `format: "byte"` | `Vec<u8>` renders as an integer array and projects to `repeated int64` — **no error at all**, just every byte as a tagged varint |
//!
//! The last one is why this test exists as a test rather than as a one-off check:
//! it is the only one that fails silently, and it sits on the cache hot path.
//!
//! # Everything under test is generated
//!
//! The contract IR comes from `#[toolkit::contract]`, the binding IR from
//! `#[toolkit::grpc_contract]`, the schemas from `schemars`. This test runs the same
//! `generate_proto_file` call `examples/gen_grpc_proto.rs` does, so a DTO change that
//! stops projecting fails here rather than at the next regeneration.
#![cfg(feature = "grpc-client")]

use cluster_sdk::contract::{
    cluster_cache_api_ir, cluster_profile_api_ir, distributed_lock_api_ir, leader_election_api_ir,
};
use cluster_sdk::dto::{
    CacheConsistencyDto, CacheDescriptor, CacheEntryDto, CacheFeaturesDto, CacheWatchEventDto,
    CacheWatchEventKind, CadRequest, CadResponse, CasRequest, CasResponse, ContainsRequest,
    ContainsResponse, DeleteRequest, DeleteResponse, DescribeProfilesRequest,
    DescribeProfilesResponse, GetRequest, GetResponse, JoinRequest, LeaderElectionDescriptor,
    LeaderElectionFeaturesDto, LeaderJoined, LeaderStatusDto, LeaderWatchEventDto,
    LeaderWatchEventKind, LeaseRef, LeaseToken, LockAcquired, LockDescriptor, LockFeaturesDto,
    LockRequest, ProfileDescriptor, ProfileHealth, PutIfAbsentResponse, PutRequest, PutResponse,
    ReleaseResponse, RenewResponse, ResignResponse, ScanRequest, ScanResponse, TryLockRequest,
    WatchPrefixRequest, WatchRequest, WireError,
};
use cluster_sdk::grpc::{
    cluster_cache_api_grpc_binding, cluster_profile_api_grpc_binding,
    distributed_lock_api_grpc_binding, leader_election_api_grpc_binding,
};
use schemars::{Schema, schema_for};
use toolkit_contract::ir::contract::ContractIr;
use toolkit_contract::ir::grpc::GrpcBindingIr;
use toolkit_contract_protogen::{ProtoLockfile, generate_proto_file};

/// Every named wire type, in one place. A DTO missing from here surfaces as
/// protogen's `unknown type reference`, which is the failure mode to want.
fn all_schemas() -> Vec<(&'static str, Schema)> {
    vec![
        // Shared
        ("LeaseToken", schema_for!(LeaseToken)),
        ("LeaseRef", schema_for!(LeaseRef)),
        ("WireError", schema_for!(WireError)),
        ("CacheEntryDto", schema_for!(CacheEntryDto)),
        // Cache
        ("GetRequest", schema_for!(GetRequest)),
        ("GetResponse", schema_for!(GetResponse)),
        ("PutRequest", schema_for!(PutRequest)),
        ("PutResponse", schema_for!(PutResponse)),
        ("PutIfAbsentResponse", schema_for!(PutIfAbsentResponse)),
        ("CasRequest", schema_for!(CasRequest)),
        ("CasResponse", schema_for!(CasResponse)),
        ("CadRequest", schema_for!(CadRequest)),
        ("CadResponse", schema_for!(CadResponse)),
        ("DeleteRequest", schema_for!(DeleteRequest)),
        ("DeleteResponse", schema_for!(DeleteResponse)),
        ("ContainsRequest", schema_for!(ContainsRequest)),
        ("ContainsResponse", schema_for!(ContainsResponse)),
        ("ScanRequest", schema_for!(ScanRequest)),
        ("ScanResponse", schema_for!(ScanResponse)),
        ("WatchRequest", schema_for!(WatchRequest)),
        ("WatchPrefixRequest", schema_for!(WatchPrefixRequest)),
        ("CacheWatchEventDto", schema_for!(CacheWatchEventDto)),
        ("CacheWatchEventKind", schema_for!(CacheWatchEventKind)),
        // Lock
        ("TryLockRequest", schema_for!(TryLockRequest)),
        ("LockRequest", schema_for!(LockRequest)),
        ("LockAcquired", schema_for!(LockAcquired)),
        ("RenewResponse", schema_for!(RenewResponse)),
        ("ReleaseResponse", schema_for!(ReleaseResponse)),
        // Leader election
        ("JoinRequest", schema_for!(JoinRequest)),
        ("LeaderJoined", schema_for!(LeaderJoined)),
        ("LeaderStatusDto", schema_for!(LeaderStatusDto)),
        ("ResignResponse", schema_for!(ResignResponse)),
        (
            "AwaitChangeRequest",
            schema_for!(cluster_sdk::dto::AwaitChangeRequest),
        ),
        ("LeaderWatchEventDto", schema_for!(LeaderWatchEventDto)),
        ("LeaderWatchEventKind", schema_for!(LeaderWatchEventKind)),
        // Profile
        (
            "DescribeProfilesRequest",
            schema_for!(DescribeProfilesRequest),
        ),
        (
            "DescribeProfilesResponse",
            schema_for!(DescribeProfilesResponse),
        ),
        ("ProfileDescriptor", schema_for!(ProfileDescriptor)),
        ("ProfileHealth", schema_for!(ProfileHealth)),
        ("CacheDescriptor", schema_for!(CacheDescriptor)),
        ("LockDescriptor", schema_for!(LockDescriptor)),
        (
            "LeaderElectionDescriptor",
            schema_for!(LeaderElectionDescriptor),
        ),
        ("CacheConsistencyDto", schema_for!(CacheConsistencyDto)),
        ("CacheFeaturesDto", schema_for!(CacheFeaturesDto)),
        ("LockFeaturesDto", schema_for!(LockFeaturesDto)),
        (
            "LeaderElectionFeaturesDto",
            schema_for!(LeaderElectionFeaturesDto),
        ),
    ]
}

/// The four contracts, each paired with the binding
/// `#[toolkit::grpc_contract]` emitted for it.
fn all_contracts() -> Vec<(ContractIr, GrpcBindingIr)> {
    vec![
        (cluster_cache_api_ir(), cluster_cache_api_grpc_binding()),
        (
            distributed_lock_api_ir(),
            distributed_lock_api_grpc_binding(),
        ),
        (leader_election_api_ir(), leader_election_api_grpc_binding()),
        (cluster_profile_api_ir(), cluster_profile_api_grpc_binding()),
    ]
}

fn generate(ir: &ContractIr, binding: &GrpcBindingIr) -> String {
    let schemas: Vec<(&str, Schema)> = all_schemas();
    generate_proto_file(ir, binding, &schemas, &mut ProtoLockfile::empty())
        .unwrap_or_else(|err| panic!("protogen rejected {}: {err}", ir.name))
}

#[test]
fn all_four_contracts_project_to_proto() {
    for (ir, binding) in all_contracts() {
        let proto = generate(&ir, &binding);
        assert!(
            proto.contains(&format!("service {}", ir.name)),
            "{} produced no service block",
            ir.name
        );
    }
}

/// The silent one. `Vec<u8>` must reach the wire as proto3 `bytes`, not as a
/// varint array — on the cache hot path the difference is roughly an order of
/// magnitude of payload.
#[test]
fn byte_fields_project_to_proto_bytes_not_repeated_ints() {
    let proto = generate(&cluster_cache_api_ir(), &cluster_cache_api_grpc_binding());

    for field in ["bytes value", "bytes new_value", "bytes expected_value"] {
        assert!(proto.contains(field), "missing `{field}` in:\n{proto}");
    }
    assert!(
        !proto.contains("repeated int64"),
        "a byte field is projecting as a varint array:\n{proto}"
    );
}

/// The security context must not appear on the wire. protogen filters
/// `FieldRole::SecurityContext`, which is only correct because the contract marks
/// the parameter with `#[secctx]` — the `ctx:`-name heuristic does not recognise
/// `PlatformSecurityContext`.
#[test]
fn the_security_context_never_reaches_the_wire() {
    for (ir, binding) in all_contracts() {
        let proto = generate(&ir, &binding);
        assert!(
            !proto.contains("SecurityContext") && !proto.contains("ctx"),
            "{} leaked the security context onto the wire:\n{proto}",
            ir.name
        );
    }
}

/// Watch streams are server-streaming, and nothing else is.
#[test]
fn only_the_push_shaped_operations_stream() {
    let cache = generate(&cluster_cache_api_ir(), &cluster_cache_api_grpc_binding());
    assert!(cache.contains("returns (stream CacheWatchEventDto)"));

    let leader = generate(
        &leader_election_api_ir(),
        &leader_election_api_grpc_binding(),
    );
    assert!(leader.contains("returns (stream LeaderWatchEventDto)"));

    let lock = generate(
        &distributed_lock_api_ir(),
        &distributed_lock_api_grpc_binding(),
    );
    assert!(
        !lock.contains("stream"),
        "the lock contract is unary throughout (section 6.5):\n{lock}"
    );
}

/// §6.11's skew rule: every projected enum carries an `_UNSPECIFIED = 0` sentinel,
/// so an unknown value from a newer server decodes to the default rather than
/// failing.
#[test]
fn projected_enums_carry_an_unspecified_sentinel() {
    let proto = generate(&cluster_cache_api_ir(), &cluster_cache_api_grpc_binding());
    assert!(
        proto.contains("CACHE_WATCH_EVENT_KIND_UNSPECIFIED = 0;"),
        "{proto}"
    );
}

/// `C2`'s exit criterion: **no acquisition method is retryable**, asserted against
/// the binding `#[toolkit::grpc_contract]` actually emitted.
///
/// `#[retryable]` is what licenses the generated client to retry. A retried
/// `try_lock`, `lock` or `join` whose first response was lost returns contention or
/// another leader — against the caller's *own* lease. That is a silent wrong answer,
/// not an error, and no other layer can detect it (§6.10).
///
/// `put_if_absent` and `compare_and_swap` are the same hazard one layer down: they
/// are what the lock and election paths are built on.
#[test]
fn no_acquisition_is_retryable() {
    const ACQUISITIONS: &[&str] = &[
        "try_lock",
        "lock",
        "join",
        "put_if_absent",
        "compare_and_swap",
    ];

    let mut checked = 0_usize;
    for (ir, binding) in all_contracts() {
        for method in &binding.methods {
            if ACQUISITIONS.contains(&method.method_name.as_str()) {
                assert!(
                    !method.retryable,
                    "{}::{} is an acquisition and must never carry #[retryable]",
                    ir.name, method.method_name
                );
                checked += 1;
            }
        }
    }
    assert_eq!(
        checked,
        ACQUISITIONS.len(),
        "an acquisition method disappeared from the contract; the rule would silently stop being checked"
    );
}

/// The same rule at the wire level: the emitted `.proto` must never mark an
/// acquisition idempotent, since `IDEMPOTENT` is what licenses a *conformant peer*
/// to retry — including one that is not this client.
#[test]
fn no_acquisition_is_marked_idempotent_on_the_wire() {
    let lock = generate(
        &distributed_lock_api_ir(),
        &distributed_lock_api_grpc_binding(),
    );
    for rpc in ["TryLock", "Lock"] {
        let block = rpc_block(&lock, rpc);
        assert!(
            block.contains("IDEMPOTENCY_UNKNOWN"),
            "{rpc} must not be advertised idempotent:\n{block}"
        );
    }

    let leader = generate(
        &leader_election_api_ir(),
        &leader_election_api_grpc_binding(),
    );
    let join = rpc_block(&leader, "Join");
    assert!(
        join.contains("IDEMPOTENCY_UNKNOWN"),
        "Join must not be advertised idempotent:\n{join}"
    );
}

/// The `rpc Name(...) { ... }` block for one RPC.
fn rpc_block(proto: &str, rpc_name: &str) -> String {
    let needle = format!("rpc {rpc_name}(");
    let start = proto
        .find(&needle)
        .unwrap_or_else(|| panic!("no rpc {rpc_name} in:\n{proto}"));
    let rest = &proto[start..];
    let end = rest.find("\n  }").map_or(rest.len(), |i| i + 4);
    rest[..end].to_owned()
}

// ---------------------------------------------------------------------------
// The committed artefacts are the generated ones
// ---------------------------------------------------------------------------

/// `proto.lock.toml` and the four `.proto` files are **committed generated
/// artefacts**, and this is what keeps them honest — the CI half of `C2`'s exit
/// criterion, expressed as a test so it needs no pipeline change.
///
/// A contract or DTO edit that changes the wire shape without a regeneration would
/// otherwise leave `build.rs` compiling a stale `.proto`: the Rust side would move
/// and the wire would not. That is the one failure mode `proto.lock.toml` cannot
/// catch by itself, because the lockfile only pins numbers for messages it has
/// already seen.
///
/// Fix a failure by running:
/// `cargo run -p cf-gears-cluster-sdk --features grpc-client --example gen_grpc_proto`
#[test]
fn the_committed_protos_match_what_the_contract_generates() {
    use std::path::PathBuf;

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lock_path = manifest.join("proto.lock.toml");
    assert!(
        lock_path.is_file(),
        "proto.lock.toml must be committed: it is the wire-compatibility contract, \
         not the crate version (section 6.11)"
    );

    // The regeneration shares one lockfile across all four contracts, exactly as
    // `gen_grpc_proto` does, so a message reachable from two contracts is numbered
    // identically in both packages.
    let mut lock = ProtoLockfile::load(&lock_path).expect("load proto.lock.toml");

    let files = [
        "cluster/cache/v1/cache.proto",
        "cluster/lock/v1/lock.proto",
        "cluster/leader/v1/leader.proto",
        "cluster/profile/v1/profile.proto",
    ];

    for ((ir, binding), relative) in all_contracts().into_iter().zip(files) {
        let regenerated = generate_proto_file(&ir, &binding, &all_schemas(), &mut lock)
            .unwrap_or_else(|err| panic!("regenerating {}: {err}", ir.name));
        let committed = std::fs::read_to_string(manifest.join("proto").join(relative))
            .unwrap_or_else(|err| panic!("reading committed {relative}: {err}"));
        assert_eq!(
            regenerated, committed,
            "{relative} is stale; regenerate with \
             `cargo run -p cf-gears-cluster-sdk --features grpc-client --example gen_grpc_proto`"
        );
    }

    // Regenerating must not have assigned a new field number: every message in the
    // committed protos is already in the lockfile, so a diff here means a field
    // moved rather than being added.
    let after = toml::to_string(&lock).expect("serialise lockfile");
    let before = std::fs::read_to_string(&lock_path).expect("read lockfile");
    assert_eq!(
        after.trim(),
        before.trim(),
        "proto.lock.toml is stale: field numbers changed, which invariant I12 forbids"
    );
}
