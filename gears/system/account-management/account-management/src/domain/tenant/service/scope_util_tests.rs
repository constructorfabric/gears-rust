//! Unit tests for [`super::relax_barriers`]. Lives out-of-line so
//! `scope_util.rs` stays a thin helper file (DE1101 — tests in
//! separate files).

use super::*;
use toolkit_security::{ScopeValue, pep_properties};
use uuid::Uuid;

#[test]
fn unconstrained_scope_passes_through() {
    let scope = AccessScope::allow_all();
    let relaxed = relax_barriers(&scope);
    assert!(relaxed.is_unconstrained());
}

#[test]
fn deny_all_scope_passes_through() {
    let scope = AccessScope::deny_all();
    let relaxed = relax_barriers(&scope);
    assert!(relaxed.is_deny_all());
}

#[test]
fn in_tenant_subtree_flips_respect_to_ignore() {
    let root = Uuid::new_v4();
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
        InTenantSubtreeScopeFilter::new(pep_properties::RESOURCE_ID, root),
    )]));
    let relaxed = relax_barriers(&scope);
    let ScopeFilter::InTenantSubtree(its) = &relaxed.constraints()[0].filters()[0] else {
        panic!("expected InTenantSubtree filter after relax");
    };
    assert!(!its.respect_barriers(), "barrier knob must be flipped off");
    assert_eq!(its.property(), pep_properties::RESOURCE_ID);
    assert!(matches!(its.root_tenant_id(), ScopeValue::Uuid(u) if *u == root));
    assert!(its.descendant_status().is_empty());
}

#[test]
fn descendant_status_preserved_on_flip() {
    let root = Uuid::new_v4();
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
        InTenantSubtreeScopeFilter::with_descendant_status(
            pep_properties::RESOURCE_ID,
            root,
            true,
            vec![ScopeValue::Int(1), ScopeValue::Int(2)],
        ),
    )]));
    let relaxed = relax_barriers(&scope);
    let ScopeFilter::InTenantSubtree(its) = &relaxed.constraints()[0].filters()[0] else {
        panic!("expected InTenantSubtree filter after relax");
    };
    assert!(!its.respect_barriers());
    assert_eq!(its.descendant_status().len(), 2);
    assert!(matches!(its.descendant_status()[0], ScopeValue::Int(1)));
    assert!(matches!(its.descendant_status()[1], ScopeValue::Int(2)));
}

#[test]
fn non_subtree_filters_preserved_verbatim() {
    let tenant = Uuid::new_v4();
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
        ScopeFilter::eq(pep_properties::OWNER_ID, ScopeValue::Uuid(tenant)),
    ]));
    let relaxed = relax_barriers(&scope);
    assert_eq!(relaxed.constraints()[0].filters().len(), 2);
    assert!(matches!(
        &relaxed.constraints()[0].filters()[0],
        ScopeFilter::In(_)
    ));
    assert!(matches!(
        &relaxed.constraints()[0].filters()[1],
        ScopeFilter::Eq(_)
    ));
}

#[test]
fn or_of_constraints_relaxes_every_branch_independently() {
    // The production mock-PDP emits TWO constraints (one keyed on
    // OWNER_TENANT_ID, one on RESOURCE_ID) OR'd at the secure-ORM
    // boundary. relax_barriers must flip the barrier knob on every
    // branch so the carve-out covers every constraint path the
    // entity resolves.
    let root_a = Uuid::new_v4();
    let root_b = Uuid::new_v4();
    let scope = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
            InTenantSubtreeScopeFilter::new(pep_properties::OWNER_TENANT_ID, root_a),
        )]),
        ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
            InTenantSubtreeScopeFilter::new(pep_properties::RESOURCE_ID, root_b),
        )]),
    ]);
    let relaxed = relax_barriers(&scope);
    assert_eq!(relaxed.constraints().len(), 2);
    for c in relaxed.constraints() {
        let ScopeFilter::InTenantSubtree(its) = &c.filters()[0] else {
            panic!("expected InTenantSubtree in every branch");
        };
        assert!(!its.respect_barriers());
    }
}

#[test]
fn mixed_filter_constraint_only_subtree_flips() {
    // A constraint with a mix of filter kinds: only the
    // InTenantSubtree filter flips; the others are preserved.
    let root = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::InTenantSubtree(InTenantSubtreeScopeFilter::new(
            pep_properties::RESOURCE_ID,
            root,
        )),
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant]),
    ]));
    let relaxed = relax_barriers(&scope);
    let filters = relaxed.constraints()[0].filters();
    assert_eq!(filters.len(), 2);
    let ScopeFilter::InTenantSubtree(its) = &filters[0] else {
        panic!("first filter must remain InTenantSubtree");
    };
    assert!(!its.respect_barriers());
    assert!(matches!(&filters[1], ScopeFilter::In(_)));
}

/// A projection builds its own scope from live branches: removed empty
/// branches of the source are not carried over, on the early-return path
/// (all-empty) or the mapping path (mixed).
#[test]
fn removed_empty_branches_are_not_carried_into_the_relaxed_scope() {
    let root = Uuid::new_v4();
    let dead = ScopeConstraint::new(vec![
        ScopeFilter::InTenantSubtree(InTenantSubtreeScopeFilter::new(
            pep_properties::OWNER_TENANT_ID,
            root,
        )),
        ScopeFilter::in_uuids(pep_properties::RESOURCE_ID, vec![]),
    ]);
    let all_empty = AccessScope::from_constraints(vec![dead.clone()]);
    assert_eq!(relax_barriers(&all_empty), AccessScope::deny_all());

    let live = ScopeConstraint::new(vec![ScopeFilter::InTenantSubtree(
        InTenantSubtreeScopeFilter::new(pep_properties::OWNER_TENANT_ID, root),
    )]);
    let mixed = AccessScope::from_constraints(vec![dead, live]);
    let relaxed = relax_barriers(&mixed);
    assert_eq!(relaxed.constraints().len(), 1);
    assert!(relaxed.original_constraints().eq(relaxed.constraints()));
}
