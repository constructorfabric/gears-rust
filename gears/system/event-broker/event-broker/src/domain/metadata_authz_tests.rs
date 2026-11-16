//! The PDP requests the metadata reads and producer operations send.
//!
//! Who may read a topic's or an event type's metadata: a caller that may
//! consume the topic (V: `consume` on the topic, rooted at the caller's own
//! tenant), or else one that may publish one of its event types (P: `produce`
//! on the event type, rooted at the caller's own tenant). Producers read specs
//! to validate what they publish; consumers may read them too, for example for
//! code generation. A list keeps what V or P allows and hides the rest; a
//! segments read refuses with `403 TopicNotAuthorized` before it looks the
//! topic up. Producer registration, cursors and reset ask the tenant check (G:
//! `produce` on the request resource), at the caller's tenant for a
//! registration and at the producer's own tenant otherwise.
//!
//! Each test records every request the PDP sees and asserts the whole list,
//! so its length is the call count. A list asks its items concurrently, so
//! its recorded requests arrive in no fixed order: those tests sort them by
//! resource type, then action, before comparing.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, Constraint, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
    InPredicate, PolicyEnforcer, Predicate,
};
use chrono::{TimeZone, Utc};
use sea_orm::ConnectionTrait;
use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ingest::{
    ProducerMode, ProducerRegistrationInput, PublishMeta, PublishRequest, Selection,
};
use crate::test_support::{
    DenyingAuthZ, EventBrokerHarness, Grant, Json, PolicyAuthZ, StaticTypesRegistry, Tenant,
};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const TOPIC_A: &str = "gts.cf.core.events.topic.v1~x.eb.metaauthz.a.v1";
const TOPIC_B: &str = "gts.cf.core.events.topic.v1~x.eb.metaauthz.b.v1";
const TOPIC_MISSING: &str = "gts.cf.core.events.topic.v1~x.eb.metaauthz.missing.v1";
const TYPE_A1: &str = "gts.cf.core.events.event.v1~x.eb.metaauthz.a1.v1~";
const TYPE_A2: &str = "gts.cf.core.events.event.v1~x.eb.metaauthz.a2.v1~";
const TYPE_A3: &str = "gts.cf.core.events.event.v1~x.eb.metaauthz.a3.v1~";
const TYPE_B1: &str = "gts.cf.core.events.event.v1~x.eb.metaauthz.b1.v1~";
const TYPE_B2: &str = "gts.cf.core.events.event.v1~x.eb.metaauthz.b2.v1~";
const TYPE_B3: &str = "gts.cf.core.events.event.v1~x.eb.metaauthz.b3.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.metaauthz.subject.v1~";

/// The caller.
const SUBJECT: Uuid = Uuid::from_u128(0x456);
const CALLER_TENANT: Uuid = Uuid::from_u128(0x123);
/// The tenant a producer row of another principal lives in, and its owner.
const PRODUCER_TENANT: Uuid = Uuid::from_u128(0x777);
const PRODUCER_OWNER: Uuid = Uuid::from_u128(0x888);

/// How many times the in-flight test's PDP yields before it answers. Each
/// yield lets the runtime poll the other spawned checks, so every check the
/// list has started is in flight before the first one answers, and the count
/// it sees is the list's bound.
const PARK_YIELDS: usize = 16;

type Recorded = Arc<Mutex<Vec<Value>>>;

/// Records each request, serialized, then answers as `inner` does.
struct Recording<A> {
    inner: A,
    recorded: Recorded,
}

#[async_trait]
impl<A: AuthZResolverApi> AuthZResolverApi for Recording<A> {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.recorded
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        self.inner.evaluate(ctx, request).await
    }
}

fn recording(inner: impl AuthZResolverApi + 'static) -> (PolicyEnforcer, Recorded) {
    let recorded = Recorded::default();
    let enforcer = PolicyEnforcer::new(Arc::new(Recording {
        inner,
        recorded: Arc::clone(&recorded),
    }));
    (enforcer, recorded)
}

/// What a scripted PDP answers one request.
#[derive(Clone, Copy)]
enum Answer {
    Allow,
    Deny,
    /// "Allowed" with one constraint on `subject`, a property no metadata
    /// resource declares, so the enforcer cannot compile it.
    Uncompilable,
    /// The evaluation itself fails, as an unreachable PDP does.
    Unavailable,
}

/// A PDP that answers each serialized request with what `script` picks.
struct Scripted {
    script: fn(&Value) -> Answer,
}

#[async_trait]
impl AuthZResolverApi for Scripted {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let constraints = match (self.script)(&serde_json::to_value(&request).unwrap()) {
            Answer::Allow => Vec::new(),
            Answer::Deny => {
                return Ok(EvaluationResponse {
                    decision: false,
                    context: EvaluationResponseContext::default(),
                });
            }
            Answer::Uncompilable => vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    "subject",
                    [Uuid::from_u128(0x77)],
                ))],
            }],
            Answer::Unavailable => {
                return Err(
                    CanonicalError::internal("the policy decision point is unreachable").create(),
                );
            }
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }
}

