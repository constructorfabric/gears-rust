//! The consume rows of #5494 through real policy.
//!
//! The scripted and recording doubles of `join_authz_tests.rs`,
//! `stream_open_authz_tests.rs` and `consume_authz_pin_tests.rs` prove the
//! request shapes, the call order and the memo; this file proves the answers.
//! Every row JOINs, and the delivery rows also seek, publish and stream,
//! through the harness's real delivery path under three PDPs: the
//! action-aware `PolicyAuthZ` double, the real tr-authz service over an
//! in-memory tenant tree, and the real static-authz service. A row's expected
//! answer for each PDP is #5494's Validation table, the consume PDP contract,
//! and, for static-authz, that it allows any non-nil target tenant: it has no
//! tree.
//!
//! The caller is `U` (subject `0x890`) of tenant `0x234` in the example tree
//! of `pdp_fixtures`. Under `PolicyAuthZ` it holds `produce` on every event
//! type a row publishes, and the `consume` and `consume_ignore_barrier`
//! grants on the topic a row names, and `define` and `consume` on consumer
//! groups. Every PDP creates and lets it JOIN its own anonymous group: the
//! group check asks for the group's tenant, which is the caller's.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, EvaluationRequest, EvaluationResponse, PolicyEnforcer};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use super::pdp_fixtures::{
    B, B1, C, InMemoryTenantTree, P, ROOT, StaticAuthz, T234, T999, TREE, TrAuthz, U, caller,
};
use crate::domain::ingest::{PublishAck, PublishRequest};
use crate::domain::outbox::OutboxPartitions;
use crate::domain::repo::ConsumerGroupRepo;
use crate::domain::streaming::frames::{ControlCode, Frame};
use crate::domain::streaming::session::FrameStream;
use crate::test_support::response::TestResponse;
use crate::test_support::{
    EventBrokerHarness, Grant, Json, PolicyAuthZ, StaticTypesRegistry, Tenant,
};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const TOPIC_RESOURCE: &str = "gts.cf.core.events.topic.v1~";
const EVENT_TYPE_RESOURCE: &str = "gts.cf.core.events.event_type.v1~";
const CONSUMER_GROUP_RESOURCE: &str = "gts.cf.core.events.consumer_group.v1~";

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.consumepdp.orders.v1";
const PLACED: &str = "gts.cf.core.events.event.v1~x.eb.consumepdp.placed.v1~";
const PAID: &str = "gts.cf.core.events.event.v1~x.eb.consumepdp.paid.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.consumepdp.order.v1~";

/// A topic with more event types than the candidate cap, so every type it
/// carries takes the single-type consume decision at delivery.
const OVER_CAP_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.consumepdp.audit.v1";
const NOTED: &str = "gts.cf.core.events.event.v1~x.eb.consumepdp.noted.v1~";

/// Bounds each stream open and each complete event batch.
const BOUND: Duration = Duration::from_secs(5);

/// Bounds the wait that shows a stream still open after its events. It spans
/// the harness's 1 s heartbeat interval, so the session runs at least one
/// idle pass within it.
const STAYS_OPEN: Duration = Duration::from_millis(1_500);

/// `TOPIC` with two event types.
fn registry() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": TOPIC, "partitions": 1 },
        { "id": PLACED, "topic": TOPIC, "allowed_subject_types": [SUBJECT_TYPE] },
        { "id": PAID, "topic": TOPIC, "allowed_subject_types": [SUBJECT_TYPE] },
    ]))
}

/// [`registry`] plus `OVER_CAP_TOPIC`, which carries `NOTED` and 257 more
/// event types: 258, over the candidate cap of 256.
fn delivery_registry() -> StaticTypesRegistry {
    let mut spec = vec![
        json!({ "id": TOPIC, "partitions": 1 }),
        json!({ "id": PLACED, "topic": TOPIC, "allowed_subject_types": [SUBJECT_TYPE] }),
        json!({ "id": PAID, "topic": TOPIC, "allowed_subject_types": [SUBJECT_TYPE] }),
        json!({ "id": OVER_CAP_TOPIC, "partitions": 1 }),
        json!({ "id": NOTED, "topic": OVER_CAP_TOPIC, "allowed_subject_types": [SUBJECT_TYPE] }),
    ];
    spec.extend((0..257).map(|n| {
        json!({
            "id": format!("gts.cf.core.events.event.v1~x.eb.consumepdp.filler_{n:03}.v1~"),
            "topic": OVER_CAP_TOPIC,
        })
    }));
    StaticTypesRegistry::of(Value::Array(spec))
}

/// The example tree under `PolicyAuthZ`, with `U`'s `produce` grant on
/// `PLACED` and `NOTED` and its `define` and `consume` grants on consumer
/// groups; a row that publishes another type adds its own. It holds no
/// consume grant on a topic.
fn policy() -> PolicyAuthZ {
    policy_without_group_consume().with_grant(Grant::new(U, CONSUMER_GROUP_RESOURCE, "consume"))
}

/// [`policy`] without `U`'s `consume` grant on consumer groups: `U` may create
/// a group but not JOIN one.
fn policy_without_group_consume() -> PolicyAuthZ {
    let tenants = TREE
        .iter()
        .map(|&(id, parent, self_managed)| match (parent, self_managed) {
            (None, _) => Tenant::root(id),
            (Some(parent), false) => Tenant::child(id, parent),
            (Some(parent), true) => Tenant::self_managed_child(id, parent),
        });
    PolicyAuthZ::new(tenants)
        .expect("the example tree is well formed")
        .with_grant(Grant::new(U, PLACED, "produce"))
        .with_grant(Grant::new(U, NOTED, "produce"))
        .with_grant(Grant::new(U, CONSUMER_GROUP_RESOURCE, "define"))
}

fn policy_authz(policy: PolicyAuthZ) -> Arc<dyn AuthZResolverApi> {
    Arc::new(policy)
}

fn tr_authz() -> Arc<dyn AuthZResolverApi> {
    Arc::new(TrAuthz(tr_authz_plugin::domain::Service::new(Arc::new(
        InMemoryTenantTree,
    ))))
}

fn static_authz() -> Arc<dyn AuthZResolverApi> {
    Arc::new(StaticAuthz(static_authz_plugin::domain::Service::new()))
}

