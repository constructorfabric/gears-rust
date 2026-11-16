// Created: 2026-04-14 by Constructor Tech
use super::*;
use crate::constraints::{EqPredicate, InPredicate};
use crate::models::EvaluationResponseContext;
use serde_json::json;
use toolkit_security::pep_properties;
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

/// Low-level compilation with a group membership type and no capability
/// negotiation (default options).
fn compile_to_access_scope_with_resource_type(
    response: &EvaluationResponse,
    require_constraints: bool,
    supported_properties: &[&str],
    group_membership_type: Option<&str>,
) -> Result<AccessScope, ConstraintCompileError> {
    compile_to_access_scope_impl(
        response,
        require_constraints,
        supported_properties,
        group_membership_type,
        None,
        false,
    )
}

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

/// DESIGN.md, PEP requirement #7: a NON-empty constraints list containing a
/// degenerate `predicates: []` constraint is malformed and must fail closed —
/// it must never fold into `allow_all()`, which would turn a degenerate PDP
/// permit into unrestricted access. Pinned for both `require_constraints`
/// modes: the empty-outer-list gate at the top of compilation never sees this
/// shape.
#[test]
fn empty_predicates_constraint_fails_closed_never_allow_all() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint { predicates: vec![] }],
            ..Default::default()
        },
    };

    for require_constraints in [true, false] {
        let result = compile_to_access_scope(&response, require_constraints, DEFAULT_PROPS);
        let Err(ConstraintCompileError::AllConstraintsFailed { reason }) = result else {
            panic!(
                "empty-predicates constraint must fail closed \
                 (require_constraints={require_constraints}): {result:?}"
            );
        };
        assert!(reason.contains("empty predicates"));
    }
}

/// A degenerate empty-predicates constraint alongside a valid one is dropped
/// (constraints are OR-ed alternative grants — losing an unexecutable branch
/// only narrows access), while the valid constraint survives.
#[test]
fn empty_predicates_constraint_dropped_when_sibling_is_valid() {
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![
                Constraint {
                    predicates: vec![Predicate::Eq(EqPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        value: jid(T1),
                    })],
                },
                Constraint { predicates: vec![] },
            ],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    assert!(!scope.is_unconstrained());
    assert_eq!(scope.constraints().len(), 1);
    assert_eq!(
        scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
        &[uuid(T1)]
    );
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

    // Should succeed - the second constraint compiled
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
    // T1+R1 (AND) OR T2 - two different-shaped constraints
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

// === InGroup / InGroupSubtree Compilation Tests ===

const GROUP_MEMBERSHIP_TYPE: &str = "gts.example.core.resource.v1~";

fn compile_group_scope(
    response: &EvaluationResponse,
) -> Result<AccessScope, ConstraintCompileError> {
    compile_to_access_scope_with_resource_type(
        response,
        true,
        DEFAULT_PROPS,
        Some(GROUP_MEMBERSHIP_TYPE),
    )
}

#[test]
fn in_group_predicate_compiles_to_type_qualified_in_group_filter() {
    use crate::constraints::InGroupPredicate;

    let g1 = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T1)],
                    }),
                    Predicate::InGroup(InGroupPredicate {
                        property: pep_properties::RESOURCE_ID.to_owned(),
                        group_ids: vec![jid(g1)],
                    }),
                ],
            }],
            ..Default::default()
        },
    };

    let scope = compile_group_scope(&response).unwrap();
    assert_eq!(scope.constraints().len(), 1);
    let filter = &scope.constraints()[0].filters()[1];
    assert_eq!(filter.property(), pep_properties::RESOURCE_ID);
    let ScopeFilter::InGroup(filter) = filter else {
        panic!("expected InGroup filter, got: {filter:?}");
    };
    assert_eq!(filter.membership_resource_type(), GROUP_MEMBERSHIP_TYPE);
}