/// A PDP that counts the calls in flight at once, parks each call for
/// [`PARK_YIELDS`] yields, and denies everything.
#[derive(Default)]
struct InFlight {
    now: std::sync::atomic::AtomicUsize,
    most: std::sync::atomic::AtomicUsize,
}

struct InFlightPdp(Arc<InFlight>);

#[async_trait]
impl AuthZResolverApi for InFlightPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        use std::sync::atomic::Ordering::SeqCst;
        let now = self.0.now.fetch_add(1, SeqCst) + 1;
        self.0.most.fetch_max(now, SeqCst);
        for _ in 0..PARK_YIELDS {
            tokio::task::yield_now().await;
        }
        self.0.now.fetch_sub(1, SeqCst);
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// Records the `x-request-id` each call's task holds and logs one line in the
/// call's span, then answers as `inner` does.
struct Correlating<A> {
    inner: A,
    request_ids: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl<A: AuthZResolverApi> AuthZResolverApi for Correlating<A> {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.request_ids
            .lock()
            .unwrap()
            .push(toolkit::api::current_request_id());
        tracing::info!("the policy double answers a metadata check");
        self.inner.evaluate(ctx, request).await
    }
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// The recorded requests, emptied: what setup asked is not part of the
/// checked call.
fn take(recorded: &Recorded) -> Vec<Value> {
    std::mem::take(&mut *recorded.lock().unwrap())
}

/// The recorded requests sorted by resource type, then action: a list asks
/// its items concurrently, so they arrive in no fixed order.
fn sorted(recorded: &Recorded) -> Vec<Value> {
    let mut requests = take(recorded);
    requests.sort_by_key(|request| {
        (
            request["resource"]["type"].as_str().unwrap().to_owned(),
            request["action"]["name"].as_str().unwrap().to_owned(),
        )
    });
    requests
}

fn context(subject: Uuid, tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("valid security context")
}

fn caller() -> SecurityContext {
    context(SUBJECT, CALLER_TENANT)
}

/// Topics A and B, three event types on each.
fn two_topics() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": TOPIC_A, "partitions": 1 },
        { "id": TOPIC_B, "partitions": 1 },
        { "id": TYPE_A1, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_TYPE] },
        { "id": TYPE_A2, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_TYPE] },
        { "id": TYPE_A3, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_TYPE] },
        { "id": TYPE_B1, "topic": TOPIC_B, "allowed_subject_types": [SUBJECT_TYPE] },
        { "id": TYPE_B2, "topic": TOPIC_B, "allowed_subject_types": [SUBJECT_TYPE] },
        { "id": TYPE_B3, "topic": TOPIC_B, "allowed_subject_types": [SUBJECT_TYPE] },
    ]))
}

/// Topic A with its one event type A1.
fn one_topic() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": TOPIC_A, "partitions": 1 },
        { "id": TYPE_A1, "topic": TOPIC_A, "allowed_subject_types": [SUBJECT_TYPE] },
    ]))
}

/// A policy over the caller's tenant and the producer tenant, two roots, so
/// neither reaches the other.
fn policy() -> PolicyAuthZ {
    PolicyAuthZ::new([Tenant::root(CALLER_TENANT), Tenant::root(PRODUCER_TENANT)])
        .expect("a valid tenant tree")
}

async fn harness(
    registry: StaticTypesRegistry,
    enforcer: PolicyEnforcer,
    ctx: SecurityContext,
) -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(registry)
        .with_policy_enforcer(enforcer)
        .with_security_context(ctx)
        .build()
        .await
}

/// V as `subject` of `tenant` asks it about `topic`.
fn view(subject: Uuid, tenant: Uuid, topic: &str) -> Value {
    json!({
        "subject": { "id": subject, "properties": { "tenant_id": tenant } },
        "action": { "name": "consume" },
        "resource": {
            "type": topic,
            "properties": { "topic_id": topic, "owner_tenant_id": tenant },
        },
        "context": {
            "tenant_context": { "mode": "root_only", "root_id": tenant, "barrier_mode": "respect" },
            "token_scopes": [],
            "require_constraints": false,
            "capabilities": [],
            "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
        },
    })
}

/// P as `subject` of `tenant` asks it about `event_type`.
fn produce(subject: Uuid, tenant: Uuid, event_type: &str) -> Value {
    json!({
        "subject": { "id": subject, "properties": { "tenant_id": tenant } },
        "action": { "name": "produce" },
        "resource": {
            "type": event_type,
            "properties": { "event_type_id": event_type, "owner_tenant_id": tenant },
        },
        "context": {
            "tenant_context": { "mode": "root_only", "root_id": tenant, "barrier_mode": "respect" },
            "token_scopes": [],
            "require_constraints": false,
            "capabilities": [],
            "supported_properties": ["owner_tenant_id"],
        },
    })
}

