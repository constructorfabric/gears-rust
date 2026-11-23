use anyhow::Context as _;
use serde_json::{Value, json};

use super::*;

const A: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const B: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const TA: &str = "00000000-0000-0000-0000-000000000002";
const TC: &str = "00000000-0000-0000-0000-000000000004";
const TOPIC: &str = "gts.cf.core.events.topic.v1~acme.orders.x.orders.v1";
const E: &str = "gts.cf.core.events.event.v1~";
const E1: &str = "gts.cf.core.events.event.v1~acme.orders.x.placed.v1~";
const S: &str = "gts.acme.orders.x.customer.v1~";
const S1: &str = "gts.acme.orders.x.customer.v1~acme.orders.x.vip.v1~";

fn policy(grants: &[Value]) -> Result<TestPolicyConfig> {
    Ok(serde_json::from_value(json!({"grants": grants}))?)
}

fn consume() -> Result<EvaluationRequest> {
    Ok(serde_json::from_value(json!({
        "subject": {"id": A, "properties": {"tenant_id": TA}},
        "action": {"name": "consume"},
        "resource": {"type": TOPIC, "id": B},
        "context": {"require_constraints": true, "supported_properties": [
            "owner_tenant_id", "owner_id", "event_type", "event_subject_type"
        ]}
    }))?)
}

fn hierarchy() -> Result<EvaluationResponse> {
    Ok(serde_json::from_value(
        json!({"decision": true, "context": {"constraints": [
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TA]},
                {"op": "eq", "property": "owner_id", "value": A}
            ]},
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TC]},
                {"op": "eq", "property": "owner_id", "value": B}
            ]}
        ]}}),
    )?)
}

#[tokio::test]
async fn the_policy_returns_the_shipped_client_error_unchanged() -> Result<()> {
    struct UnavailableHierarchy;

    #[async_trait]
    impl AuthZResolverPluginClient for UnavailableHierarchy {
        async fn evaluate(
            &self,
            _: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            Err(AuthZResolverError::ServiceUnavailable(
                "hierarchy unavailable".into(),
            ))
        }
    }

    let policy = NarrowingPolicy {
        hierarchy: Arc::new(UnavailableHierarchy),
        config: policy(&[
            json!({"subject_id": A, "action": "consume", "resource": TOPIC, "event_types": [E]}),
        ])?,
    };
    let response = policy.evaluate(consume()?).await;
    let Err(AuthZResolverError::ServiceUnavailable(detail)) = response else {
        panic!("expected the shipped client's service error, got {response:?}");
    };
    assert_eq!(detail, "hierarchy unavailable");
    Ok(())
}

