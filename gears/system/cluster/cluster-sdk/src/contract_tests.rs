//! Tests for the `cluster.v1` contract traits.
//!
//! These assert against the **IR the macro emits**, not against hand-written
//! expectations of it. That is the point: `C1`'s exit criteria are about what the
//! contract pipeline sees — a security-plane context on every method, an
//! idempotency classification on every method, and no acquisition marked
//! retryable — and the IR is where all three are readable.

use toolkit_contract::descriptor::ContractKind;
use toolkit_contract::ir::contract::{FieldRole, Idempotency, MethodKind};

use super::{
    CLUSTER_CACHE_API_DESCRIPTOR, CLUSTER_PROFILE_API_DESCRIPTOR, DISTRIBUTED_LOCK_API_DESCRIPTOR,
    LEADER_ELECTION_API_DESCRIPTOR, cluster_cache_api_ir, cluster_profile_api_ir,
    distributed_lock_api_ir, leader_election_api_ir,
};

fn all_irs() -> Vec<toolkit_contract::ir::contract::ContractIr> {
    vec![
        cluster_cache_api_ir(),
        distributed_lock_api_ir(),
        leader_election_api_ir(),
        cluster_profile_api_ir(),
    ]
}

/// The four contracts, and the method count each must carry. A method added or
/// removed without updating this fails here rather than silently changing the wire.
#[test]
fn the_four_contracts_are_declared_with_their_expected_methods() {
    let expected = [
        ("ClusterCacheApi", 10),
        ("DistributedLockApi", 4),
        ("LeaderElectionApi", 4),
        ("ClusterProfileApi", 1),
    ];

    for (ir, (name, method_count)) in all_irs().iter().zip(expected) {
        assert_eq!(ir.name, name);
        assert_eq!(ir.gear, "cluster");
        assert_eq!(ir.version, "v1");
        assert_eq!(ir.methods.len(), method_count, "{name} method count");
    }
}

/// All four classify as `Api` — remote-capable, provided. The suffix is what
/// classifies them, and it must not collide with the plugin-facing `*Backend`
/// traits (§6.2, invariant I11).
#[test]
fn all_four_classify_as_provided_api_contracts() {
    for descriptor in [
        &CLUSTER_CACHE_API_DESCRIPTOR,
        &DISTRIBUTED_LOCK_API_DESCRIPTOR,
        &LEADER_ELECTION_API_DESCRIPTOR,
        &CLUSTER_PROFILE_API_DESCRIPTOR,
    ] {
        assert_eq!(
            descriptor.kind,
            ContractKind::Api,
            "{} is not classified as an Api contract",
            descriptor.contract
        );
        assert_eq!(descriptor.gear, "cluster");
        assert_eq!(descriptor.version, "v1");
    }
}

/// `C1`'s first exit criterion, half one: a security-plane context is the **first**
/// non-`self` parameter of every method, and the IR records it as such.
///
/// This is the assertion that catches the trap the `#[secctx]` attribute exists to
/// avoid: `#[toolkit::contract]`'s `ctx:`-name heuristic matches a type path ending
/// in the segment `SecurityContext`, which `PlatformSecurityContext` does not. Drop
/// the attribute and the context is classified `FieldRole::Wire` — and protogen
/// filters on exactly that role, so the credential would land *on the wire*. The
/// test fails loudly if that regresses.
#[test]
fn every_method_takes_a_security_context_first() {
    for ir in all_irs() {
        for method in &ir.methods {
            let first = method
                .input
                .fields
                .first()
                .unwrap_or_else(|| panic!("{}::{} has no parameters", ir.name, method.name));
            assert_eq!(
                first.role,
                FieldRole::SecurityContext,
                "{}::{}'s first parameter is not a security context — if this is `ctx`, the \
                 `#[secctx]` attribute is missing and the context is about to be serialised",
                ir.name,
                method.name
            );
            assert_eq!(
                method
                    .input
                    .fields
                    .iter()
                    .filter(|f| f.role == FieldRole::SecurityContext)
                    .count(),
                1,
                "{}::{} declares more than one security context",
                ir.name,
                method.name
            );
        }
    }
}