#[test]
fn in_group_subtree_predicate_compiles_to_type_qualified_subtree_filter() {
    use crate::constraints::InGroupSubtreePredicate;

    let ancestor = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T1)],
                    }),
                    Predicate::InGroupSubtree(InGroupSubtreePredicate {
                        property: pep_properties::RESOURCE_ID.to_owned(),
                        ancestor_ids: vec![jid(ancestor)],
                    }),
                ],
            }],
            ..Default::default()
        },
    };

    let scope = compile_group_scope(&response).unwrap();
    assert_eq!(scope.constraints().len(), 1);
    let filter = &scope.constraints()[0].filters()[1];
    let ScopeFilter::InGroupSubtree(filter) = filter else {
        panic!("expected InGroupSubtree filter, got: {filter:?}");
    };
    assert_eq!(filter.membership_resource_type(), GROUP_MEMBERSHIP_TYPE);
}

#[test]
fn native_group_predicate_without_membership_type_fails_closed() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T1)],
                    }),
                    Predicate::InGroup(InGroupPredicate::new(
                        pep_properties::RESOURCE_ID,
                        [uuid(R1)],
                    )),
                ],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    let Err(ConstraintCompileError::AllConstraintsFailed { reason }) = result else {
        panic!("untyped native group predicate must fail closed: {result:?}");
    };
    assert!(reason.contains("requires a canonical GTS resource type"));
}

#[test]
fn group_predicate_without_tenant_in_same_constraint_fails_closed() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InGroup(InGroupPredicate::new(
                    pep_properties::RESOURCE_ID,
                    [uuid(R1)],
                ))],
            }],
            ..Default::default()
        },
    };

    let result = compile_group_scope(&response);
    let Err(ConstraintCompileError::AllConstraintsFailed { reason }) = result else {
        panic!("group-only constraint must fail closed: {result:?}");
    };
    assert!(reason.contains("owner_tenant_id predicate in the same constraint"));
}

#[test]
fn malformed_group_uuid_fails_before_sql_generation() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [uuid(T1)],
                    )),
                    Predicate::InGroup(InGroupPredicate {
                        property: pep_properties::RESOURCE_ID.to_owned(),
                        group_ids: vec![json!("not-a-uuid")],
                    }),
                ],
            }],
            ..Default::default()
        },
    };

    let result = compile_group_scope(&response);
    let Err(ConstraintCompileError::AllConstraintsFailed { reason }) = result else {
        panic!("malformed group UUID must fail closed: {result:?}");
    };
    assert!(reason.contains("group_ids must contain UUID strings"));
}

#[test]
fn malformed_tenant_predicate_cannot_satisfy_group_tenant_guard() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::Eq(EqPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        value: json!(false),
                    }),
                    Predicate::InGroup(InGroupPredicate::new(
                        pep_properties::RESOURCE_ID,
                        [uuid(R1)],
                    )),
                ],
            }],
            ..Default::default()
        },
    };

    let result = compile_group_scope(&response);
    let Err(ConstraintCompileError::AllConstraintsFailed { reason }) = result else {
        panic!("malformed tenant scope must fail closed: {result:?}");
    };
    assert!(reason.contains("owner_tenant_id must contain UUID strings"));
}

#[test]
fn group_predicate_on_non_resource_property_fails_closed() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [uuid(T1)],
                    )),
                    Predicate::InGroup(InGroupPredicate::new(pep_properties::OWNER_ID, [uuid(R1)])),
                ],
            }],
            ..Default::default()
        },
    };
    let supported = &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::RESOURCE_ID,
        pep_properties::OWNER_ID,
    ];

    let result = compile_to_access_scope_with_resource_type(
        &response,
        true,
        supported,
        Some(GROUP_MEMBERSHIP_TYPE),
    );
    let Err(ConstraintCompileError::AllConstraintsFailed { reason }) = result else {
        panic!("group predicate on owner_id must fail closed: {result:?}");
    };
    assert!(reason.contains("must target 'id'"));
}

