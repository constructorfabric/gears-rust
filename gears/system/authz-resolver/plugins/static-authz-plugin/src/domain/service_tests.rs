// Created: 2026-04-14 by Constructor Tech
use super::*;
use authz_resolver_sdk::pep::{ConstraintCompileError, IntoPropertyValue, compile_to_access_scope};
use authz_resolver_sdk::{Action, EvaluationRequestContext, Resource, Subject, TenantContext};
use std::collections::HashMap;
use toolkit_gts::gts_id;

/// Build a request whose PEP declares nothing it can constrain on -- the shape a
/// platform-global `ResourceType` (`from_static(.., &[])`) sends.
fn make_request(require_constraints: bool, tenant_id: Option<Uuid>) -> EvaluationRequest {
    let mut subject_properties = HashMap::new();
    subject_properties.insert(
        "tenant_id".to_owned(),
        serde_json::Value::String("22222222-2222-2222-2222-222222222222".to_owned()),
    );

    EvaluationRequest {
        subject: Subject {
            id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            subject_type: None,
            properties: subject_properties,
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: gts_id!("cf.core.users.user.v1~").to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: tenant_id.map(|id| TenantContext {
                root_id: Some(id),
                ..TenantContext::default()
            }),
            token_scopes: vec!["*".to_owned()],
            require_constraints,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    }
}

/// Build a request whose PEP declares `OWNER_TENANT_ID` -- the shape any entity
/// declared with `tenant_col` sends, and the only shape the baseline clamp binds
/// against.
fn make_owner_tenant_request(
    require_constraints: bool,
    tenant_id: Option<Uuid>,
) -> EvaluationRequest {
    let mut req = make_request(require_constraints, tenant_id);
    req.context.supported_properties = vec![pep_properties::OWNER_TENANT_ID.to_owned()];
    req
}

/// Build a request that mirrors what an AM-style PEP sends:
/// `Capability::TenantHierarchy` advertised + `RESOURCE_ID` declared on
/// the supported-properties list, so the plugin should emit the
/// `InTenantSubtree(RESOURCE_ID, tid)` constraint alongside the
/// baseline `In(OWNER_TENANT_ID, [tid])`.
fn make_tenant_hierarchy_request(tenant_id: Uuid) -> EvaluationRequest {
    let mut req = make_request(true, Some(tenant_id));
    req.context.capabilities = vec![Capability::TenantHierarchy];
    req.context.supported_properties = vec![
        pep_properties::OWNER_TENANT_ID.to_owned(),
        pep_properties::RESOURCE_ID.to_owned(),
    ];
    req
}

#[test]
fn list_operation_with_tenant_context() {
    let tenant_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let service = Service::new();
    let response = service.evaluate(&make_owner_tenant_request(true, Some(tenant_id)));

    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);

    let constraint = &response.context.constraints[0];
    assert_eq!(constraint.predicates.len(), 1);

    match &constraint.predicates[0] {
        Predicate::In(in_pred) => {
            assert_eq!(in_pred.property, pep_properties::OWNER_TENANT_ID);
            assert_eq!(in_pred.values, vec![tenant_id.into_filter_value()]);
        }
        other => panic!("Expected In predicate, got: {other:?}"),
    }
}

#[test]
fn group_selector_is_denied_instead_of_ignored() {
    let tenant_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let service = Service::new();

    for property in ["group_ids", "ancestor_group_ids"] {
        let mut request = make_owner_tenant_request(true, Some(tenant_id));
        request.resource.properties.insert(
            property.to_owned(),
            serde_json::json!([Uuid::new_v4().to_string()]),
        );

        let response = service.evaluate(&request);
        assert!(
            !response.decision,
            "the static PDP must deny unsupported {property} rather than widen scope"
        );
    }
}

#[test]
fn list_operation_without_tenant_falls_back_to_subject_properties() {
    let service = Service::new();
    let response = service.evaluate(&make_owner_tenant_request(true, None));

    // Falls back to subject.properties["tenant_id"]
    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);

    match &response.context.constraints[0].predicates[0] {
        Predicate::In(in_pred) => {
            assert_eq!(
                in_pred.values,
                vec![
                    Uuid::parse_str("22222222-2222-2222-2222-222222222222")
                        .unwrap()
                        .into_filter_value()
                ]
            );
        }
        other => panic!("Expected In predicate, got: {other:?}"),
    }
}

