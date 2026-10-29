use std::collections::HashMap;

use authz_resolver_sdk::{
    Action, Constraint, EvaluationRequest, EvaluationRequestContext, EvaluationResponse, Predicate,
    Resource, Subject,
};
use serde_json::{Value, json};
use uuid::Uuid;

use super::*;
use crate::config::{ConfigError, OrdersTestAuthZPluginConfig};

const BUYER: &str = "11111111-6a88-4768-9dfc-6bcd5187d9ed";
const SELLER_OP: &str = "22222222-6a88-4768-9dfc-6bcd5187d9ed";
const PARTNER: &str = "dddddddd-6a88-4768-9dfc-6bcd5187d9ed";
const PRODUCER: &str = "0e0e0000-0000-4000-8000-000000000005";
const PRODUCER_TENANT: &str = "0e0e0000-0000-4000-8000-0000000000aa";
const STRANGER: &str = "99999999-6a88-4768-9dfc-6bcd5187d9ed";
const TENANT_A: &str = "a0000000-0000-4000-8000-00000000000a";
const TENANT_B: &str = "b0000000-0000-4000-8000-00000000000b";
const TENANT_S: &str = "c0000000-0000-4000-8000-00000000000c";
const ORDER: &str = "gts.cf.bss.orders.order.v1~";
const EVENT_TYPE: &str = "gts.cf.core.events.event_type.v1~";
const EVENTS_REQUEST: &str = "gts.cf.core.events.request.v1~";
const ROOT_TENANT: &str = "00000000-df51-5b42-9538-d2b56b7ee953";
const PRICING_READER: &str = "33333333-6a88-4768-9dfc-6bcd5187d9ed";
const PRICE: &str = "gts.cf.bss.pricing.price.v1~";

