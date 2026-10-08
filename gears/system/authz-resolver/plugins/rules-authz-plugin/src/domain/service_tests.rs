use super::*;
use crate::config::{
    DelegationConfig, ExactSubject, PathConfig, PayerUseConfig, PredicateConfig, RuleConfig,
    RulesAuthZPluginConfig, SubjectMatch, UnconditionalGrantConfig,
};
use authz_resolver_sdk::{Action, EvaluationRequestContext, Resource, Subject};
use std::collections::HashMap;

const THING: &str = "gts.cf.example.thing.v1~";
fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn path(predicates: &[(&str, &[u128])]) -> PathConfig {
    PathConfig {
        predicates: predicates
            .iter()
            .map(|(property, values)| PredicateConfig {
                property: (*property).to_owned(),
                values: values.iter().map(|v| u(*v)).collect(),
            })
            .collect(),
    }
}
fn rule(id: &str, subject: u128, actions: &[&str], paths: Vec<PathConfig>) -> RuleConfig {
    RuleConfig {
        id: id.to_owned(),
        subject: SubjectMatch {
            id: Some(u(subject)),
            ..SubjectMatch::default()
        },
        resource_type: THING.to_owned(),
        actions: actions.iter().map(|a| (*a).to_owned()).collect(),
        paths,
        payer_use: None,
        delegation: None,
    }
}
fn service(rules: Vec<RuleConfig>) -> Service {
    Service::from_config(&RulesAuthZPluginConfig {
        vendor: "constructorfabric".to_owned(),
        priority: 10,
        policy_revision: "rev-1".to_owned(),
        rules,
        unconditional_grants: Vec::new(),
    })
    .unwrap()
}
fn request(subject: u128, action: &str, props: &[(&str, serde_json::Value)]) -> EvaluationRequest {
    EvaluationRequest {
        subject: Subject {
            id: u(subject),
            subject_type: Some("gts.cf.core.security.subject_user.v1~".to_owned()),
            properties: HashMap::from([(
                "tenant_id".to_owned(),
                serde_json::Value::String(u(500).to_string()),
            )]),
        },
        action: Action {
            name: action.to_owned(),
        },
        resource: Resource {
            resource_type: THING.to_owned(),
            id: None,
            properties: props
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: vec![],
            require_constraints: true,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    }
}
fn s(n: u128) -> serde_json::Value {
    serde_json::Value::String(u(n).to_string())
}
fn deny_code(response: &EvaluationResponse) -> Option<&str> {
    response
        .context
        .deny_reason
        .as_ref()
        .map(|r| r.error_code.as_str())
}

#[test]
fn no_rule_means_deny_and_actions_are_exact() {
    let svc = service(vec![rule(
        "r",
        1,
        &["read"],
        vec![path(&[("seller_tenant_id", &[2])])],
    )]);
    let other_subject = svc.evaluate(&request(9, "read", &[]));
    assert!(!other_subject.decision);
    assert_eq!(deny_code(&other_subject), Some(DENY_NO_MATCHING_RULE));
    // Holding `read` grants no other action, including a longer action name.
    assert!(!svc.evaluate(&request(1, "write", &[])).decision);
    assert!(!svc.evaluate(&request(1, "read_all", &[])).decision);
    let mut other_type = request(1, "read", &[]);
    other_type.resource.resource_type = "gts.cf.example.other.v1~".to_owned();
    assert!(!svc.evaluate(&other_type).decision);
    // Empty policy denies everything.
    assert!(!service(vec![]).evaluate(&request(1, "read", &[])).decision);
}

#[test]
fn allowed_collection_returns_every_path_as_or_constraints() {
    let svc = service(vec![rule(
        "r",
        1,
        &["read"],
        vec![
            path(&[("seller_tenant_id", &[2])]),
            path(&[("payer_tenant_id", &[3, 4])]),
        ],
    )]);
    let response = svc.evaluate(&request(1, "read", &[]));
    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 2);
    assert!(matches!(
        response.context.constraints[0].predicates[0],
        Predicate::Eq(_)
    ));
    assert!(matches!(
        response.context.constraints[1].predicates[0],
        Predicate::In(_)
    ));
}

