use super::*;
use modkit_security::{AccessScope, ScopeConstraint, ScopeFilter, ScopeValue, pep_properties};
use uuid::Uuid;

fn uid() -> Uuid {
    Uuid::new_v4()
}

#[test]
fn test_scope_to_sql_empty_scope_fail_closed() {
    let scope = AccessScope::deny_all();
    assert!(matches!(
        scope_to_sql(&scope),
        Err(ScopeTranslationError::EmptyScope)
    ));
}

#[test]
fn test_scope_to_sql_unconstrained_scope_fail_closed() {
    let scope = AccessScope::allow_all();
    assert!(matches!(
        scope_to_sql(&scope),
        Err(ScopeTranslationError::EmptyScope)
    ));
}

#[test]
fn test_scope_to_sql_single_group() {
    let tid = uid();
    let scope = AccessScope::for_tenant(tid);
    let (sql, params) = scope_to_sql(&scope).unwrap();
    assert!(sql.contains("tenant_id = ANY($1)"), "sql: {sql}");
    assert_eq!(params.len(), 1);
    assert_eq!(params[0], SqlValue::UuidArray(vec![tid]));
}

#[test]
fn test_scope_to_sql_multiple_groups_or_of_ands_preserved() {
    let tid1 = uid();
    let tid2 = uid();
    let scope = AccessScope::from_constraints(vec![
        ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::OWNER_TENANT_ID,
            vec![tid1],
        )]),
        ScopeConstraint::new(vec![ScopeFilter::in_uuids(
            pep_properties::OWNER_TENANT_ID,
            vec![tid2],
        )]),
    ]);
    let (sql, params) = scope_to_sql(&scope).unwrap();
    // Must have exactly one OR joining two AND-groups
    assert!(sql.contains(" OR "), "sql must contain OR: {sql}");
    assert_eq!(params.len(), 2, "each group contributes one bind param");
    // Must be wrapped in outer parens
    assert!(sql.starts_with('(') && sql.ends_with(')'), "sql: {sql}");
}

#[test]
fn test_scope_to_sql_ingroup_predicate_rejection() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_group(
        pep_properties::OWNER_TENANT_ID,
        vec![ScopeValue::Uuid(uid())],
    )]));
    match scope_to_sql(&scope) {
        Err(ScopeTranslationError::UnsupportedPredicate { kind }) => {
            assert!(kind.contains("InGroup"), "kind: {kind}");
        }
        other => panic!("expected UnsupportedPredicate, got: {other:?}"),
    }
}

#[test]
fn test_scope_to_sql_resource_id_filter() {
    let rid = uid();
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        PROP_RESOURCE_ID,
        vec![rid],
    )]));
    let (sql, params) = scope_to_sql(&scope).unwrap();
    assert!(sql.contains("resource_id = ANY($1)"), "sql: {sql}");
    assert_eq!(params[0], SqlValue::UuidArray(vec![rid]));
}

#[test]
fn test_scope_to_sql_resource_type_filter() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
        PROP_RESOURCE_TYPE,
        vec![ScopeValue::String("vm".to_owned())],
    )]));
    let (sql, params) = scope_to_sql(&scope).unwrap();
    assert!(sql.contains("resource_type = ANY($1)"), "sql: {sql}");
    assert_eq!(params[0], SqlValue::TextArray(vec!["vm".to_owned()]));
}

#[test]
fn test_scope_to_sql_module_filter() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
        PROP_MODULE,
        vec![ScopeValue::String("billing".to_owned())],
    )]));
    let (sql, params) = scope_to_sql(&scope).unwrap();
    assert!(sql.contains("module = ANY($1)"), "sql: {sql}");
    assert_eq!(params[0], SqlValue::TextArray(vec!["billing".to_owned()]));
}

#[test]
fn test_scope_to_sql_subject_id_filter() {
    let sid = uid();
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        PROP_SUBJECT_ID,
        vec![sid],
    )]));
    let (sql, params) = scope_to_sql(&scope).unwrap();
    assert!(sql.contains("subject_id = ANY($1)"), "sql: {sql}");
    assert_eq!(params[0], SqlValue::UuidArray(vec![sid]));
}

#[test]
fn test_scope_to_sql_subject_type_filter() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
        PROP_SUBJECT_TYPE,
        vec![ScopeValue::String("user".to_owned())],
    )]));
    let (sql, params) = scope_to_sql(&scope).unwrap();
    assert!(sql.contains("subject_type = ANY($1)"), "sql: {sql}");
    assert_eq!(params[0], SqlValue::TextArray(vec!["user".to_owned()]));
}

#[test]
fn test_scope_to_sql_rejects_uuid_value_on_text_column_eq() {
    // A PDP that surfaces a UUID-typed value against a text column must
    // produce an UnsupportedPredicate (mirrors `build_uuid_filter`'s
    // discipline) instead of silently stringifying the UUID.
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::eq(
        PROP_RESOURCE_TYPE,
        ScopeValue::Uuid(uid()),
    )]));
    match scope_to_sql(&scope) {
        Err(ScopeTranslationError::UnsupportedPredicate { kind }) => {
            assert!(kind.contains("non-text"), "kind: {kind}");
            assert!(kind.contains("resource_type"), "kind: {kind}");
        }
        other => panic!("expected UnsupportedPredicate, got: {other:?}"),
    }
}

#[test]
fn test_scope_to_sql_rejects_uuid_value_on_text_column_in() {
    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::r#in(
        PROP_SUBJECT_TYPE,
        vec![ScopeValue::Uuid(uid())],
    )]));
    match scope_to_sql(&scope) {
        Err(ScopeTranslationError::UnsupportedPredicate { kind }) => {
            assert!(kind.contains("non-text"), "kind: {kind}");
            assert!(kind.contains("subject_type"), "kind: {kind}");
        }
        other => panic!("expected UnsupportedPredicate, got: {other:?}"),
    }
}

#[test]
fn test_scope_to_sql_multi_predicate_and_within_group() {
    let tid = uid();
    let rid = uid();
    let scope = AccessScope::single(ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tid]),
        ScopeFilter::in_uuids(PROP_RESOURCE_ID, vec![rid]),
    ]));
    let (sql, params) = scope_to_sql(&scope).unwrap();
    assert!(
        sql.contains(" AND "),
        "sql must AND predicates within group: {sql}"
    );
    assert_eq!(params.len(), 2);
}