/// A harness acting as `U` of `0x234` under `authz`, with one outbox
/// partition so events reach their topic partition in publish order, and
/// `U`'s anonymous consumer group.
async fn harness(
    authz: Arc<dyn AuthZResolverApi>,
    registry: StaticTypesRegistry,
) -> (EventBrokerHarness, String) {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry)
        .with_outbox_partitions(OutboxPartitions::new(1).expect("a power of 2 within 64"))
        .with_policy_enforcer(PolicyEnforcer::new(authz))
        .with_security_context(caller(U, T234))
        .build()
        .await;
    let created = harness.api_v1().post_consumer_groups().send().await;
    created.assert_status(201);
    let group = created.json()["id"]
        .as_str()
        .expect("the 201 body names the group")
        .to_owned();
    (harness, group)
}

async fn join(harness: &EventBrokerHarness, group: &str, interests: Value) -> TestResponse {
    harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": interests,
        })))
        .send()
        .await
}

/// Read after the refusal is asserted: listing asks the PDP for the caller's
/// own tenant, which every PDP here allows.
async fn assert_no_subscription(harness: &EventBrokerHarness) {
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

/// Seeks every partition of both topics of a delivery row to earliest.
async fn seek_earliest(harness: &EventBrokerHarness, subscription: &str) {
    harness
        .api_v1()
        .post_subscription_seek(subscription)
        .with_body(Json(&json!({
            "topology_version": 1,
            "positions": {
                (TOPIC): [{ "partition": 0, "value": "earliest" }],
                (OVER_CAP_TOPIC): [{ "partition": 0, "value": "earliest" }],
            },
        })))
        .send()
        .await
        .assert_status(200);
}

/// Publishes one unowned event as `U`.
async fn publish(harness: &EventBrokerHarness, id: u128, event_type: &str, tenant: Uuid) {
    publish_request(harness, request(id, event_type, tenant)).await;
}

/// Publishes one `PLACED` event in `tenant` owned by `owner`, as `U`.
async fn publish_owned(harness: &EventBrokerHarness, id: u128, tenant: Uuid, owner: Uuid) {
    publish_request(
        harness,
        PublishRequest {
            subject_owner: Some(owner),
            ..request(id, PLACED, tenant)
        },
    )
    .await;
}

/// An unowned event of `event_type` in `tenant`.
fn request(id: u128, event_type: &str, tenant: Uuid) -> PublishRequest {
    PublishRequest {
        id: Uuid::from_u128(id),
        r#type: crate::test_support::event_type_id(event_type),
        subject_tenant: tenant,
        source: "consume-pdp-test".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: gts::GtsTypeId::new(SUBJECT_TYPE),
        subject_owner: None,
        occurred_at: Utc.with_ymd_and_hms(2026, 11, 13, 10, 0, 0).unwrap(),
        trace_parent: None,
        data: json!({}),
        meta: None,
    }
}

/// Publishes `request` as `U`; the ack is setup, so only its variant is
/// checked.
async fn publish_request(harness: &EventBrokerHarness, request: PublishRequest) {
    let id = request.id;
    let result = harness
        .ingest()
        .publish_event(&caller(U, T234), request)
        .await;
    assert!(
        matches!(result, Ok(PublishAck::Accepted(_))),
        "the publish of {id} is accepted, got {result:?}"
    );
}

/// The four events of a delivery row, published before the stream opens:
/// `PLACED` in `0x234`, `PLACED` in `0xC`, `NOTED` in `0x234` on the
/// over-cap topic, and `PLACED` in `0x234` again. The last one follows the
/// `0xC` event on the same partition, so reading it proves the `0xC` event
/// was passed over, not still on its way.
async fn publish_delivery_events(harness: &EventBrokerHarness) {
    publish(harness, 0x10_01, PLACED, T234).await;
    publish(harness, 0x10_02, PLACED, C).await;
    publish(harness, 0x10_03, NOTED, T234).await;
    publish(harness, 0x10_04, PLACED, T234).await;
}

/// The first `count` event topics and ids in delivery order, all within [`BOUND`].
/// Heartbeats cannot extend the batch deadline.
async fn read_events(
    stream: &mut (impl tokio_stream::Stream<Item = Frame> + Unpin),
    count: usize,
) -> Vec<(String, Uuid)> {
    tokio::time::timeout(BOUND, async {
        let mut delivered = Vec::new();
        while delivered.len() < count {
            match stream.next().await {
                Some(Frame::Event(event)) => delivered.push((event.topic.to_string(), event.id)),
                Some(_) => {}
                None => panic!("the stream ended after {delivered:?}"),
            }
        }
        delivered
    })
    .await
    .expect("the events arrive within the bound")
}

/// A one-second heartbeat keeps a per-frame deadline alive. Paused Tokio time
/// separates the five-second read deadline from the ten-second watchdog exactly.
#[tokio::test(start_paused = true)]
async fn heartbeats_do_not_extend_the_event_read_deadline() {
    const WATCHDOG: Duration = Duration::from_secs(10);
    let heartbeats = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&heartbeats);
    let started = tokio::time::Instant::now();
    let mut task = tokio::spawn(async move {
        let stream = tokio_stream::iter(std::iter::repeat(())).then(move |()| {
            let observed = Arc::clone(&observed);
            async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                observed.fetch_add(1, Ordering::Relaxed);
                Frame::Heartbeat {
                    at: Utc.with_ymd_and_hms(2026, 11, 13, 10, 0, 0).unwrap(),
                }
            }
        });
        tokio::pin!(stream);
        read_events(&mut stream, 1).await
    });
    let outcome = tokio::time::timeout(WATCHDOG, &mut task).await;
    if outcome.is_err() {
        task.abort();
        let cancelled = tokio::time::timeout(BOUND, task)
            .await
            .expect("the aborted event read joins within the bound")
            .expect_err("the event read is cancelled");
        assert!(cancelled.is_cancelled());
        panic!("the event read exceeds its deadline and reaches the watchdog");
    }
    let failure = outcome
        .unwrap()
        .expect_err("a heartbeat-only stream cannot deliver an event");
    assert!(
        failure.is_panic(),
        "the event read fails through its deadline"
    );
    let payload = failure.into_panic();
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .expect("the deadline panic carries a message");
    assert!(message.contains("the events arrive within the bound"));
    assert!(heartbeats.load(Ordering::Relaxed) >= 2);
    assert_eq!(started.elapsed(), BOUND);
}