#[test]
fn the_policy_intersects_each_tenant_branch_with_each_matching_grant() -> Result<()> {
    let request = consume()?;
    let base = json!({"subject_id": A, "action": "consume", "resource": "gts.cf.core.events.topic.v1~*",
        "event_types": [E], "subject_types": [S]});
    let derived = json!({"subject_id": A, "action": "consume", "resource": TOPIC,
        "event_types": [E1], "subject_types": [S1]});
    let only_base = intersect(
        &request,
        hierarchy()?,
        &policy(std::slice::from_ref(&base))?,
    );
    let only_base = serde_json::to_value(only_base)?;
    assert_eq!(
        only_base,
        json!({"decision": true, "context": {"constraints": [
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TA]},
                {"op": "eq", "property": "owner_id", "value": A},
                {"op": "in", "property": "event_type", "values": [E]},
                {"op": "in", "property": "event_subject_type", "values": [S]}
            ]},
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TC]},
                {"op": "eq", "property": "owner_id", "value": B},
                {"op": "in", "property": "event_type", "values": [E]},
                {"op": "in", "property": "event_subject_type", "values": [S]}
            ]}
        ]}})
    );
    let both = serde_json::to_value(intersect(
        &request,
        hierarchy()?,
        &policy(&[base, derived])?,
    ))?;
    assert_eq!(
        both,
        json!({"decision": true, "context": {"constraints": [
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TA]},
                {"op": "eq", "property": "owner_id", "value": A},
                {"op": "in", "property": "event_type", "values": [E]},
                {"op": "in", "property": "event_subject_type", "values": [S]}
            ]},
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TA]},
                {"op": "eq", "property": "owner_id", "value": A},
                {"op": "in", "property": "event_type", "values": [E1]},
                {"op": "in", "property": "event_subject_type", "values": [S1]}
            ]},
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TC]},
                {"op": "eq", "property": "owner_id", "value": B},
                {"op": "in", "property": "event_type", "values": [E]},
                {"op": "in", "property": "event_subject_type", "values": [S]}
            ]},
            {"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TC]},
                {"op": "eq", "property": "owner_id", "value": B},
                {"op": "in", "property": "event_type", "values": [E1]},
                {"op": "in", "property": "event_subject_type", "values": [S1]}
            ]}
        ]}})
    );
    for (event, subject, in_base, in_both) in [
        (E, S, true, true),
        (E1, S, false, false),
        (E, S1, false, false),
        (E1, S1, false, true),
    ] {
        for (response, expected) in [(&only_base, in_base), (&both, in_both)] {
            let branches = response["context"]["constraints"]
                .as_array()
                .context("constraint branches")?;
            assert_eq!(
                branches.iter().any(|branch| {
                    let Some(predicates) = branch["predicates"].as_array() else {
                        return false;
                    };
                    predicates
                        .iter()
                        .any(|p| p["property"] == "event_type" && p["values"] == json!([event]))
                        && predicates.iter().any(|p| {
                            p["property"] == "event_subject_type" && p["values"] == json!([subject])
                        })
                }),
                expected,
                "type pair ({event}, {subject})"
            );
        }
    }
    Ok(())
}

#[test]
fn the_policy_preserves_a_hierarchy_denial_and_fails_closed_on_incomplete_fences() -> Result<()> {
    let request = consume()?;
    let config = policy(&[
        json!({"subject_id": A, "action": "consume", "resource": TOPIC, "event_types": [E]}),
    ])?;
    let denied = json!({"decision": false, "context": {"constraints": [],
        "deny_reason": {"error_code": "fixture-denied", "details": "hierarchy denied the scope"}}});
    assert_eq!(
        serde_json::to_value(intersect(
            &request,
            serde_json::from_value(denied.clone())?,
            &config
        ))?,
        denied
    );
    for (label, branches) in [
        ("no branches", json!([])),
        (
            "no tenant fence",
            json!([{"predicates": [{"op": "eq", "property": "owner_id", "value": A}]}]),
        ),
        (
            "membership of another property provides no tenant fence",
            json!([{"predicates": [{"op": "in", "property": "owner_id", "values": [A]}]}]),
        ),
        (
            "tenant equality needs a UUID",
            json!([{"predicates": [{"op": "eq", "property": "owner_tenant_id", "value": 1}]}]),
        ),
        (
            "only the first branch has a tenant fence",
            json!([
                {"predicates": [{"op": "in", "property": "owner_tenant_id", "values": [TA]}]},
                {"predicates": [{"op": "eq", "property": "owner_id", "value": A}]}
            ]),
        ),
        (
            "tenant fence has no values",
            json!([{"predicates": [{"op": "in", "property": "owner_tenant_id", "values": []}]}]),
        ),
        (
            "tenant fence contains an invalid ID",
            json!([{"predicates": [{"op": "in", "property": "owner_tenant_id", "values": [TA, "invalid"]}]}]),
        ),
    ] {
        let fabricated = serde_json::from_value(
            json!({"decision": true, "context": {"constraints": branches}}),
        )?;
        assert_eq!(
            serde_json::to_value(intersect(&request, fabricated, &config))?,
            json!({"decision": false, "context": {"constraints": []}}),
            "{label}"
        );
    }
    Ok(())
}

