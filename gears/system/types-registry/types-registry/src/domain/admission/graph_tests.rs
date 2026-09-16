//! Unit tests for the pure batch ordering (T19, SPEC §8.1 steps 1–2).
//!
//! Every test here runs without a database, which is the point of the ordering
//! being a function of a candidate set: the cycle cases below are exactly the
//! ones a fixture database would make expensive to reach.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::{Value, json};
use toolkit_gts::gts_id;

use super::{
    BatchCandidate, BlockKind, CycleKind, DependencyLink, order_batch, order_deletion_batch,
};

const ROOT: &str = gts_id!("cf.core.batch.root.v1~");
const DERIVED: &str = gts_id!("cf.core.batch.root.v1~cf.core.batch.leaf.v1~");
const INSTANCE: &str = gts_id!("cf.core.batch.root.v1~cf.core.batch.first.v1");
const OTHER: &str = gts_id!("cf.core.batch.other.v1~");
const OUTSIDE: &str = gts_id!("cf.core.batch.outside.v1~");
const V1_0: &str = gts_id!("cf.core.batch.thing.v1.0~");
const V1_1: &str = gts_id!("cf.core.batch.thing.v1.1~");

fn schema(gts_id: &str, body: Value) -> Value {
    let mut doc = json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    });
    let Value::Object(extra) = body else {
        panic!("a schema body fixture must be an object");
    };
    for (key, value) in extra {
        doc[key] = value;
    }
    doc
}

/// A schema whose sole property `$ref`s `target`.
fn refs(gts_id: &str, target: &str) -> Value {
    schema(
        gts_id,
        json!({ "properties": { "other": { "$ref": format!("gts://{target}") } } }),
    )
}

fn candidate(gts_id: &str, content: Value) -> BatchCandidate {
    BatchCandidate {
        gts_id: gts_id.to_owned(),
        content: Some(content),
    }
}

/// The admitted identifiers in the order the worker will process them.
fn ordered_ids(candidates: &[BatchCandidate]) -> Vec<String> {
    order_batch(candidates)
        .order()
        .iter()
        .map(|&i| candidates[i].gts_id.clone())
        .collect()
}

/// The refused identifiers, sorted, so a cycle's member set is comparable.
fn cyclic_ids(candidates: &[BatchCandidate]) -> Vec<String> {
    let order = order_batch(candidates);
    let mut ids: Vec<String> = order
        .cyclic()
        .iter()
        .map(|member| candidates[member.index].gts_id.clone())
        .collect();
    ids.sort();
    ids
}

#[test]
fn an_empty_batch_orders_to_nothing() {
    let order = order_batch(&[]);
    assert!(order.order().is_empty());
    assert!(order.cyclic().is_empty());
}

#[test]
fn independent_candidates_all_appear_exactly_once() {
    let candidates = vec![
        candidate(OTHER, schema(OTHER, json!({}))),
        candidate(ROOT, schema(ROOT, json!({}))),
    ];
    let mut ids = ordered_ids(&candidates);
    ids.sort();
    assert_eq!(ids, vec![OTHER.to_owned(), ROOT.to_owned()]);
}

#[test]
fn a_ref_target_is_ordered_before_the_candidate_that_refs_it() {
    // Declared refs-first, so a pass-through of the input order would fail.
    let candidates = vec![
        candidate(ROOT, refs(ROOT, OTHER)),
        candidate(OTHER, schema(OTHER, json!({}))),
    ];
    assert_eq!(
        ordered_ids(&candidates),
        vec![OTHER.to_owned(), ROOT.to_owned()],
    );
}

#[test]
fn a_derived_candidate_is_ordered_after_its_base() {
    let candidates = vec![
        candidate(DERIVED, schema(DERIVED, json!({}))),
        candidate(ROOT, schema(ROOT, json!({}))),
    ];
    assert_eq!(
        ordered_ids(&candidates),
        vec![ROOT.to_owned(), DERIVED.to_owned()],
    );
}

/// Conformance closes no cycle, and is still an ordering edge: an Instance must
/// not commit ahead of a Type Schema that may then be refused (SPEC §8.1 step 1).
#[test]
fn an_instance_is_ordered_after_its_conforming_type() {
    let candidates = vec![
        candidate(INSTANCE, json!({ "name": "first" })),
        candidate(ROOT, schema(ROOT, json!({}))),
    ];
    assert_eq!(
        ordered_ids(&candidates),
        vec![ROOT.to_owned(), INSTANCE.to_owned()],
    );
}

#[test]
fn a_later_minor_is_ordered_after_its_predecessor_in_the_same_batch() {
    let candidates = vec![
        candidate(V1_1, schema(V1_1, json!({}))),
        candidate(V1_0, schema(V1_0, json!({}))),
    ];
    assert_eq!(
        ordered_ids(&candidates),
        vec![V1_0.to_owned(), V1_1.to_owned()],
    );
}