/// G as `subject` of `tenant` asks it about `target`.
fn tenant_check(subject: Uuid, tenant: Uuid, target: Uuid) -> Value {
    json!({
        "subject": { "id": subject, "properties": { "tenant_id": tenant } },
        "action": { "name": "produce" },
        "resource": {
            "type": REQUEST_RESOURCE,
            "properties": { "owner_tenant_id": target },
        },
        "context": {
            "tenant_context": { "mode": "root_only", "root_id": target, "barrier_mode": "respect" },
            "token_scopes": [],
            "require_constraints": false,
            "capabilities": [],
            "supported_properties": ["owner_tenant_id"],
        },
    })
}

fn topic_item(id: &str) -> Value {
    json!({ "id": id, "description": "a topic this test publishes to", "retention": null })
}

fn event_type_item(id: &str, topic: &str) -> Value {
    json!({
        "id": id,
        "topic": topic,
        "description": null,
        "allowed_subject_types": [SUBJECT_TYPE],
        "partition_key": "/subject_tenant",
        "data_schema": {
            "allOf": [
                { "type": ["object", "null"] },
                { "type": ["object", "null"] },
            ],
        },
    })
}

fn page(items: &[Value]) -> Value {
    json!({
        "items": items,
        "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
    })
}

/// The `403` a refused segments read answers.
fn topic_refused() -> Value {
    json!({
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": "/event-broker/v1/topics/segments",
        "context": {
            "reason": "access denied by policy",
            "resource_type": "gts.cf.core.events.topic.v1~",
        },
    })
}

/// The `403` a refused tenant check answers at `instance`.
fn tenant_refused(instance: &str) -> Value {
    json!({
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": instance,
        "context": {
            "reason": "access denied by policy",
            "resource_type": "gts.cf.core.events.request.v1~",
        },
    })
}

/// How many producer rows the harness's database holds.
async fn producer_rows(harness: &EventBrokerHarness) -> i64 {
    let conn = sea_orm::Database::connect(harness.dsn())
        .await
        .expect("connect");
    conn.query_one(
        Query::select()
            .expr_as(Expr::col(Alias::new("id")).count(), "n")
            .from(Alias::new("event_broker_producer")),
    )
    .await
    .expect("count the producer rows")
    .expect("a count row")
    .try_get::<i64>("", "n")
    .expect("a count")
}

fn audit_records(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.split_once("event_broker.events: "))
        .map(|(_, record)| record.to_owned())
        .collect()
}

// -- topic list ---------------------------------------------------------------

#[tokio::test]
async fn a_topic_list_keeps_the_topic_the_caller_may_consume_and_hides_the_rest() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TOPIC_A, "consume")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness.api_v1().get_topics().send().await;

    resp.assert_status(200);
    assert_eq!(resp.json(), page(&[topic_item(TOPIC_A)]));
    assert_eq!(
        sorted(&requests),
        vec![
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
            produce(SUBJECT, CALLER_TENANT, TYPE_B2),
            produce(SUBJECT, CALLER_TENANT, TYPE_B3),
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
        ]
    );
}

#[tokio::test]
async fn a_topic_list_shows_a_topic_through_a_produce_grant_on_one_of_its_types() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TYPE_B2, "produce")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness.api_v1().get_topics().send().await;

    resp.assert_status(200);
    assert_eq!(resp.json(), page(&[topic_item(TOPIC_B)]));
    // B's types are asked in id order and the first allow ends them, so B3
    // is never asked.
    assert_eq!(
        sorted(&requests),
        vec![
            produce(SUBJECT, CALLER_TENANT, TYPE_A1),
            produce(SUBJECT, CALLER_TENANT, TYPE_A2),
            produce(SUBJECT, CALLER_TENANT, TYPE_A3),
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
            produce(SUBJECT, CALLER_TENANT, TYPE_B2),
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
        ]
    );
}

#[tokio::test]
async fn a_topic_list_for_a_caller_with_no_grant_is_empty_after_one_call_per_topic_and_type() {
    let (enforcer, requests) = recording(policy());
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness.api_v1().get_topics().send().await;

    resp.assert_status(200);
    assert_eq!(resp.json(), page(&[]));
    // 2 topics x 3 types: one V per topic and one P per type, 8 in all.
    assert_eq!(
        sorted(&requests),
        vec![
            produce(SUBJECT, CALLER_TENANT, TYPE_A1),
            produce(SUBJECT, CALLER_TENANT, TYPE_A2),
            produce(SUBJECT, CALLER_TENANT, TYPE_A3),
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
            produce(SUBJECT, CALLER_TENANT, TYPE_B2),
            produce(SUBJECT, CALLER_TENANT, TYPE_B3),
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
        ]
    );
}