#[test]
fn supplied_properties_filter_paths_without_mixing_incomplete_paths() {
    let svc = service(vec![
        rule(
            "seller",
            1,
            &["hold"],
            vec![path(&[
                ("seller_tenant_id", &[2]),
                ("resource_tenant_id", &[5]),
            ])],
        ),
        rule(
            "payer",
            1,
            &["hold"],
            vec![path(&[("payer_tenant_id", &[3])])],
        ),
    ]);
    // Each property satisfies some path predicate, but no single complete path holds.
    let mixed = svc.evaluate(&request(
        1,
        "hold",
        &[
            ("seller_tenant_id", s(2)),
            ("resource_tenant_id", s(6)),
            ("payer_tenant_id", s(7)),
        ],
    ));
    assert!(!mixed.decision);
    let complete = svc.evaluate(&request(
        1,
        "hold",
        &[
            ("seller_tenant_id", s(2)),
            ("resource_tenant_id", s(5)),
            ("payer_tenant_id", s(7)),
        ],
    ));
    assert!(complete.decision);
    assert_eq!(complete.context.constraints.len(), 1);
    // A malformed (non-UUID) supplied value satisfies nothing. (The payer rule's path, whose
    // property is not supplied here, would still apply, so evaluate the seller rule alone.)
    let seller_only = service(vec![rule(
        "seller",
        1,
        &["hold"],
        vec![path(&[
            ("seller_tenant_id", &[2]),
            ("resource_tenant_id", &[5]),
        ])],
    )]);
    let malformed = seller_only.evaluate(&request(
        1,
        "hold",
        &[
            ("seller_tenant_id", serde_json::json!("x")),
            ("resource_tenant_id", s(5)),
        ],
    ));
    assert!(!malformed.decision);
}

#[test]
fn finite_resource_ids_bind_the_request_id() {
    let svc = service(vec![rule(
        "svc",
        1,
        &["read"],
        vec![path(&[("id", &[42, 43])])],
    )]);
    let mut granted = request(1, "read", &[]);
    granted.resource.id = Some(u(42));
    assert!(svc.evaluate(&granted).decision);
    let mut other = request(1, "read", &[]);
    other.resource.id = Some(u(44));
    assert!(!svc.evaluate(&other).decision);
}

#[test]
fn payer_use_is_a_separate_required_condition() {
    let mut r = rule(
        "create",
        1,
        &["create"],
        vec![path(&[("resource_tenant_id", &[5])])],
    );
    r.payer_use = Some(PayerUseConfig {
        property: "payer_tenant_id".to_owned(),
        values: vec![u(3)],
    });
    let svc = service(vec![r]);
    let allowed = svc.evaluate(&request(
        1,
        "create",
        &[("resource_tenant_id", s(5)), ("payer_tenant_id", s(3))],
    ));
    assert!(allowed.decision);
    // The payer-use predicate is part of the returned path, so a scope check enforces it too.
    assert_eq!(allowed.context.constraints[0].predicates.len(), 2);
    let unauthorized_payer = svc.evaluate(&request(
        1,
        "create",
        &[("resource_tenant_id", s(5)), ("payer_tenant_id", s(4))],
    ));
    assert!(!unauthorized_payer.decision);
}

#[test]
fn delegated_path_needs_an_accepted_proof_and_reports_interim_codes() {
    let mut delegated = rule(
        "partner",
        1,
        &["read"],
        vec![path(&[("resource_tenant_id", &[5])])],
    );
    delegated.delegation = Some(DelegationConfig {
        proof_property: "delegation_proof_ref".to_owned(),
        accepted: vec!["proof-ok".to_owned()],
    });
    let svc = service(vec![delegated.clone()]);
    let missing = svc.evaluate(&request(1, "read", &[]));
    assert_eq!(deny_code(&missing), Some(DENY_DELEGATION_PROOF_REQUIRED));
    let invalid = svc.evaluate(&request(
        1,
        "read",
        &[("delegation_proof_ref", serde_json::json!("revoked"))],
    ));
    assert_eq!(deny_code(&invalid), Some(DENY_DELEGATION_PROOF_INVALID));
    let ok = svc.evaluate(&request(
        1,
        "read",
        &[("delegation_proof_ref", serde_json::json!("proof-ok"))],
    ));
    assert!(ok.decision);
    // An independently complete direct rule still authorizes without proof.
    let direct = rule(
        "direct",
        1,
        &["read"],
        vec![path(&[("seller_tenant_id", &[2])])],
    );
    let both = service(vec![delegated, direct]);
    let response = both.evaluate(&request(1, "read", &[]));
    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);
}

