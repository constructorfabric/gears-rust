//! Canonical bytes, the fingerprint, and the scope hash. All pure.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;
use toolkit_gts::gts_id;
use uuid::Uuid;

use super::{
    FingerprintCandidate, FingerprintInput, canonical_text, idempotency_scope_hash,
    request_fingerprint,
};
use crate::domain::admission::Precondition;
use crate::domain::enums::{OperationKind, OwnershipScope, Plane};

fn candidate<'a>(gts_id: &'a str, body: &'a str) -> FingerprintCandidate<'a> {
    FingerprintCandidate {
        gts_id,
        canonical_body: body,
        precondition: Precondition::MustNotExist,
        force: false,
    }
}

fn input<'a>(candidates: &'a [FingerprintCandidate<'a>]) -> FingerprintInput<'a> {
    FingerprintInput {
        kind: OperationKind::Registration,
        dry_run: false,
        plane: Plane::Platform,
        tenant_id: None,
        principal_id: Uuid::nil(),
        ownership_scope: OwnershipScope::Global,
        candidates,
    }
}

// ---------------------------------------------------------------------------
// Canonical bytes
// ---------------------------------------------------------------------------

/// Key order in the input must not reach the output. This is the property the
/// whole fingerprint rests on, and the reason the sort is explicit rather than
/// borrowed from `serde_json`'s default map type.
#[test]
fn canonical_text_is_independent_of_input_key_order() {
    let a = json!({ "b": 1, "a": 2, "c": { "z": 1, "y": 2 } });
    let b = json!({ "c": { "y": 2, "z": 1 }, "a": 2, "b": 1 });
    assert_eq!(canonical_text(&a), canonical_text(&b));
    assert_eq!(canonical_text(&a), r#"{"a":2,"b":1,"c":{"y":2,"z":1}}"#);
}

/// Array order *is* significant — arrays are ordered in JSON, and `required` or
/// `allOf` mean different things reordered.
#[test]
fn array_order_is_preserved() {
    let a = json!({ "required": ["a", "b"] });
    let b = json!({ "required": ["b", "a"] });
    assert_ne!(canonical_text(&a), canonical_text(&b));
}

/// Canonicalization reaches inside arrays, not only top-level objects.
#[test]
fn objects_inside_arrays_are_canonicalized() {
    let a = json!({ "allOf": [{ "b": 1, "a": 2 }] });
    let b = json!({ "allOf": [{ "a": 2, "b": 1 }] });
    assert_eq!(canonical_text(&a), canonical_text(&b));
}

/// Numbers are **not** canonicalized, and the consequence is stated rather than
/// hidden: a reformatted number yields a fresh fingerprint, which is a `409` on a
/// reused key and never a false replay.
#[test]
fn number_spelling_is_not_canonicalized() {
    let a = json!({ "n": 1 });
    let b: serde_json::Value = serde_json::from_str(r#"{"n":1.0}"#).expect("parse");
    assert_ne!(canonical_text(&a), canonical_text(&b));
}

// ---------------------------------------------------------------------------
// The fingerprint
// ---------------------------------------------------------------------------

/// Identical inputs, computed twice, byte-identical. Trivial to state and the
/// property a replay depends on.
#[test]
fn the_fingerprint_is_deterministic() {
    let candidates = [candidate(gts_id!("acme.crm.customer.type.v1~"), "{}")];
    assert_eq!(
        request_fingerprint(&input(&candidates)),
        request_fingerprint(&input(&candidates))
    );
}

/// Every field the criterion names moves the fingerprint. Written as a table so a
/// field added to `FingerprintInput` without a case here is visible as an
/// omission rather than passing silently.
#[test]
fn every_covered_field_moves_the_fingerprint() {
    let base_candidates = [candidate(
        gts_id!("acme.crm.customer.type.v1~"),
        "{\"a\":1}",
    )];
    let base = request_fingerprint(&input(&base_candidates));

    // body
    let other_body = [candidate(
        gts_id!("acme.crm.customer.type.v1~"),
        "{\"a\":2}",
    )];
    assert_ne!(base, request_fingerprint(&input(&other_body)), "body");

    // identifier
    let other_id = [candidate(gts_id!("acme.crm.order.type.v1~"), "{\"a\":1}")];
    assert_ne!(base, request_fingerprint(&input(&other_id)), "identifier");

    // precondition
    let mut precondition = base_candidates;
    precondition[0].precondition = Precondition::Version(3);
    assert_ne!(
        base,
        request_fingerprint(&input(&precondition)),
        "precondition"
    );

    // force
    let mut forced = base_candidates;
    forced[0].force = true;
    assert_ne!(base, request_fingerprint(&input(&forced)), "force");

    // kind
    let mut kind = input(&base_candidates);
    kind.kind = OperationKind::Deletion;
    assert_ne!(base, request_fingerprint(&kind), "kind");

    // dry run
    let mut dry = input(&base_candidates);
    dry.dry_run = true;
    assert_ne!(base, request_fingerprint(&dry), "dry_run");

    // owner
    let mut owner = input(&base_candidates);
    owner.ownership_scope = OwnershipScope::Tenant;
    assert_ne!(base, request_fingerprint(&owner), "ownership_scope");

    // plane and tenant
    let mut plane = input(&base_candidates);
    plane.plane = Plane::Tenant;
    plane.tenant_id = Some(Uuid::from_u128(7));
    assert_ne!(base, request_fingerprint(&plane), "plane");

    // principal
    let mut principal = input(&base_candidates);
    principal.principal_id = Uuid::from_u128(9);
    assert_ne!(base, request_fingerprint(&principal), "principal");
}

/// A dry run and a commit under one key must be a **mismatch**, not a replay of
/// the dry-run result (`database.sql`). Restated on its own because it is the one
/// case a reader is most likely to assume is a replay.
#[test]
fn a_dry_run_never_fingerprints_as_its_commit() {
    let candidates = [candidate(gts_id!("acme.crm.customer.type.v1~"), "{}")];
    let mut dry = input(&candidates);
    dry.dry_run = true;
    assert_ne!(
        request_fingerprint(&input(&candidates)),
        request_fingerprint(&dry)
    );
}

/// Fields are length-prefixed, so a digest cannot confuse one split of two
/// adjacent fields with another. Without the prefix these two inputs hash the
/// same bytes.
#[test]
fn field_boundaries_cannot_be_confused() {
    let a = [candidate("ab", "c")];
    let b = [candidate("a", "bc")];
    assert_ne!(
        request_fingerprint(&input(&a)),
        request_fingerprint(&input(&b))
    );
}

/// A reordered batch is a different request: the candidates are hashed in order,
/// and the count is hashed too, so neither a permutation nor a repeat is silent.
#[test]
fn candidate_order_and_count_are_covered() {
    let first = candidate(gts_id!("acme.crm.a.type.v1~"), "{}");
    let second = candidate(gts_id!("acme.crm.b.type.v1~"), "{}");
    let forward = [first, second];
    let backward = [second, first];
    assert_ne!(
        request_fingerprint(&input(&forward)),
        request_fingerprint(&input(&backward))
    );

    let one = [first];
    assert_ne!(
        request_fingerprint(&input(&one)),
        request_fingerprint(&input(&forward))
    );
}

// ---------------------------------------------------------------------------
// The scope hash
// ---------------------------------------------------------------------------

/// All three inputs move the scope hash, which is what will separate key
/// namespaces once P1 supplies real values (ceiling C2).
#[test]
fn every_scope_input_moves_the_scope_hash() {
    let base = idempotency_scope_hash(Plane::Platform, None, Uuid::nil());
    assert_eq!(
        base,
        idempotency_scope_hash(Plane::Platform, None, Uuid::nil())
    );

    assert_ne!(
        base,
        idempotency_scope_hash(Plane::Tenant, Some(Uuid::from_u128(1)), Uuid::nil()),
        "plane and tenant",
    );
    assert_ne!(
        base,
        idempotency_scope_hash(Plane::Platform, None, Uuid::from_u128(2)),
        "principal",
    );
}

/// An absent tenant is not the same as a tenant whose UUID is all zeroes — the
/// length prefix is what keeps those apart, since `None` contributes no bytes.
#[test]
fn an_absent_tenant_differs_from_a_nil_tenant() {
    assert_ne!(
        idempotency_scope_hash(Plane::Tenant, None, Uuid::nil()),
        idempotency_scope_hash(Plane::Tenant, Some(Uuid::nil()), Uuid::nil()),
    );
}