#[test]
fn nil_tenant_is_denied() {
    let service = Service::new();
    let response = service.evaluate(&make_request(true, Some(Uuid::default())));

    assert!(!response.decision);
    assert!(response.context.constraints.is_empty());
}

#[test]
fn missing_tenant_context_and_subject_property_is_denied() {
    let request = EvaluationRequest {
        subject: Subject {
            id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            subject_type: None,
            properties: HashMap::new(), // no tenant_id property
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: gts_id!("cf.core.users.user.v1~").to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: vec!["*".to_owned()],
            require_constraints: true,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    };

    let service = Service::new();
    let response = service.evaluate(&request);

    assert!(!response.decision);
    assert!(response.context.constraints.is_empty());
}

#[test]
fn no_bindable_property_emits_no_constraints() {
    // The clamp cannot bind, so it must not be emitted: as the only constraint it
    // failed the whole set (`AllConstraintsFailed`, a deny). Empty either way --
    // the compiler owns what that means, see the composition tests below.
    let tenant_id = Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap();
    let service = Service::new();

    for require_constraints in [false, true] {
        let response = service.evaluate(&make_request(require_constraints, Some(tenant_id)));

        assert!(
            response.decision,
            "require_constraints: {require_constraints}"
        );
        assert!(
            response.context.constraints.is_empty(),
            "require_constraints: {require_constraints}"
        );
    }
}

#[test]
fn empty_constraint_set_compiles_to_allow_all_when_constraints_are_not_required() {
    // Composition guard over the real compiler: a PEP that declares nothing gets
    // the `allow_all` a decision-only caller asked for, not a deny.
    let tenant_id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let service = Service::new();
    let response = service.evaluate(&make_request(false, Some(tenant_id)));

    let scope = compile_to_access_scope(&response, false, &[]).expect("compiles");

    assert!(scope.is_unconstrained());
}

#[test]
fn empty_constraint_set_is_denied_by_the_pep_compiler_when_constraints_are_required() {
    // Who denies when constraints were required but none could be built: the
    // compiler (`compiler.rs` Step 1), which every gear maps to a 403. That is why
    // this service does not repeat the check.
    let tenant_id = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let service = Service::new();
    let response = service.evaluate(&make_request(true, Some(tenant_id)));

    let err = compile_to_access_scope(&response, true, &[]).expect_err("fails closed");

    assert!(
        matches!(err, ConstraintCompileError::ConstraintsRequiredButAbsent),
        "expected ConstraintsRequiredButAbsent, got: {err:?}"
    );
}

#[test]
fn require_constraints_false_still_clamps_when_owner_tenant_id_binds() {
    // Security regression guard. `require_constraints: false` alone must not
    // drop the clamp. A PEP that declares OWNER_TENANT_ID can bind the clamp, so
    // the service must deliver the tenant narrowing. Some callers read a row with
    // an unscoped prefetch, then use the constrained scope to force a scoped
    // re-read. An `allow_all` scope turns that path into a cross-tenant read.
    let tenant_id = Uuid::parse_str("77777777-7777-7777-7777-777777777777").unwrap();
    let request = make_owner_tenant_request(false, Some(tenant_id));

    let service = Service::new();
    let response = service.evaluate(&request);

    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);
    match &response.context.constraints[0].predicates[0] {
        Predicate::In(in_pred) => {
            assert_eq!(in_pred.property, pep_properties::OWNER_TENANT_ID);
            assert_eq!(in_pred.values, vec![tenant_id.into_filter_value()]);
        }
        other => panic!("Expected In predicate, got: {other:?}"),
    }
}

