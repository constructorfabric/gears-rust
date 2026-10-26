//! [`Subtree`]: barrier-respecting reachability inside a tenant subtree.

use uuid::Uuid;

use super::Subtree;

const ROOT: Uuid = Uuid::from_u128(1);
const A: Uuid = Uuid::from_u128(2);
const B: Uuid = Uuid::from_u128(3);
const C: Uuid = Uuid::from_u128(4);

fn sorted(mut tenants: Vec<Uuid>) -> Vec<Uuid> {
    tenants.sort();
    tenants
}

#[test]
fn the_root_reaches_itself_even_when_it_is_self_managed() {
    // The root is not a descendant: a flag on it can only come from a
    // stray entry naming it, which is ignored.
    let tree = Subtree::new(ROOT, [(ROOT, A, true)]);
    assert!(tree.reaches(ROOT, ROOT));
    assert_eq!(tree.reachable(), vec![ROOT]);
    assert_eq!(tree.tenants(), vec![ROOT]);
}

#[test]
fn a_self_managed_child_is_cut_off_with_its_subtree_but_still_listed() {
    let tree = Subtree::new(ROOT, [(A, ROOT, true), (B, A, false), (C, ROOT, false)]);
    assert_eq!(tree.reachable(), vec![ROOT, C]);
    assert_eq!(sorted(tree.tenants()), sorted(vec![ROOT, A, B, C]));
    assert!(tree.contains(A) && tree.contains(B));
}

#[test]
fn reaching_resumes_below_a_self_managed_tenant_from_that_tenant() {
    let tree = Subtree::new(ROOT, [(A, ROOT, true), (B, A, false)]);
    assert!(tree.reaches(A, B));
    assert!(!tree.reaches(ROOT, B));
}

#[test]
fn a_barrier_strictly_between_blocks_reaching() {
    let tree = Subtree::new(ROOT, [(A, ROOT, false), (B, A, true), (C, B, false)]);
    assert!(!tree.reaches(ROOT, C));
    assert!(!tree.reaches(A, C));
    assert!(tree.reaches(B, C));
}

#[test]
fn a_self_managed_origin_reaches_its_own_descendants_and_itself() {
    let tree = Subtree::new(ROOT, [(A, ROOT, true), (B, A, false)]);
    assert!(tree.reaches(A, A));
    assert!(tree.reaches(A, B));
}

#[test]
fn a_target_that_is_not_below_from_is_not_reached() {
    let tree = Subtree::new(ROOT, [(A, ROOT, false), (B, ROOT, false)]);
    assert!(!tree.reaches(A, B));
    assert!(!tree.reaches(A, ROOT));
}

#[test]
fn a_descendant_whose_chain_misses_the_root_is_dropped() {
    let stray = Uuid::from_u128(99);
    let tree = Subtree::new(ROOT, [(A, ROOT, false), (B, stray, false)]);
    assert!(!tree.contains(B));
    assert_eq!(sorted(tree.tenants()), sorted(vec![ROOT, A]));
}

#[test]
fn a_cycle_detached_from_the_root_is_dropped_and_terminates() {
    let tree = Subtree::new(ROOT, [(A, B, false), (B, A, false), (C, ROOT, false)]);
    assert!(!tree.contains(A) && !tree.contains(B));
    assert_eq!(tree.reachable(), vec![ROOT, C]);
    assert!(!tree.reaches(ROOT, A));
}

#[test]
fn the_root_is_first_and_known() {
    let tree = Subtree::new(ROOT, [(A, ROOT, false)]);
    assert_eq!(tree.root(), ROOT);
    assert!(tree.contains(ROOT));
    assert_eq!(tree.tenants()[0], ROOT);
    assert_eq!(tree.reachable()[0], ROOT);
}
