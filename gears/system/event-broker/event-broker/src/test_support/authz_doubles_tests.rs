//! The constraint the allowing doubles answer. Each test calls `evaluate`
//! directly with a hand-built request and compares the whole response, as
//! JSON, to a literal derived from the response types' fields and serde
//! attributes (`authz-resolver-sdk` `models.rs`, `constraints.rs`).

use std::collections::HashMap;

use authz_resolver_sdk::{
    Action, AuthZResolverApi, BarrierMode, EvaluationRequest, EvaluationRequestContext,
    EvaluationResponse, Resource, Subject, TenantContext,
};
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use super::{AllowAllAuthZ, DenyingAuthZ, Grant, PolicyAuthZ, Tenant};

const ROOT: Uuid = Uuid::from_u128(0x7001);

/// A `produce` request on the tenant-scope resource, with the given tenant
/// context and supported properties.
fn request(
    tenant_context: Option<TenantContext>,
    supported_properties: &[&str],
) -> EvaluationRequest {
    EvaluationRequest {
        subject: Subject {
            id: Uuid::from_u128(0x5001),
            subject_type: None,
            properties: HashMap::new(),
        },
        action: Action {
            name: "produce".to_owned(),
        },
        resource: Resource {
            resource_type: "gts.cf.core.eb.request.v1~".to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context,
            token_scopes: Vec::new(),
            require_constraints: true,
            capabilities: Vec::new(),
            supported_properties: supported_properties
                .iter()
                .map(|property| (*property).to_owned())
                .collect(),
            bearer_token: None,
        },
    }
}

fn rooted_at_root() -> TenantContext {
    TenantContext {
        root_id: Some(ROOT),
        ..Default::default()
    }
}

async fn evaluate(
    authz: &dyn AuthZResolverApi,
    request: EvaluationRequest,
) -> Result<EvaluationResponse, CanonicalError> {
    authz
        .evaluate(PlatformSecurityContext::outbound_marker(), request)
        .await
}

/// `decision: true` with the one constraint `In(owner_tenant_id, [ROOT])`.
fn allow_with_the_root_constraint() -> Value {
    json!({
        "decision": true,
        "context": {
            "constraints": [{
                "predicates": [{
                    "op": "in",
                    "property": "owner_tenant_id",
                    "values": ["00000000-0000-0000-0000-000000007001"],
                }],
            }],
        },
    })
}

fn allow_without_constraints() -> Value {
    json!({"decision": true, "context": {"constraints": []}})
}

#[tokio::test]
async fn allow_all_answers_the_owner_tenant_constraint_when_a_root_is_sent() {
    let response = evaluate(
        &AllowAllAuthZ,
        request(Some(rooted_at_root()), &["owner_tenant_id"]),
    )
    .await
    .unwrap();

    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        allow_with_the_root_constraint()
    );
}

#[tokio::test]
async fn allow_all_answers_no_constraint_when_owner_tenant_id_is_not_supported() {
    let response = evaluate(
        &AllowAllAuthZ,
        request(Some(rooted_at_root()), &["resource_id"]),
    )
    .await
    .unwrap();

    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        allow_without_constraints()
    );
}

#[tokio::test]
async fn allow_all_answers_no_constraint_without_a_root() {
    let no_context = evaluate(&AllowAllAuthZ, request(None, &["owner_tenant_id"]))
        .await
        .unwrap();
    let no_root = evaluate(
        &AllowAllAuthZ,
        request(Some(TenantContext::default()), &["owner_tenant_id"]),
    )
    .await
    .unwrap();

    assert_eq!(
        serde_json::to_value(&no_context).unwrap(),
        allow_without_constraints()
    );
    assert_eq!(
        serde_json::to_value(&no_root).unwrap(),
        allow_without_constraints()
    );
}

#[tokio::test]
async fn denying_authz_answers_like_allow_all_when_it_allows() {
    let authz = DenyingAuthZ {
        deny_if: |_request: &EvaluationRequest| false,
    };

    let response = evaluate(
        &authz,
        request(Some(rooted_at_root()), &["owner_tenant_id"]),
    )
    .await
    .unwrap();

    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        allow_with_the_root_constraint()
    );
}

#[tokio::test]
async fn denying_authz_answers_no_constraint_when_it_denies() {
    let authz = DenyingAuthZ {
        deny_if: |_request: &EvaluationRequest| true,
    };

    let response = evaluate(
        &authz,
        request(Some(rooted_at_root()), &["owner_tenant_id"]),
    )
    .await
    .unwrap();

    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"decision": false, "context": {"constraints": []}})
    );
}