#[test]
fn require_constraints_false_still_clamps_when_only_the_subtree_predicate_binds() {
    // Security regression guard for the `no_tenant, resource_col = "..."` shape.
    // OWNER_TENANT_ID does not bind here, but the subtree predicate binds with
    // `Capability::TenantHierarchy` and RESOURCE_ID. The service must emit that
    // predicate, and not an unconstrained permit.
    let tenant_id = Uuid::parse_str("88888888-8888-8888-8888-888888888888").unwrap();
    let mut request = make_request(false, Some(tenant_id));
    request.context.capabilities = vec![Capability::TenantHierarchy];
    request.context.supported_properties = vec![pep_properties::RESOURCE_ID.to_owned()];

    let service = Service::new();
    let response = service.evaluate(&request);

    assert!(response.decision);
    // No baseline clamp: the compiler would have failed it anyway. The subtree
    // predicate carries the narrowing on its own.
    assert_eq!(response.context.constraints.len(), 1);
    match &response.context.constraints[0].predicates[0] {
        Predicate::InTenantSubtree(sub_pred) => {
            assert_eq!(sub_pred.property, pep_properties::RESOURCE_ID);
            assert_eq!(sub_pred.root_tenant_id, tenant_id.into_filter_value());
        }
        other => panic!("Expected InTenantSubtree predicate, got: {other:?}"),
    }

    // Composition guard: dropping the dead baseline must not widen the compiled
    // scope to `allow_all`.
    let scope = compile_to_access_scope(&response, false, &[pep_properties::RESOURCE_ID])
        .expect("compiles");
    assert!(!scope.is_unconstrained());
    assert_eq!(scope.constraints().len(), 1);
}

#[test]
fn require_constraints_false_nil_tenant_is_still_denied() {
    // Security regression guard. The tenant denials run before any constraint is
    // built, so a permissive constraint set cannot bypass the nil-tenant deny.
    let service = Service::new();
    let response = service.evaluate(&make_request(false, Some(Uuid::default())));

    assert!(!response.decision);
    assert!(response.context.constraints.is_empty());
}

#[test]
fn require_constraints_false_unresolvable_tenant_is_still_denied() {
    // Security regression guard. The tenant denials run before any constraint is
    // built, so no resolvable tenant is a deny even when the PEP declares nothing.
    let request = EvaluationRequest {
        subject: Subject {
            id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            subject_type: None,
            properties: HashMap::new(), // no tenant_id property
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: gts_id!("cf.core.users.user.v1~").to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: vec!["*".to_owned()],
            require_constraints: false,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    };

    let service = Service::new();
    let response = service.evaluate(&request);

    assert!(!response.decision);
    assert!(response.context.constraints.is_empty());
}

#[test]
fn tenant_hierarchy_capability_emits_in_tenant_subtree_for_both_supported_properties() {
    let tenant_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
    let service = Service::new();
    let response = service.evaluate(&make_tenant_hierarchy_request(tenant_id));

    assert!(response.decision);

    // Three parallel constraints OR-ed by the PEP compiler:
    //   1. legacy `In(OWNER_TENANT_ID)` clamp (binds via `tenant_col`)
    //   2. `InTenantSubtree(OWNER_TENANT_ID, tid)` (binds via `tenant_col`
    //      against entities that opt-in via `Capability::TenantHierarchy`
    //      so children of the caller's tenant become visible)
    //   3. `InTenantSubtree(RESOURCE_ID, tid)` (binds via `resource_col`
    //      against `no_tenant` entities like AM's `tenants`)
    //
    // The SecureORM compiler drops the predicates whose property doesn't
    // resolve on the entity, so each entity shape ends up with only the
    // constraints that actually bind.
    assert_eq!(response.context.constraints.len(), 3);

    match &response.context.constraints[0].predicates[0] {
        Predicate::In(in_pred) => {
            assert_eq!(in_pred.property, pep_properties::OWNER_TENANT_ID);
            assert_eq!(in_pred.values, vec![tenant_id.into_filter_value()]);
        }
        other => panic!("Expected In predicate, got: {other:?}"),
    }

    match &response.context.constraints[1].predicates[0] {
        Predicate::InTenantSubtree(sub_pred) => {
            assert_eq!(sub_pred.property, pep_properties::OWNER_TENANT_ID);
            assert_eq!(sub_pred.root_tenant_id, tenant_id.into_filter_value());
        }
        other => panic!("Expected InTenantSubtree predicate, got: {other:?}"),
    }

    match &response.context.constraints[2].predicates[0] {
        Predicate::InTenantSubtree(sub_pred) => {
            assert_eq!(sub_pred.property, pep_properties::RESOURCE_ID);
            assert_eq!(sub_pred.root_tenant_id, tenant_id.into_filter_value());
        }
        other => panic!("Expected InTenantSubtree predicate, got: {other:?}"),
    }
}

#[test]
fn tenant_hierarchy_capability_only_emits_for_declared_supported_properties() {
    let tenant_id = Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
    let mut request = make_request(true, Some(tenant_id));
    request.context.capabilities = vec![Capability::TenantHierarchy];
    // RESOURCE_ID is intentionally omitted -- the PEP did not declare it
    // as a constraint property, so the plugin must NOT emit a predicate
    // bound to it (the secure-extension would have no column to bind).
    request.context.supported_properties = vec![pep_properties::OWNER_TENANT_ID.to_owned()];

    let service = Service::new();
    let response = service.evaluate(&request);

    assert!(response.decision);
    // Two constraints: the baseline In(OWNER_TENANT_ID) plus a single
    // InTenantSubtree(OWNER_TENANT_ID) since that's the only property
    // the PEP declared. No InTenantSubtree(RESOURCE_ID) is emitted.
    assert_eq!(response.context.constraints.len(), 2);
    match &response.context.constraints[0].predicates[0] {
        Predicate::In(in_pred) => {
            assert_eq!(in_pred.property, pep_properties::OWNER_TENANT_ID);
        }
        other => panic!("Expected In predicate, got: {other:?}"),
    }
    match &response.context.constraints[1].predicates[0] {
        Predicate::InTenantSubtree(sub_pred) => {
            assert_eq!(sub_pred.property, pep_properties::OWNER_TENANT_ID);
        }
        other => panic!("Expected InTenantSubtree predicate, got: {other:?}"),
    }
}

// ── property grants ──────────────────────────────────────────────────────────

const CREDENTIAL_TYPE: &str = "gts.cf.core.credstore.credential.v1~";
const API_KEY_GTS: &str = "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~";

fn credential_request(
    action: &str,
    subject: Uuid,
    tenant_id: Uuid,
    supported: &[&str],
) -> EvaluationRequest {
    let mut req = make_request(true, Some(tenant_id));
    req.resource.resource_type = CREDENTIAL_TYPE.to_owned();
    req.action.name = action.to_owned();
    req.subject.id = subject;
    req.context.supported_properties = supported.iter().map(|s| (*s).to_owned()).collect();
    req
}

fn grant(property: &str, actions: &[&str], subjects: &[Uuid], values: &[&str]) -> PropertyGrant {
    PropertyGrant::try_from_config(&crate::config::PropertyGrantConfig {
        resource_type: CREDENTIAL_TYPE.to_owned(),
        property: property.to_owned(),
        actions: actions.iter().map(|s| (*s).to_owned()).collect(),
        subjects: subjects.to_vec(),
        values: values.iter().map(|s| (*s).to_owned()).collect(),
    })
    .expect("valid grant")
}

fn in_predicates<'a>(constraint: &'a Constraint, property: &str) -> Vec<&'a InPredicate> {
    constraint
        .predicates
        .iter()
        .filter_map(|p| match p {
            Predicate::In(i) if i.property == property => Some(i),
            _ => None,
        })
        .collect()
}