#[test]
fn unadvertised_native_group_predicate_fails_closed() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [uuid(T1)],
                    )),
                    Predicate::InGroup(InGroupPredicate::new(
                        pep_properties::RESOURCE_ID,
                        [uuid(R1)],
                    )),
                ],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope_with_negotiated_capabilities(
        &response,
        true,
        DEFAULT_PROPS,
        Some(GROUP_MEMBERSHIP_TYPE),
        &[Capability::TenantHierarchy],
        false,
    );
    let Err(ConstraintCompileError::UnadvertisedCapabilities { predicate, missing }) = result
    else {
        panic!("unadvertised group predicate must fail closed: {result:?}");
    };
    assert_eq!(predicate, "InGroup");
    assert_eq!(missing, vec!["group_membership"]);
}

#[test]
fn unadvertised_tenant_subtree_predicate_fails_closed() {
    use crate::constraints::InTenantSubtreePredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: jid(T1),
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope_with_negotiated_capabilities(
        &response,
        true,
        DEFAULT_PROPS,
        None,
        &[],
        false,
    );
    let Err(ConstraintCompileError::UnadvertisedCapabilities { predicate, missing }) = result
    else {
        panic!("unadvertised tenant subtree must fail closed: {result:?}");
    };
    assert_eq!(predicate, "InTenantSubtree");
    assert_eq!(missing, vec!["tenant_hierarchy"]);
}

/// A capability-negotiation failure mixed with a structural failure must
/// aggregate to the generic `AllConstraintsFailed` (joined reasons), not the
/// typed variant — the typed error is reserved for the case where every
/// failed constraint is a negotiation violation.
#[test]
fn mixed_unadvertised_and_structural_failures_aggregate_generic() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![
                // Fails on capability negotiation.
                Constraint {
                    predicates: vec![
                        Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [uuid(T1)],
                        )),
                        Predicate::InGroup(InGroupPredicate::new(
                            pep_properties::RESOURCE_ID,
                            [uuid(R1)],
                        )),
                    ],
                },
                // Fails structurally (unsupported property).
                Constraint {
                    predicates: vec![Predicate::Eq(EqPredicate::new(
                        "no_such_property",
                        uuid(T1),
                    ))],
                },
            ],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope_with_negotiated_capabilities(
        &response,
        true,
        DEFAULT_PROPS,
        Some(GROUP_MEMBERSHIP_TYPE),
        &[Capability::TenantHierarchy],
        false,
    );
    let Err(ConstraintCompileError::AllConstraintsFailed { reason }) = result else {
        panic!("mixed failures must aggregate to AllConstraintsFailed: {result:?}");
    };
    assert!(reason.contains("unadvertised capabilities: group_membership"));
    assert!(reason.contains("unsupported property: no_such_property"));
}

/// `InGroupSubtree` requires both group capabilities; with only
/// `GroupMembership` negotiated the typed error must name the one missing
/// capability, and with neither negotiated it must render both, exercising
/// the multi-element `missing` list.
#[test]
fn in_group_subtree_reports_missing_capability_set() {
    use crate::constraints::InGroupSubtreePredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [uuid(T1)],
                    )),
                    Predicate::InGroupSubtree(InGroupSubtreePredicate::new(
                        pep_properties::RESOURCE_ID,
                        [uuid(R1)],
                    )),
                ],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope_with_negotiated_capabilities(
        &response,
        true,
        DEFAULT_PROPS,
        Some(GROUP_MEMBERSHIP_TYPE),
        &[Capability::GroupMembership],
        false,
    );
    let Err(ConstraintCompileError::UnadvertisedCapabilities { predicate, missing }) = result
    else {
        panic!("subtree without GroupHierarchy must fail closed: {result:?}");
    };
    assert_eq!(predicate, "InGroupSubtree");
    assert_eq!(missing, vec!["group_hierarchy"]);

    let result = compile_to_access_scope_with_negotiated_capabilities(
        &response,
        true,
        DEFAULT_PROPS,
        Some(GROUP_MEMBERSHIP_TYPE),
        &[],
        false,
    );
    let Err(error @ ConstraintCompileError::UnadvertisedCapabilities { .. }) = result else {
        panic!("subtree without either capability must fail closed: {result:?}");
    };
    assert_eq!(
        error.to_string(),
        "InGroupSubtree predicate requires unadvertised capabilities: \
         group_membership, group_hierarchy (fail-closed)"
    );
}

