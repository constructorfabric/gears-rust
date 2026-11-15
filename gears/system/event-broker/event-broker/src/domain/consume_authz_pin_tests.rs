//! Pins the consume-side PDP requests of the event broker.
//!
//! Every caller of `tenant_authorized` on the consume side sends a fixed
//! `EvaluationRequest` to the PDP, and delivery sends none per event. Each
//! test records every request its operation sends and asserts the full
//! serialized value.
//!
//! The expected values derive from `PolicyEnforcer::build_request_with`: a
//! `PolicyEnforcer::new` has no capabilities, and the harness caller has no
//! subject type and no token scopes. The tenant gate names its target tenant
//! as the request's tenant context: that tenant is the `root_id`, in
//! `root_only` mode, with barriers respected, and it is also the
//! `owner_tenant_id`.
//!
//! - `a_subscription_lookup_...` covers `find_authorized_subscription`, which
//!   `leave`, `stream` and `seek` share.
//! - `listing_...` tests cover `filter_by_authorized_tenant`, one request per
//!   distinct tenant.
//! - The anonymous-group lookup covers `find_authorized_consumer_group`.
//! - The JOIN test covers the group check, which a JOIN sends in place of a
//!   tenant gate on its group's tenant.
//! - The delivery test covers a stream reading events of a candidate type.

use std::sync::{Arc, Mutex};

use std::time::Duration;

use authz_resolver_sdk::{EvaluationRequest, PolicyEnforcer};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use crate::domain::model::{ConsumerGroup, ConsumerGroupKind};
use crate::domain::repo::ConsumerGroupRepo;
use crate::domain::streaming::frames::Frame;
use crate::test_support::{DenyingAuthZ, EventBrokerHarness, Json, StaticTypesRegistry};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const CONSUMER_GROUP_RESOURCE: &str = "gts.cf.core.events.consumer_group.v1~";
const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.t1.topic.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.t1.foo.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.t1.subject.v1~";
const FOREIGN_GROUP: &str =
    "gts.cf.core.events.consumer_group.v1~0190f1c2-7a3b-7c4d-8e5f-6a7b8c9d0e1f";

/// Bounds the stream open and each frame read: an open that succeeds
/// streams without end.
const BOUND: Duration = Duration::from_secs(5);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// A `PolicyEnforcer` that allows everything and records each request it sees.
fn recording_enforcer() -> (PolicyEnforcer, Recorded) {
    let recorded = Recorded::default();
    let sink = Arc::clone(&recorded);
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingAuthZ {
        deny_if: move |req: &EvaluationRequest| {
            sink.lock()
                .unwrap()
                .push(serde_json::to_value(req).unwrap());
            false
        },
    }));
    (enforcer, recorded)
}

fn recorded_len(recorded: &Recorded) -> usize {
    recorded.lock().unwrap().len()
}

/// The tenant-scope requests recorded since `from`. The other requests of the
/// same operations are not pinned here (see each test).
fn tenant_scope_requests_since(recorded: &Recorded, from: usize) -> Vec<Value> {
    recorded.lock().unwrap()[from..]
        .iter()
        .filter(|req| req["resource"]["type"] == REQUEST_RESOURCE)
        .cloned()
        .collect()
}

fn harness_with_topic(
    enforcer: PolicyEnforcer,
) -> impl std::future::Future<Output = EventBrokerHarness> {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(enforcer)
        .build()
}

/// Seeds an anonymous group owned by a tenant the caller does not hold.
async fn seed_foreign_group(harness: &EventBrokerHarness, foreign_tenant: Uuid) {
    harness
        .repo()
        .create_consumer_group(ConsumerGroup {
            id: GtsInstanceId::try_new(FOREIGN_GROUP).expect("valid group id"),
            kind: ConsumerGroupKind::Anonymous,
            tenant_id: foreign_tenant,
            owner_principal_id: Uuid::from_u128(0x0b),
            description: None,
            created_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        })
        .await
        .expect("seeding a consumer group must not fail");
}