fn uuid(n: u8) -> Uuid {
    Uuid::from_u128(u128::from(n))
}

#[test]
fn without_grants_the_response_is_unchanged() {
    let tenant = uuid(7);
    let req = credential_request("read", uuid(1), tenant, &["owner_tenant_id", "secret_type"]);
    let plain = Service::new().evaluate(&req);
    let empty = Service::with_grants(Vec::new()).evaluate(&req);
    assert_eq!(
        serde_json::to_value(&plain.context.constraints).unwrap(),
        serde_json::to_value(&empty.context.constraints).unwrap()
    );
    assert_eq!(plain.context.constraints.len(), 1);
    assert_eq!(plain.context.constraints[0].predicates.len(), 1);
}

#[test]
fn matching_grant_narrows_every_constraint_with_the_values() {
    let tenant = uuid(7);
    let (u1, u2) = (uuid(11), uuid(12));
    let service = Service::with_grants(vec![grant(
        "secret_type",
        &[],
        &[],
        &[&u1.to_string(), &u2.to_string()],
    )]);
    let req = credential_request("read", uuid(1), tenant, &["owner_tenant_id", "secret_type"]);
    let response = service.evaluate(&req);

    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);
    let c = &response.context.constraints[0];
    assert_eq!(in_predicates(c, "owner_tenant_id").len(), 1);
    let granted = in_predicates(c, "secret_type");
    assert_eq!(granted.len(), 1);
    assert_eq!(
        granted[0].values,
        vec![u1.into_filter_value(), u2.into_filter_value()]
    );
}