/// The first frame other than a heartbeat, a topology report or a progress
/// report within [`STAYS_OPEN`]; `None` when there is none, the stream still
/// open.
async fn next_notable_frame(stream: &mut FrameStream) -> Option<Option<Frame>> {
    tokio::time::timeout(STAYS_OPEN, async {
        loop {
            match stream.next().await {
                Some(
                    Frame::Heartbeat { .. }
                    | Frame::Topology { .. }
                    | Frame::Control {
                        code: ControlCode::Progress,
                        ..
                    },
                ) => {}
                other => return other,
            }
        }
    })
    .await
    .ok()
}

/// `body` with its `assigned` partitions sorted by topic: the coordinator
/// does not fix their order across topics.
fn sorted_assignments(mut body: Value) -> Value {
    if let Some(assigned) = body["assigned"].as_array_mut() {
        assigned.sort_by(|a, b| a["topic"].as_str().cmp(&b["topic"].as_str()));
    }
    body
}

/// The JOIN of a delivery row: one interest on each topic, both in `0x234`.
async fn join_both_topics(harness: &EventBrokerHarness, group: &str) -> TestResponse {
    join(
        harness,
        group,
        json!([
            { "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PLACED] },
            { "topic": OVER_CAP_TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [NOTED] },
        ]),
    )
    .await
}

// -- row 1: JOIN 0x234, respect --

/// #5494 Validation: (234, 890) JOIN 234, `entire_tenant`: 201.
#[tokio::test]
async fn policy_authz_lets_a_user_join_its_own_tenant() {
    let (harness, group) = harness(
        policy_authz(policy().with_grant(Grant::new(U, TOPIC, "consume"))),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 1, PolicyAuthZ: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 1, PolicyAuthZ"
    );
}

/// #5494 Validation: (234, 890) JOIN 234, `entire_tenant`: 201.
#[tokio::test]
async fn tr_authz_lets_a_user_join_its_own_tenant() {
    let (harness, group) = harness(tr_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 1, tr-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 1, tr-authz"
    );
}

/// #5494 Validation: (234, 890) JOIN 234, `entire_tenant`: 201.
#[tokio::test]
async fn static_authz_lets_a_user_join_its_own_tenant() {
    let (harness, group) = harness(static_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 1, static-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 1, static-authz"
    );
}

// -- row 2: JOIN 0x999, respect --

/// #5494 Validation: (234, 890) JOIN 999: 403 `TenantIdNotAuthorized`. The
/// consume scope is refused, and the tenant check that classifies it is too.
#[tokio::test]
async fn policy_authz_refuses_a_join_of_a_tenant_outside_the_callers_subtree() {
    let (harness, group) = harness(
        policy_authz(policy().with_grant(Grant::new(U, TOPIC, "consume"))),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T999, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
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
        }),
        "row 2, PolicyAuthZ: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// #5494 Validation: (234, 890) JOIN 999: 403 under tr-authz, which answers
/// the root-only consume scope with an ancestry check.
#[tokio::test]
async fn tr_authz_refuses_a_join_of_a_tenant_outside_the_callers_subtree() {
    let (harness, group) = harness(tr_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T999, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
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
        }),
        "row 2, tr-authz: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// static-authz allows any non-nil target tenant: it has no tenant tree.
#[tokio::test]
async fn static_authz_has_no_tree_and_lets_a_join_of_a_tenant_outside_the_callers_subtree_through()
{
    let (harness, group) = harness(static_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T999, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 2, static-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": T999,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 2, static-authz"
    );
}

// -- row 3: JOIN 0xB, respect --

/// #5494 Validation: (234, 890) JOIN B, `entire_tenant`, respect: 403
/// `TenantIdNotAuthorized`: a self-managed tenant is not reached while
/// barriers are respected.
#[tokio::test]
async fn policy_authz_refuses_a_join_of_a_barrier_tenant_that_respects_barriers() {
    let (harness, group) = harness(
        policy_authz(policy().with_grant(Grant::new(U, TOPIC, "consume"))),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
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
        }),
        "row 3, PolicyAuthZ: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// #5494 Validation: (234, 890) JOIN B, `entire_tenant`, respect: 403
/// `TenantIdNotAuthorized` under tr-authz too.
#[tokio::test]
async fn tr_authz_refuses_a_join_of_a_barrier_tenant_that_respects_barriers() {
    let (harness, group) = harness(tr_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
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
        }),
        "row 3, tr-authz: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// static-authz allows any non-nil target tenant: it has no tenant tree, so
/// no barrier either.
#[tokio::test]
async fn static_authz_has_no_tree_and_lets_a_join_of_a_barrier_tenant_through() {
    let (harness, group) = harness(static_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 3, static-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": B,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 3, static-authz"
    );
}

// -- row 4: JOIN 0xB1, respect --

/// A tenant below a self-managed parent is not reached while barriers are
/// respected: 403 `TenantIdNotAuthorized`, as for the barrier tenant itself.
#[tokio::test]
async fn policy_authz_refuses_a_join_of_a_tenant_below_a_barrier_that_respects_barriers() {
    let (harness, group) = harness(
        policy_authz(policy().with_grant(Grant::new(U, TOPIC, "consume"))),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B1, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
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
        }),
        "row 4, PolicyAuthZ: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// A tenant below a self-managed parent is not reached while barriers are
/// respected: 403 `TenantIdNotAuthorized`, as for the barrier tenant itself.
#[tokio::test]
async fn tr_authz_refuses_a_join_of_a_tenant_below_a_barrier_that_respects_barriers() {
    let (harness, group) = harness(tr_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B1, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
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
        }),
        "row 4, tr-authz: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

// -- row 5: JOIN 0xB, ignore, no barrier right --

/// #5494 Validation: `ignore` without `consume_ignore_barrier`: 403
/// `TenantIdNotAuthorized`.
#[tokio::test]
async fn policy_authz_refuses_a_barrier_ignoring_join_without_the_barrier_right() {
    let (harness, group) = harness(
        policy_authz(policy().with_grant(Grant::new(U, TOPIC, "consume"))),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED], "barrier_mode": "ignore" }]),
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
        }),
        "row 5, PolicyAuthZ: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// #5494 Validation: `ignore` without `consume_ignore_barrier`: 201 under