#[tokio::test]
async fn a_subscription_lookup_sends_the_pinned_tenant_scope_request() {
    let (enforcer, recorded) = recording_enforcer();
    let harness = harness_with_topic(enforcer).await;
    let ctx = harness.security_context().clone();
    let interest_tenant = Uuid::from_u128(0x1a);
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joined = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "pin-agent",
            "interests": [{ "topic": TOPIC, "subject_tenant": interest_tenant, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE] }],
        })))
        .send()
        .await;
    joined.assert_status(201);
    let id = joined.json()["id"].as_str().unwrap().to_owned();

    let before = recorded_len(&recorded);
    let resp = harness.api_v1().get_subscription(&id).send().await;

    resp.assert_status(200);
    assert_eq!(
        tenant_scope_requests_since(&recorded, before),
        vec![json!({
            "subject": {
                "id": ctx.subject_id(),
                "properties": { "tenant_id": ctx.subject_tenant_id() },
            },
            "action": { "name": "consume" },
            "resource": {
                "type": REQUEST_RESOURCE,
                "properties": { "owner_tenant_id": ctx.subject_tenant_id() },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": ctx.subject_tenant_id(),
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })]
    );
}

#[tokio::test]
async fn listing_subscriptions_sends_the_pinned_tenant_scope_request() {
    let (enforcer, recorded) = recording_enforcer();
    let harness = harness_with_topic(enforcer).await;
    let ctx = harness.security_context().clone();
    let interest_tenant = Uuid::from_u128(0x1a);
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "pin-agent",
            "interests": [{ "topic": TOPIC, "subject_tenant": interest_tenant, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE] }],
        })))
        .send()
        .await
        .assert_status(201);

    let before = recorded_len(&recorded);
    let resp = harness.api_v1().get_subscriptions().send().await;

    resp.assert_status(200);
    assert_eq!(
        tenant_scope_requests_since(&recorded, before),
        vec![json!({
            "subject": {
                "id": ctx.subject_id(),
                "properties": { "tenant_id": ctx.subject_tenant_id() },
            },
            "action": { "name": "consume" },
            "resource": {
                "type": REQUEST_RESOURCE,
                "properties": { "owner_tenant_id": ctx.subject_tenant_id() },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": ctx.subject_tenant_id(),
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })]
    );
}