#[tokio::test]
async fn a_topic_list_filtered_to_one_id_asks_only_about_that_topic() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TOPIC_A, "consume")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness
        .api_v1()
        .get_topics()
        .with_query("$filter", format!("id%20eq%20'{TOPIC_A}'"))
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(resp.json(), page(&[topic_item(TOPIC_A)]));
    assert_eq!(
        recorded(&requests),
        vec![view(SUBJECT, CALLER_TENANT, TOPIC_A)]
    );
}

#[tokio::test]
async fn a_topic_with_more_types_than_the_cap_is_hidden_without_asking_about_its_types() {
    let mut entries = vec![json!({ "id": TOPIC_A, "partitions": 1 })];
    entries.extend((0..257).map(|n| {
        json!({
            "id": format!("gts.cf.core.events.event.v1~x.eb.metaauthz.filler_{n:03}.v1~"),
            "topic": TOPIC_A,
        })
    }));
    let (enforcer, requests) = recording(policy().with_grant(Grant::new(
        SUBJECT,
        "gts.cf.core.events.event.v1~x.eb.metaauthz.filler_000.v1~",
        "produce",
    )));
    let harness = harness(StaticTypesRegistry::of(json!(entries)), enforcer, caller()).await;

    let topics = harness
        .ingest()
        .list_topics(&caller(), &Selection::all())
        .await
        .expect("the list answers");

    assert!(topics.is_empty(), "expected no topic, got {topics:?}");
    assert_eq!(
        recorded(&requests),
        vec![view(SUBJECT, CALLER_TENANT, TOPIC_A)]
    );
}

#[tokio::test]
async fn a_topic_list_asks_at_most_five_policy_calls_at_once() {
    let in_flight = Arc::new(InFlight::default());
    let topics: Vec<Value> = (0..8)
        .map(|n| {
            json!({
                "id": format!("gts.cf.core.events.topic.v1~x.eb.metaauthz.t{n}.v1"),
                "partitions": 1,
            })
        })
        .collect();
    let harness = harness(
        StaticTypesRegistry::of(json!(topics)),
        PolicyEnforcer::new(Arc::new(InFlightPdp(Arc::clone(&in_flight)))),
        caller(),
    )
    .await;

    let listed = harness
        .ingest()
        .list_topics(&caller(), &Selection::all())
        .await
        .expect("the list answers");

    assert!(listed.is_empty(), "expected no topic, got {listed:?}");
    assert_eq!(in_flight.most.load(std::sync::atomic::Ordering::SeqCst), 5);
}

// On worker threads: a task polled on the thread that holds the test's
// entered span would log in that span without `in_current_span`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[tracing_test::traced_test]
async fn a_topic_list_asks_its_policy_calls_under_the_requests_id_and_in_its_span() {
    // Seven topics, more than one wave of five concurrent checks.
    let topics: Vec<String> = (0..7)
        .map(|n| format!("gts.cf.core.events.topic.v1~x.eb.metaauthz.t{n}.v1"))
        .collect();
    let registry = StaticTypesRegistry::of(json!(
        topics
            .iter()
            .map(|id| json!({ "id": id, "partitions": 1 }))
            .collect::<Vec<_>>()
    ));
    let requests = Recorded::default();
    let request_ids = Arc::new(Mutex::new(Vec::new()));
    let enforcer = PolicyEnforcer::new(Arc::new(Correlating {
        inner: Recording {
            inner: policy(),
            recorded: Arc::clone(&requests),
        },
        request_ids: Arc::clone(&request_ids),
    }));
    let harness = harness(registry, enforcer, caller()).await;

    let listed = toolkit::api::with_request_id(
        Some("req-metadata-list".to_owned()),
        harness.ingest().list_topics(&caller(), &Selection::all()),
    )
    .await
    .expect("the list answers");

    assert!(listed.is_empty(), "expected no topic, got {listed:?}");
    assert_eq!(
        sorted(&requests),
        topics
            .iter()
            .map(|topic| view(SUBJECT, CALLER_TENANT, topic))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        request_ids.lock().unwrap().clone(),
        vec![Some("req-metadata-list".to_owned()); 7]
    );
    logs_assert(|lines| {
        let answered = lines
            .iter()
            .filter(|line| line.contains("the policy double answers a metadata check"))
            .count();
        if answered == 7 {
            Ok(())
        } else {
            Err(format!(
                "expected 7 policy calls logged in the test's span, got {answered}"
            ))
        }
    });
}

