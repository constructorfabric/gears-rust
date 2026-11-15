//! The PDP calls a JOIN makes for what it consumes, their order, what each
//! answer does to the JOIN, the post-check of exact selectors, and the memo.
//!
//! Interests on one topic, interest tenant and barrier mode form a group. Per
//! group, in the order the groups' first interests appear, JOIN asks I (the
//! barrier right, only for `ignore`), then E (consume on the topic), then R
//! (the tenant check, only when E fails). Each test scripts the answer per
//! call, records every request the PDP sees after the consumer group exists,
//! and asserts the whole list: the group check first, then the calls in order.
//!
//! The caller sits in tenant `0x123`; the interests name `0x234` (and `0x345`),
//! so a request rooted at the caller's tenant shows in the literal.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AccessRequest, AuthZResolverApi, BarrierMode, Constraint, DenyReason, EvaluationRequest,
    EvaluationResponse, EvaluationResponseContext, InPredicate, PolicyEnforcer, Predicate,
    ResourceType, TenantMode,
};
use serde_json::{Value, json};
use tokio::time::Instant;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{
    AccessScope, PlatformSecurityContext, ScopeConstraint, ScopeFilter, SecurityContext,
    pep_properties,
};
use uuid::Uuid;

use crate::domain::streaming::scope::{CompiledScope, TenantSetInterner};
use crate::domain::streaming::scope_memo::{MemoKey, MemoResult};
use crate::test_support::response::TestResponse;
use crate::test_support::{EventBrokerHarness, Json, StaticTypesRegistry};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const CONSUMER_GROUP_RESOURCE: &str = "gts.cf.core.events.consumer_group.v1~";
const TOPIC_A: &str = "gts.cf.core.events.topic.v1~x.eb.joinauthz.a.v1";
const TOPIC_B: &str = "gts.cf.core.events.topic.v1~x.eb.joinauthz.b.v1";
const CREATED: &str = "gts.cf.core.events.event.v1~x.eb.joinauthz.created.v1~";
const SHIPPED: &str = "gts.cf.core.events.event.v1~x.eb.joinauthz.shipped.v1~";
const B_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.joinauthz.b_happened.v1~";
const ORDER: &str = "gts.x.eb.joinauthz.order.v1~";
const INVOICE: &str = "gts.x.eb.joinauthz.invoice.v1~";
/// A selector outside the event family: it cannot match any event type.
const NOT_AN_EVENT: &str = "gts.x.eb.joinauthz.not_an_event.v1~";

const SUBJECT: Uuid = Uuid::from_u128(0x456);
const CALLER_TENANT: Uuid = Uuid::from_u128(0x123);
const TENANT: Uuid = Uuid::from_u128(0x234);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x345);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// Which call of a JOIN a request is. The group check, E, R and the tenant
/// gate of a later read share their action: the group check is on a
/// consumer-group resource, E on the topic, and the gate and R on the
/// tenant-scope resource, where only R requires constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Define,
    Gate,
    Group,
    I,
    E,
    R,
}

impl Call {
    fn of(request: &EvaluationRequest) -> Self {
        match (
            request.action.name.as_str(),
            request.resource.resource_type.as_str(),
        ) {
            ("define", _) => Self::Define,
            ("consume_ignore_barrier", _) => Self::I,
            ("consume", resource) if resource.starts_with(CONSUMER_GROUP_RESOURCE) => Self::Group,
            ("consume", REQUEST_RESOURCE) if !request.context.require_constraints => Self::Gate,
            ("consume", REQUEST_RESOURCE) => Self::R,
            ("consume", _) => Self::E,
            (action, resource) => panic!("unexpected PDP call {action} on {resource}"),
        }
    }
}

