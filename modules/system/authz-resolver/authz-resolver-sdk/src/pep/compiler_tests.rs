use super::*;
use crate::constraints::{EqPredicate, InPredicate};
use crate::models::EvaluationResponseContext;
use modkit_security::pep_properties;
use serde_json::json;
use uuid::Uuid;

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

/// Helper: UUID string as `serde_json::Value`.
fn jid(s: &str) -> serde_json::Value {
    json!(s)
}

const T1: &str = "11111111-1111-1111-1111-111111111111";
const T2: &str = "22222222-2222-2222-2222-222222222222";
const R1: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

const DEFAULT_PROPS: &[&str] = &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID];

// === Constraint Compilation Matrix Tests ===

#[test]
fn no_require_constraints_empty_returns_allow_all() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext::default(),
    };

    let scope = compile_to_access_scope(&response, false, DEFAULT_PROPS).unwrap();
    assert!(scope.is_unconstrained());
}

#[test]
fn no_require_constraints_with_constraints_compiles_them() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::Eq(EqPredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    value: jid(T1),
                })],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, false, DEFAULT_PROPS).unwrap();
    assert!(!scope.is_unconstrained());
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        &[uuid(T1)]
    );
}

#[test]
fn decision_true_require_constraints_empty_returns_error() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext::default(),
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    assert!(matches!(
        result,
        Err(ConstraintCompileError::ConstraintsRequiredButAbsent)
    ));
}

// === Constraint Compilation Tests ===

#[test]
fn single_tenant_eq_constraint() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::Eq(EqPredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    value: jid(T1),
                })],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        &[uuid(T1)]
    );
    assert!(
        scope
            .all_uuid_values_for(pep_properties::RESOURCE_ID)
            .is_empty()
    );

    // Verify Predicate::Eq produces ScopeFilter::Eq (not In)
    let filter = &scope.constraints()[0].filters()[0];
    assert!(matches!(filter, ScopeFilter::Eq(_)));
}

#[test]
fn multiple_tenants_in_constraint() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::In(InPredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    values: vec![jid(T1), jid(T2)],
                })],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        &[uuid(T1), uuid(T2)]
    );
}

#[test]
fn resource_id_eq_constraint() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::Eq(EqPredicate {
                    property: pep_properties::RESOURCE_ID.to_owned(),
                    value: jid(R1),
                })],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    assert!(
        scope
            .all_uuid_values_for(pep_properties::OWNER_TENANT_ID)
            .is_empty()
    );
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::RESOURCE_ID),
        &[uuid(R1)]
    );

    // Verify Predicate::Eq produces ScopeFilter::Eq
    let filter = &scope.constraints()[0].filters()[0];
    assert!(matches!(filter, ScopeFilter::Eq(_)));
}

#[test]
fn multiple_constraints_produce_or_scope() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![
                Constraint {
                    predicates: vec![Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T1)],
                    })],
                },
                Constraint {
                    predicates: vec![Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T2)],
                    })],
                },
            ],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    // Each constraint is a separate ScopeConstraint (ORed)
    assert_eq!(scope.constraints().len(), 2);
    // Both tenants accessible
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uuid(T1)));
    assert!(scope.contains_uuid(pep_properties::OWNER_TENANT_ID, uuid(T2)));
}

#[test]
fn unknown_predicate_fails_constraint() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::Eq(EqPredicate {
                    property: "unknown_property".to_owned(),
                    value: jid(T1),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    assert!(matches!(
        result,
        Err(ConstraintCompileError::AllConstraintsFailed { .. })
    ));
}

#[test]
fn mixed_known_and_unknown_constraints() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![
                // This constraint has an unknown property → fails
                Constraint {
                    predicates: vec![Predicate::Eq(EqPredicate {
                        property: "group_id".to_owned(),
                        value: jid(T1),
                    })],
                },
                // This constraint is valid → succeeds
                Constraint {
                    predicates: vec![Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T2)],
                    })],
                },
            ],
            ..Default::default()
        },
    };

    // Should succeed — the second constraint compiled
    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        &[uuid(T2)]
    );
}

#[test]
fn both_tenant_and_resource_in_single_constraint() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T1)],
                    }),
                    Predicate::Eq(EqPredicate {
                        property: pep_properties::RESOURCE_ID.to_owned(),
                        value: jid(R1),
                    }),
                ],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    // Single constraint with both properties (AND)
    assert_eq!(scope.constraints().len(), 1);
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        &[uuid(T1)]
    );
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::RESOURCE_ID),
        &[uuid(R1)]
    );
}

#[test]
fn mixed_shape_constraints_produce_or_scope() {
    // T1+R1 (AND) OR T2 — two different-shaped constraints
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![
                Constraint {
                    predicates: vec![
                        Predicate::In(InPredicate {
                            property: pep_properties::OWNER_TENANT_ID.to_owned(),
                            values: vec![jid(T1)],
                        }),
                        Predicate::Eq(EqPredicate {
                            property: pep_properties::RESOURCE_ID.to_owned(),
                            value: jid(R1),
                        }),
                    ],
                },
                Constraint {
                    predicates: vec![Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T2)],
                    })],
                },
            ],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    assert_eq!(scope.constraints().len(), 2);
    // First constraint has 2 filters (AND), second has 1 filter
    assert_eq!(scope.constraints()[0].filters().len(), 2);
    assert_eq!(scope.constraints()[1].filters().len(), 1);
}

#[test]
fn supported_properties_validation() {
    // Only owner_tenant_id is supported — id should fail
    let limited_props: &[&str] = &[pep_properties::OWNER_TENANT_ID];

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::Eq(EqPredicate {
                    property: pep_properties::RESOURCE_ID.to_owned(),
                    value: jid(R1),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, limited_props);
    assert!(matches!(
        result,
        Err(ConstraintCompileError::AllConstraintsFailed { .. })
    ));
}