/// tr-authz, which grants barrier-ignoring reach without checking the action
/// (#5830).
#[tokio::test]
async fn tr_authz_lets_a_barrier_ignoring_join_through_without_the_barrier_right() {
    // Pin (#5830): scenario `auth/1.09` answers 403; tr-authz answers 201
    // until it checks the barrier right on a barrier-ignoring request.
    let (harness, group) = harness(tr_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED], "barrier_mode": "ignore" }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 5, tr-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": B,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "ignore",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 5, tr-authz"
    );
}

/// static-authz allows any non-nil target tenant and checks no action.
#[tokio::test]
async fn static_authz_lets_a_barrier_ignoring_join_through_without_the_barrier_right() {
    let (harness, group) = harness(static_authz(), registry()).await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED], "barrier_mode": "ignore" }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 5, static-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": B,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "ignore",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 5, static-authz"
    );
}

// -- rows 6 to 8: the barrier right and the consume grant under PolicyAuthZ --

/// #5494 Validation: `ignore`, right held: 201.
#[tokio::test]
async fn policy_authz_lets_a_barrier_ignoring_join_through_with_the_barrier_right() {
    let (harness, group) = harness(
        policy_authz(
            policy()
                .with_grant(Grant::new(U, TOPIC, "consume"))
                .with_grant(Grant::new(U, TOPIC, "consume_ignore_barrier")),
        ),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED], "barrier_mode": "ignore" }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 6, PolicyAuthZ: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": B,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "ignore",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 6, PolicyAuthZ"
    );
}

/// The barrier right without `consume` on the topic: the right is allowed,
/// the consume scope is refused, and the tenant check under `ignore` reaches
/// B, so the refusal is the topic's: 403 `TopicNotAuthorized`.
#[tokio::test]
async fn policy_authz_refuses_the_topic_to_a_barrier_ignoring_join_with_the_barrier_right_but_no_consume_grant()
 {
    let (harness, group) = harness(
        policy_authz(policy().with_grant(Grant::new(U, TOPIC, "consume_ignore_barrier"))),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": B, "ownership_filter": "entire_tenant", "types": [PLACED], "barrier_mode": "ignore" }]),
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
                "resource_type": TOPIC_RESOURCE,
            },
        }),
        "row 7, PolicyAuthZ: TopicNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// The barrier right is decided on the grant alone: with no self-managed
/// tenant below `0xC`, `ignore` still needs `consume_ignore_barrier`, so the
/// JOIN gets 403 `TenantIdNotAuthorized`.
#[tokio::test]
async fn policy_authz_refuses_a_barrier_ignoring_join_without_the_barrier_right_even_with_no_barrier_below_the_tenant()
 {
    let (harness, group) = harness(
        policy_authz(policy().with_grant(Grant::new(U, TOPIC, "consume"))),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": C, "ownership_filter": "entire_tenant", "types": [PLACED], "barrier_mode": "ignore" }]),
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
        }),
        "row 8, PolicyAuthZ: TenantIdNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

// -- row 9: an event-type rule under PolicyAuthZ --

/// The rule grants `PLACED` alone. One interest naming both types gets a
/// scope that admits `PLACED` only, so the post-check refuses the exact
/// `PAID` entry: 403 `EventTypeNotAuthorized` naming it.
#[tokio::test]
async fn policy_authz_refuses_an_interest_that_names_a_type_its_event_type_rule_excludes() {
    let (harness, group) = harness(
        policy_authz(
            policy()
                .with_grant(Grant::new(U, TOPIC, "consume"))
                .with_event_types(U, TOPIC, [PLACED]),
        ),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PLACED, PAID] }]),
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
                "reason": format!("calling principal lacks consume on event type '{PAID}'"),
                "resource_type": EVENT_TYPE_RESOURCE,
            },
        }),
        "row 9 (a), PolicyAuthZ: EventTypeNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

/// The rule grants `PLACED` alone, and the interest names only `PLACED`:
/// 201.
#[tokio::test]
async fn policy_authz_lets_an_interest_join_with_only_the_types_its_event_type_rule_grants() {
    let (harness, group) = harness(
        policy_authz(
            policy()
                .with_grant(Grant::new(U, TOPIC, "consume"))
                .with_event_types(U, TOPIC, [PLACED]),
        ),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 9 (b), PolicyAuthZ: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [PLACED],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 9 (b), PolicyAuthZ"
    );
}

/// The rule grants `PLACED` alone, and the interest names only `PAID`: the
/// rule and the candidates do not meet, so the consume scope is denied
/// rather than answered with an empty `in`; the tenant check allows `0x234`,
/// so the refusal is the topic's: 403 `TopicNotAuthorized`.
#[tokio::test]
async fn policy_authz_refuses_the_topic_when_its_event_type_rule_grants_none_of_the_candidates() {
    let (harness, group) = harness(
        policy_authz(
            policy()
                .with_grant(Grant::new(U, TOPIC, "consume"))
                .with_event_types(U, TOPIC, [PLACED]),
        ),
        registry(),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PAID] }]),
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
                "resource_type": TOPIC_RESOURCE,
            },
        }),
        "row 9 (c), PolicyAuthZ: TopicNotAuthorized"
    );
    assert_no_subscription(&harness).await;
}

// -- row 10: delivery --