#[tokio::test]
async fn a_topic_list_answers_500_when_the_view_check_cannot_be_evaluated() {
    let (enforcer, requests) = recording(Scripted {
        script: |_| Answer::Unavailable,
    });
    let harness = harness(one_topic(), enforcer, caller()).await;

    let resp = harness.api_v1().get_topics().send().await;

    resp.assert_status(500);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/event-broker/v1/topics",
            "context": {},
        })
    );
    assert_eq!(
        recorded(&requests),
        vec![view(SUBJECT, CALLER_TENANT, TOPIC_A)]
    );
}

#[tokio::test]
async fn a_topic_whose_view_answer_does_not_compile_is_shown_through_the_produce_check() {
    let (enforcer, requests) = recording(Scripted {
        script: |request| {
            if request["action"]["name"] == "consume" {
                Answer::Uncompilable
            } else {
                Answer::Allow
            }
        },
    });
    let harness = harness(one_topic(), enforcer, caller()).await;

    let resp = harness.api_v1().get_topics().send().await;

    resp.assert_status(200);
    assert_eq!(resp.json(), page(&[topic_item(TOPIC_A)]));
    assert_eq!(
        recorded(&requests),
        vec![
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            produce(SUBJECT, CALLER_TENANT, TYPE_A1),
        ]
    );
}

#[tokio::test]
async fn the_in_process_topic_list_keeps_only_what_the_caller_may_read() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TOPIC_A, "consume")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let topics = harness
        .broker()
        .list_topics(&caller())
        .await
        .expect("the list answers");

    let ids: Vec<&str> = topics.iter().map(|topic| topic.id.as_ref()).collect();
    assert_eq!(ids, vec![TOPIC_A]);
    assert_eq!(
        sorted(&requests),
        vec![
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
            produce(SUBJECT, CALLER_TENANT, TYPE_B2),
            produce(SUBJECT, CALLER_TENANT, TYPE_B3),
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
        ]
    );
}

// -- event-type list and point read -------------------------------------------

#[tokio::test]
async fn an_event_type_list_keeps_the_types_of_a_topic_the_caller_may_consume() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TOPIC_A, "consume")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness.api_v1().get_event_types().send().await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        page(&[
            event_type_item(TYPE_A1, TOPIC_A),
            event_type_item(TYPE_A2, TOPIC_A),
            event_type_item(TYPE_A3, TOPIC_A),
        ])
    );
    // One V per distinct topic, then one P per type of the refused topic.
    assert_eq!(
        sorted(&requests),
        vec![
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
            produce(SUBJECT, CALLER_TENANT, TYPE_B2),
            produce(SUBJECT, CALLER_TENANT, TYPE_B3),
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
        ]
    );
}

#[tokio::test]
async fn an_event_type_list_shows_a_type_of_a_refused_topic_through_its_produce_grant() {
    let (enforcer, requests) = recording(
        policy()
            .with_grant(Grant::new(SUBJECT, TOPIC_A, "consume"))
            .with_grant(Grant::new(SUBJECT, TYPE_B2, "produce")),
    );
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness.api_v1().get_event_types().send().await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        page(&[
            event_type_item(TYPE_A1, TOPIC_A),
            event_type_item(TYPE_A2, TOPIC_A),
            event_type_item(TYPE_A3, TOPIC_A),
            event_type_item(TYPE_B2, TOPIC_B),
        ])
    );
    assert_eq!(
        sorted(&requests),
        vec![
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
            produce(SUBJECT, CALLER_TENANT, TYPE_B2),
            produce(SUBJECT, CALLER_TENANT, TYPE_B3),
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
        ]
    );
}

#[tokio::test]
async fn an_in_process_read_of_a_type_the_caller_may_not_read_answers_as_unknown() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TOPIC_A, "consume")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let result = harness.broker().get_event_type(&caller(), TYPE_B1).await;

    let Err(event_broker_sdk::EventBrokerError::EventTypeUnknown { event_type, detail }) = result
    else {
        panic!("expected EventTypeUnknown, got {result:?}");
    };
    assert_eq!(
        (event_type.as_str(), detail.as_str()),
        (TYPE_B1, "no such event type")
    );
    assert_eq!(
        recorded(&requests),
        vec![
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
        ]
    );
}

#[tokio::test]
async fn an_in_process_read_of_a_type_on_a_consumable_topic_asks_one_policy_call() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TOPIC_A, "consume")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let event_type = harness
        .broker()
        .get_event_type(&caller(), TYPE_A2)
        .await
        .expect("the type is readable");

    let subject_types: Vec<&str> = event_type
        .allowed_subject_types
        .iter()
        .map(gts::GtsIdPattern::pattern)
        .collect();
    assert_eq!(
        (
            event_type.id.as_ref(),
            event_type.topic.as_ref(),
            event_type.description.as_deref(),
            subject_types,
            event_type.partition_key.as_str(),
            &event_type.data_schema,
        ),
        (
            TYPE_A2,
            TOPIC_A,
            None,
            vec![SUBJECT_TYPE],
            "/subject_tenant",
            &json!({
                "allOf": [
                    { "type": ["object", "null"] },
                    { "type": ["object", "null"] },
                ],
            }),
        )
    );
    assert_eq!(
        recorded(&requests),
        vec![view(SUBJECT, CALLER_TENANT, TOPIC_A)]
    );
}