#[test]
fn grant_without_tenant_property_emits_a_constraint_of_only_the_grant() {
    let u1 = uuid(11);
    let service = Service::with_grants(vec![grant("secret_type", &[], &[], &[&u1.to_string()])]);
    let req = credential_request("read", uuid(1), uuid(7), &["secret_type"]);
    let response = service.evaluate(&req);

    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);
    assert_eq!(response.context.constraints[0].predicates.len(), 1);
    assert_eq!(
        in_predicates(&response.context.constraints[0], "secret_type").len(),
        1
    );
}

#[test]
fn tenant_hierarchy_subtree_constraints_carry_the_grant_too() {
    let u1 = uuid(11);
    let service = Service::with_grants(vec![grant("secret_type", &[], &[], &[&u1.to_string()])]);
    let mut req = credential_request(
        "read",
        uuid(1),
        uuid(7),
        &["owner_tenant_id", "resource_id", "secret_type"],
    );
    req.context.capabilities = vec![Capability::TenantHierarchy];
    let response = service.evaluate(&req);

    assert!(response.decision);
    // The tenant `In` constraint plus the subtree constraint(s).
    assert!(response.context.constraints.len() >= 2);
    assert!(response.context.constraints.iter().any(|c| {
        c.predicates
            .iter()
            .any(|p| matches!(p, Predicate::InTenantSubtree(_)))
    }));
    for c in &response.context.constraints {
        assert_eq!(in_predicates(c, "secret_type").len(), 1, "{c:?}");
    }
}

#[test]
fn action_filter_leaves_other_actions_unrestricted() {
    let u1 = uuid(11);
    let service = Service::with_grants(vec![grant(
        "secret_type",
        &["read_secret"],
        &[],
        &[&u1.to_string()],
    )]);
    let supported = ["owner_tenant_id", "secret_type"];

    let restricted = service.evaluate(&credential_request(
        "read_secret",
        uuid(1),
        uuid(7),
        &supported,
    ));
    assert_eq!(
        in_predicates(&restricted.context.constraints[0], "secret_type").len(),
        1
    );

    let other = service.evaluate(&credential_request("read", uuid(1), uuid(7), &supported));
    assert!(other.decision);
    assert!(in_predicates(&other.context.constraints[0], "secret_type").is_empty());
}

#[test]
fn subject_filter_leaves_other_subjects_unrestricted() {
    let u1 = uuid(11);
    let subject = uuid(1);
    let service = Service::with_grants(vec![grant(
        "secret_type",
        &[],
        &[subject],
        &[&u1.to_string()],
    )]);
    let supported = ["owner_tenant_id", "secret_type"];

    let restricted = service.evaluate(&credential_request("read", subject, uuid(7), &supported));
    assert_eq!(
        in_predicates(&restricted.context.constraints[0], "secret_type").len(),
        1
    );

    let other = service.evaluate(&credential_request("read", uuid(2), uuid(7), &supported));
    assert!(other.decision);
    assert!(in_predicates(&other.context.constraints[0], "secret_type").is_empty());
}

#[test]
fn rules_on_the_same_property_are_unioned_and_deduplicated() {
    let (u1, u2) = (uuid(11), uuid(12));
    let service = Service::with_grants(vec![
        grant("secret_type", &[], &[], &[&u2.to_string(), &u1.to_string()]),
        grant("secret_type", &["read"], &[], &[&u1.to_string()]),
    ]);
    let req = credential_request(
        "read",
        uuid(1),
        uuid(7),
        &["owner_tenant_id", "secret_type"],
    );
    let response = service.evaluate(&req);
    let granted = in_predicates(&response.context.constraints[0], "secret_type");
    assert_eq!(granted.len(), 1);
    assert_eq!(
        granted[0].values,
        vec![u1.into_filter_value(), u2.into_filter_value()]
    );
}

#[test]
fn undeclared_grant_property_fails_closed() {
    let u1 = uuid(11);
    let service = Service::with_grants(vec![grant("secret_type", &[], &[], &[&u1.to_string()])]);
    let req = credential_request("read", uuid(1), uuid(7), &["owner_tenant_id"]);
    let response = service.evaluate(&req);
    assert!(!response.decision);
    assert!(response.context.constraints.is_empty());
}