#[tokio::test]
async fn an_anonymous_group_lookup_sends_the_pinned_tenant_scope_request() {
    let (enforcer, recorded) = recording_enforcer();
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(enforcer)
        .build()
        .await;
    let ctx = harness.security_context().clone();
    let foreign_tenant = Uuid::from_u128(0x0f);
    seed_foreign_group(&harness, foreign_tenant).await;

    let before = recorded_len(&recorded);
    let resp = harness
        .api_v1()
        .get_consumer_group(FOREIGN_GROUP)
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        tenant_scope_requests_since(&recorded, before),
        vec![json!({
            "subject": {
                "id": ctx.subject_id(),
                "properties": { "tenant_id": ctx.subject_tenant_id() },
            },
            "action": { "name": "consume" },
            "resource": {
                "type": REQUEST_RESOURCE,
                "properties": { "owner_tenant_id": foreign_tenant },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": foreign_tenant,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })]
    );
}

#[tokio::test]
async fn listing_consumer_groups_sends_the_pinned_tenant_scope_request_per_anonymous_tenant() {
    let (enforcer, recorded) = recording_enforcer();
    let harness = EventBrokerHarness::builder()
        .with_policy_enforcer(enforcer)
        .build()
        .await;
    let ctx = harness.security_context().clone();
    let foreign_tenant = Uuid::from_u128(0x0f);
    seed_foreign_group(&harness, foreign_tenant).await;

    let before = recorded_len(&recorded);
    let resp = harness.api_v1().get_consumer_groups().send().await;

    resp.assert_status(200);
    // The listing must hold the seeded group, so the request below was sent for a real row.
    assert_eq!(
        resp.json(),
        json!({
            "items": [{
                "id": FOREIGN_GROUP,
                "kind": "anonymous",
                "tenant": foreign_tenant,
                "owner_principal": Uuid::from_u128(0x0b),
                "description": null,
                "created_at": "2026-01-02T03:04:05Z",
            }],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
    assert_eq!(
        tenant_scope_requests_since(&recorded, before),
        vec![json!({
            "subject": {
                "id": ctx.subject_id(),
                "properties": { "tenant_id": ctx.subject_tenant_id() },
            },
            "action": { "name": "consume" },
            "resource": {
                "type": REQUEST_RESOURCE,
                "properties": { "owner_tenant_id": foreign_tenant },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": foreign_tenant,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })]
    );
}

/// A JOIN also sends a consume request on the topic for its group of
/// interests; that request is pinned in `join_authz_tests`, so only the group
/// check is pinned here. It names the anonymous group and its tenant, the
/// caller's, and the JOIN sends no request on the tenant-scope resource. A
/// named group is checked on its own id (`group_check_tests`).
#[tokio::test]
async fn a_join_sends_the_pinned_group_check_for_its_group() {
    let (enforcer, recorded) = recording_enforcer();
    let harness = harness_with_topic(enforcer).await;
    let ctx = harness.security_context().clone();
    let interest_tenant = Uuid::from_u128(0x1a);
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let before = recorded_len(&recorded);
    let resp = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "pin-agent",
            "interests": [{ "topic": TOPIC, "subject_tenant": interest_tenant, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE] }],
        })))
        .send()
        .await;

    resp.assert_status(201);
    assert_eq!(
        tenant_scope_requests_since(&recorded, before),
        Vec::<Value>::new()
    );
    let group_checks: Vec<Value> = recorded.lock().unwrap()[before..]
        .iter()
        .filter(|req| req["resource"]["type"] == CONSUMER_GROUP_RESOURCE)
        .cloned()
        .collect();
    assert_eq!(
        group_checks,
        vec![json!({
            "subject": {
                "id": ctx.subject_id(),
                "properties": { "tenant_id": ctx.subject_tenant_id() },
            },
            "action": { "name": "consume" },
            "resource": {
                "type": CONSUMER_GROUP_RESOURCE,
                "properties": {
                    "consumer_group_id": group,
                    "owner_tenant_id": ctx.subject_tenant_id(),
                },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": ctx.subject_tenant_id(),
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })]
    );
}

/// JOINs, seeks and publishes three events of a registered type, a candidate
/// of the interest's group, then opens the stream and reads all three.
#[tokio::test]
async fn delivery_makes_no_pdp_call_for_a_candidate_type() {
    let (enforcer, recorded) = recording_enforcer();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            { "id": EVENT_TYPE, "topic": TOPIC, "allowed_subject_types": [SUBJECT_TYPE] },
        ])))
        .with_policy_enforcer(enforcer)
        .build()
        .await;
    let ctx = harness.security_context().clone();
    let interest_tenant = Uuid::from_u128(0x1a);
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joined = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "pin-agent",
            "interests": [{ "topic": TOPIC, "subject_tenant": interest_tenant, "ownership_filter": "entire_tenant", "types": [EVENT_TYPE] }],
        })))
        .send()
        .await;
    joined.assert_status(201);
    let id = joined.json()["id"].as_str().unwrap().to_owned();
    harness
        .api_v1()
        .post_subscription_seek(&id)
        .with_body(Json(&json!({
            "topology_version": 1,
            "positions": { (TOPIC): [{ "partition": 0, "value": "earliest" }] },
        })))
        .send()
        .await
        .assert_status(200);
    let published = [
        Uuid::from_u128(0x0190_f1c2_7a3b_7c4d_8e5f_6a7b_8c9d_0e01),
        Uuid::from_u128(0x0190_f1c2_7a3b_7c4d_8e5f_6a7b_8c9d_0e02),
        Uuid::from_u128(0x0190_f1c2_7a3b_7c4d_8e5f_6a7b_8c9d_0e03),
    ];
    for event_id in published {
        harness
            .api_v1()
            .post_events()
            .with_body(Json(&json!({
                "id": event_id,
                "type": EVENT_TYPE,
                "subject_tenant": interest_tenant,
                "source": "pin-test",
                "subject": "order-1",
                "subject_type": SUBJECT_TYPE,
                "occurred_at": "2026-01-02T03:04:05Z",
            })))
            .send()
            .await
            .assert_status(202);
    }

    let before = recorded_len(&recorded);
    let mut stream = tokio::time::timeout(
        BOUND,
        harness
            .delivery()
            .stream(&ctx, Uuid::parse_str(&id).unwrap()),
    )
    .await
    .expect("the open answers within the bound")
    .expect("the open is allowed");
    let mut delivered = Vec::new();
    while delivered.len() < published.len() {
        match tokio::time::timeout(BOUND, stream.next())
            .await
            .expect("each frame arrives within the bound")
        {
            Some(Frame::Event(event)) => delivered.push(event.id),
            Some(_) => {}
            None => panic!("the stream ended after {delivered:?}"),
        }
    }

    assert_eq!(delivered, published);
    // Pin (#5494): stream open asks the tenant gate alone, since JOIN left its
    // group check and its consume scope in the memo, and delivery asks nothing
    // per event.
    assert_eq!(
        recorded.lock().unwrap()[before..].to_vec(),
        vec![json!({
            "subject": {
                "id": ctx.subject_id(),
                "properties": { "tenant_id": ctx.subject_tenant_id() },
            },
            "action": { "name": "consume" },
            "resource": {
                "type": REQUEST_RESOURCE,
                "properties": { "owner_tenant_id": ctx.subject_tenant_id() },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": ctx.subject_tenant_id(),
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })]
    );
}