// -- segments -----------------------------------------------------------------

#[tokio::test]
async fn segments_of_a_registered_topic_the_caller_may_not_read_answer_403() {
    let (enforcer, requests) = recording(policy());
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query("topic", TOPIC_B)
        .with_query("partition", "0")
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(resp.json(), topic_refused());
    assert_eq!(
        recorded(&requests),
        vec![
            view(SUBJECT, CALLER_TENANT, TOPIC_B),
            produce(SUBJECT, CALLER_TENANT, TYPE_B1),
            produce(SUBJECT, CALLER_TENANT, TYPE_B2),
            produce(SUBJECT, CALLER_TENANT, TYPE_B3),
        ]
    );
}

#[tokio::test]
async fn segments_of_a_topic_the_caller_may_consume_answer_its_manifest() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TOPIC_A, "consume")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query("topic", TOPIC_A)
        .with_query("partition", "0")
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        json!({
            "topic": TOPIC_A,
            "partition": 0,
            "start_sequence": 0,
            "end_sequence": 0,
            "start_time": null,
            "end_time": null,
            "segments": [],
        })
    );
    assert_eq!(
        recorded(&requests),
        vec![view(SUBJECT, CALLER_TENANT, TOPIC_A)]
    );
}

#[tokio::test]
async fn segments_of_a_topic_the_caller_may_publish_to_answer_its_manifest() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(SUBJECT, TYPE_A2, "produce")));
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query("topic", TOPIC_A)
        .with_query("partition", "0")
        .send()
        .await;

    resp.assert_status(200);
    assert_eq!(
        resp.json(),
        json!({
            "topic": TOPIC_A,
            "partition": 0,
            "start_sequence": 0,
            "end_sequence": 0,
            "start_time": null,
            "end_time": null,
            "segments": [],
        })
    );
    assert_eq!(
        recorded(&requests),
        vec![
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            produce(SUBJECT, CALLER_TENANT, TYPE_A1),
            produce(SUBJECT, CALLER_TENANT, TYPE_A2),
        ]
    );
}

// -- producer registration, cursors and reset ---------------------------------

