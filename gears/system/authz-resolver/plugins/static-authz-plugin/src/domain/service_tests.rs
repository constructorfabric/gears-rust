// Created: 2026-04-14 by Constructor Tech
use super::*;
use authz_resolver_sdk::pep::IntoPropertyValue;
use authz_resolver_sdk::{Action, EvaluationRequestContext, Resource, Subject, TenantContext};
use std::collections::HashMap;
use toolkit_gts::gts_id;

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
    let response = service.evaluate(&make_request(true, Some(tenant_id)));

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
fn list_operation_without_tenant_falls_back_to_subject_properties() {
    let service = Service::new();
    let response = service.evaluate(&make_request(true, None));

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
