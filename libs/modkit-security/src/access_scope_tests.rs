use super::*;
use uuid::Uuid;

const T1: &str = "11111111-1111-1111-1111-111111111111";
const T2: &str = "22222222-2222-2222-2222-222222222222";

fn uid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

#[test]
fn scope_filter_eq_constructor() {
    let f = ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, uid(T1));
    assert_eq!(f.property(), pep_properties::OWNER_TENANT_ID);
    assert!(matches!(f, ScopeFilter::Eq(_)));
    assert!(f.values().contains(&ScopeValue::Uuid(uid(T1))));
}

#[test]
fn all_values_for_works_with_eq() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        pep_properties::OWNER_TENANT_ID,
        uid(T1),
    )]));
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        &[uid(T1)]
    );
}

#[test]
fn all_values_for_works_with_mixed_eq_and_in() {
    let scope = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![ScopeFilter::eq(
            pep_properties::OWNER_TENANT_ID,
            uid(T1),
        )]),
        ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::OWNER_TENANT_ID,
            vec![uid(T2)],
        )]),
    ]);
    let values = scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID);
    assert_eq!(values, &[uid(T1), uid(T2)]);
}

#[test]
fn contains_value_works_with_eq() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        pep_properties::OWNER_TENANT_ID,
        uid(T1),
    )]));
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T1)));
    assert!(!scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T2)));
}

#[test]
fn tenant_only_strips_owner_id() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, uid(T1)),
        ScopeFilter::eq(pep_properties::OWNER_ID, uid(T2)),
    ]));

    let tenant_scope = scope.tenant_only();
    assert!(tenant_scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T1)));
    assert!(!tenant_scope.has_property(pep_properties::OWNER_ID));
}

#[test]
fn tenant_only_unconstrained_becomes_deny_all() {
    let scope = AccessScope::allow_all();
    let tenant_scope = scope.tenant_only();
    assert!(tenant_scope.is_deny_all());
}

#[test]
fn tenant_only_deny_all_when_no_tenant_filters() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        pep_properties::OWNER_ID,
        uid(T1),
    )]));

    let tenant_scope = scope.tenant_only();
    assert!(tenant_scope.is_deny_all());
}

#[test]
fn tenant_only_on_deny_all_stays_deny_all() {
    let scope = AccessScope::deny_all();
    let tenant_scope = scope.tenant_only();
    assert!(tenant_scope.is_deny_all());
}

#[test]
fn tenant_and_owner_keeps_both_properties() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, uid(T1)),
        ScopeFilter::eq(pep_properties::OWNER_ID, uid(T2)),
        ScopeFilter::eq(pep_properties::RESOURCE_ID, uid(T1)),
    ]));

    let narrowed = scope.tenant_and_owner();
    assert!(narrowed.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T1)));
    assert!(narrowed.contains_uuid(pep_properties::OWNER_ID, uid(T2)));
    assert!(!narrowed.has_property(pep_properties::RESOURCE_ID));
}

#[test]
fn tenant_and_owner_unconstrained_becomes_deny_all() {
    let scope = AccessScope::allow_all();
    assert!(scope.tenant_and_owner().is_deny_all());
}

#[test]
fn tenant_and_owner_deny_all_when_no_matching_filters() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        pep_properties::RESOURCE_ID,
        uid(T1),
    )]));
    assert!(scope.tenant_and_owner().is_deny_all());
}

#[test]
fn ensure_owner_adds_owner_when_missing() {
    let scope = AccessScope::for_tenant(uid(T1));
    let owner_id = uid(T2);

    let scoped = scope.ensure_owner(owner_id);
    assert!(scoped.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T1)));
    assert!(scoped.contains_uuid(pep_properties::OWNER_ID, owner_id));
}