#[tokio::test]
async fn a_producer_registration_the_tenant_check_denies_answers_403_and_stores_nothing() {
    let (enforcer, requests) = recording(DenyingAuthZ {
        deny_if: |_: &EvaluationRequest| true,
    });
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness
        .api_v1()
        .post_producers()
        .with_body(Json(&json!({ "mode": "chained", "client_agent": "agent" })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(resp.json(), tenant_refused("/event-broker/v1/producers"));
    assert_eq!(
        recorded(&requests),
        vec![tenant_check(SUBJECT, CALLER_TENANT, CALLER_TENANT)]
    );
    assert_eq!(producer_rows(&harness).await, 0);
}

#[tokio::test]
async fn a_producer_registration_with_a_bad_client_agent_answers_400_before_any_policy_call() {
    let (enforcer, requests) = recording(DenyingAuthZ {
        deny_if: |_: &EvaluationRequest| true,
    });
    let harness = harness(two_topics(), enforcer, caller()).await;

    let resp = harness
        .api_v1()
        .post_producers()
        .with_body(Json(
            &json!({ "mode": "chained", "client_agent": "agent-\u{e9}" }),
        ))
        .send()
        .await;

    resp.assert_status(400);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "Request validation failed",
            "instance": "/event-broker/v1/producers",
            "context": {
                "field_violations": [{
                    "field": "client_agent",
                    "description": "must contain only printable ASCII (0x20-0x7E)",
                    "reason": "ascii_only",
                }],
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(recorded(&requests), Vec::<Value>::new());
    assert_eq!(producer_rows(&harness).await, 0);
}

/// Registers a producer of [`PRODUCER_OWNER`] in [`PRODUCER_TENANT`].
async fn register_foreign_producer(harness: &EventBrokerHarness) -> Uuid {
    harness
        .ingest()
        .register_producer(
            &context(PRODUCER_OWNER, PRODUCER_TENANT),
            ProducerRegistrationInput {
                mode: ProducerMode::Chained,
                client_agent: "owner-agent".to_owned(),
            },
        )
        .await
        .expect("the owner registers in its own tenant")
        .id
}

#[tokio::test]
async fn cursors_and_reset_by_a_non_owner_in_the_producers_tenant_answer_producer_not_owned() {
    let in_producer_tenant = context(SUBJECT, PRODUCER_TENANT);
    let (enforcer, requests) = recording(policy());
    let harness = harness(two_topics(), enforcer, in_producer_tenant).await;
    let producer = register_foreign_producer(&harness).await;
    take(&requests);

    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer.to_string())
        .send()
        .await;
    let cursor_requests = take(&requests);
    let reset = harness
        .api_v1()
        .post_producer_reset(&producer.to_string())
        .send()
        .await;

    let not_owned = |instance: String| {
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": instance,
            "context": {
                "reason": "calling principal does not own this producer_id",
                "resource_type": "gts.cf.core.events.producer.v1~",
            },
        })
    };
    cursors.assert_status(403);
    assert_eq!(
        cursors.json(),
        not_owned(format!("/event-broker/v1/producers/{producer}/cursors"))
    );
    reset.assert_status(403);
    assert_eq!(
        reset.json(),
        not_owned(format!("/event-broker/v1/producers/{producer}:reset"))
    );
    assert_eq!(
        cursor_requests,
        vec![tenant_check(SUBJECT, PRODUCER_TENANT, PRODUCER_TENANT)]
    );
    assert_eq!(
        take(&requests),
        vec![tenant_check(SUBJECT, PRODUCER_TENANT, PRODUCER_TENANT)]
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn cursors_and_reset_by_a_caller_outside_the_producers_tenant_answer_403_first() {
    let (enforcer, requests) =
        recording(policy().with_grant(Grant::new(PRODUCER_OWNER, TYPE_A1, "produce")));
    let harness = harness(two_topics(), enforcer, caller()).await;
    let producer = register_foreign_producer(&harness).await;
    let owner = context(PRODUCER_OWNER, PRODUCER_TENANT);
    harness
        .ingest()
        .publish_event(
            &owner,
            PublishRequest {
                id: Uuid::from_u128(0xe1),
                r#type: crate::test_support::event_type_id(TYPE_A1),
                subject_tenant: PRODUCER_TENANT,
                source: "metadata-authz-test".to_owned(),
                subject: "s1".to_owned(),
                subject_type: gts::GtsTypeId::new(SUBJECT_TYPE),
                subject_owner: None,
                occurred_at: Utc.with_ymd_and_hms(2026, 11, 14, 10, 0, 0).unwrap(),
                trace_parent: None,
                data: json!({}),
                meta: Some(PublishMeta {
                    version: 1,
                    producer: Some(producer),
                    previous: Some(0),
                    sequence: Some(1),
                }),
            },
        )
        .await
        .expect("the owner publishes the chain's first event");
    take(&requests);

    let cursors = harness
        .api_v1()
        .get_producer_cursors(&producer.to_string())
        .send()
        .await;
    let cursor_requests = take(&requests);
    let reset = harness
        .api_v1()
        .post_producer_reset(&producer.to_string())
        .send()
        .await;
    let reset_requests = take(&requests);

    cursors.assert_status(403);
    assert_eq!(
        cursors.json(),
        tenant_refused(&format!("/event-broker/v1/producers/{producer}/cursors"))
    );
    reset.assert_status(403);
    assert_eq!(
        reset.json(),
        tenant_refused(&format!("/event-broker/v1/producers/{producer}:reset"))
    );
    assert_eq!(
        cursor_requests,
        vec![tenant_check(SUBJECT, CALLER_TENANT, PRODUCER_TENANT)]
    );
    assert_eq!(
        reset_requests,
        vec![tenant_check(SUBJECT, CALLER_TENANT, PRODUCER_TENANT)]
    );
    // The refused reset left the chain where the owner put it.
    let kept = harness
        .ingest()
        .get_producer_cursors(&owner, producer)
        .await
        .expect("the owner reads its cursors");
    let [topic] = kept.topics.as_slice() else {
        panic!("expected one topic cursor, got {:?}", kept.topics);
    };
    assert_eq!(
        (
            kept.producer_id,
            kept.client_agent.as_str(),
            topic.topic.as_ref(),
            topic.last_sequence
        ),
        (producer, "owner-agent", TOPIC_A, 1)
    );
    logs_assert(|lines| {
        let records = audit_records(lines);
        if records.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "a refused reset emits no audit record, got {records:?}"
            ))
        }
    });
}

// -- a caller whose tenant is nil (as tr-authz refuses it) ---------------------

/// Denies every request whose subject tenant is the nil UUID, as tr-authz
/// does, and allows the rest.
fn refuses_a_nil_tenant(request: &EvaluationRequest) -> bool {
    request.subject.properties.get("tenant_id") == Some(&json!(Uuid::nil()))
}