const POLICY_YAML: &str = r#"
rules:
  - id: "buyer"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["create", "read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
  - id: "seller-operator"
    subject: { id: "22222222-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "seller_tenant_id", values: ["c0000000-0000-4000-8000-00000000000c"] }
  - id: "seller-operator-also-buyer"
    subject: { id: "22222222-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["b0000000-0000-4000-8000-00000000000b"] }
  - id: "pricing-reader"
    subject: { id: "33333333-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.pricing.*"
    actions: ["*"]
    paths:
      - predicates:
          - { property: "owner_tenant_id", value: "a0000000-0000-4000-8000-00000000000a" }
  - id: "partner-delegated"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
    delegation:
      proof_property: "delegation_proof_ref"
      accepted: ["e2e-proof-ok"]
  - id: "producer-tenant-scoped"
    subject:
      id: "0e0e0000-0000-4000-8000-000000000005"
      tenant_id: "0e0e0000-0000-4000-8000-0000000000aa"
    resource_type: "gts.cf.core.events.request.v1~"
    actions: ["produce"]
    paths:
      - predicates:
          - { property: "owner_tenant_id", values: ["00000000-df51-5b42-9538-d2b56b7ee953"] }
unconditional_grants:
  - id: "producer-event-type"
    subject:
      id: "0e0e0000-0000-4000-8000-000000000005"
      tenant_id: "0e0e0000-0000-4000-8000-0000000000aa"
    resource_type: "gts.cf.core.events.event_type.v1~"
    action: "produce"
"#;

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).expect("test UUID literal")
}

fn compile(yaml: &str) -> Result<Policy, ConfigError> {
    let config: OrdersTestAuthZPluginConfig =
        serde_saphyr::from_str(yaml).expect("test policy YAML parses");
    Policy::compile(&config)
}

fn policy() -> Policy {
    compile(POLICY_YAML).expect("test policy compiles")
}

/// A request with no subject properties, no resource properties, `require_constraints: true`.
fn request(subject: &str, resource_type: &str, action: &str) -> EvaluationRequest {
    EvaluationRequest {
        subject: Subject {
            id: uuid(subject),
            subject_type: None,
            properties: HashMap::new(),
        },
        action: Action {
            name: action.to_owned(),
        },
        resource: Resource {
            resource_type: resource_type.to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: Vec::new(),
            require_constraints: true,
            capabilities: Vec::new(),
            supported_properties: Vec::new(),
            bearer_token: None,
        },
    }
}

fn with_subject_tenant(mut req: EvaluationRequest, tenant: &str) -> EvaluationRequest {
    req.subject
        .properties
        .insert("tenant_id".to_owned(), json!(tenant));
    req
}

fn with_resource_property(
    mut req: EvaluationRequest,
    key: &str,
    value: Value,
) -> EvaluationRequest {
    req.resource.properties.insert(key.to_owned(), value);
    req
}

fn without_required_constraints(mut req: EvaluationRequest) -> EvaluationRequest {
    req.context.require_constraints = false;
    req
}

/// `(op, property, values)` of one predicate.
type PredicateShape = (&'static str, String, Vec<Value>);

/// Per constraint, per predicate — a comparable projection.
fn shape(response: &EvaluationResponse) -> Vec<Vec<PredicateShape>> {
    response
        .context
        .constraints
        .iter()
        .map(|Constraint { predicates }| {
            predicates
                .iter()
                .map(|p| match p {
                    Predicate::Eq(eq) => ("eq", eq.property.clone(), vec![eq.value.clone()]),
                    Predicate::In(inp) => ("in", inp.property.clone(), inp.values.clone()),
                    other => panic!("unexpected predicate {other:?}"),
                })
                .collect()
        })
        .collect()
}

fn assert_plain_deny(response: &EvaluationResponse) {
    assert!(!response.decision);
    assert!(response.context.constraints.is_empty());
    assert!(response.context.deny_reason.is_none());
}

fn assert_deny_reason(response: &EvaluationResponse, code: &str) {
    assert!(!response.decision);
    assert!(response.context.constraints.is_empty());
    let reason = response
        .context
        .deny_reason
        .as_ref()
        .expect("a deny reason");
    assert_eq!(reason.error_code, code);
    assert!(reason.details.is_none());
}

#[test]
fn buyer_create_with_three_axes_is_allowed_with_one_in_on_resource_tenant() {
    let req = with_resource_property(
        with_resource_property(
            with_resource_property(
                request(BUYER, ORDER, "create"),
                "resource_tenant_id",
                json!(TENANT_A),
            ),
            "seller_tenant_id",
            json!(TENANT_S),
        ),
        "payer_tenant_id",
        json!(TENANT_A),
    );
    let response = policy().evaluate(&req);
    assert!(response.decision);
    assert!(response.context.deny_reason.is_none());
    assert_eq!(
        shape(&response),
        vec![vec![(
            "in",
            "resource_tenant_id".to_owned(),
            vec![json!(TENANT_A)]
        )]]
    );
}

#[test]
fn a_different_subject_is_denied_without_a_reason() {
    assert_plain_deny(&policy().evaluate(&request(STRANGER, ORDER, "create")));
}

#[test]
fn an_undeclared_action_is_denied() {
    assert_plain_deny(&policy().evaluate(&request(BUYER, ORDER, "delete")));
}

#[test]
fn another_resource_type_is_denied() {
    assert_plain_deny(&policy().evaluate(&request(BUYER, "gts.cf.bss.orders.quote.v1~", "read")));
}

#[test]
fn seller_operator_read_and_two_rules_union_in_declaration_order() {
    let response = policy().evaluate(&request(SELLER_OP, ORDER, "read"));
    assert!(response.decision);
    assert_eq!(
        shape(&response),
        vec![
            vec![("in", "seller_tenant_id".to_owned(), vec![json!(TENANT_S)])],
            vec![("in", "resource_tenant_id".to_owned(), vec![json!(TENANT_B)])],
        ]
    );
}

#[test]
fn wildcard_type_and_action_match_a_price_read_and_value_becomes_eq() {
    let response = policy().evaluate(&request(
        "33333333-6a88-4768-9dfc-6bcd5187d9ed",
        "gts.cf.bss.pricing.price.v1~",
        "read",
    ));
    assert!(response.decision);
    assert_eq!(
        shape(&response),
        vec![vec![(
            "eq",
            "owner_tenant_id".to_owned(),
            vec![json!(TENANT_A)]
        )]]
    );
}

#[test]
fn wildcard_prefix_does_not_match_another_domain() {
    assert_plain_deny(&policy().evaluate(&request(
        "33333333-6a88-4768-9dfc-6bcd5187d9ed",
        "gts.cf.bss.products.sku.v1~",
        "read",
    )));
}

#[test]
fn a_sole_wildcard_action_matches_any_action() {
    for action in ["read", "create", "anything"] {
        let response = policy().evaluate(&request(
            "33333333-6a88-4768-9dfc-6bcd5187d9ed",
            "gts.cf.bss.pricing.price.v1~",
            action,
        ));
        assert!(response.decision, "action {action}");
    }
}

#[test]
fn delegation_absent_proof_denies_with_proof_required() {
    assert_deny_reason(
        &policy().evaluate(&request(PARTNER, ORDER, "read")),
        "delegation_proof_required",
    );
}

#[test]
fn delegation_wrong_proof_denies_with_proof_invalid() {
    let req = with_resource_property(
        request(PARTNER, ORDER, "read"),
        "delegation_proof_ref",
        json!("forged"),
    );
    assert_deny_reason(&policy().evaluate(&req), "delegation_proof_invalid");
}

#[test]
fn delegation_non_string_proof_denies_with_proof_invalid() {
    let req = with_resource_property(
        request(PARTNER, ORDER, "read"),
        "delegation_proof_ref",
        json!(42),
    );
    assert_deny_reason(&policy().evaluate(&req), "delegation_proof_invalid");
}

#[test]
fn delegation_accepted_proof_allows_with_the_rule_paths() {
    let req = with_resource_property(
        request(PARTNER, ORDER, "read"),
        "delegation_proof_ref",
        json!("e2e-proof-ok"),
    );
    let response = policy().evaluate(&req);
    assert!(response.decision);
    assert!(response.context.deny_reason.is_none());
    assert_eq!(
        shape(&response),
        vec![vec![(
            "in",
            "resource_tenant_id".to_owned(),
            vec![json!(TENANT_A)]
        )]]
    );
}

#[test]
fn a_matching_plain_rule_wins_over_a_failed_delegation_rule() {
    let yaml = r#"
rules:
  - id: "plain"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["b0000000-0000-4000-8000-00000000000b"] }
  - id: "delegated"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
    delegation: { proof_property: "delegation_proof_ref", accepted: ["ok"] }
"#;
    let response = compile(yaml)
        .expect("compiles")
        .evaluate(&request(PARTNER, ORDER, "read"));
    assert!(response.decision);
    assert!(response.context.deny_reason.is_none());
    assert_eq!(
        shape(&response),
        vec![vec![(
            "in",
            "resource_tenant_id".to_owned(),
            vec![json!(TENANT_B)]
        )]]
    );
}

#[test]
fn proof_required_wins_over_proof_invalid_when_two_delegation_rules_disagree() {
    let yaml = r#"
rules:
  - id: "proof-a"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
    delegation: { proof_property: "proof_a", accepted: ["ok-a"] }
  - id: "proof-b"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["b0000000-0000-4000-8000-00000000000b"] }
    delegation: { proof_property: "proof_b", accepted: ["ok-b"] }
"#;
    let policy = compile(yaml).expect("compiles");
    // proof_a present but wrong (invalid), proof_b absent (required) — either order.
    let req = with_resource_property(request(PARTNER, ORDER, "read"), "proof_a", json!("bad"));
    assert_deny_reason(&policy.evaluate(&req), "delegation_proof_required");
    let req = with_resource_property(request(PARTNER, ORDER, "read"), "proof_b", json!("bad"));
    assert_deny_reason(&policy.evaluate(&req), "delegation_proof_required");
}

#[test]
fn grant_allows_without_constraints_when_constraints_are_not_required() {
    let req = without_required_constraints(with_subject_tenant(
        request(PRODUCER, EVENT_TYPE, "produce"),
        PRODUCER_TENANT,
    ));
    let response = policy().evaluate(&req);
    assert!(response.decision);
    assert!(response.context.constraints.is_empty());
    assert!(response.context.deny_reason.is_none());
}

#[test]
fn grant_is_skipped_when_constraints_are_required() {
    let req = with_subject_tenant(request(PRODUCER, EVENT_TYPE, "produce"), PRODUCER_TENANT);
    assert_plain_deny(&policy().evaluate(&req));
}

#[test]
fn grant_needs_the_exact_action() {
    let req = without_required_constraints(with_subject_tenant(
        request(PRODUCER, EVENT_TYPE, "consume"),
        PRODUCER_TENANT,
    ));
    assert_plain_deny(&policy().evaluate(&req));
}

#[test]
fn declared_subject_tenant_matches_the_subject_tenant_property() {
    let req = with_subject_tenant(
        request(PRODUCER, "gts.cf.core.events.request.v1~", "produce"),
        PRODUCER_TENANT,
    );
    let response = policy().evaluate(&req);
    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 1);
}