#[test]
fn subject_tenant_and_type_selectors_must_all_match() {
    let mut r = rule("t", 1, &["read"], vec![path(&[("id", &[7])])]);
    r.subject = SubjectMatch {
        id: None,
        tenant_id: Some(u(500)),
        subject_type: Some("gts.cf.core.security.subject_service.v1~".to_owned()),
    };
    let svc = service(vec![r]);
    // The user subject has the right tenant but the wrong type.
    assert!(!svc.evaluate(&request(1, "read", &[])).decision);
    let mut service_subject = request(1, "read", &[]);
    service_subject.subject.subject_type =
        Some("gts.cf.core.security.subject_service.v1~".to_owned());
    assert!(svc.evaluate(&service_subject).decision);
}

const EVENT_TYPE: &str = "gts.cf.core.events.event_type.v1~";
fn grant_service(rules: Vec<RuleConfig>) -> Service {
    Service::from_config(&RulesAuthZPluginConfig {
        vendor: "constructorfabric".to_owned(),
        priority: 10,
        policy_revision: "rev-1".to_owned(),
        rules,
        unconditional_grants: vec![UnconditionalGrantConfig {
            id: "orders-produce".to_owned(),
            subject: ExactSubject {
                id: u(7),
                tenant_id: u(500),
            },
            resource_type: EVENT_TYPE.to_owned(),
            action: "produce".to_owned(),
        }],
    })
    .unwrap()
}
/// A property-less, constraint-free check, as Event Broker's `event_type` `produce` call makes.
fn property_less(subject: u128, resource_type: &str, action: &str) -> EvaluationRequest {
    let mut r = request(
        subject,
        action,
        &[(
            "event_type_id",
            serde_json::json!("gts.cf.core.events.event.v1~x.y.z.v1~"),
        )],
    );
    r.resource.resource_type = resource_type.to_owned();
    r.context.require_constraints = false;
    r
}

#[test]
fn an_unconditional_grant_allows_only_its_exact_triple_without_constraints() {
    let svc = grant_service(vec![]);
    let allowed = svc.evaluate(&property_less(7, EVENT_TYPE, "produce"));
    assert!(allowed.decision);
    assert!(allowed.context.constraints.is_empty());
    assert_eq!(deny_code(&allowed), None);
    // Every element of the triple is exact: other subject, home tenant, type or action denies.
    let mut other_tenant = property_less(7, EVENT_TYPE, "produce");
    other_tenant.subject.properties.insert(
        "tenant_id".to_owned(),
        serde_json::Value::String(u(501).to_string()),
    );
    let mut no_tenant = property_less(7, EVENT_TYPE, "produce");
    no_tenant.subject.properties.clear();
    for denied in [
        property_less(8, EVENT_TYPE, "produce"),
        property_less(7, "gts.cf.core.events.topic.v1~", "produce"),
        property_less(7, EVENT_TYPE, "consume"),
        property_less(7, EVENT_TYPE, "Produce"),
        other_tenant,
        no_tenant,
    ] {
        let response = svc.evaluate(&denied);
        assert!(!response.decision);
        assert_eq!(deny_code(&response), Some(DENY_NO_MATCHING_RULE));
    }
}

#[test]
fn an_unconditional_grant_never_applies_to_a_check_with_properties_or_required_constraints() {
    let svc = grant_service(vec![]);
    // The PEP declares row-level properties: the grant cannot stand in for a scope.
    let mut scoped = property_less(7, EVENT_TYPE, "produce");
    scoped.context.supported_properties = vec!["owner_tenant_id".to_owned()];
    // The PEP requires constraints: an unconstrained allow would be an allow-all.
    let mut required = property_less(7, EVENT_TYPE, "produce");
    required.context.require_constraints = true;
    for request in [scoped, required] {
        let response = svc.evaluate(&request);
        assert!(!response.decision);
        assert_eq!(deny_code(&response), Some(DENY_NO_MATCHING_RULE));
    }
    // Ordinary rules keep their own semantics beside a grant.
    let svc = grant_service(vec![rule(
        "seller",
        1,
        &["read"],
        vec![path(&[("seller_tenant_id", &[2])])],
    )]);
    let response = svc.evaluate(&request(1, "read", &[("seller_tenant_id", s(2))]));
    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);
    assert!(!svc.evaluate(&request(7, "read", &[])).decision);
}
