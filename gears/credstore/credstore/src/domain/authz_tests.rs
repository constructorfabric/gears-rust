//! Unit tests for the scope algebra over the tenant and row properties
//! (`secret_type`, `reference`; ADR-0010).

use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use uuid::Uuid;

use super::{
    REFERENCE_PROP, RowClamp, SECRET_TYPE_PROP, intersect_scopes, normalize_reference_filters,
    row_clamp, scope_admits_tenant,
};

fn tenant_filter(t: Uuid) -> ScopeFilter {
    ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![t])
}

fn type_filter(types: &[Uuid]) -> ScopeFilter {
    ScopeFilter::in_uuids(SECRET_TYPE_PROP, types.to_vec())
}

fn scope(constraints: Vec<Vec<ScopeFilter>>) -> AccessScope {
    AccessScope::from_constraints(constraints.into_iter().map(ScopeConstraint::new).collect())
}

fn ref_filter(refs: &[&str]) -> ScopeFilter {
    ScopeFilter::r#in(
        REFERENCE_PROP,
        refs.iter()
            .map(|r| toolkit_security::ScopeValue::String((*r).to_owned()))
            .collect(),
    )
}

fn clamp_of(alternatives: Vec<Vec<ScopeFilter>>) -> RowClamp {
    RowClamp::Constraints(alternatives.into_iter().map(ScopeConstraint::new).collect())
}

fn empty() -> RowClamp {
    RowClamp::Constraints(Vec::new())
}

#[test]
fn tenant_only_scope_admits_every_row() {
    let t = Uuid::new_v4();
    let s = scope(vec![vec![tenant_filter(t)]]);
    assert!(scope_admits_tenant(&s, t));
    assert_eq!(row_clamp(&s, t), RowClamp::Any);
    assert!(RowClamp::Any.admits(Uuid::new_v4(), "anything"));
    assert!(!RowClamp::Any.is_empty());
}

#[test]
fn empty_clamp_admits_nothing() {
    assert!(empty().is_empty());
    assert!(!empty().admits(Uuid::new_v4(), "x"));
}

#[test]
fn row_predicates_do_not_affect_the_tenant_gate_but_narrow_the_clamp() {
    let t = Uuid::new_v4();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let s = scope(vec![vec![
        tenant_filter(t),
        type_filter(&[a, b]),
        ref_filter(&["r"]),
    ]]);
    assert!(scope_admits_tenant(&s, t));
    assert!(!scope_admits_tenant(&s, Uuid::new_v4()));
    let clamp = row_clamp(&s, t);
    assert!(clamp.admits(a, "r") && clamp.admits(b, "r"));
    assert!(!clamp.admits(a, "other") && !clamp.admits(Uuid::new_v4(), "r"));
}

#[test]
fn type_only_clamp() {
    let t = Uuid::new_v4();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let clamp = row_clamp(&scope(vec![vec![tenant_filter(t), type_filter(&[a])]]), t);
    assert!(clamp.admits(a, "any") && !clamp.admits(b, "any"));
}

#[test]
fn reference_only_clamp() {
    let t = Uuid::new_v4();
    let clamp = row_clamp(
        &scope(vec![vec![tenant_filter(t), ref_filter(&["smtp-password"])]]),
        t,
    );
    assert!(clamp.admits(Uuid::new_v4(), "smtp-password"));
    assert!(!clamp.admits(Uuid::new_v4(), "other"));
}

#[test]
fn uuid_valued_reference_filters_are_normalised_to_lowercase_strings() {
    let t = Uuid::new_v4();
    let r = Uuid::new_v4();
    let s = scope(vec![vec![
        tenant_filter(t),
        ScopeFilter::in_uuids(REFERENCE_PROP, vec![r]),
    ]]);
    let normalised = normalize_reference_filters(&s);
    let filter = &normalised.constraints()[0].filters()[1];
    assert_eq!(
        filter.values().iter().cloned().collect::<Vec<_>>(),
        vec![toolkit_security::ScopeValue::String(r.to_string())]
    );
    // The tenant filter is untouched.
    assert!(normalised.contains_uuid(pep_properties::OWNER_TENANT_ID, t));

    let eq = scope(vec![vec![
        tenant_filter(t),
        ScopeFilter::eq(REFERENCE_PROP, r),
    ]]);
    let normalised = normalize_reference_filters(&eq);
    assert_eq!(
        normalised.constraints()[0].filters()[1]
            .values()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![toolkit_security::ScopeValue::String(r.to_string())]
    );

    // The clamp carries string values only, and admits the lowercase form.
    let clamp = row_clamp(&s, t);
    assert!(clamp.admits(Uuid::new_v4(), &r.to_string()));
    assert!(!clamp.admits(Uuid::new_v4(), &r.to_string().to_uppercase()));
    match clamp.to_scope().constraints()[0].filters()[0]
        .values()
        .iter()
        .next()
    {
        Some(toolkit_security::ScopeValue::String(_)) => {}
        other => panic!("expected a string value, got {other:?}"),
    }
}