#[test]
fn the_policy_preserves_unrestricted_dimensions_but_refuses_empty_or_unsupported_ones() -> Result<()>
{
    let request = consume()?;
    let input = json!({"decision": true, "context": {"constraints": [
        {"predicates": [{"op": "eq", "property": "owner_tenant_id", "value": TA}]}
    ]}});
    let unscoped = json!({"subject_id": A, "action": "consume", "resource": TOPIC});
    let mut event = unscoped.clone();
    event["event_types"] = json!([E]);
    let mut subject = unscoped.clone();
    subject["subject_types"] = json!([S]);
    let mut empty_event = unscoped.clone();
    empty_event["event_types"] = json!([]);
    let mut empty_subject = unscoped.clone();
    empty_subject["subject_types"] = json!([]);
    for (label, grants, properties, expected) in [
        (
            "both dimensions are absent",
            vec![unscoped],
            &["owner_tenant_id"][..],
            input.clone(),
        ),
        (
            "event dimension only",
            vec![event.clone()],
            &["owner_tenant_id", "event_type"],
            json!({"decision": true, "context": {"constraints": [{"predicates": [
                {"op": "eq", "property": "owner_tenant_id", "value": TA},
                {"op": "in", "property": "event_type", "values": [E]}
            ]}]}}),
        ),
        (
            "subject dimension only",
            vec![subject.clone()],
            &["owner_tenant_id", "event_subject_type"],
            json!({"decision": true, "context": {"constraints": [{"predicates": [
                {"op": "eq", "property": "owner_tenant_id", "value": TA},
                {"op": "in", "property": "event_subject_type", "values": [S]}
            ]}]}}),
        ),
        (
            "empty event dimension",
            vec![empty_event.clone()],
            &["owner_tenant_id", "event_type"],
            json!({"decision": false, "context": {"constraints": []}}),
        ),
        (
            "empty subject dimension",
            vec![empty_subject],
            &["owner_tenant_id", "event_subject_type"],
            json!({"decision": false, "context": {"constraints": []}}),
        ),
        (
            "unsupported event dimension",
            vec![event.clone()],
            &["owner_tenant_id"],
            json!({"decision": false, "context": {"constraints": []}}),
        ),
        (
            "unsupported subject dimension",
            vec![subject],
            &["owner_tenant_id"],
            json!({"decision": false, "context": {"constraints": []}}),
        ),
        (
            "unrelated empty alternative leaves valid grant",
            vec![empty_event, event],
            &["owner_tenant_id", "event_type"],
            json!({"decision": true, "context": {"constraints": [{"predicates": [
                {"op": "eq", "property": "owner_tenant_id", "value": TA},
                {"op": "in", "property": "event_type", "values": [E]}
            ]}]}}),
        ),
    ] {
        let mut request = request.clone();
        request.context.supported_properties = properties
            .iter()
            .map(|property| (*property).into())
            .collect();
        assert_eq!(
            serde_json::to_value(intersect(
                &request,
                serde_json::from_value(input.clone())?,
                &policy(&grants)?
            ))?,
            expected,
            "{label}"
        );
    }
    Ok(())
}