/// #5494 Validation: (234, 890) JOIN 234, `entire_tenant`: delivers 234 only.
/// The `PLACED` events are candidates, decided from the scope JOIN kept; the
/// `NOTED` event is on a topic over the candidate cap, so delivery asks the
/// PDP about its type alone. The `0xC` event is not delivered, and the
/// stream stays open.
#[tokio::test]
async fn policy_authz_delivers_only_the_joined_tenants_events_and_keeps_the_stream_open() {
    let (harness, group) = harness(
        policy_authz(
            policy()
                .with_grant(Grant::new(U, TOPIC, "consume"))
                .with_grant(Grant::new(U, OVER_CAP_TOPIC, "consume")),
        ),
        delivery_registry(),
    )
    .await;
    let resp = join_both_topics(&harness, &group).await;
    resp.assert_status(201);
    let body = sorted_assignments(resp.json());
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 10, PolicyAuthZ: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [
                {
                    "topic": TOPIC,
                    "subject_tenant": T234,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [PLACED],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": OVER_CAP_TOPIC,
                    "subject_tenant": T234,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [NOTED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": OVER_CAP_TOPIC, "partition": 0 },
                { "topic": TOPIC, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 10, PolicyAuthZ: the JOIN"
    );
    seek_earliest(&harness, id).await;
    publish_delivery_events(&harness).await;

    let mut stream = tokio::time::timeout(
        BOUND,
        harness
            .delivery()
            .stream(&caller(U, T234), Uuid::parse_str(id).unwrap()),
    )
    .await
    .expect("the open answers within the bound")
    .expect("the open is allowed");
    let delivered = read_events(&mut stream, 3).await;

    let on = |topic: &str| -> Vec<Uuid> {
        delivered
            .iter()
            .filter(|(of, _)| of == topic)
            .map(|(_, event)| *event)
            .collect()
    };
    assert_eq!(
        (on(TOPIC), on(OVER_CAP_TOPIC)),
        (
            vec![Uuid::from_u128(0x10_01), Uuid::from_u128(0x10_04)],
            vec![Uuid::from_u128(0x10_03)],
        ),
        "row 10, PolicyAuthZ: the events delivered, per topic"
    );
    let after = next_notable_frame(&mut stream).await;
    assert!(
        after.is_none(),
        "row 10, PolicyAuthZ: the stream stays open, got {after:?}"
    );
}

/// #5494 Validation: (234, 890) JOIN 234, `entire_tenant`: delivers 234 only.
/// Under tr-authz the consume scope and the single-type decision for the
/// over-cap topic's `NOTED` both compile.
#[tokio::test]
async fn tr_authz_delivers_only_the_joined_tenants_events_and_keeps_the_stream_open() {
    let (harness, group) = harness(tr_authz(), delivery_registry()).await;
    let resp = join_both_topics(&harness, &group).await;
    resp.assert_status(201);
    let body = sorted_assignments(resp.json());
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 10, tr-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [
                {
                    "topic": TOPIC,
                    "subject_tenant": T234,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [PLACED],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": OVER_CAP_TOPIC,
                    "subject_tenant": T234,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [NOTED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": OVER_CAP_TOPIC, "partition": 0 },
                { "topic": TOPIC, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 10, tr-authz: the JOIN"
    );
    seek_earliest(&harness, id).await;
    publish_delivery_events(&harness).await;

    let mut stream = tokio::time::timeout(
        BOUND,
        harness
            .delivery()
            .stream(&caller(U, T234), Uuid::parse_str(id).unwrap()),
    )
    .await
    .expect("the open answers within the bound")
    .expect("the open is allowed");
    let delivered = read_events(&mut stream, 3).await;

    let on = |topic: &str| -> Vec<Uuid> {
        delivered
            .iter()
            .filter(|(of, _)| of == topic)
            .map(|(_, event)| *event)
            .collect()
    };
    assert_eq!(
        (on(TOPIC), on(OVER_CAP_TOPIC)),
        (
            vec![Uuid::from_u128(0x10_01), Uuid::from_u128(0x10_04)],
            vec![Uuid::from_u128(0x10_03)],
        ),
        "row 10, tr-authz: the events delivered, per topic"
    );
    let after = next_notable_frame(&mut stream).await;
    assert!(
        after.is_none(),
        "row 10, tr-authz: the stream stays open, got {after:?}"
    );
}

/// Opens a stream on `id` as `U` and returns the ids of the first `count`
/// events it delivers, all within [`BOUND`], then checks the stream stays open
/// with nothing else.
async fn delivered_then_quiet(harness: &EventBrokerHarness, id: &str, count: usize) -> Vec<Uuid> {
    let mut stream = tokio::time::timeout(
        BOUND,
        harness
            .delivery()
            .stream(&caller(U, T234), Uuid::parse_str(id).unwrap()),
    )
    .await
    .expect("the open answers within the bound")
    .expect("the open is allowed");
    let delivered = read_events(&mut stream, count)
        .await
        .into_iter()
        .map(|(_, event)| event)
        .collect();
    let after = next_notable_frame(&mut stream).await;
    assert!(after.is_none(), "the stream stays open, got {after:?}");
    delivered
}

/// Seeks partition 0 of `TOPIC` alone to earliest.
async fn seek_topic_earliest(harness: &EventBrokerHarness, subscription: &str) {
    harness
        .api_v1()
        .post_subscription_seek(subscription)
        .with_body(Json(&json!({
            "topology_version": 1,
            "positions": { (TOPIC): [{ "partition": 0, "value": "earliest" }] },
        })))
        .send()
        .await
        .assert_status(200);
}

/// The id of the subscription a 201 JOIN created.
fn joined_id(resp: &TestResponse) -> String {
    resp.assert_status(201);
    resp.json()["id"]
        .as_str()
        .expect("the 201 body names the subscription")
        .to_owned()
}

/// Under `only_mine`, the default: of `U`'s events in `0x234`, of another owner's, of an unowned
/// one and of `U`'s in child `0xC`, only the first is delivered. A last `U` event in `0x234`
/// follows the others on the partition, so reading it proves they were passed over.
#[tokio::test]
async fn tr_authz_delivers_only_the_callers_own_events_of_its_tenant_under_only_mine() {
    let (harness, group) = harness(tr_authz(), delivery_registry()).await;
    let id = joined_id(
        &join(
            &harness,
            &group,
            json!([{ "topic": TOPIC, "types": [PLACED] }]),
        )
        .await,
    );
    seek_topic_earliest(&harness, &id).await;
    publish_owned(&harness, 0x20_01, T234, U).await;
    publish_owned(&harness, 0x20_02, T234, Uuid::from_u128(0x777)).await;
    publish(&harness, 0x20_03, PLACED, T234).await;
    publish_owned(&harness, 0x20_04, C, U).await;
    publish_owned(&harness, 0x20_05, T234, U).await;

    assert_eq!(
        delivered_then_quiet(&harness, &id, 2).await,
        vec![Uuid::from_u128(0x20_01), Uuid::from_u128(0x20_05)]
    );
}

/// Under `entire_tenant` on `0x234`: every event of `0x234`, owned or not, and none of child `0xC`.
#[tokio::test]
async fn tr_authz_delivers_every_event_of_the_tenant_and_none_below_under_entire_tenant() {
    let (harness, group) = harness(tr_authz(), delivery_registry()).await;
    let id = joined_id(
        &join(
            &harness,
            &group,
            json!([{
                "topic": TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "entire_tenant",
                "types": [PLACED],
            }]),
        )
        .await,
    );
    seek_topic_earliest(&harness, &id).await;
    publish_owned(&harness, 0x21_01, T234, U).await;
    publish_owned(&harness, 0x21_02, T234, Uuid::from_u128(0x777)).await;
    publish(&harness, 0x21_03, PLACED, T234).await;
    publish_owned(&harness, 0x21_04, C, U).await;
    publish(&harness, 0x21_05, PLACED, C).await;
    publish(&harness, 0x21_06, PLACED, T234).await;

    assert_eq!(
        delivered_then_quiet(&harness, &id, 4).await,
        vec![
            Uuid::from_u128(0x21_01),
            Uuid::from_u128(0x21_02),
            Uuid::from_u128(0x21_03),
            Uuid::from_u128(0x21_06),
        ]
    );
}

/// Under `tenant_subtree_included` on `0x234`: tr-authz answers the subtree as the tenants under
/// `0x234` that respect its barriers, so the events of `0x234` and of child `0xC` are delivered,
/// owned or not.
#[tokio::test]
async fn tr_authz_delivers_the_tenant_and_its_children_under_tenant_subtree_included() {
    let (harness, group) = harness(tr_authz(), delivery_registry()).await;
    let id = joined_id(
        &join(
            &harness,
            &group,
            json!([{
                "topic": TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "tenant_subtree_included",
                "types": [PLACED],
            }]),
        )
        .await,
    );
    seek_topic_earliest(&harness, &id).await;
    publish_owned(&harness, 0x22_01, T234, U).await;
    publish_owned(&harness, 0x22_02, T234, Uuid::from_u128(0x777)).await;
    publish_owned(&harness, 0x22_03, C, U).await;
    publish(&harness, 0x22_04, PLACED, C).await;
    publish(&harness, 0x22_05, PLACED, T234).await;

    assert_eq!(
        delivered_then_quiet(&harness, &id, 5).await,
        vec![
            Uuid::from_u128(0x22_01),
            Uuid::from_u128(0x22_02),
            Uuid::from_u128(0x22_03),
            Uuid::from_u128(0x22_04),
            Uuid::from_u128(0x22_05),
        ]
    );
}

/// #5494 Validation: (234, 890) JOIN 234, `entire_tenant`: delivers 234 only.
/// The `0xC` event stays out through the interest's tenant, as under every
/// PDP.
#[tokio::test]
async fn static_authz_delivers_only_the_joined_tenants_events_and_keeps_the_stream_open() {
    let (harness, group) = harness(static_authz(), delivery_registry()).await;
    let resp = join_both_topics(&harness, &group).await;
    resp.assert_status(201);
    let body = sorted_assignments(resp.json());
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("row 10, static-authz: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [
                {
                    "topic": TOPIC,
                    "subject_tenant": T234,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [PLACED],
                    "filter": null,
                    "unmatched_types": [],
                },
                {
                    "topic": OVER_CAP_TOPIC,
                    "subject_tenant": T234,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [NOTED],
                    "filter": null,
                    "unmatched_types": [],
                },
            ],
            "assigned": [
                { "topic": OVER_CAP_TOPIC, "partition": 0 },
                { "topic": TOPIC, "partition": 0 },
            ],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "row 10, static-authz: the JOIN"
    );
    seek_earliest(&harness, id).await;
    publish_delivery_events(&harness).await;

    let mut stream = tokio::time::timeout(
        BOUND,
        harness
            .delivery()
            .stream(&caller(U, T234), Uuid::parse_str(id).unwrap()),
    )
    .await
    .expect("the open answers within the bound")
    .expect("the open is allowed");
    let delivered = read_events(&mut stream, 3).await;

    let on = |topic: &str| -> Vec<Uuid> {
        delivered
            .iter()
            .filter(|(of, _)| of == topic)
            .map(|(_, event)| *event)
            .collect()
    };
    assert_eq!(
        (on(TOPIC), on(OVER_CAP_TOPIC)),
        (
            vec![Uuid::from_u128(0x10_01), Uuid::from_u128(0x10_04)],
            vec![Uuid::from_u128(0x10_03)],
        ),
        "row 10, static-authz: the events delivered, per topic"
    );
    let after = next_notable_frame(&mut stream).await;
    assert!(
        after.is_none(),
        "row 10, static-authz: the stream stays open, got {after:?}"
    );
}

// -- row 10, a denied single-type answer under PolicyAuthZ --

/// #5494: a single-type answer that denies skips its type's events and the
/// stream goes on. `U`'s event-type rule on `OVER_CAP_TOPIC` grants only
/// `FILLER_000`. The JOIN's scope sends no `event_types` for a topic over the
/// candidate cap, so it is allowed with no event-type predicate and is not
/// post-checked. At delivery the single-type ask for `NOTED` sends
/// `event_types: [NOTED]`, which the rule does not meet, so it is denied and
/// the `NOTED` event is passed over; the `FILLER_000` event after it on the
/// same partition is delivered. The registry gives `FILLER_000` the rows'
/// subject type, so it can be published.
///
/// Timing: the open stream is shown by no frame other than a heartbeat, a
/// topology or a progress report within `STAYS_OPEN`, which spans one
/// heartbeat interval.
#[tokio::test]
async fn policy_authz_skips_a_type_its_single_type_answer_denies_and_keeps_the_stream_open() {
    const FILLER_000: &str = "gts.cf.core.events.event.v1~x.eb.consumepdp.filler_000.v1~";
    let mut spec = vec![
        json!({ "id": OVER_CAP_TOPIC, "partitions": 1 }),
        json!({ "id": NOTED, "topic": OVER_CAP_TOPIC, "allowed_subject_types": [SUBJECT_TYPE] }),
        json!({
            "id": FILLER_000,
            "topic": OVER_CAP_TOPIC,
            "allowed_subject_types": [SUBJECT_TYPE],
        }),
    ];
    spec.extend((1..257).map(|n| {
        json!({
            "id": format!("gts.cf.core.events.event.v1~x.eb.consumepdp.filler_{n:03}.v1~"),
            "topic": OVER_CAP_TOPIC,
        })
    }));
    let (harness, group) = harness(
        policy_authz(
            policy()
                .with_grant(Grant::new(U, FILLER_000, "produce"))
                .with_grant(Grant::new(U, OVER_CAP_TOPIC, "consume"))
                .with_event_types(U, OVER_CAP_TOPIC, [FILLER_000]),
        ),
        StaticTypesRegistry::of(Value::Array(spec)),
    )
    .await;

    let resp = join(
        &harness,
        &group,
        json!([{ "topic": OVER_CAP_TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [NOTED, FILLER_000] }]),
    )
    .await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("a denied single-type answer: the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": OVER_CAP_TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [NOTED, FILLER_000],
                "filter": null,
                "unmatched_types": [],
            }],
            "assigned": [{ "topic": OVER_CAP_TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        "a denied single-type answer: the JOIN"
    );
    harness
        .api_v1()
        .post_subscription_seek(id)
        .with_body(Json(&json!({
            "topology_version": 1,
            "positions": {
                (OVER_CAP_TOPIC): [{ "partition": 0, "value": "earliest" }],
            },
        })))
        .send()
        .await
        .assert_status(200);
    publish(&harness, 0x11_01, NOTED, T234).await;
    publish(&harness, 0x11_02, FILLER_000, T234).await;

    let mut stream = tokio::time::timeout(
        BOUND,
        harness
            .delivery()
            .stream(&caller(U, T234), Uuid::parse_str(id).unwrap()),
    )
    .await
    .expect("the open answers within the bound")
    .expect("the open is allowed");
    let delivered = read_events(&mut stream, 1).await;

    assert_eq!(
        delivered,
        vec![(OVER_CAP_TOPIC.to_owned(), Uuid::from_u128(0x11_02))],
        "a denied single-type answer: only the FILLER_000 event is delivered"
    );
    let after = next_notable_frame(&mut stream).await;
    assert!(
        after.is_none(),
        "a denied single-type answer: the stream stays open, got {after:?}"
    );
}

// -- group creation and the group check --

/// Creating a group asks `define` on the group family resource with the
/// caller's tenant as the root of a `root_only` context; tr-authz answers it
/// with its root-only rule, reflexive on the caller's own tenant, and the
/// resource declares the `owner_tenant_id` its allow names.
#[tokio::test]
async fn tr_authz_lets_a_user_create_a_group_in_its_own_tenant() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .with_policy_enforcer(PolicyEnforcer::new(tr_authz()))
        .with_security_context(caller(U, T234))
        .build()
        .await;

    let resp = harness.api_v1().post_consumer_groups().send().await;

    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    assert_eq!(
        body,
        json!({
            "id": id,
            "kind": "anonymous",
            "tenant": T234,
            "owner_principal": U,
            "description": null,
            "created_at": created_at,
        })
    );
    let groups: Vec<String> = harness
        .repo()
        .list_consumer_groups()
        .await
        .expect("the repo lists groups")
        .into_iter()
        .map(|group| group.id.to_string())
        .collect();
    assert_eq!(groups, vec![id.to_owned()], "the group is stored");
}

/// Passes every request to the PDP under test and records it.
struct Recording {
    pdp: Arc<dyn AuthZResolverApi>,
    recorded: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl AuthZResolverApi for Recording {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.recorded
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        self.pdp.evaluate(ctx, request).await
    }
}

/// `U` JOINs its own group under `authz` with no interests, so the group check
/// is the only call the JOIN makes; returns the JOIN's answer and the
/// requests it sent.
async fn join_own_group(
    authz: Arc<dyn AuthZResolverApi>,
) -> (EventBrokerHarness, String, TestResponse, Vec<Value>) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let (harness, group) = harness(
        Arc::new(Recording {
            pdp: authz,
            recorded: Arc::clone(&recorded),
        }),
        registry(),
    )
    .await;
    recorded.lock().unwrap().clear();
    let resp = join(&harness, &group, json!([])).await;
    let sent = recorded.lock().unwrap().clone();
    (harness, group, resp, sent)
}

/// The anonymous group check `U` sends for `group`: `consume` on the group
/// family resource, naming the group and its tenant `0x234`, with `0x234` as
/// the root of a `root_only` context with barriers respected.
fn group_check(group: &str) -> Value {
    json!({
        "subject": { "id": U, "properties": { "tenant_id": T234 } },
        "action": { "name": "consume" },
        "resource": {
            "type": CONSUMER_GROUP_RESOURCE,
            "properties": { "consumer_group_id": group, "owner_tenant_id": T234 },
        },
        "context": {
            "tenant_context": {
                "mode": "root_only",
                "root_id": T234,
                "barrier_mode": "respect",
            },
            "token_scopes": [],
            "require_constraints": false,
            "capabilities": [],
            "supported_properties": ["owner_tenant_id"],
        },
    })
}

/// The 201 body of `U`'s JOIN of `group` with no interests.
fn assert_joined_with_no_interests(resp: &TestResponse, group: &str) {
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
            "client_agent": "consume-pdp-test",
            "interests": [],
            "assigned": [],
            "topology_version": 1,
            "created_at": created_at,
        })
    );
}