/// The blocker's kind is what chooses between the two refusal reasons, so it is
/// asserted rather than inferred from the ordering.
#[test]
fn the_two_blocking_edge_kinds_are_distinguished() {
    let candidates = vec![
        candidate(V1_0, schema(V1_0, json!({}))),
        candidate(V1_1, refs(V1_1, OTHER)),
        candidate(OTHER, schema(OTHER, json!({}))),
    ];
    let order = order_batch(&candidates);
    let kinds: Vec<BlockKind> = order.blockers(1).iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds,
        vec![BlockKind::Predecessor, BlockKind::Dependency],
        "v1.1~ waits on its predecessor and on the schema it $refs, in that order",
    );
    assert!(
        order.blockers(0).is_empty() && order.blockers(2).is_empty(),
        "neither v1.0~ nor the $ref target waits on anything in this batch",
    );
}

/// An edge whose target is not a candidate orders nothing: it is either already
/// committed or simply absent, and neither is this function's business.
#[test]
fn an_edge_leaving_the_batch_blocks_nothing() {
    let candidates = vec![candidate(ROOT, refs(ROOT, OUTSIDE))];
    assert_eq!(ordered_ids(&candidates), vec![ROOT.to_owned()]);
    assert!(order_batch(&candidates).blockers(0).is_empty());
}

/// The overlay is what makes this reachable: each candidate sees the other, so
/// nothing has refused the pair before the ordering does.
#[test]
fn a_ref_cycle_between_two_candidates_refuses_both() {
    let candidates = vec![
        candidate(ROOT, refs(ROOT, OTHER)),
        candidate(OTHER, refs(OTHER, ROOT)),
    ];
    assert_eq!(
        cyclic_ids(&candidates),
        vec![OTHER.to_owned(), ROOT.to_owned()]
    );
    assert!(
        order_batch(&candidates).order().is_empty(),
        "a cycle member is never ordered, so it is never evaluated",
    );
}

/// A `$ref`-only cycle check would order this batch and admit it: the base
/// `$ref`s the schema derived from it, and derivation supplies the return edge.
#[test]
fn a_cycle_mixing_a_ref_with_derivation_refuses_both() {
    let candidates = vec![
        candidate(ROOT, refs(ROOT, DERIVED)),
        candidate(DERIVED, schema(DERIVED, json!({}))),
    ];
    // Sorted: a base identifier is a byte prefix of everything derived from it.
    assert_eq!(
        cyclic_ids(&candidates),
        vec![ROOT.to_owned(), DERIVED.to_owned()],
    );
}

#[test]
fn a_self_referential_ref_refuses_its_candidate() {
    let candidates = vec![candidate(ROOT, refs(ROOT, ROOT))];
    assert_eq!(cyclic_ids(&candidates), vec![ROOT.to_owned()]);
    assert!(order_batch(&candidates).order().is_empty());
}

/// Conformance cannot close a cycle, so an Instance and its type are ordered
/// rather than refused — the property that makes the two edge sets differ.
#[test]
fn a_conformance_edge_never_makes_a_cycle() {
    let candidates = vec![
        candidate(ROOT, schema(ROOT, json!({}))),
        candidate(INSTANCE, json!({ "name": "first" })),
    ];
    assert!(order_batch(&candidates).cyclic().is_empty());
}

/// The pathological residue: `v1.0~` `$ref`s `v1.1~`, which waits on `v1.0~` as
/// its predecessor. No cycle exists in the `$ref`-and-derivation graph, so the
/// pair survives cycle detection and is caught by the ordering being impossible.
/// Refused rather than ordered arbitrarily.
#[test]
fn a_cycle_closed_only_by_the_predecessor_edge_refuses_both() {
    let candidates = vec![
        candidate(V1_0, refs(V1_0, V1_1)),
        candidate(V1_1, schema(V1_1, json!({}))),
    ];
    assert_eq!(
        cyclic_ids(&candidates),
        vec![V1_0.to_owned(), V1_1.to_owned()]
    );
    assert!(order_batch(&candidates).order().is_empty());
}

#[test]
fn a_predecessor_cycle_leaves_its_dependants_ordered() {
    let candidates = vec![
        candidate(ROOT, refs(ROOT, V1_0)),
        candidate(OTHER, refs(OTHER, ROOT)),
        candidate(V1_0, refs(V1_0, V1_1)),
        candidate(V1_1, schema(V1_1, json!({}))),
        candidate(OUTSIDE, schema(OUTSIDE, json!({}))),
    ];
    let order = order_batch(&candidates);

    assert_eq!(order.order(), &[0, 1, 4]);
    assert_eq!(order.cyclic().len(), 2);
    for (member, index) in order.cyclic().iter().zip([2, 3]) {
        assert_eq!(member.index, index);
        assert_eq!(member.kind, CycleKind::Unorderable);
        assert_eq!(member.cycle, [V1_0, V1_1]);
    }
}