#[test]
fn declared_subject_tenant_mismatch_denies() {
    let req = with_subject_tenant(
        request(PRODUCER, "gts.cf.core.events.request.v1~", "produce"),
        TENANT_A,
    );
    assert_plain_deny(&policy().evaluate(&req));
    let grant_req = without_required_constraints(with_subject_tenant(
        request(PRODUCER, EVENT_TYPE, "produce"),
        TENANT_A,
    ));
    assert_plain_deny(&policy().evaluate(&grant_req));
}

#[test]
fn declared_subject_tenant_missing_or_unparsable_denies() {
    let missing = request(PRODUCER, "gts.cf.core.events.request.v1~", "produce");
    assert_plain_deny(&policy().evaluate(&missing));
    let unparsable = with_subject_tenant(
        request(PRODUCER, "gts.cf.core.events.request.v1~", "produce"),
        "not-a-uuid",
    );
    assert_plain_deny(&policy().evaluate(&unparsable));
}

#[test]
fn an_undeclared_subject_tenant_ignores_the_subject_tenant_property() {
    let req = with_subject_tenant(request(BUYER, ORDER, "read"), TENANT_B);
    assert!(policy().evaluate(&req).decision);
}

#[test]
fn compile_refuses_an_invalid_config() {
    let yaml = r#"
rules:
  - id: "no-paths"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths: []
"#;
    assert!(matches!(compile(yaml), Err(ConfigError::EmptyPaths(id)) if id == "no-paths"));
}