/// tr-authz answers the anonymous group check with its root-only rule on the
/// group's tenant. Only the group's creator in its own tenant passes the
/// owner check before it, so that rule is reflexive and a tr-authz refusal of
/// the anonymous group check is unreachable: `PolicyAuthZ` and the recording
/// doubles of `group_check_tests` cover the refusal.
#[tokio::test]
async fn tr_authz_lets_the_creator_join_its_anonymous_group() {
    let (_harness, group, resp, sent) = join_own_group(tr_authz()).await;

    assert_joined_with_no_interests(&resp, &group);
    assert_eq!(sent, vec![group_check(&group)]);
}

/// static-authz has no rule but the caller's tenant: it allows the group
/// check, and the `owner_tenant_id` constraint it names compiles.
#[tokio::test]
async fn static_authz_lets_the_creator_join_its_anonymous_group() {
    let (_harness, group, resp, sent) = join_own_group(static_authz()).await;

    assert_joined_with_no_interests(&resp, &group);
    assert_eq!(sent, vec![group_check(&group)]);
}

#[tokio::test]
async fn policy_authz_lets_the_creator_join_with_the_group_consume_grant() {
    let (_harness, group, resp, sent) = join_own_group(policy_authz(policy())).await;

    assert_joined_with_no_interests(&resp, &group);
    assert_eq!(sent, vec![group_check(&group)]);
}