#[test]
fn separate_predecessor_cycles_report_their_own_members() {
    const SECOND_ZERO: &str = gts_id!("cf.core.batch.second.v1.0~");
    const SECOND_ONE: &str = gts_id!("cf.core.batch.second.v1.1~");
    let candidates = vec![
        candidate(V1_0, refs(V1_0, V1_1)),
        candidate(V1_1, schema(V1_1, json!({}))),
        candidate(SECOND_ZERO, refs(SECOND_ZERO, SECOND_ONE)),
        candidate(SECOND_ONE, schema(SECOND_ONE, json!({}))),
    ];
    let order = order_batch(&candidates);

    assert!(order.order().is_empty());
    assert_eq!(order.cyclic().len(), 4);
    for (member, expected) in order.cyclic().iter().zip([
        [V1_0, V1_1],
        [V1_0, V1_1],
        [SECOND_ZERO, SECOND_ONE],
        [SECOND_ZERO, SECOND_ONE],
    ]) {
        assert_eq!(member.kind, CycleKind::Unorderable);
        assert_eq!(member.cycle, expected);
    }
}

#[test]
fn an_inlined_cycle_does_not_absorb_a_minor_waiting_on_it() {
    let candidates = vec![
        candidate(
            V1_0,
            schema(
                V1_0,
                json!({ "allOf": [
                { "$ref": format!("gts://{ROOT}") },
                { "$ref": format!("gts://{V1_1}") },
            ] }),
            ),
        ),
        candidate(ROOT, refs(ROOT, V1_0)),
        candidate(V1_1, schema(V1_1, json!({}))),
    ];
    let order = order_batch(&candidates);

    assert_eq!(order.order(), &[2]);
    assert_eq!(order.cyclic().len(), 2);
    for member in order.cyclic() {
        assert_eq!(member.kind, CycleKind::Inlined);
        assert_eq!(member.cycle, [ROOT, V1_0]);
    }
    assert_eq!(order.blockers(2)[0].kind, BlockKind::Predecessor);
}

/// A candidate whose stored payload does not parse as its identifier's shape
/// contributes no edge and is still ordered: its own evaluation refuses it, and
/// dropping it here would lose the outcome the operation owes it.
#[test]
fn an_unparsable_candidate_is_ordered_rather_than_dropped() {
    let candidates = vec![BatchCandidate {
        gts_id: "not a gts identifier".to_owned(),
        content: Some(json!({})),
    }];
    assert_eq!(order_batch(&candidates).order(), &[0]);
}

/// A candidate with no document contributes no edge, and that is deliberate
/// rather than an omission: the only kind that submits none is a deletion, whose
/// ordering is the **reverse** of a registration's — a dependant must go before
/// the base it consumes. Ordering deletions by the registration graph would be
/// exactly backwards, so T20 owns the question and this function stays silent on
/// it. Acceptance refuses every non-registration kind until then, so nothing
/// reaches here with `content: None`.
#[test]
fn a_candidate_without_content_carries_no_edge() {
    let candidates = vec![
        BatchCandidate {
            gts_id: DERIVED.to_owned(),
            content: None,
        },
        candidate(ROOT, schema(ROOT, json!({}))),
    ];
    assert_eq!(
        ordered_ids(&candidates),
        vec![DERIVED.to_owned(), ROOT.to_owned()],
        "nothing orders these two, so they keep their input order",
    );
    assert!(order_batch(&candidates).blockers(0).is_empty());
}

/// Same input, same output: the worker's processing order must not depend on
/// hash iteration, or a batch's outcome would vary between runs.
#[test]
fn the_ordering_is_deterministic_across_repeated_calls() {
    let candidates = vec![
        candidate(DERIVED, schema(DERIVED, json!({}))),
        candidate(OTHER, refs(OTHER, ROOT)),
        candidate(ROOT, schema(ROOT, json!({}))),
        candidate(INSTANCE, json!({ "name": "first" })),
    ];
    let first = ordered_ids(&candidates);
    for _ in 0..8 {
        assert_eq!(ordered_ids(&candidates), first);
    }
    assert_eq!(
        first[0], ROOT,
        "the only candidate nothing waits on is first"
    );
    assert_eq!(first.len(), 4);
}