#[test]
fn an_empty_policy_denies_everything() {
    let policy = compile("{}").expect("compiles");
    assert_plain_deny(
        &policy.evaluate(&without_required_constraints(request(BUYER, ORDER, "read"))),
    );
}

#[test]
fn grants_match_a_prefix_resource_type_and_a_wildcard_action() {
    let yaml = r#"
unconditional_grants:
  - id: "events-prefix"
    subject: { id: "0e0e0000-0000-4000-8000-000000000005" }
    resource_type: "gts.cf.core.events.*"
    action: "produce"
  - id: "any-action"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    action: "*"
"#;
    let policy = compile(yaml).expect("compiles");
    let prefix = policy.evaluate(&without_required_constraints(request(
        PRODUCER, EVENT_TYPE, "produce",
    )));
    assert!(prefix.decision);
    assert!(prefix.context.constraints.is_empty());
    let any_action = policy.evaluate(&without_required_constraints(request(
        BUYER, ORDER, "cancel",
    )));
    assert!(any_action.decision);
    assert!(any_action.context.constraints.is_empty());
}

#[test]
fn a_matching_rule_wins_over_a_matching_grant_when_constraints_are_not_required() {
    let yaml = r#"
rules:
  - id: "buyer"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
unconditional_grants:
  - id: "buyer-grant"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    action: "read"
"#;
    let response = compile(yaml)
        .expect("compiles")
        .evaluate(&without_required_constraints(with_resource_property(
            request(BUYER, ORDER, "read"),
            "resource_tenant_id",
            json!(TENANT_A),
        )));
    assert!(response.decision);
    assert_eq!(
        shape(&response),
        vec![vec![(
            "in",
            "resource_tenant_id".to_owned(),
            vec![json!(TENANT_A)]
        )]]
    );
}

#[test]
fn an_unsatisfied_rule_falls_through_to_a_grant_when_constraints_are_not_required() {
    let yaml = r#"
rules:
  - id: "buyer"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
unconditional_grants:
  - id: "buyer-grant"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    action: "read"
"#;
    let response = compile(yaml)
        .expect("compiles")
        .evaluate(&without_required_constraints(with_resource_property(
            request(BUYER, ORDER, "read"),
            "resource_tenant_id",
            json!(TENANT_B),
        )));
    assert!(response.decision);
    assert!(response.context.constraints.is_empty());
}