#[test]
fn tenant_and_group_in_separate_or_branches_drops_unsafe_group_branch() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![
                Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [uuid(T1)],
                    ))],
                },
                Constraint {
                    predicates: vec![Predicate::InGroup(InGroupPredicate::new(
                        pep_properties::RESOURCE_ID,
                        [uuid(R1)],
                    ))],
                },
            ],
            ..Default::default()
        },
    };

    let scope = compile_group_scope(&response).expect("safe tenant branch should remain");
    assert_eq!(scope.constraints().len(), 1);
    assert_eq!(scope.constraints()[0].filters().len(), 1);
    assert_eq!(
        scope.constraints()[0].filters()[0].property(),
        pep_properties::OWNER_TENANT_ID
    );
}

#[test]
fn tenant_plus_in_group_in_single_constraint() {
    use crate::constraints::InGroupPredicate;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![
                    Predicate::In(InPredicate {
                        property: pep_properties::OWNER_TENANT_ID.to_owned(),
                        values: vec![jid(T1)],
                    }),
                    Predicate::InGroup(InGroupPredicate {
                        property: pep_properties::RESOURCE_ID.to_owned(),
                        group_ids: vec![jid(R1)],
                    }),
                ],
            }],
            ..Default::default()
        },
    };

    let scope = compile_group_scope(&response).unwrap();
    assert_eq!(scope.constraints().len(), 1);
    // Constraint should have 2 filters: In(tenant) AND InGroup(resource)
    assert_eq!(scope.constraints()[0].filters().len(), 2);
}

#[test]
fn supported_properties_validation() {
    // Only owner_tenant_id is supported - id should fail
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

// === Empty value lists are valid, constrained predicates that match nothing ===

fn respond(constraints: Vec<Constraint>) -> EvaluationResponse {
    EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints,
            ..Default::default()
        },
    }
}

fn empty_tenant_set() -> Constraint {
    Constraint {
        predicates: vec![Predicate::In(InPredicate {
            property: pep_properties::OWNER_TENANT_ID.to_owned(),
            values: vec![],
        })],
    }
}

fn tenant_eq(tenant: &str) -> Constraint {
    Constraint {
        predicates: vec![Predicate::Eq(EqPredicate {
            property: pep_properties::OWNER_TENANT_ID.to_owned(),
            value: jid(tenant),
        })],
    }
}

/// Semantically invalid branch (unsupported property) next to valid ones.
fn invalid_branch() -> Constraint {
    Constraint {
        predicates: vec![Predicate::Eq(EqPredicate {
            property: "unsupported_property".to_owned(),
            value: jid(T1),
        })],
    }
}

fn with_allow(allow: bool) -> CompileOptions {
    CompileOptions::new().allow_empty_scope(allow)
}

#[test]
fn compile_options_default_to_refusing_empty_scopes() {
    assert_eq!(CompileOptions::default(), CompileOptions::new());
    assert_eq!(CompileOptions::new(), with_allow(false));
    assert_ne!(with_allow(true), with_allow(false));
}