#[test]
fn the_policy_selects_grants_by_subject_action_and_the_gts_resource() -> Result<()> {
    let request = consume()?;
    let grant = json!({"subject_id": A, "action": "consume", "resource": TOPIC});
    let config = policy(std::slice::from_ref(&grant))?;
    let expected = serde_json::to_value(hierarchy()?)?;
    assert_eq!(
        serde_json::to_value(intersect(&request, hierarchy()?, &config))?,
        expected
    );
    assert_eq!(
        serde_json::to_value(intersect(&request, hierarchy()?, &policy(&[])?))?,
        json!({"decision": false, "context": {"constraints": []}})
    );
    let mut wildcard = request.clone();
    wildcard.resource.resource_type = "gts.cf.core.events.topic.v1~*".to_owned();
    assert_eq!(
        serde_json::to_value(intersect(&wildcard, hierarchy()?, &config))?,
        json!({"decision": false, "context": {"constraints": []}})
    );
    for (field, value) in [("subject_id", B), ("action", "Consume"), ("resource", E)] {
        let mut unmatched = grant.clone();
        unmatched[field] = json!(value);
        assert_eq!(
            serde_json::to_value(intersect(&request, hierarchy()?, &policy(&[unmatched])?))?,
            json!({"decision": false, "context": {"constraints": []}}),
            "grant mismatch at {field}"
        );
    }
    let mut unrelated = request;
    unrelated.resource.resource_type = E.to_owned();
    let scoped = json!({"subject_id": A, "action": "consume", "resource": E, "event_types": [E]});
    assert_eq!(
        serde_json::to_value(intersect(&unrelated, hierarchy()?, &policy(&[scoped])?))?,
        json!({"decision": false, "context": {"constraints": []}})
    );
    unrelated.resource.resource_type = TOPIC.to_owned();
    unrelated.action.name = "produce".to_owned();
    let scoped =
        json!({"subject_id": A, "action": "produce", "resource": TOPIC, "event_types": [E]});
    assert_eq!(
        serde_json::to_value(intersect(&unrelated, hierarchy()?, &policy(&[scoped])?))?,
        json!({"decision": false, "context": {"constraints": []}})
    );
    Ok(())
}

#[test]
fn a_decision_only_create_keeps_the_hierarchy_decision_without_a_scope() -> Result<()> {
    let grant = json!({"subject_id": A, "action": "create", "resource": TOPIC});
    let config = policy(&[grant])?;
    let mut request = consume()?;
    request.action.name = "create".into();
    request.resource.id = None;
    request.context.require_constraints = false;
    request.context.supported_properties.clear();
    let allowed = json!({"decision": true, "context": {"constraints": []}});
    assert_eq!(
        serde_json::to_value(intersect(
            &request,
            serde_json::from_value(allowed.clone())?,
            &config
        ))?,
        allowed
    );
    for flag in [
        "resource id",
        "constraints requested",
        "supported properties",
        "group_ids",
        "ancestor_group_ids",
    ] {
        let mut request = request.clone();
        match flag {
            "resource id" => request.resource.id = Some(Uuid::parse_str(B)?),
            "constraints requested" => request.context.require_constraints = true,
            "supported properties" => {
                request.context.supported_properties = vec!["owner_tenant_id".into()];
            }
            property => {
                let previous = request
                    .resource
                    .properties
                    .insert(property.into(), json!([B]));
                assert_eq!(previous, None);
            }
        }
        assert_eq!(
            serde_json::to_value(intersect(
                &request,
                serde_json::from_value(allowed.clone())?,
                &config
            ))?,
            json!({"decision": false, "context": {"constraints": []}}),
            "create with {flag}"
        );
    }
    request.action.name = "define".to_owned();
    let config = policy(&[json!({"subject_id": A, "action": "define", "resource": TOPIC})])?;
    assert_eq!(
        serde_json::to_value(intersect(
            &request,
            serde_json::from_value(allowed)?,
            &config
        ))?,
        json!({"decision": false, "context": {"constraints": []}})
    );
    Ok(())
}

fn effective_config(policy: &Value) -> AppConfig {
    AppConfig {
        gears: [("eb-test-policy".into(), json!({"config": policy}))].into(),
        ..AppConfig::default()
    }
}