/// The barrier right needs reach as well as the grant. Caller `0x890` sits
/// in `0x234` of the example tree of `pdp_fixtures`: root `0x123` ->
/// `0x234` -> {`0xC`, self-managed `0xB` -> `0xB1`}, and `0x999`, a second
/// child of `0x123`. `0xB` is in `0x234`'s subtree with barriers ignored, so
/// the grant allows there; `0x999` is a sibling of `0x234`, outside its
/// subtree under any barrier mode, so the same grant is denied there.
#[tokio::test]
async fn the_barrier_right_is_denied_outside_the_callers_reach_even_with_the_grant() {
    const CALLER: Uuid = Uuid::from_u128(0x890);
    const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.doubles.orders.v1";
    let [t123, t234, c, b, b1, t999] = [0x123, 0x234, 0xC, 0xB, 0xB1, 0x999].map(Uuid::from_u128);
    let policy = PolicyAuthZ::new([
        Tenant::root(t123),
        Tenant::child(t234, t123),
        Tenant::child(c, t234),
        Tenant::self_managed_child(b, t234),
        Tenant::child(b1, b),
        Tenant::child(t999, t123),
    ])
    .expect("the example tree is well formed")
    .with_grant(Grant::new(CALLER, TOPIC, "consume_ignore_barrier"));
    let barrier_right = |root: Uuid| EvaluationRequest {
        subject: Subject {
            id: CALLER,
            subject_type: None,
            properties: HashMap::from([("tenant_id".to_owned(), json!(t234.to_string()))]),
        },
        action: Action {
            name: "consume_ignore_barrier".to_owned(),
        },
        resource: Resource {
            resource_type: TOPIC.to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: Some(TenantContext {
                root_id: Some(root),
                barrier_mode: BarrierMode::Ignore,
                ..Default::default()
            }),
            token_scopes: Vec::new(),
            require_constraints: false,
            capabilities: Vec::new(),
            supported_properties: vec!["owner_tenant_id".to_owned()],
            bearer_token: None,
        },
    };

    let reached = evaluate(&policy, barrier_right(b)).await.unwrap();
    let unreached = evaluate(&policy, barrier_right(t999)).await.unwrap();

    assert_eq!(
        serde_json::to_value(&reached).unwrap(),
        json!({
            "decision": true,
            "context": {
                "constraints": [{
                    "predicates": [{
                        "op": "in",
                        "property": "owner_tenant_id",
                        "values": ["00000000-0000-0000-0000-00000000000b"],
                    }],
                }],
            },
        })
    );
    assert_eq!(
        serde_json::to_value(&unreached).unwrap(),
        json!({"decision": false, "context": {"constraints": []}})
    );
}

/// The consumer-group rules over the example tree of `pdp_fixtures`, with
/// caller `0x890` of `0x234`: each action needs its own grant on the resource
/// type and the root within the caller's reach. A named group's resource type
/// is its own id, so a grant on one named group is no grant on another.
#[tokio::test]
async fn consumer_group_actions_need_their_grant_and_the_root_in_reach() {
    const CALLER: Uuid = Uuid::from_u128(0x890);
    const FAMILY: &str = "gts.cf.core.events.consumer_group.v1~";
    const NAMED: &str = "gts.cf.core.events.consumer_group.v1~x.eb.doubles.named.v1";
    const OTHER_NAMED: &str = "gts.cf.core.events.consumer_group.v1~x.eb.doubles.other.v1";
    let [t123, t234, t999] = [0x123, 0x234, 0x999].map(Uuid::from_u128);
    let policy = PolicyAuthZ::new([
        Tenant::root(t123),
        Tenant::child(t234, t123),
        Tenant::child(t999, t123),
    ])
    .expect("the tree is well formed")
    .with_grant(Grant::new(CALLER, FAMILY, "consume"))
    .with_grant(Grant::new(CALLER, FAMILY, "define"))
    .with_grant(Grant::new(CALLER, NAMED, "manage"));
    let group_request = |action: &str, resource_type: &str, root: Option<Uuid>| EvaluationRequest {
        subject: Subject {
            id: CALLER,
            subject_type: None,
            properties: HashMap::from([("tenant_id".to_owned(), json!(t234.to_string()))]),
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
            tenant_context: root.map(|root| TenantContext {
                root_id: Some(root),
                ..Default::default()
            }),
            token_scopes: Vec::new(),
            require_constraints: false,
            capabilities: Vec::new(),
            supported_properties: vec!["owner_tenant_id".to_owned()],
            bearer_token: None,
        },
    };

    let mut decisions = Vec::new();
    for (action, resource_type, root) in [
        ("consume", FAMILY, Some(t234)),
        ("define", FAMILY, Some(t234)),
        ("manage", FAMILY, Some(t234)),
        ("consume", FAMILY, Some(t999)),
        ("consume", FAMILY, None),
        ("manage", NAMED, Some(t234)),
        ("manage", OTHER_NAMED, Some(t234)),
        ("consume", NAMED, Some(t234)),
    ] {
        let response = evaluate(&policy, group_request(action, resource_type, root))
            .await
            .unwrap();
        decisions.push((action, resource_type, root, response.decision));
    }

    assert_eq!(
        decisions,
        vec![
            ("consume", FAMILY, Some(t234), true),
            ("define", FAMILY, Some(t234), true),
            ("manage", FAMILY, Some(t234), false),
            ("consume", FAMILY, Some(t999), false),
            ("consume", FAMILY, None, false),
            ("manage", NAMED, Some(t234), true),
            ("manage", OTHER_NAMED, Some(t234), false),
            ("consume", NAMED, Some(t234), false),
        ]
    );
    let allowed = evaluate(&policy, group_request("consume", FAMILY, Some(t234)))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&allowed).unwrap(),
        json!({
            "decision": true,
            "context": {
                "constraints": [{
                    "predicates": [{
                        "op": "in",
                        "property": "owner_tenant_id",
                        "values": ["00000000-0000-0000-0000-000000000234"],
                    }],
                }],
            },
        })
    );
}