#[test]
fn alternatives_are_or_of_ands() {
    let t = Uuid::new_v4();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    // (type a) OR (type b AND reference x)
    let clamp = row_clamp(
        &scope(vec![
            vec![tenant_filter(t), type_filter(&[a])],
            vec![tenant_filter(t), type_filter(&[b]), ref_filter(&["x"])],
        ]),
        t,
    );
    assert!(clamp.admits(a, "whatever"));
    assert!(clamp.admits(b, "x"));
    assert!(!clamp.admits(b, "y"));
    assert!(!clamp.admits(Uuid::new_v4(), "x"));
}

#[test]
fn mixed_type_or_reference_alternatives() {
    let t = Uuid::new_v4();
    let a = Uuid::new_v4();
    let clamp = row_clamp(
        &scope(vec![
            vec![tenant_filter(t), type_filter(&[a])],
            vec![tenant_filter(t), ref_filter(&["x"])],
        ]),
        t,
    );
    assert!(clamp.admits(a, "y") && clamp.admits(Uuid::new_v4(), "x"));
    assert!(!clamp.admits(Uuid::new_v4(), "y"));
}

#[test]
fn a_constraint_without_a_row_filter_makes_the_clamp_any() {
    let t = Uuid::new_v4();
    let broad = scope(vec![
        vec![tenant_filter(t), type_filter(&[Uuid::new_v4()])],
        vec![tenant_filter(t)],
    ]);
    assert_eq!(row_clamp(&broad, t), RowClamp::Any);
}

#[test]
fn a_constraint_without_a_tenant_filter_never_admits_the_tenant() {
    let t = Uuid::new_v4();
    let s = scope(vec![vec![type_filter(&[Uuid::new_v4()])]]);
    assert!(!scope_admits_tenant(&s, t));
    assert!(row_clamp(&s, t).is_empty());
}

#[test]
fn sibling_filters_below_tenant_granularity_fail_closed() {
    let t = Uuid::new_v4();
    let owner = ScopeFilter::in_uuids(pep_properties::OWNER_ID, vec![Uuid::new_v4()]);
    let s = scope(vec![vec![tenant_filter(t), owner, ref_filter(&["x"])]]);
    assert!(!scope_admits_tenant(&s, t));
    assert!(row_clamp(&s, t).is_empty());
}

#[test]
fn non_uuid_type_values_admit_nothing() {
    let t = Uuid::new_v4();
    let s = scope(vec![vec![
        tenant_filter(t),
        ScopeFilter::r#in(
            SECRET_TYPE_PROP,
            vec![toolkit_security::ScopeValue::String(
                "not-a-uuid".to_owned(),
            )],
        ),
    ]]);
    assert!(!row_clamp(&s, t).admits(Uuid::new_v4(), "x"));
}

#[test]
fn unknown_filter_kind_or_value_kind_fails_closed() {
    let t = Uuid::new_v4();
    let a = Uuid::new_v4();
    // Group filter on a row property: not evaluable in memory.
    let subtree = ScopeFilter::in_group_typed(
        REFERENCE_PROP,
        "gts.x.test.group.v1~",
        vec![Uuid::new_v4().into()],
    );
    let clamp = row_clamp(&scope(vec![vec![tenant_filter(t), subtree]]), t);
    assert!(!clamp.admits(a, "x"));
    // Integer value on the reference property.
    let ints = ScopeFilter::r#in(REFERENCE_PROP, vec![toolkit_security::ScopeValue::Int(1)]);
    let clamp = row_clamp(&scope(vec![vec![tenant_filter(t), ints]]), t);
    assert!(!clamp.admits(a, "1"));
}

#[test]
fn intersecting_scopes_intersects_their_rows() {
    let t = Uuid::new_v4();
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let left = scope(vec![vec![tenant_filter(t), type_filter(&[a, b])]]);
    let right = scope(vec![vec![
        tenant_filter(t),
        type_filter(&[b, c]),
        ref_filter(&["x"]),
    ]]);
    let both = intersect_scopes(&left, &right);
    assert!(scope_admits_tenant(&both, t));
    let clamp = row_clamp(&both, t);
    assert!(clamp.admits(b, "x") && !clamp.admits(a, "x") && !clamp.admits(b, "y"));

    assert!(row_clamp(&intersect_scopes(&AccessScope::allow_all(), &left), t).admits(a, "z"));
    assert!(intersect_scopes(&AccessScope::deny_all(), &left).is_deny_all());
}

#[test]
fn clamp_restriction_and_scope_projection() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let any_a = RowClamp::Any.restrict_to(Some(&[a]));
    assert!(any_a.admits(a, "x") && !any_a.admits(b, "x"));

    let by_ref = clamp_of(vec![vec![ref_filter(&["x"])], vec![type_filter(&[a, b])]]);
    let narrowed = by_ref.clone().restrict_to(Some(&[b]));
    assert!(narrowed.admits(b, "x") && narrowed.admits(b, "y"));
    assert!(!narrowed.admits(a, "x") && !narrowed.admits(a, "y"));

    assert_eq!(by_ref.clone().restrict_to(None), by_ref);

    assert!(RowClamp::Any.to_scope().is_unconstrained());
    assert!(empty().to_scope().is_deny_all());
    let scope = by_ref.to_scope();
    assert_eq!(scope.constraints().len(), 2);
}
