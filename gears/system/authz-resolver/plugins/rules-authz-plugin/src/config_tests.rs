use super::*;

fn u(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn valid_rule() -> RuleConfig {
    RuleConfig {
        id: "r1".to_owned(),
        subject: SubjectMatch {
            id: Some(u(1)),
            ..SubjectMatch::default()
        },
        resource_type: "gts.cf.example.thing.v1~".to_owned(),
        actions: vec!["read".to_owned()],
        paths: vec![PathConfig {
            predicates: vec![PredicateConfig {
                property: "seller_tenant_id".to_owned(),
                values: vec![u(2)],
            }],
        }],
        payer_use: None,
        delegation: None,
    }
}

fn config(rules: Vec<RuleConfig>) -> RulesAuthZPluginConfig {
    RulesAuthZPluginConfig {
        vendor: "constructorfabric".to_owned(),
        priority: 10,
        policy_revision: "rev-1".to_owned(),
        rules,
        unconditional_grants: Vec::new(),
    }
}

#[test]
fn valid_policy_and_empty_policy_are_accepted() {
    assert!(config(vec![valid_rule()]).validate().is_ok());
    // An empty policy is valid and denies everything.
    assert!(config(vec![]).validate().is_ok());
}

type Mutation = Box<dyn Fn(&mut RulesAuthZPluginConfig)>;

#[test]
fn malformed_policies_fail_startup() {
    let mutations: Vec<(&str, Mutation)> = vec![
        ("blank vendor", Box::new(|c| c.vendor = " ".to_owned())),
        ("blank revision", Box::new(|c| c.policy_revision.clear())),
        (
            "duplicate id",
            Box::new(|c| c.rules.push(c.rules[0].clone())),
        ),
        (
            "no subject",
            Box::new(|c| c.rules[0].subject = SubjectMatch::default()),
        ),
        (
            "nil subject",
            Box::new(|c| c.rules[0].subject.id = Some(Uuid::nil())),
        ),
        (
            "wildcard action",
            Box::new(|c| c.rules[0].actions = vec!["*".to_owned()]),
        ),
        (
            "wildcard resource",
            Box::new(|c| c.rules[0].resource_type = "gts.*".to_owned()),
        ),
        ("no actions", Box::new(|c| c.rules[0].actions.clear())),
        ("no paths", Box::new(|c| c.rules[0].paths.clear())),
        (
            "unconstrained path",
            Box::new(|c| c.rules[0].paths[0].predicates.clear()),
        ),
        (
            "empty values",
            Box::new(|c| c.rules[0].paths[0].predicates[0].values.clear()),
        ),
        (
            "nil value",
            Box::new(|c| c.rules[0].paths[0].predicates[0].values = vec![Uuid::nil()]),
        ),
        (
            "empty payer use",
            Box::new(|c| {
                c.rules[0].payer_use = Some(PayerUseConfig {
                    property: "payer_tenant_id".to_owned(),
                    values: vec![],
                });
            }),
        ),
        (
            "delegation accepts nothing",
            Box::new(|c| {
                c.rules[0].delegation = Some(DelegationConfig {
                    proof_property: "delegation_proof_ref".to_owned(),
                    accepted: vec![],
                });
            }),
        ),
    ];
    for (name, mutate) in mutations {
        let mut c = config(vec![valid_rule()]);
        mutate(&mut c);
        assert!(c.validate().is_err(), "{name} must fail validation");
    }
}

#[test]
fn unknown_fields_are_rejected_by_deserialization() {
    let value = serde_json::json!({
        "vendor": "v", "priority": 1, "policy_revision": "r", "rules": [],
        "default_allow": true
    });
    assert!(serde_json::from_value::<RulesAuthZPluginConfig>(value).is_err());
    let missing_rules = serde_json::json!({"vendor": "v", "priority": 1, "policy_revision": "r"});
    assert!(serde_json::from_value::<RulesAuthZPluginConfig>(missing_rules).is_err());
}

fn grant(id: &str) -> UnconditionalGrantConfig {
    UnconditionalGrantConfig {
        id: id.to_owned(),
        subject: ExactSubject {
            id: u(7),
            tenant_id: u(8),
        },
        resource_type: "gts.cf.core.events.event_type.v1~".to_owned(),
        action: "produce".to_owned(),
    }
}

type GrantMutation = (&'static str, Box<dyn Fn(&mut RulesAuthZPluginConfig)>);

#[test]
fn unconditional_grants_are_exact_and_malformed_ones_fail_startup() {
    let mut ok = config(vec![valid_rule()]);
    ok.unconditional_grants = vec![grant("g1")];
    assert!(ok.validate().is_ok());
    let mutations: Vec<GrantMutation> = vec![
        (
            "blank id",
            Box::new(|c| c.unconditional_grants[0].id = " ".to_owned()),
        ),
        (
            "id shared with a rule",
            Box::new(|c| c.unconditional_grants[0].id = "r1".to_owned()),
        ),
        (
            "wildcard type",
            Box::new(|c| c.unconditional_grants[0].resource_type = "gts.cf.*".to_owned()),
        ),
        (
            "blank type",
            Box::new(|c| c.unconditional_grants[0].resource_type = String::new()),
        ),
        (
            "wildcard action",
            Box::new(|c| c.unconditional_grants[0].action = "*".to_owned()),
        ),
        (
            "blank action",
            Box::new(|c| c.unconditional_grants[0].action = String::new()),
        ),
        (
            "nil subject",
            Box::new(|c| c.unconditional_grants[0].subject.id = Uuid::nil()),
        ),
        (
            "nil tenant",
            Box::new(|c| c.unconditional_grants[0].subject.tenant_id = Uuid::nil()),
        ),
        (
            "duplicate triple",
            Box::new(|c| c.unconditional_grants.push(grant("g2"))),
        ),
    ];
    for (name, mutate) in mutations {
        let mut c = ok.clone();
        mutate(&mut c);
        assert!(c.validate().is_err(), "{name} must fail validation");
    }
    // Shape: one action (no list), an exact subject (both fields), no unknown fields.
    let base = serde_json::json!({
        "vendor": "v", "priority": 1, "policy_revision": "r", "rules": [],
        "unconditional_grants": [{"id": "g", "subject": {"id": u(7), "tenant_id": u(8)},
            "resource_type": "t", "action": "produce"}]
    });
    assert!(serde_json::from_value::<RulesAuthZPluginConfig>(base.clone()).is_ok());
    for (pointer, value) in [
        (
            "/unconditional_grants/0/action",
            serde_json::json!(["produce"]),
        ),
        (
            "/unconditional_grants/0/subject",
            serde_json::json!({"id": u(7)}),
        ),
        (
            "/unconditional_grants/0/subject",
            serde_json::json!({"subject_type": "user"}),
        ),
        ("/unconditional_grants/0/constraints", serde_json::json!([])),
    ] {
        let mut value_doc = base.clone();
        if let Some(slot) = value_doc.pointer_mut(pointer) {
            *slot = value;
        } else {
            value_doc["unconditional_grants"][0]["constraints"] = value;
        }
        assert!(
            serde_json::from_value::<RulesAuthZPluginConfig>(value_doc).is_err(),
            "{pointer}"
        );
    }
}