#[test]
fn rule_for_another_resource_type_does_not_apply() {
    let u1 = uuid(11);
    let service = Service::with_grants(vec![grant("secret_type", &[], &[], &[&u1.to_string()])]);
    let mut req = credential_request(
        "read",
        uuid(1),
        uuid(7),
        &["owner_tenant_id", "secret_type"],
    );
    req.resource.resource_type = API_KEY_GTS.to_owned();
    let response = service.evaluate(&req);
    assert!(response.decision);
    assert!(in_predicates(&response.context.constraints[0], "secret_type").is_empty());
}

#[test]
fn empty_values_emit_an_empty_in_that_admits_nothing() {
    let service = Service::with_grants(vec![grant("secret_type", &[], &[], &[])]);
    let req = credential_request(
        "read",
        uuid(1),
        uuid(7),
        &["owner_tenant_id", "secret_type"],
    );
    let response = service.evaluate(&req);
    assert!(response.decision);
    let granted = in_predicates(&response.context.constraints[0], "secret_type");
    assert_eq!(granted.len(), 1);
    assert!(granted[0].values.is_empty());
}

#[test]
fn reference_grant_emits_string_values_for_one_subject() {
    let subject = uuid(1);
    let service = Service::with_grants(vec![grant(
        "reference",
        &["read_secret"],
        &[subject],
        &["smtp-password", "api.token"],
    )]);
    let req = credential_request(
        "read_secret",
        subject,
        uuid(7),
        &["owner_tenant_id", "secret_type", "reference"],
    );
    let response = service.evaluate(&req);
    let granted = in_predicates(&response.context.constraints[0], "reference");
    assert_eq!(granted.len(), 1);
    assert_eq!(
        granted[0].values,
        vec![
            serde_json::Value::String("api.token".to_owned()),
            serde_json::Value::String("smtp-password".to_owned()),
        ]
    );
    // It compiles to a scope with a string reference filter.
    let scope = compile_to_access_scope(
        &response,
        true,
        &["owner_tenant_id", "secret_type", "reference"],
    )
    .expect("compiles");
    assert!(!scope.is_unconstrained());
}

#[test]
fn gts_id_value_converts_to_its_uuid() {
    let expected = GtsId::try_new(API_KEY_GTS).unwrap().to_uuid();
    assert_eq!(
        GrantValue::parse(API_KEY_GTS).unwrap(),
        GrantValue::Uuid(expected)
    );
    let plain = uuid(42);
    assert_eq!(
        GrantValue::parse(&plain.to_string()).unwrap(),
        GrantValue::Uuid(plain)
    );
    assert_eq!(
        GrantValue::parse("smtp-password").unwrap(),
        GrantValue::String("smtp-password".to_owned())
    );
}

#[test]
fn malformed_gts_value_and_empty_names_are_init_errors() {
    assert!(GrantValue::parse("gts.not a valid id").is_err());
    let mut cfg = crate::config::PropertyGrantConfig {
        resource_type: String::new(),
        property: "secret_type".to_owned(),
        actions: vec![],
        subjects: vec![],
        values: vec![],
    };
    assert!(PropertyGrant::try_from_config(&cfg).is_err());
    cfg.resource_type = CREDENTIAL_TYPE.to_owned();
    cfg.property = " ".to_owned();
    assert!(PropertyGrant::try_from_config(&cfg).is_err());
    cfg.property = "secret_type".to_owned();
    cfg.values = vec!["gts.bad value".to_owned()];
    assert!(PropertyGrant::try_from_config(&cfg).is_err());
}

#[test]
fn config_deserializes_property_grants_and_rejects_unknown_rule_keys() {
    let ok = serde_json::json!({
        "vendor": "v",
        "priority": 5,
        "property_grants": [{
            "resource_type": CREDENTIAL_TYPE,
            "property": "reference",
            "actions": ["read"],
            "subjects": ["11111111-1111-1111-1111-111111111111"],
            "values": ["smtp-password"],
        }],
    });
    let cfg: crate::config::StaticAuthZPluginConfig = serde_json::from_value(ok).unwrap();
    assert_eq!(cfg.property_grants.len(), 1);
    assert_eq!(cfg.property_grants[0].values, vec!["smtp-password"]);

    let bad = serde_json::json!({
        "property_grants": [{
            "resource_type": CREDENTIAL_TYPE,
            "property": "reference",
            "bogus": 1,
        }],
    });
    assert!(serde_json::from_value::<crate::config::StaticAuthZPluginConfig>(bad).is_err());

    let defaults: crate::config::StaticAuthZPluginConfig =
        serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(defaults.property_grants.is_empty());
}
