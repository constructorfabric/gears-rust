// Created: 2026-09-29 by Virtuozzo International GmbH
//! The bounded walk's parent links, and the chains read off them.

use std::collections::HashMap;

use uuid::Uuid;

use super::Subtree;

/// `target → a → b → c`, and `d` a second child of the target.
fn branch() -> (Subtree, [Uuid; 5]) {
    let ids: [Uuid; 5] = std::array::from_fn(|_| Uuid::new_v4());
    let [target, a, b, c, d] = ids;
    let subtree = Subtree {
        order: vec![a, d, b, c],
        parent: HashMap::from([(a, target), (d, target), (b, a), (c, b)]),
        truncated: false,
    };
    (subtree, ids)
}

#[test]
fn the_path_to_a_descendant_runs_from_the_walked_tenant_down_to_it() {
    let (subtree, [target, a, b, c, d]) = branch();
    assert_eq!(subtree.path_from(target, c), vec![target, a, b, c]);
    assert_eq!(subtree.path_from(target, a), vec![target, a]);
    assert_eq!(subtree.path_from(target, d), vec![target, d]);
    // The walked tenant's own path is itself: nothing above it is known here.
    assert_eq!(subtree.path_from(target, target), vec![target]);
}

#[test]
fn a_tenant_is_contained_exactly_when_it_was_walked() {
    let (subtree, [target, a, _, c, _]) = branch();
    assert!(subtree.contains(a));
    assert!(subtree.contains(c));
    // The walked tenant is above the subtree, not in it: a row of its own is
    // not a row below the target.
    assert!(!subtree.contains(target));
    assert!(!subtree.contains(Uuid::new_v4()));
}

#[test]
fn a_path_whose_links_do_not_reach_the_tenant_ends_where_they_end() {
    // Not a state a walk produces — every descendant was reached through the
    // links it records — but a caller handing in a tenant the walk never
    // started from gets the links it has, not a loop.
    let (subtree, [target, a, b, c, _]) = branch();
    let elsewhere = Uuid::new_v4();
    assert_eq!(subtree.path_from(elsewhere, c), vec![target, a, b, c]);
}