/// Flag matrix for an all-empty compiled scope: the public default and an
/// explicit `false` refuse it with a typed reason distinct from malformed
/// responses; an explicit `true` returns the normalized scope with originals.
#[test]
fn all_empty_scope_flag_matrix_on_the_public_compiler() {
    let response = respond(vec![empty_tenant_set(), empty_tenant_set()]);
    for require_constraints in [true, false] {
        for result in [
            compile_to_access_scope(&response, require_constraints, DEFAULT_PROPS),
            compile_to_access_scope_with_options(
                &response,
                require_constraints,
                DEFAULT_PROPS,
                with_allow(false),
            ),
        ] {
            assert!(
                matches!(result, Err(ConstraintCompileError::ScopeMatchesNothing)),
                "{result:?}"
            );
        }

        let scope = compile_to_access_scope_with_options(
            &response,
            require_constraints,
            DEFAULT_PROPS,
            with_allow(true),
        )
        .unwrap();
        assert!(!scope.is_unconstrained());
        assert!(scope.is_deny_all());
        assert!(scope.constraints().is_empty());
        assert_eq!(scope.original_constraints().len(), 2);
        for original in scope.original_constraints() {
            let [ScopeFilter::In(filter)] = original.filters() else {
                panic!("expected the empty In filter, got {original:?}");
            };
            assert_eq!(filter.property(), pep_properties::OWNER_TENANT_ID);
            assert!(filter.values().is_empty());
        }
    }
}

/// `allow_empty_scope` never changes how absent outer constraints are handled.
#[test]
fn absent_constraints_follow_require_constraints_for_both_flags() {
    let response = respond(vec![]);
    for allow in [false, true] {
        let scope = compile_to_access_scope_with_options(
            &response,
            false,
            DEFAULT_PROPS,
            with_allow(allow),
        )
        .unwrap();
        assert!(scope.is_unconstrained(), "allow={allow}");
        let err =
            compile_to_access_scope_with_options(&response, true, DEFAULT_PROPS, with_allow(allow))
                .unwrap_err();
        assert!(
            matches!(err, ConstraintCompileError::ConstraintsRequiredButAbsent),
            "allow={allow}: {err:?}"
        );
    }
}

/// The invalid-branch policy is unchanged (#5958 tracks strict rejection):
/// an invalid sibling is dropped; the request fails only if nothing compiled.
/// A valid empty branch is not invalid, so it decides via the flag.
#[test]
fn invalid_sibling_is_dropped_and_valid_empty_branch_decides() {
    let empty_and_invalid = respond(vec![empty_tenant_set(), invalid_branch()]);
    let err = compile_to_access_scope(&empty_and_invalid, true, DEFAULT_PROPS).unwrap_err();
    assert!(
        matches!(err, ConstraintCompileError::ScopeMatchesNothing),
        "{err:?}"
    );
    let scope = compile_to_access_scope_with_options(
        &empty_and_invalid,
        true,
        DEFAULT_PROPS,
        with_allow(true),
    )
    .unwrap();
    assert!(scope.is_deny_all());
    assert_eq!(
        scope.original_constraints().len(),
        1,
        "only compiled branches are originals, never the dropped invalid one"
    );

    for allow in [false, true] {
        let only_invalid = respond(vec![invalid_branch()]);
        let err = compile_to_access_scope_with_options(
            &only_invalid,
            true,
            DEFAULT_PROPS,
            with_allow(allow),
        )
        .unwrap_err();
        assert!(
            matches!(&err, ConstraintCompileError::AllConstraintsFailed { reason } if reason.contains("unsupported property")),
            "allow={allow}: {err:?}"
        );

        let mixed = respond(vec![empty_tenant_set(), invalid_branch(), tenant_eq(T1)]);
        let scope =
            compile_to_access_scope_with_options(&mixed, true, DEFAULT_PROPS, with_allow(allow))
                .unwrap();
        assert_eq!(scope.constraints().len(), 1);
        assert_eq!(
            scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
            &[uuid(T1)]
        );
        assert_eq!(scope.original_constraints().len(), 2);
        assert!(
            scope
                .original_constraints()
                .next()
                .is_some_and(ScopeConstraint::matches_nothing),
            "original order keeps the empty branch first"
        );
    }
}