#[test]
fn ensure_owner_keeps_existing_owner() {
    let existing_owner = uid(T2);
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, uid(T1)),
        ScopeFilter::eq(pep_properties::OWNER_ID, existing_owner),
    ]));

    let scoped = scope.ensure_owner(existing_owner);
    assert_eq!(
        scoped.all_uuid_values_for(pep_properties::OWNER_ID),
        &[existing_owner]
    );
}

#[test]
fn ensure_owner_on_unconstrained_creates_owner_scope() {
    let scope = AccessScope::allow_all();
    let owner_id = uid(T1);

    let scoped = scope.ensure_owner(owner_id);
    assert!(!scoped.is_unconstrained());
    assert!(scoped.contains_uuid(pep_properties::OWNER_ID, owner_id));
}

#[test]
fn ensure_owner_on_deny_all_stays_deny_all() {
    let scope = AccessScope::deny_all();
    let scoped = scope.ensure_owner(uid(T1));
    assert!(scoped.is_deny_all());
}

#[test]
fn ensure_owner_narrows_existing_owner_to_subject() {
    let user_a = uid(T1);
    let user_b = uid(T2);
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, uid(T1)),
        ScopeFilter::in_uuids(pep_properties::OWNER_ID, vec![user_a, user_b]),
    ]));

    let scoped = scope.ensure_owner(user_a);
    assert_eq!(
        scoped.all_uuid_values_for(pep_properties::OWNER_ID),
        &[user_a],
        "Must narrow to exactly the subject's owner_id"
    );
    assert!(scoped.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T1)));
}

#[test]
fn ensure_owner_drops_constraint_when_subject_not_in_pdp() {
    let user_x = uid(T1);
    let user_y = uid(T2);
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, uid(T1)),
        ScopeFilter::eq(pep_properties::OWNER_ID, user_x),
    ]));

    let scoped = scope.ensure_owner(user_y);
    assert!(
        scoped.is_deny_all(),
        "Must be deny-all when subject not in PDP's owner set"
    );
}

#[test]
fn ensure_owner_checks_all_owner_filters_in_constraint() {
    let alice = uid(T1);
    let bob = uid(T2);
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_ID, vec![alice, bob]),
        ScopeFilter::in_uuids(pep_properties::OWNER_ID, vec![bob]),
    ]));

    let scoped = scope.ensure_owner(alice);
    assert!(
        scoped.is_deny_all(),
        "Must deny when subject is missing from any owner_id filter"
    );

    let scoped = scope.ensure_owner(bob);
    assert!(!scoped.is_deny_all());
    assert_eq!(
        scoped.all_uuid_values_for(pep_properties::OWNER_ID),
        &[bob],
        "Must narrow to single Eq for the matching owner"
    );
}

#[test]
fn ensure_owner_multi_constraint_keeps_only_matching() {
    let alice = uid(T1);
    let bob = uid(T2);
    let tenant = uid(T1);

    let c1 = ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
        ScopeFilter::eq(pep_properties::OWNER_ID, alice),
    ]);
    let c2 = ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
        ScopeFilter::eq(pep_properties::OWNER_ID, bob),
    ]);

    let scope = AccessScope::from_constraints(vec![c1, c2]);
    let scoped = scope.ensure_owner(alice);

    assert!(
        !scoped.is_deny_all(),
        "Must not be deny-all - one constraint matches"
    );
    assert_eq!(
        scoped.all_uuid_values_for(pep_properties::OWNER_ID),
        &[alice],
        "Must keep only the constraint matching alice"
    );
    assert!(
        scoped.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant),
        "Tenant filter must be preserved"
    );
}

#[test]
fn contains_uuid_matches_string_variant() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        pep_properties::OWNER_TENANT_ID,
        ScopeValue::String(T1.to_owned()),
    )]));
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T1)));
    assert!(!scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T2)));
}

#[test]
fn contains_uuid_does_not_match_invalid_string() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        pep_properties::OWNER_TENANT_ID,
        ScopeValue::String("not-a-uuid".to_owned()),
    )]));
    assert!(!scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uid(T1)));
}