/// An action-aware PDP is the one that tells a missing `consume` grant on
/// consumer groups apart from the tenant rule.
#[tokio::test]
async fn policy_authz_refuses_a_join_without_the_group_consume_grant() {
    let (harness, group, resp, sent) =
        join_own_group(policy_authz(policy_without_group_consume())).await;

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
                "resource_type": CONSUMER_GROUP_RESOURCE,
            },
        })
    );
    assert_eq!(sent, vec![group_check(&group)]);
    assert_no_subscription(&harness).await;
}

// -- the tenant gate of the point and list operations --

/// `U`'s harness under `authz` with one subscription that joined `U`'s group
/// in `0x234`: the group, the subscription's id and the JOIN's `created_at`.
async fn joined_in_t234(
    authz: Arc<dyn AuthZResolverApi>,
) -> (EventBrokerHarness, String, Uuid, String) {
    let (harness, group) = harness(authz, registry()).await;
    let resp = join(
        &harness,
        &group,
        json!([{ "topic": TOPIC, "subject_tenant": T234, "ownership_filter": "entire_tenant", "types": [PLACED] }]),
    )
    .await;
    resp.assert_status(201);
    let body = resp.json();
    let (Some(id), Some(created_at)) = (body["id"].as_str(), body["created_at"].as_str()) else {
        panic!("the 201 body carries an id and a created_at: {body}");
    };
    let id = Uuid::parse_str(id).expect("the JOIN answers a uuid id");
    let created_at = created_at.to_owned();
    (harness, group, id, created_at)
}