#[test]
fn empty_group_selectors_compile_with_tenant_and_negotiated_capabilities() {
    use crate::constraints::{InGroupPredicate, InGroupSubtreePredicate};

    for (predicate, subtree) in [
        (
            Predicate::InGroup(InGroupPredicate {
                property: pep_properties::RESOURCE_ID.to_owned(),
                group_ids: vec![],
            }),
            false,
        ),
        (
            Predicate::InGroupSubtree(InGroupSubtreePredicate {
                property: pep_properties::RESOURCE_ID.to_owned(),
                ancestor_ids: vec![],
            }),
            true,
        ),
    ] {
        let response = respond(vec![Constraint {
            predicates: vec![
                Predicate::Eq(EqPredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    value: jid(T1),
                }),
                predicate,
            ],
        }]);
        let capabilities = [Capability::GroupMembership, Capability::GroupHierarchy];
        let err = compile_to_access_scope_with_negotiated_capabilities(
            &response,
            true,
            DEFAULT_PROPS,
            Some(GROUP_MEMBERSHIP_TYPE),
            &capabilities,
            false,
        )
        .unwrap_err();
        assert!(
            matches!(err, ConstraintCompileError::ScopeMatchesNothing),
            "{err:?}"
        );
        let scope = compile_to_access_scope_with_negotiated_capabilities(
            &response,
            true,
            DEFAULT_PROPS,
            Some(GROUP_MEMBERSHIP_TYPE),
            &capabilities,
            true,
        )
        .unwrap();
        assert!(!scope.is_unconstrained());
        assert!(scope.matches_nothing());
        let [original] = scope.original_constraints().collect::<Vec<_>>()[..] else {
            panic!("expected one original branch");
        };
        let filters = original.filters();
        assert_eq!(filters.len(), 2);
        match (&filters[1], subtree) {
            (ScopeFilter::InGroup(f), false) => {
                assert_eq!(f.property(), pep_properties::RESOURCE_ID);
                assert_eq!(f.membership_resource_type(), GROUP_MEMBERSHIP_TYPE);
                assert!(f.group_ids().is_empty());
            }
            (ScopeFilter::InGroupSubtree(f), true) => {
                assert_eq!(f.property(), pep_properties::RESOURCE_ID);
                assert_eq!(f.membership_resource_type(), GROUP_MEMBERSHIP_TYPE);
                assert!(f.ancestor_ids().is_empty());
            }
            (other, _) => panic!("expected the empty group selector, got {other:?}"),
        }
    }
}

#[test]
fn empty_in_branch_preserves_a_valid_or_alternative() {
    let response = respond(vec![empty_tenant_set(), tenant_eq(T1)]);
    for allow in [false, true] {
        let scope =
            compile_to_access_scope_with_options(&response, true, DEFAULT_PROPS, with_allow(allow))
                .unwrap();
        assert!(!scope.is_unconstrained());
        assert!(!scope.matches_nothing());
        assert_eq!(
            scope.constraints().len(),
            1,
            "empty branch is normalized away"
        );
        assert_eq!(scope.original_constraints().len(), 2);
        assert_eq!(
            scope.all_uuid_values_for(pep_properties::OWNER_TENANT_ID),
            &[uuid(T1)]
        );
    }
}

#[test]
fn in_tenant_subtree_predicate_compiles_to_subtree_filter_respecting_barriers() {
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;

    let root = "cccccccc-cccc-cccc-cccc-cccccccccccc";
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: jid(root),
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    assert_eq!(scope.constraints().len(), 1);
    let filter = &scope.constraints()[0].filters()[0];
    match filter {
        ScopeFilter::InTenantSubtree(sf) => {
            assert_eq!(sf.property(), pep_properties::OWNER_TENANT_ID);
            assert!(sf.respect_barriers(), "Respect must clamp on barrier");
        }
        other => panic!("expected InTenantSubtree filter, got: {other:?}"),
    }
}