/// Every candidate lands in exactly one of the two outputs. The worker indexes
/// its outcome vector by position and fills it from these two, so a candidate in
/// neither would be an operation that owes an outcome it never produces.
#[test]
fn every_candidate_is_either_ordered_or_refused_exactly_once() {
    let candidates = vec![
        candidate(ROOT, refs(ROOT, OTHER)),
        candidate(OTHER, refs(OTHER, ROOT)),
        candidate(DERIVED, schema(DERIVED, json!({}))),
        candidate(V1_0, schema(V1_0, json!({}))),
        candidate(V1_1, schema(V1_1, json!({}))),
        candidate(INSTANCE, json!({ "name": "first" })),
    ];
    let order = order_batch(&candidates);
    let mut covered: Vec<usize> = order.order().to_vec();
    covered.extend(order.cyclic().iter().map(|member| member.index));
    covered.sort_unstable();
    assert_eq!(covered, (0..candidates.len()).collect::<Vec<_>>());
    assert!(
        !order.cyclic().is_empty(),
        "the ROOT/OTHER pair must be refused, or this test proves only the easy half",
    );
}

// ---------------------------------------------------------------------------
// Deletion order (T20): the reverse relation, over stored edges
// ---------------------------------------------------------------------------

/// Identifiers in the order a batch would be deleted in.
fn deletion_order(ids: &[&str], edges: &[(&str, &str)]) -> Vec<String> {
    let owned: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    let links: Vec<DependencyLink> = edges
        .iter()
        .map(|(dependant, target)| DependencyLink {
            dependant: (*dependant).to_owned(),
            target: (*target).to_owned(),
        })
        .collect();
    order_deletion_batch(&owned, &links)
        .order()
        .iter()
        .map(|&index| owned[index].clone())
        .collect()
}

/// The whole point: a dependant is deleted **before** what it consumes. This is
/// the mirror of a registration's order, not the same order.
#[test]
fn a_dependant_is_deleted_before_its_target() {
    // Submitted target-first, which is the order that fails without this.
    assert_eq!(
        deletion_order(&[ROOT, OTHER], &[(OTHER, ROOT)]),
        vec![OTHER.to_owned(), ROOT.to_owned()],
    );
}

#[test]
fn a_chain_is_deleted_from_its_far_end() {
    assert_eq!(
        deletion_order(&[ROOT, OTHER, DERIVED], &[(OTHER, ROOT), (DERIVED, OTHER)]),
        vec![DERIVED.to_owned(), OTHER.to_owned(), ROOT.to_owned()],
    );
}

/// An edge whose other end is not in the batch orders nothing: that dependant
/// survives the deletion and is exactly what the commit-time recheck refuses on.
#[test]
fn an_edge_leaving_the_batch_does_not_order_a_deletion() {
    let order = deletion_order(&[ROOT], &[(OUTSIDE, ROOT)]);
    assert_eq!(order, vec![ROOT.to_owned()]);
}

#[test]
fn unrelated_deletions_keep_their_submission_order() {
    assert_eq!(
        deletion_order(&[ROOT, OTHER], &[]),
        vec![ROOT.to_owned(), OTHER.to_owned()],
    );
}

/// Every candidate is placed exactly once, whatever the edges say. A deletion
/// the order dropped would be an item the operation never answers.
#[test]
fn every_deletion_candidate_is_placed_exactly_once() {
    let ids = [ROOT, OTHER, DERIVED, V1_0, V1_1];
    let order = deletion_order(&ids, &[(OTHER, ROOT), (DERIVED, OTHER), (V1_1, V1_0)]);
    let mut sorted = order.clone();
    sorted.sort();
    let mut expected: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
    expected.sort();
    assert_eq!(sorted, expected);
    assert_eq!(order.len(), ids.len());
}

/// The committed dependency relation is acyclic (ADR-0012), so this shape is
/// corrupt state rather than a candidate error — and a deletion batch must not
/// lose an item over it. The cycle members are appended in submission order and
/// each still earns its own outcome from the commit-time recheck.
#[test]
fn a_cycle_in_stored_edges_still_places_every_candidate() {
    let mut sorted = deletion_order(&[ROOT, OTHER], &[(OTHER, ROOT), (ROOT, OTHER)]);
    sorted.sort();
    assert_eq!(sorted, vec![OTHER.to_owned(), ROOT.to_owned()]);
}

/// Same input, same output — the same requirement the registration order has.
#[test]
fn the_deletion_order_is_deterministic() {
    let ids = [ROOT, OTHER, DERIVED, V1_0];
    let edges = [(OTHER, ROOT), (DERIVED, OTHER)];
    let first = deletion_order(&ids, &edges);
    for _ in 0..8 {
        assert_eq!(deletion_order(&ids, &edges), first);
    }
}