#[test]
fn a_matching_grant_wins_over_a_failed_delegation_when_constraints_are_not_required() {
    let yaml = r#"
rules:
  - id: "delegated"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
    delegation: { proof_property: "delegation_proof_ref", accepted: ["ok"] }
unconditional_grants:
  - id: "partner-grant"
    subject: { id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    action: "read"
"#;
    let policy = compile(yaml).expect("compiles");
    let response = policy.evaluate(&without_required_constraints(request(
        PARTNER, ORDER, "read",
    )));
    assert!(response.decision);
    assert!(response.context.constraints.is_empty());
    assert!(response.context.deny_reason.is_none());
    // With constraints required the grant is skipped and the delegation reason surfaces.
    assert_deny_reason(
        &policy.evaluate(&request(PARTNER, ORDER, "read")),
        "delegation_proof_required",
    );
}

#[test]
fn a_non_string_subject_tenant_with_a_declared_tenant_denies() {
    let mut req = request(PRODUCER, "gts.cf.core.events.request.v1~", "produce");
    req.subject
        .properties
        .insert("tenant_id".to_owned(), json!(42));
    assert_plain_deny(&policy().evaluate(&req));
}

/// Event Broker's `tenant_authorized` check: `owner_tenant_id` = the target tenant,
/// `require_constraints(false)`, the returned scope discarded.
fn event_broker_tenant_check(owner_tenant_id: Option<Value>) -> EvaluationRequest {
    let req = without_required_constraints(with_subject_tenant(
        request(PRODUCER, EVENTS_REQUEST, "produce"),
        PRODUCER_TENANT,
    ));
    match owner_tenant_id {
        Some(value) => with_resource_property(req, "owner_tenant_id", value),
        None => req,
    }
}

#[test]
fn event_broker_tenant_check_for_the_root_tenant_is_allowed_locally() {
    let response = policy().evaluate(&event_broker_tenant_check(Some(json!(ROOT_TENANT))));
    assert!(response.decision);
    assert!(response.context.deny_reason.is_none());
    // The rule's paths still travel; a PEP that does not require constraints ignores them.
    assert_eq!(
        shape(&response),
        vec![vec![(
            "in",
            "owner_tenant_id".to_owned(),
            vec![json!(ROOT_TENANT)]
        )]]
    );
}

#[test]
fn event_broker_tenant_check_compares_uuids_parsed() {
    let upper = ROOT_TENANT.to_uppercase();
    assert!(
        policy()
            .evaluate(&event_broker_tenant_check(Some(json!(upper))))
            .decision
    );
}

#[test]
fn event_broker_tenant_check_for_another_tenant_is_denied() {
    assert_plain_deny(&policy().evaluate(&event_broker_tenant_check(Some(json!(TENANT_A)))));
}

#[test]
fn event_broker_tenant_check_without_the_property_is_denied() {
    assert_plain_deny(&policy().evaluate(&event_broker_tenant_check(None)));
}

#[test]
fn a_non_uuid_or_non_string_value_of_a_uuid_property_is_unsatisfied() {
    assert_plain_deny(&policy().evaluate(&event_broker_tenant_check(Some(json!("root")))));
    assert_plain_deny(&policy().evaluate(&event_broker_tenant_check(Some(json!(42)))));
}

#[test]
fn an_eq_path_is_decided_locally_when_constraints_are_not_required() {
    let allowed = policy().evaluate(&without_required_constraints(with_resource_property(
        request(PRICING_READER, PRICE, "read"),
        "owner_tenant_id",
        json!(TENANT_A),
    )));
    assert!(allowed.decision);
    assert_eq!(
        shape(&allowed),
        vec![vec![(
            "eq",
            "owner_tenant_id".to_owned(),
            vec![json!(TENANT_A)]
        )]]
    );
    assert_plain_deny(
        &policy().evaluate(&without_required_constraints(with_resource_property(
            request(PRICING_READER, PRICE, "read"),
            "owner_tenant_id",
            json!(TENANT_B),
        ))),
    );
}

#[test]
fn local_decision_needs_every_predicate_of_one_path_and_compares_free_text_exactly() {
    let yaml = r#"
rules:
  - id: "two-paths"
    subject: { id: "11111111-6a88-4768-9dfc-6bcd5187d9ed" }
    resource_type: "gts.cf.bss.orders.order.v1~"
    actions: ["read"]
    paths:
      - predicates:
          - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
          - { property: "channel", value: "web" }
      - predicates:
          - { property: "seller_tenant_id", values: ["c0000000-0000-4000-8000-00000000000c"] }
"#;
    let policy = compile(yaml).expect("compiles");
    let base = || without_required_constraints(request(BUYER, ORDER, "read"));

    // First path: both predicates hold.
    let both = with_resource_property(
        with_resource_property(base(), "resource_tenant_id", json!(TENANT_A)),
        "channel",
        json!("web"),
    );
    let response = policy.evaluate(&both);
    assert!(response.decision);
    assert_eq!(response.context.constraints.len(), 2);

    // First path: one predicate missing, another wrong; second path absent.
    let partial = with_resource_property(base(), "resource_tenant_id", json!(TENANT_A));
    assert_plain_deny(&policy.evaluate(&partial));
    let wrong_text = with_resource_property(
        with_resource_property(base(), "resource_tenant_id", json!(TENANT_A)),
        "channel",
        json!("WEB"),
    );
    assert_plain_deny(&policy.evaluate(&wrong_text));

    // Second path alone suffices.
    let seller = with_resource_property(base(), "seller_tenant_id", json!(TENANT_S));
    assert!(policy.evaluate(&seller).decision);
}

#[test]
fn required_constraints_return_the_paths_without_checking_properties() {
    // Same Event Broker shape, but the PEP requires constraints: no property, a non-root
    // tenant — the constraints are returned for the PEP to enforce.
    for owner in [None, Some(json!(TENANT_A))] {
        let mut req = event_broker_tenant_check(owner);
        req.context.require_constraints = true;
        let response = policy().evaluate(&req);
        assert!(response.decision);
        assert_eq!(
            shape(&response),
            vec![vec![(
                "in",
                "owner_tenant_id".to_owned(),
                vec![json!(ROOT_TENANT)]
            )]]
        );
    }
}

#[test]
fn a_failed_delegation_is_still_reported_when_deciding_locally() {
    assert_deny_reason(
        &policy().evaluate(&without_required_constraints(with_resource_property(
            request(PARTNER, ORDER, "read"),
            "resource_tenant_id",
            json!(TENANT_A),
        ))),
        "delegation_proof_required",
    );
    let accepted = with_resource_property(
        with_resource_property(
            without_required_constraints(request(PARTNER, ORDER, "read")),
            "resource_tenant_id",
            json!(TENANT_A),
        ),
        "delegation_proof_ref",
        json!("e2e-proof-ok"),
    );
    assert!(policy().evaluate(&accepted).decision);
}

#[test]
fn an_unsatisfied_delegation_rule_records_no_reason_when_deciding_locally() {
    let local = |tenant: &str, proof: Option<&str>| {
        let req = with_resource_property(
            without_required_constraints(request(PARTNER, ORDER, "read")),
            "resource_tenant_id",
            json!(tenant),
        );
        match proof {
            Some(p) => with_resource_property(req, "delegation_proof_ref", json!(p)),
            None => req,
        }
    };
    // Tenant B, no proof: the rule is unsatisfied, so no delegation reason.
    assert_plain_deny(&policy().evaluate(&local(TENANT_B, None)));
    // Tenant B, accepted proof: still unsatisfied.
    assert_plain_deny(&policy().evaluate(&local(TENANT_B, Some("e2e-proof-ok"))));
    // Tenant A, no proof: unchanged.
    assert_deny_reason(
        &policy().evaluate(&local(TENANT_A, None)),
        "delegation_proof_required",
    );
    // With constraints required nothing changes: the reason surfaces whatever the tenant.
    let mut req = local(TENANT_B, None);
    req.context.require_constraints = true;
    assert_deny_reason(&policy().evaluate(&req), "delegation_proof_required");
}

#[test]
fn owner_tenant_bypass_shapes_are_denied_and_the_braced_uuid_is_the_same_identity() {
    // An array value, even holding the right tenant, is not a string.
    assert_plain_deny(&policy().evaluate(&event_broker_tenant_check(Some(json!([ROOT_TENANT])))));
    // An empty string is not a UUID.
    assert_plain_deny(&policy().evaluate(&event_broker_tenant_check(Some(json!("")))));
    // A braced UUID of the same tenant parses to the same identity.
    let braced = format!("{{{ROOT_TENANT}}}");
    assert!(
        policy()
            .evaluate(&event_broker_tenant_check(Some(json!(braced))))
            .decision
    );
}