/// What the scripted PDP answers to one call.
enum Answer {
    /// Allowed, with `In(owner_tenant_id, [root_id])` when the request names a
    /// root and declares the property, as tr-authz answers.
    Allow,
    /// Allowed with these constraints, each an AND of its predicates.
    Scope(Vec<Vec<Predicate>>),
    /// Allowed with a constraint on `subject`, which no consume resource
    /// declares, so the enforcer cannot compile it.
    Uncompilable,
    /// Denied, with this deny reason when given.
    Deny(Option<&'static str>),
    /// The evaluation itself fails, as an unreachable PDP does.
    Fail,
}

/// Records every request and answers it by `answer`.
struct ScriptedPdp<F> {
    recorded: Recorded,
    answer: F,
}

#[async_trait]
impl<F> AuthZResolverApi for ScriptedPdp<F>
where
    F: Fn(Call, &EvaluationRequest) -> Answer + Send + Sync,
{
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.recorded
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        let constraints = |predicates: Vec<Vec<Predicate>>| EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: predicates
                    .into_iter()
                    .map(|predicates| Constraint { predicates })
                    .collect(),
                ..Default::default()
            },
        };
        Ok(match (self.answer)(Call::of(&request), &request) {
            Answer::Allow => {
                let declared = request
                    .context
                    .supported_properties
                    .iter()
                    .any(|property| property == pep_properties::OWNER_TENANT_ID);
                let root = request
                    .context
                    .tenant_context
                    .as_ref()
                    .and_then(|tenant_context| tenant_context.root_id);
                match root {
                    Some(root) if declared => constraints(vec![vec![Predicate::In(
                        InPredicate::new(pep_properties::OWNER_TENANT_ID, [root]),
                    )]]),
                    _ => constraints(Vec::new()),
                }
            }
            Answer::Scope(predicates) => constraints(predicates),
            Answer::Uncompilable => constraints(vec![vec![Predicate::In(InPredicate::new(
                "subject",
                [Uuid::from_u128(0x77)],
            ))]]),
            Answer::Deny(details) => EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    deny_reason: details.map(|details| DenyReason {
                        error_code: "denied".to_owned(),
                        details: Some(details.to_owned()),
                    }),
                    ..Default::default()
                },
            },
            Answer::Fail => {
                return Err(
                    CanonicalError::internal("the policy decision point is unreachable").create(),
                );
            }
        })
    }
}

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(CALLER_TENANT)
        .build()
        .expect("valid security context")
}

fn registry() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": TOPIC_A, "partitions": 1 },
        { "id": TOPIC_B, "partitions": 1 },
        { "id": CREATED, "topic": TOPIC_A, "allowed_subject_types": [ORDER] },
        { "id": SHIPPED, "topic": TOPIC_A, "allowed_subject_types": [ORDER, INVOICE] },
        { "id": B_TYPE, "topic": TOPIC_B, "allowed_subject_types": [ORDER] },
    ]))
}

/// A harness whose PDP answers by `answer`, the enforcer it uses, and an
/// anonymous group of the caller. The group create's request is cleared, so
/// the list holds what the JOIN asks.
async fn harness_with_enforcer(
    registry: StaticTypesRegistry,
    answer: impl Fn(Call, &EvaluationRequest) -> Answer + Send + Sync + 'static,
) -> (EventBrokerHarness, PolicyEnforcer, Recorded, String) {
    let recorded = Recorded::default();
    let enforcer = PolicyEnforcer::new(Arc::new(ScriptedPdp {
        recorded: Arc::clone(&recorded),
        answer,
    }));
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry)
        .with_policy_enforcer(enforcer.clone())
        .with_security_context(caller())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    recorded.lock().unwrap().clear();
    (harness, enforcer, recorded, group)
}

async fn harness_with_group(
    answer: impl Fn(Call, &EvaluationRequest) -> Answer + Send + Sync + 'static,
) -> (EventBrokerHarness, Recorded, String) {
    let (harness, _, recorded, group) = harness_with_enforcer(registry(), answer).await;
    (harness, recorded, group)
}

async fn join(harness: &EventBrokerHarness, group: &str, interests: Value) -> TestResponse {
    harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": interests,
        })))
        .send()
        .await
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// Read after the PDP list is asserted: listing asks the PDP per tenant.
async fn assert_no_subscription(harness: &EventBrokerHarness) {
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

#[tokio::test]
async fn a_respect_interest_sends_e_only() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
}

#[tokio::test]
async fn an_ignore_interest_sends_i_then_e() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED], "barrier_mode": "ignore" },
        ]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "ignore",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume_ignore_barrier" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
}

#[tokio::test]
async fn interests_on_one_topic_and_tenant_share_one_e() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [SHIPPED] },
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED, SHIPPED] },
        ]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [SHIPPED],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED, SHIPPED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED, SHIPPED],
                        "event_types": [CREATED, SHIPPED],
                        "event_subject_type_patterns": [INVOICE, ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
}

/// The group that would sort last comes first, so the calls follow the order
/// the interests appear in, not a sort.
#[tokio::test]
async fn interests_differing_in_tenant_get_their_own_e() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": OTHER_TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": OTHER_TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": OTHER_TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": OTHER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
}