/// What a domain read of the subscription [`joined_in_t234`] made returns,
/// as the read body plus the fields that body leaves out.
fn read_back(
    subscription: crate::domain::model::Subscription,
) -> (Value, Uuid, Vec<String>, Duration) {
    let tenant_id = subscription.tenant_id;
    let topics = subscription
        .topics
        .iter()
        .map(ToString::to_string)
        .collect();
    let session_timeout = subscription.session_timeout;
    let body = serde_json::to_value(
        crate::api::rest::handlers::delivery::dto::SubscriptionDto::from(subscription),
    )
    .expect("the read body serializes");
    (body, tenant_id, topics, session_timeout)
}

/// The subscription [`joined_in_t234`] made, as [`read_back`] reads it.
fn expected_read_back(
    group: &str,
    id: Uuid,
    created_at: &str,
) -> (Value, Uuid, Vec<String>, Duration) {
    (
        json!({
            "id": id,
            "consumer_group": group,
            "client_agent": "consume-pdp-test",
            "interests": [{
                "topic": TOPIC,
                "subject_tenant": T234,
                "ownership_filter": "entire_tenant",
                "barrier_mode": "respect",
                "types": [PLACED],
                "filter": null,
            }],
            "assigned": [{ "topic": TOPIC, "partition": 0 }],
            "topology_version": 1,
            "created_at": created_at,
        }),
        T234,
        vec![TOPIC.to_owned()],
        Duration::from_secs(30),
    )
}

/// A tenant gate's refusal: the PDP's denial with the gate's code.
fn assert_tenant_refused<T: std::fmt::Debug>(
    result: Result<T, crate::domain::error::DomainError>,
    row: &str,
) {
    let Err(crate::domain::error::DomainError::Forbidden {
        code,
        message,
        resource,
    }) = result
    else {
        panic!("{row}: expected Forbidden, got {result:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            crate::domain::error::ErrorCode::TenantIdNotAuthorized,
            "access denied by policy",
            "",
        ),
        "{row}"
    );
}

/// tr-authz: a caller in `0x999`, outside `0x234`'s subtree, may not read a
/// subscription of `0x234`.
#[tokio::test]
async fn tr_authz_refuses_a_subscription_lookup_from_outside_its_tenants_subtree() {
    let (harness, _group, id, _created_at) = joined_in_t234(tr_authz()).await;

    let result = harness
        .delivery()
        .get_subscription(&caller(P, T999), id)
        .await;

    assert_tenant_refused(result, "tr-authz, 0x999 reads 0x234's subscription");
    assert_eq!(
        harness
            .groups()
            .list()
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        vec![id]
    );
}

/// tr-authz: a listing by a caller in `0x999` leaves out the subscription of
/// `0x234`.
#[tokio::test]
async fn tr_authz_leaves_a_subscription_outside_the_callers_subtree_out_of_its_listing() {
    let (harness, _group, id, _created_at) = joined_in_t234(tr_authz()).await;

    let listed = harness
        .delivery()
        .list_subscriptions(&caller(P, T999))
        .await
        .expect("the listing is allowed");

    assert_eq!(
        listed.iter().map(|s| s.id).collect::<Vec<_>>(),
        Vec::<Uuid>::new()
    );
    assert_eq!(
        harness
            .groups()
            .list()
            .iter()
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        vec![id]
    );
}

/// tr-authz: a caller in `0x999` may not read the anonymous group of
/// `0x234`.
#[tokio::test]
async fn tr_authz_refuses_a_group_lookup_from_outside_its_tenants_subtree() {
    let (harness, group, _id, _created_at) = joined_in_t234(tr_authz()).await;

    let result = harness
        .delivery()
        .get_consumer_group(
            &caller(P, T999),
            &toolkit_gts::GtsInstanceId::try_new(&group).unwrap(),
        )
        .await;

    assert_tenant_refused(result, "tr-authz, 0x999 reads 0x234's group");
}

/// tr-authz: a caller in `0x123`, an ancestor of `0x234`, reads the
/// subscription of `0x234`.
#[tokio::test]
async fn tr_authz_lets_an_ancestor_tenant_read_a_subscription() {
    let (harness, group, id, created_at) = joined_in_t234(tr_authz()).await;

    let subscription = harness
        .delivery()
        .get_subscription(&caller(P, ROOT), id)
        .await
        .expect("an ancestor tenant reads the subscription");

    assert_eq!(
        read_back(subscription),
        expected_read_back(&group, id, &created_at)
    );
}

/// static-authz has no tenant tree, so a caller in `0x999` reads the
/// subscription of `0x234`.
#[tokio::test]
async fn static_authz_lets_any_tenant_read_a_subscription() {
    let (harness, group, id, created_at) = joined_in_t234(static_authz()).await;

    let subscription = harness
        .delivery()
        .get_subscription(&caller(P, T999), id)
        .await
        .expect("static-authz allows any non-nil target tenant");

    assert_eq!(
        read_back(subscription),
        expected_read_back(&group, id, &created_at)
    );
}