#[tokio::test]
async fn a_caller_with_a_nil_tenant_sees_no_topic_and_no_event_type() {
    let (enforcer, requests) = recording(DenyingAuthZ {
        deny_if: refuses_a_nil_tenant,
    });
    let nil_tenant = context(SUBJECT, Uuid::nil());
    let harness = harness(two_topics(), enforcer, nil_tenant).await;

    let topics = harness.api_v1().get_topics().send().await;
    let topic_requests = sorted(&requests);
    let event_types = harness.api_v1().get_event_types().send().await;

    topics.assert_status(200);
    assert_eq!(topics.json(), page(&[]));
    event_types.assert_status(200);
    assert_eq!(event_types.json(), page(&[]));
    let every_call = vec![
        produce(SUBJECT, Uuid::nil(), TYPE_A1),
        produce(SUBJECT, Uuid::nil(), TYPE_A2),
        produce(SUBJECT, Uuid::nil(), TYPE_A3),
        produce(SUBJECT, Uuid::nil(), TYPE_B1),
        produce(SUBJECT, Uuid::nil(), TYPE_B2),
        produce(SUBJECT, Uuid::nil(), TYPE_B3),
        view(SUBJECT, Uuid::nil(), TOPIC_A),
        view(SUBJECT, Uuid::nil(), TOPIC_B),
    ];
    assert_eq!(topic_requests, every_call);
    assert_eq!(sorted(&requests), every_call);
}

#[tokio::test]
async fn a_caller_with_a_nil_tenant_is_refused_segments_with_403() {
    let (enforcer, requests) = recording(DenyingAuthZ {
        deny_if: refuses_a_nil_tenant,
    });
    let nil_tenant = context(SUBJECT, Uuid::nil());
    let harness = harness(two_topics(), enforcer, nil_tenant).await;

    let resp = harness
        .api_v1()
        .get_topic_segments()
        .with_query("topic", TOPIC_MISSING)
        .with_query("partition", "0")
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(resp.json(), topic_refused());
    assert_eq!(
        recorded(&requests),
        vec![view(SUBJECT, Uuid::nil(), TOPIC_MISSING)]
    );
}

#[tokio::test]
async fn a_caller_with_a_nil_tenant_is_refused_producer_registration_with_403() {
    let (enforcer, requests) = recording(DenyingAuthZ {
        deny_if: refuses_a_nil_tenant,
    });
    let nil_tenant = context(SUBJECT, Uuid::nil());
    let harness = harness(two_topics(), enforcer, nil_tenant).await;

    let resp = harness
        .api_v1()
        .post_producers()
        .with_body(Json(&json!({ "mode": "chained", "client_agent": "agent" })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(resp.json(), tenant_refused("/event-broker/v1/producers"));
    assert_eq!(
        recorded(&requests),
        vec![tenant_check(SUBJECT, Uuid::nil(), Uuid::nil())]
    );
    assert_eq!(producer_rows(&harness).await, 0);
}

#[tokio::test]
async fn an_in_process_topic_list_propagates_a_failed_view_evaluation() {
    let (enforcer, requests) = recording(Scripted {
        script: |_| Answer::Unavailable,
    });
    let harness = harness(one_topic(), enforcer, caller()).await;

    let result = harness
        .ingest()
        .list_topics(&caller(), &Selection::all())
        .await;

    let Err(DomainError::Internal(message)) = result else {
        panic!("expected Internal, got {result:?}");
    };
    assert_eq!(
        message,
        "authz evaluation failed: internal: An internal error occurred. Please retry later."
    );
    assert_eq!(
        recorded(&requests),
        vec![view(SUBJECT, CALLER_TENANT, TOPIC_A)]
    );
}

#[tokio::test]
async fn an_event_type_list_shows_a_type_through_produce_when_the_view_answer_does_not_compile() {
    let (enforcer, requests) = recording(Scripted {
        script: |request| {
            if request["action"]["name"] == "consume" {
                Answer::Uncompilable
            } else {
                Answer::Allow
            }
        },
    });
    let harness = harness(one_topic(), enforcer, caller()).await;

    let resp = harness.api_v1().get_event_types().send().await;

    resp.assert_status(200);
    assert_eq!(resp.json(), page(&[event_type_item(TYPE_A1, TOPIC_A)]));
    assert_eq!(
        recorded(&requests),
        vec![
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            produce(SUBJECT, CALLER_TENANT, TYPE_A1),
        ]
    );
}

#[tokio::test]
async fn an_event_type_list_hides_a_type_whose_produce_answer_does_not_compile() {
    let (enforcer, requests) = recording(Scripted {
        script: |request| {
            if request["action"]["name"] == "consume" {
                Answer::Deny
            } else {
                Answer::Uncompilable
            }
        },
    });
    let harness = harness(one_topic(), enforcer, caller()).await;

    let resp = harness.api_v1().get_event_types().send().await;

    resp.assert_status(200);
    assert_eq!(resp.json(), page(&[]));
    assert_eq!(
        recorded(&requests),
        vec![
            view(SUBJECT, CALLER_TENANT, TOPIC_A),
            produce(SUBJECT, CALLER_TENANT, TYPE_A1),
        ]
    );
}