/// The group that would sort last comes first, so the calls follow the order
/// the interests appear in, not a sort.
#[tokio::test]
async fn interests_differing_in_barrier_mode_get_their_own_e() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED], "barrier_mode": "ignore" },
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "ignore",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume_ignore_barrier" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
}

/// The group that would sort last comes first, so the calls follow the order
/// the interests appear in, not a sort.
#[tokio::test]
async fn interests_differing_in_topic_get_their_own_e() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_B, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [B_TYPE] },
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(201);
    let mut body = resp.json();
    let (Some(id), Some(created_at)) = (
        body["id"].as_str().map(str::to_owned),
        body["created_at"].as_str().map(str::to_owned),
    ) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    // The coordinator assigns topics in hash order.
    let Some(assigned) = body["assigned"].as_array_mut() else {
        panic!("the 201 body carries an assigned array");
    };
    assigned.sort_by_key(|a| a["topic"].as_str().unwrap_or_default().to_owned());
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_B,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [B_TYPE],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
                { "topic": TOPIC_B, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_B,
                    "properties": {
                        "topic_id": TOPIC_B,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [B_TYPE],
                        "event_types": [B_TYPE],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
}

#[tokio::test]
async fn a_denied_i_answers_tenant_not_authorized_and_sends_no_e() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::I => Answer::Deny(None),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED], "barrier_mode": "ignore" },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": REQUEST_RESOURCE,
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume_ignore_barrier" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

/// I allows with a constraint on `subject`, which its resource does not
/// declare, so the enforcer cannot compile the answer.
#[tokio::test]
async fn an_uncompilable_i_answer_is_a_tenant_refusal() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::I => Answer::Uncompilable,
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED], "barrier_mode": "ignore" },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": REQUEST_RESOURCE,
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume_ignore_barrier" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn an_unavailable_pdp_on_i_answers_500() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::I => Answer::Fail,
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED], "barrier_mode": "ignore" },
        ]),
    )
    .await;

    resp.assert_status(500);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/event-broker/v1/subscriptions",
            "context": {},
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume_ignore_barrier" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn a_denied_e_and_a_denied_r_answer_tenant_not_authorized() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E | Call::R => Answer::Deny(None),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": REQUEST_RESOURCE,
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": {
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

/// The refusal carries E's deny reason.
#[tokio::test]
async fn a_denied_e_and_an_allowed_r_answer_topic_not_authorized() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Deny(Some("no consume grant on this topic")),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "no consume grant on this topic",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": {
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

/// E allows with a constraint on `subject`, which `CONSUME_PROPS` does not
/// list, so the enforcer cannot compile the answer.
#[tokio::test]
async fn an_uncompilable_e_answer_falls_back_to_r() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Uncompilable,
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": {
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

/// E allows with one constraint whose `event_type` value is not a type id:
/// the SDK compiler keeps it, and the consume scope drops it as invalid.
#[tokio::test]
async fn an_e_answer_with_nothing_enforceable_falls_back_to_r() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_type", ["not-a-type-id"])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": {
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn an_unavailable_pdp_on_e_answers_500_and_sends_no_r() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Fail,
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(500);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/event-broker/v1/subscriptions",
            "context": {},
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn an_uncompilable_r_answer_is_a_topic_refusal() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Deny(None),
        Call::R => Answer::Uncompilable,
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": {
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn an_unavailable_pdp_on_r_answers_500() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Deny(None),
        Call::R => Answer::Fail,
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(500);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/event-broker/v1/subscriptions",
            "context": {},
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": {
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

/// Topic A's group is refused, so topic B's group is never asked.
#[tokio::test]
async fn the_first_failing_group_ends_the_join() {
    let (harness, pdp, group) = harness_with_group(|call, req| match call {
        Call::E if req.resource.resource_type == TOPIC_A => Answer::Deny(None),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
            { "topic": TOPIC_B, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [B_TYPE] },
        ]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.topic.v1~",
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": CONSUMER_GROUP_RESOURCE,
                    "properties": {
                        "consumer_group_id": group,
                        "owner_tenant_id": CALLER_TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": TOPIC_A,
                    "properties": {
                        "topic_id": TOPIC_A,
                        "owner_tenant_id": TENANT,
                        "event_type_patterns": [CREATED],
                        "event_types": [CREATED],
                        "event_subject_type_patterns": [ORDER],
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": [
                        "owner_tenant_id",
                        "owner_id",
                        "event_type",
                        "event_subject_type",
                    ],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "consume" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": {
                        "owner_tenant_id": TENANT,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": true,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_no_subscription(&harness).await;
}

/// The candidate query reads the spec cache, so a cache that cannot be read
/// ends the JOIN before any consume call, as a retryable `503`.
#[tokio::test]
async fn an_unreadable_candidate_query_answers_503() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;
    harness.drop_metadata_table("event_broker_spec_cache").await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] },
        ]),
    )
    .await;

    resp.assert_status(503);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service Unavailable",
            "status": 503,
            "detail": "Service temporarily unavailable",
            "instance": "/event-broker/v1/subscriptions",
            "context": {},
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": CONSUMER_GROUP_RESOURCE,
                "properties": {
                    "consumer_group_id": group,
                    "owner_tenant_id": CALLER_TENANT,
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": CALLER_TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        }),]
    );
    assert_no_subscription(&harness).await;
}