#[test]
fn in_tenant_subtree_predicate_ignore_barriers_propagates_to_filter() {
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;

    let root = "cccccccc-cccc-cccc-cccc-cccccccccccc";
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: jid(root),
                    barrier_mode: BarrierMode::Ignore,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    let filter = &scope.constraints()[0].filters()[0];
    match filter {
        ScopeFilter::InTenantSubtree(sf) => {
            assert!(!sf.respect_barriers(), "Ignore must not clamp on barrier");
        }
        other => panic!("expected InTenantSubtree filter, got: {other:?}"),
    }
}

#[test]
fn in_tenant_subtree_with_descendant_status_compiles_to_smallint_filter() {
    // Non-empty descendant_status must flow through the compiler unchanged
    // in semantics — each TenantStatus lowers to its canonical SMALLINT
    // encoding (Active=1, Suspended=2, Deleted=3) on the ScopeFilter so the
    // SQL bind matches the `tenant_closure.descendant_status` domain.
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;
    use tenant_resolver_sdk::TenantStatus;
    use toolkit_security::ScopeValue;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: jid(T1),
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: vec![TenantStatus::Active, TenantStatus::Suspended],
                })],
            }],
            ..Default::default()
        },
    };

    let scope = compile_to_access_scope(&response, true, DEFAULT_PROPS).unwrap();
    let filter = &scope.constraints()[0].filters()[0];
    match filter {
        ScopeFilter::InTenantSubtree(sf) => {
            assert_eq!(
                sf.descendant_status(),
                &[ScopeValue::Int(1), ScopeValue::Int(2)],
                "TenantStatus must lower to as_smallint() values"
            );
            assert!(sf.respect_barriers());
        }
        other => panic!("expected InTenantSubtree filter, got: {other:?}"),
    }
}

#[test]
fn in_tenant_subtree_non_uuid_root_bool_fails_constraint() {
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: serde_json::json!(true),
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    assert!(
        matches!(
            result,
            Err(ConstraintCompileError::AllConstraintsFailed { .. })
        ),
        "bool root_tenant_id must fail-closed, got: {result:?}"
    );
}

#[test]
fn in_tenant_subtree_non_uuid_root_int_fails_constraint() {
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: serde_json::json!(42),
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    assert!(
        matches!(
            result,
            Err(ConstraintCompileError::AllConstraintsFailed { .. })
        ),
        "integer root_tenant_id must fail-closed, got: {result:?}"
    );
}

#[test]
fn in_tenant_subtree_non_uuid_root_string_fails_constraint() {
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: serde_json::json!("not-a-uuid"),
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    assert!(
        matches!(
            result,
            Err(ConstraintCompileError::AllConstraintsFailed { .. })
        ),
        "non-UUID string root_tenant_id must fail-closed, got: {result:?}"
    );
}

#[test]
fn in_tenant_subtree_non_uuid_root_null_fails_constraint() {
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: serde_json::Value::Null,
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    assert!(
        matches!(
            result,
            Err(ConstraintCompileError::AllConstraintsFailed { .. })
        ),
        "null root_tenant_id must fail-closed, got: {result:?}"
    );
}

#[test]
fn in_tenant_subtree_non_uuid_root_array_fails_constraint() {
    use crate::constraints::InTenantSubtreePredicate;
    use crate::models::BarrierMode;

    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate {
                    property: pep_properties::OWNER_TENANT_ID.to_owned(),
                    root_tenant_id: serde_json::json!([T1]),
                    barrier_mode: BarrierMode::Respect,
                    descendant_status: Vec::new(),
                })],
            }],
            ..Default::default()
        },
    };

    let result = compile_to_access_scope(&response, true, DEFAULT_PROPS);
    assert!(
        matches!(
            result,
            Err(ConstraintCompileError::AllConstraintsFailed { .. })
        ),
        "array root_tenant_id must fail-closed, got: {result:?}"
    );
}