/// `C1`'s first exit criterion, half two. `#[toolkit::contract]` *defaults* an
/// un-annotated method to `NonIdempotentWrite`, so a missing annotation is silent
/// and would be indistinguishable from a deliberate one. The table below is
/// therefore explicit per method rather than a "not absent" check.
#[test]
fn every_method_declares_its_documented_idempotency() {
    let expected: &[(&str, &str, Idempotency)] = &[
        ("ClusterCacheApi", "get", Idempotency::SafeRead),
        ("ClusterCacheApi", "put", Idempotency::IdempotentWrite),
        (
            "ClusterCacheApi",
            "put_if_absent",
            Idempotency::NonIdempotentWrite,
        ),
        (
            "ClusterCacheApi",
            "compare_and_swap",
            Idempotency::NonIdempotentWrite,
        ),
        (
            "ClusterCacheApi",
            "compare_and_delete",
            Idempotency::IdempotentWrite,
        ),
        ("ClusterCacheApi", "delete", Idempotency::IdempotentWrite),
        ("ClusterCacheApi", "contains", Idempotency::SafeRead),
        ("ClusterCacheApi", "scan_prefix", Idempotency::SafeRead),
        ("ClusterCacheApi", "watch", Idempotency::SafeRead),
        ("ClusterCacheApi", "watch_prefix", Idempotency::SafeRead),
        (
            "DistributedLockApi",
            "try_lock",
            Idempotency::NonIdempotentWrite,
        ),
        (
            "DistributedLockApi",
            "lock",
            Idempotency::NonIdempotentWrite,
        ),
        ("DistributedLockApi", "renew", Idempotency::IdempotentWrite),
        (
            "DistributedLockApi",
            "release",
            Idempotency::IdempotentWrite,
        ),
        ("LeaderElectionApi", "join", Idempotency::NonIdempotentWrite),
        ("LeaderElectionApi", "renew", Idempotency::IdempotentWrite),
        ("LeaderElectionApi", "resign", Idempotency::IdempotentWrite),
        ("LeaderElectionApi", "await_change", Idempotency::SafeRead),
        (
            "ClusterProfileApi",
            "describe_profiles",
            Idempotency::SafeRead,
        ),
    ];

    let irs = all_irs();
    for (contract, method_name, idempotency) in expected {
        let ir = irs
            .iter()
            .find(|ir| ir.name == *contract)
            .unwrap_or_else(|| panic!("no contract named {contract}"));
        let method = ir
            .methods
            .iter()
            .find(|m| m.name == *method_name)
            .unwrap_or_else(|| panic!("{contract} has no method {method_name}"));
        assert_eq!(
            method.idempotency, *idempotency,
            "{contract}::{method_name} carries the wrong idempotency"
        );
    }

    // And the table covers every method, so a new one cannot slip in unclassified.
    let declared: usize = irs.iter().map(|ir| ir.methods.len()).sum();
    assert_eq!(
        declared,
        expected.len(),
        "a method is missing from the table"
    );
}

/// **No acquisition is idempotent.** A retried `try_lock`, `lock` or `join` whose
/// first response was lost reports contention or another leader against the
/// caller's *own* lease — a silent wrong answer, not an error (§6.10). This is the
/// contract-level half of the rule; `C2` adds the static check that no acquisition
/// carries `#[retryable]` on the projection.
#[test]
fn no_acquisition_method_is_classified_idempotent() {
    let acquisitions = [
        ("DistributedLockApi", "try_lock"),
        ("DistributedLockApi", "lock"),
        ("LeaderElectionApi", "join"),
        // `put_if_absent` and `compare_and_swap` are the same hazard one layer
        // down — they are what the lock and election paths are built on.
        ("ClusterCacheApi", "put_if_absent"),
        ("ClusterCacheApi", "compare_and_swap"),
    ];

    let irs = all_irs();
    for (contract, method_name) in acquisitions {
        let method = irs
            .iter()
            .find(|ir| ir.name == contract)
            .and_then(|ir| ir.methods.iter().find(|m| m.name == method_name))
            .unwrap_or_else(|| panic!("no {contract}::{method_name}"));
        assert_eq!(
            method.idempotency,
            Idempotency::NonIdempotentWrite,
            "{contract}::{method_name} is an acquisition and must never be retryable"
        );
    }
}

/// The three push-shaped operations are server-streaming and everything else is
/// unary — cluster needs no bidirectional streaming, so it needs no IR
/// extension (§6.2).
#[test]
fn exactly_the_three_push_shaped_operations_stream() {
    let streaming: Vec<(String, String)> = all_irs()
        .iter()
        .flat_map(|ir| {
            ir.methods
                .iter()
                .filter(|m| m.kind == MethodKind::ServerStreaming)
                .map(|m| (ir.name.clone(), m.name.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    assert_eq!(
        streaming,
        vec![
            ("ClusterCacheApi".to_owned(), "watch".to_owned()),
            ("ClusterCacheApi".to_owned(), "watch_prefix".to_owned()),
            ("LeaderElectionApi".to_owned(), "await_change".to_owned()),
        ]
    );
}

/// Every method returns a **named** type. `toolkit-contract-protogen`'s
/// `method_output_type` accepts only `TypeRef::Named` — a primitive return is an
/// explicit `PrimitiveMethodReturn` error and a unit return has no mapping at all —
/// so this is what keeps `C2` able to generate a `.proto` from this IR.
#[test]
fn every_method_returns_a_named_type() {
    use toolkit_contract::ir::contract::TypeRef;

    for ir in all_irs() {
        for method in &ir.methods {
            assert!(
                matches!(method.output, TypeRef::Named(_)),
                "{}::{} returns {:?}, which protogen cannot project",
                ir.name,
                method.name,
                method.output
            );
        }
    }
}

/// Exactly one wire parameter per method, which lets protogen reuse the
/// named request DTO as the proto message instead of synthesising one.
#[test]
fn every_method_takes_exactly_one_wire_parameter() {
    for ir in all_irs() {
        for method in &ir.methods {
            let wire: Vec<&str> = method
                .input
                .fields
                .iter()
                .filter(|f| f.role == FieldRole::Wire)
                .map(|f| f.name.as_str())
                .collect();
            assert_eq!(
                wire.len(),
                1,
                "{}::{} has wire parameters {wire:?}; protogen reuses a single named \
                 request DTO and synthesises a request type otherwise",
                ir.name,
                method.name
            );
        }
    }
}