/// A selector that cannot match any event type is a bad request, refused
/// after the group check and before any consume call.
#[tokio::test]
async fn a_type_pattern_outside_the_event_family_is_refused_before_any_consume_call() {
    let (harness, pdp, group) = harness_with_group(|_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([
            { "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [NOT_AN_EVENT] },
        ]),
    )
    .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "BadTypePattern: event type selector 'gts.x.eb.joinauthz.not_an_event.v1~' \
                       cannot match any event type (gts.cf.core.events.event.v1~\u{2026})",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "format": "BadTypePattern: event type selector \
                           'gts.x.eb.joinauthz.not_an_event.v1~' cannot match any event type \
                           (gts.cf.core.events.event.v1~\u{2026})",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(
        recorded(&pdp),
        vec![json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": CONSUMER_GROUP_RESOURCE,
                "properties": {
                    "consumer_group_id": group,
                    "owner_tenant_id": CALLER_TENANT,
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": CALLER_TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        }),]
    );
    assert_no_subscription(&harness).await;
}

// -- post-check of exact selectors --

/// E's scope names only `SHIPPED`, so the exact `CREATED` the interest asks
/// for can never be delivered.
#[tokio::test]
async fn an_exact_type_the_scope_excludes_is_refused_naming_it() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_type", [SHIPPED])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] }]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "calling principal lacks consume on event type \
                           'gts.cf.core.events.event.v1~x.eb.joinauthz.created.v1~'",
                "resource_type": "gts.cf.core.events.event_type.v1~",
            },
        })
    );
    assert_eq!(recorded(&pdp).len(), 2, "the group check and E, no R");
    assert_no_subscription(&harness).await;
}

/// Each exact pattern is checked against its own candidates: the scope admits
/// `SHIPPED`, which the same interest also asks for, and still `CREATED` is
/// refused.
#[tokio::test]
async fn each_exact_type_is_checked_against_its_own_candidates() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_type", [SHIPPED])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [SHIPPED, CREATED] }]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "calling principal lacks consume on event type \
                           'gts.cf.core.events.event.v1~x.eb.joinauthz.created.v1~'",
                "resource_type": "gts.cf.core.events.event_type.v1~",
            },
        })
    );
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn an_exact_type_the_scope_admits_joins() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_type", [CREATED])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// The wildcard matches both registered types and the scope admits neither,
/// yet the broker cannot tell what a wildcard will match, so it joins.
#[tokio::test]
async fn a_wildcard_pattern_is_never_post_checked() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new(
                "event_type",
                ["gts.cf.core.events.event.v1~x.eb.joinauthz.elsewhere.v1~"],
            )),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.*"],
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.*"],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// The exact type is not registered on the topic, so there is no candidate to
/// check it against, and the scope that names only `CREATED` does not refuse it.
#[tokio::test]
async fn an_exact_type_with_no_candidates_is_not_post_checked() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_type", [CREATED])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.unregistered.v1~"],
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.unregistered.v1~"],
                    "filter": null,
                    "unmatched_types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.unregistered.v1~"],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// A topic with 257 registered types is over the candidate cap, so its types