#[test]
fn empty_sets_do_not_bypass_compiler_validation() {
    use crate::constraints::{InGroupPredicate, InGroupSubtreePredicate};
    for predicate in [
        Predicate::InGroup(InGroupPredicate {
            property: pep_properties::RESOURCE_ID.into(),
            group_ids: vec![],
        }),
        Predicate::InGroupSubtree(InGroupSubtreePredicate {
            property: pep_properties::RESOURCE_ID.into(),
            ancestor_ids: vec![],
        }),
    ] {
        let tenant = Predicate::Eq(EqPredicate {
            property: pep_properties::OWNER_TENANT_ID.into(),
            value: jid(T1),
        });
        let wrong_property = match &predicate {
            Predicate::InGroup(_) => Predicate::InGroup(InGroupPredicate {
                property: pep_properties::OWNER_ID.into(),
                group_ids: vec![],
            }),
            _ => Predicate::InGroupSubtree(InGroupSubtreePredicate {
                property: pep_properties::OWNER_ID.into(),
                ancestor_ids: vec![],
            }),
        };
        for (predicates, membership_type, capabilities, expected_reason) in [
            (
                vec![tenant.clone(), predicate.clone()],
                Some(GROUP_MEMBERSHIP_TYPE),
                vec![],
                None,
            ),
            (
                vec![predicate.clone()],
                Some(GROUP_MEMBERSHIP_TYPE),
                vec![Capability::GroupMembership, Capability::GroupHierarchy],
                Some("owner_tenant_id"),
            ),
            (
                vec![tenant.clone(), predicate],
                None,
                vec![Capability::GroupMembership, Capability::GroupHierarchy],
                Some("canonical GTS resource type"),
            ),
            (
                vec![tenant, wrong_property],
                Some(GROUP_MEMBERSHIP_TYPE),
                vec![Capability::GroupMembership, Capability::GroupHierarchy],
                Some("must target 'id'"),
            ),
        ] {
            let response = EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint { predicates }],
                    ..Default::default()
                },
            };
            // Validation precedes emptiness: opting in never skips it.
            for allow in [false, true] {
                let err = compile_to_access_scope_with_negotiated_capabilities(
                    &response,
                    true,
                    DEFAULT_PROPS,
                    membership_type,
                    &capabilities,
                    allow,
                )
                .unwrap_err();
                if let Some(expected) = expected_reason {
                    assert!(
                        matches!(&err, ConstraintCompileError::AllConstraintsFailed { reason } if reason.contains(expected)),
                        "allow={allow}: {err:?}"
                    );
                } else {
                    assert!(
                        matches!(err, ConstraintCompileError::UnadvertisedCapabilities { .. }),
                        "allow={allow}: {err:?}"
                    );
                }
            }
        }
    }
    let response = EvaluationResponse {
        decision: true,
        context: EvaluationResponseContext {
            constraints: vec![Constraint {
                predicates: vec![Predicate::In(InPredicate {
                    property: "unknown_property".into(),
                    values: vec![],
                })],
            }],
            ..Default::default()
        },
    };
    let malformed_tenant = respond(vec![Constraint {
        predicates: vec![
            Predicate::Eq(EqPredicate {
                property: pep_properties::OWNER_TENANT_ID.into(),
                value: json!("not-a-uuid"),
            }),
            Predicate::In(InPredicate {
                property: pep_properties::RESOURCE_ID.into(),
                values: vec![],
            }),
        ],
    }]);
    for allow in [false, true] {
        let err =
            compile_to_access_scope_with_options(&response, true, DEFAULT_PROPS, with_allow(allow))
                .unwrap_err();
        assert!(
            matches!(&err, ConstraintCompileError::AllConstraintsFailed { reason } if reason.contains("unsupported property")),
            "allow={allow}: {err:?}"
        );
        let err = compile_to_access_scope_with_options(
            &malformed_tenant,
            true,
            DEFAULT_PROPS,
            with_allow(allow),
        )
        .unwrap_err();
        assert!(
            matches!(&err, ConstraintCompileError::AllConstraintsFailed { reason } if reason.contains("UUID")),
            "allow={allow}: {err:?}"
        );
    }
}