#[test]
fn the_policy_configuration_accepts_only_bounded_exact_type_lists() -> Result<()> {
    let grant =
        json!({"subject_id": A, "action": "consume", "resource": "gts.cf.core.events.topic.v1~*"});
    for (count, expected) in [
        (0, Ok(())),
        (256, Ok(())),
        (257, Err(CONFIG_ERROR.to_owned())),
    ] {
        assert_eq!(
            validate_config(&effective_config(
                &json!({"grants": vec![grant.clone(); count]})
            ))
            .map_err(|error| error.to_string()),
            expected,
            "grant count {count}"
        );
    }
    for (events, subjects, expected) in [
        (128, 128, Ok(())),
        (128, 129, Err(CONFIG_ERROR.to_owned())),
        (256, 0, Ok(())),
        (257, 0, Err(CONFIG_ERROR.to_owned())),
        (0, 256, Ok(())),
        (0, 257, Err(CONFIG_ERROR.to_owned())),
    ] {
        let mut typed = grant.clone();
        typed["event_types"] = json!(vec![E; events]);
        typed["subject_types"] = json!(vec![S; subjects]);
        assert_eq!(
            validate_config(&effective_config(&json!({"grants": [typed]})))
                .map_err(|error| error.to_string()),
            expected,
            "event/subject counts {events}/{subjects}"
        );
    }
    let mut typed = grant.clone();
    typed["event_types"] = json!([E, E1]);
    typed["subject_types"] = json!([S, S1]);
    validate_config(&effective_config(&json!({"grants": [typed]})))?;
    for field in ["event_types", "subject_types"] {
        for invalid in ["gts.cf.core.events.event.v1~*", "gts.invalid", TOPIC] {
            let mut typed = grant.clone();
            typed[field] = json!([invalid]);
            assert_eq!(
                validate_config(&effective_config(&json!({"grants": [typed]})))
                    .map_err(|error| error.to_string()),
                Err(CONFIG_ERROR.to_owned()),
                "invalid {field} member {invalid}"
            );
        }
    }
    for (field, value) in [("resource", json!("gts.invalid")), ("unknown", json!(true))] {
        let mut malformed = grant.clone();
        malformed[field] = value;
        assert_eq!(
            validate_config(&effective_config(&json!({"grants": [malformed]})))
                .map_err(|error| error.to_string()),
            Err(CONFIG_ERROR.to_owned()),
            "invalid grant field {field}"
        );
    }
    assert_eq!(
        validate_config(&effective_config(
            &json!({"grants": [grant], "unknown": true})
        ))
        .map_err(|error| error.to_string()),
        Err(CONFIG_ERROR.to_owned())
    );
    Ok(())
}

#[cfg(feature = "event-broker")]
#[test]
fn the_binary_links_exactly_the_event_broker_gears_and_capabilities() -> Result<()> {
    let registry = toolkit::GearRegistry::discover_and_build()?;
    let mut gear_and_capabilities: Vec<_> = registry
        .gears()
        .iter()
        .map(|gear| {
            let mut capabilities = gear.caps().labels();
            capabilities.sort_unstable();
            (gear.name(), capabilities)
        })
        .collect();
    gear_and_capabilities.sort_unstable();
    assert_eq!(
        gear_and_capabilities,
        [
            ("api-gateway", vec!["rest", "rest_host", "stateful"]),
            ("authn-resolver", vec!["gts_publication"]),
            ("authz-resolver", vec!["gts_publication", "rest"]),
            #[cfg(feature = "cluster")]
            ("cluster", vec!["grpc", "rest", "stateful", "system"]),
            ("eb-test-policy", vec!["gts_publication"]),
            ("grpc-hub", vec!["grpc_hub", "stateful", "system"]),
            (
                "service-discovery",
                vec!["grpc", "rest", "stateful", "system"]
            ),
            ("static-authn-plugin", vec!["gts_publication"]),
            ("static-tr-plugin", vec!["gts_publication"]),
            ("tenant-resolver", vec!["gts_publication"]),
            ("test-flight-control-authn", vec!["grpc"]),
            ("tr-authz-plugin", vec!["gts_publication"]),
            ("types-registry", vec!["db", "rest", "stateful", "system"]),
        ]
    );
    Ok(())
}