/// are not listed and nothing is post-checked, even an exact type the scope
/// excludes.
#[tokio::test]
async fn an_over_cap_topic_is_not_post_checked() {
    const BUSY_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.joinauthz.busy.v1";
    let mut types = vec![json!({ "id": BUSY_TOPIC, "partitions": 1 })];
    types.extend((0..257).map(|n| {
        json!({
            "id": format!("gts.cf.core.events.event.v1~x.eb.joinauthz.busy{n:03}.v1~"),
            "topic": BUSY_TOPIC,
            "allowed_subject_types": [ORDER],
        })
    }));
    let (harness, _enforcer, pdp, group) = harness_with_enforcer(
        StaticTypesRegistry::of(Value::Array(types)),
        |call, _| match call {
            Call::E => Answer::Scope(vec![vec![
                Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
                Predicate::In(InPredicate::new(
                    "event_type",
                    ["gts.cf.core.events.event.v1~x.eb.joinauthz.busy001.v1~"],
                )),
            ]]),
            _ => Answer::Allow,
        },
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": BUSY_TOPIC,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.busy000.v1~"],
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": BUSY_TOPIC,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.busy000.v1~"],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": BUSY_TOPIC, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
    let requests = recorded(&pdp);
    let [_group, e] = requests.as_slice() else {
        panic!("expected the group check and E, got {requests:?}");
    };
    assert_eq!(
        e["resource"]["properties"],
        json!({
            "topic_id": BUSY_TOPIC,
            "owner_tenant_id": TENANT,
            "event_type_patterns": ["gts.cf.core.events.event.v1~x.eb.joinauthz.busy000.v1~"],
        }),
        "the topic is over the cap, so E lists no candidate"
    );
}

/// An OR of a constraint that excludes `CREATED` and one that admits it.
#[tokio::test]
async fn one_admitting_constraint_is_enough() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![
            vec![
                Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
                Predicate::In(InPredicate::new("event_type", [SHIPPED])),
            ],
            vec![
                Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
                Predicate::In(InPredicate::new("event_type", [CREATED])),
            ],
        ]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// E's scope names only `INVOICE` as subject type, so the exact `ORDER` the
/// interest selects can never be delivered.
#[tokio::test]
async fn an_exact_subject_type_the_scope_excludes_is_refused() {
    let (harness, pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_subject_type", [INVOICE])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": [SHIPPED],
            "subject_types": [ORDER],
        }]),
    )
    .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/subscriptions",
            "context": {
                "reason": "calling principal lacks consume on subject type \
                           'gts.x.eb.joinauthz.order.v1~'",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(recorded(&pdp).len(), 2, "the group check and E, no R");
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn an_exact_subject_type_the_scope_admits_joins() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_subject_type", [ORDER])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": [SHIPPED],
            "subject_types": [ORDER],
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [SHIPPED],
                    "subject_types": [ORDER],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// E's scope names only a subject type derived from `ORDER`, which the exact
/// `ORDER` selector covers, so the interest can receive events and joins.
#[tokio::test]
async fn a_join_whose_scope_lists_only_derived_subject_types_succeeds() {
    const SPECIAL_ORDER: &str = "gts.x.eb.joinauthz.order.v1~x.eb.joinauthz.special.v1~";
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_subject_type", [SPECIAL_ORDER])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": [CREATED],
            "subject_types": [ORDER],
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [CREATED],
                    "subject_types": [ORDER],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// The exact type is not registered on the topic, so the group has no
/// candidate, and the subject type the scope excludes is not checked either.
#[tokio::test]
async fn an_exact_subject_type_with_no_candidates_is_not_post_checked() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_subject_type", [INVOICE])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.unregistered.v1~"],
            "subject_types": [ORDER],
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.unregistered.v1~"],
                    "subject_types": [ORDER],
                    "filter": null,
                    "unmatched_types": ["gts.cf.core.events.event.v1~x.eb.joinauthz.unregistered.v1~"],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

#[tokio::test]
async fn a_wildcard_subject_type_is_never_post_checked() {
    let (harness, _pdp, group) = harness_with_group(|call, _| match call {
        Call::E => Answer::Scope(vec![vec![
            Predicate::In(InPredicate::new(pep_properties::OWNER_TENANT_ID, [TENANT])),
            Predicate::In(InPredicate::new("event_subject_type", [INVOICE])),
        ]]),
        _ => Answer::Allow,
    })
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": [SHIPPED],
            "subject_types": ["gts.x.eb.joinauthz.*"],
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [SHIPPED],
                    "subject_types": ["gts.x.eb.joinauthz.*"],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

// -- memo --

/// The I request a JOIN of one `ignore` interest on `TOPIC_A` sends.
fn i_request(enforcer: &PolicyEnforcer) -> EvaluationRequest {
    enforcer.build_request_with(
        &caller(),
        &ResourceType::from_static(TOPIC_A, &[pep_properties::OWNER_TENANT_ID]),
        "consume_ignore_barrier",
        None,
        false,
        &AccessRequest::new()
            .context_tenant_id(TENANT)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(BarrierMode::Ignore)
            .resource_property("topic_id", TOPIC_A)
            .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
            .require_constraints(false),
    )
}

/// The E request a JOIN of one interest on `TOPIC_A` for `CREATED` sends.
fn e_request(enforcer: &PolicyEnforcer, barrier_mode: BarrierMode) -> EvaluationRequest {
    enforcer.build_request_with(
        &caller(),
        &ResourceType::from_static(
            TOPIC_A,
            &[
                pep_properties::OWNER_TENANT_ID,
                pep_properties::OWNER_ID,
                "event_type",
                "event_subject_type",
            ],
        ),
        "consume",
        None,
        true,
        &AccessRequest::new()
            .context_tenant_id(TENANT)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(barrier_mode)
            .resource_property("topic_id", TOPIC_A)
            .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
            .resource_property("event_type_patterns", json!([CREATED]))
            .resource_property("event_types", json!([CREATED]))
            .resource_property("event_subject_type_patterns", json!([ORDER]))
            .require_constraints(true),
    )
}

fn key(request: &EvaluationRequest) -> MemoKey {
    MemoKey::try_from(request).expect("a request keys the memo")
}

#[tokio::test]
async fn a_successful_join_writes_the_e_scope_and_the_i_decision() {
    let (harness, enforcer, pdp, group) =
        harness_with_enforcer(registry(), |_, _| Answer::Allow).await;

    let resp = join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": [CREATED],
            "barrier_mode": "ignore",
        }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "join-authz-test",
            "interests": [
                {
                    "topic": TOPIC_A,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "ignore",
                    "types": [CREATED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": TOPIC_A, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        })
    );

    let e_request = e_request(&enforcer, BarrierMode::Ignore);
    let requests = recorded(&pdp);
    let [_group, i_sent, e_sent] = requests.as_slice() else {
        panic!("expected the group check, I and E, got {requests:?}");
    };
    assert_eq!(
        *i_sent,
        serde_json::to_value(i_request(&enforcer)).unwrap(),
        "the I key is the request I sent"
    );
    assert_eq!(
        *e_sent,
        serde_json::to_value(&e_request).unwrap(),
        "the E key is the request E sent"
    );
    let answered = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        pep_properties::OWNER_TENANT_ID,
        vec![TENANT],
    )]));
    let now = Instant::now();
    assert_eq!(
        harness.scope_memo().get(&key(&e_request), now),
        Some(MemoResult::Scope(Arc::new(
            CompiledScope::compile(&answered, &TenantMode::RootOnly, &TenantSetInterner::new())
                .unwrap()
        ))),
        "E"
    );
    assert_eq!(
        harness.scope_memo().get(&key(&i_request(&enforcer)), now),
        Some(MemoResult::Decision(true)),
        "I"
    );
}

/// A denial is not memoized, so a grant fixed after a `403` counts on the
/// next JOIN: neither a denied E nor a denied I leaves an entry.
#[tokio::test]
async fn a_refused_join_writes_nothing() {
    let (harness, enforcer, _pdp, group) =
        harness_with_enforcer(registry(), |call, _| match call {
            Call::E | Call::R => Answer::Deny(None),
            _ => Answer::Allow,
        })
        .await;
    join(
        &harness,
        &group,
        json!([{ "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] }]),
    )
    .await
    .assert_status(403);
    assert_eq!(
        harness.scope_memo().get(
            &key(&e_request(&enforcer, BarrierMode::Respect)),
            Instant::now()
        ),
        None,
        "a denied E"
    );

    let (harness, enforcer, _pdp, group) =
        harness_with_enforcer(registry(), |call, _| match call {
            Call::I => Answer::Deny(None),
            _ => Answer::Allow,
        })
        .await;
    join(
        &harness,
        &group,
        json!([{
            "topic": TOPIC_A,
            "subject_tenant": TENANT, "ownership_filter": "entire_tenant",
            "types": [CREATED],
            "barrier_mode": "ignore",
        }]),
    )
    .await
    .assert_status(403);
    assert_eq!(
        harness
            .scope_memo()
            .get(&key(&i_request(&enforcer)), Instant::now()),
        None,
        "a denied I"
    );
}
