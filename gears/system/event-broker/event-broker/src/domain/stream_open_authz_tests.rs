//! What a stream open asks the PDP and what it stores on the member.
//!
//! An open refuses a caller with no identity before anything else, then runs
//! the tenant gate (one PDP call, `G`), the joiner check, the group check (a
//! PDP call only when the memo holds no allow for it: JOIN and the seek leave
//! one, `group_check_tests` covers the rest), the cursors and the stream lease,
//! and then takes the member's consume scope: the one it holds when it was
//! evaluated for the same caller and no group has expired, else one evaluated
//! group by group through the memo JOIN filled, and stored on the member with
//! a compare-and-swap on its generation.
//!
//! Each test JOINs and seeks under a scripted PDP that records every request,
//! clears the list, and asserts the full list the open adds. The caller sits
//! in tenant `0x123`; the interests name `0x234`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AccessRequest, AuthZResolverApi, BarrierMode as PdpBarrierMode, Constraint, DenyReason,
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InPredicate, PolicyEnforcer,
    Predicate, ResourceType, TenantMode,
};
use event_broker_sdk::{EventBrokerError, SubscriptionId};
use gts::GtsTypeId;
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_stream::StreamExt;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{
    AccessScope, PlatformSecurityContext, ScopeConstraint, ScopeFilter, SecurityContext,
    pep_properties,
};
use uuid::Uuid;

use super::{Remembered, ScopeExpiry};
use crate::api::rest::handlers::delivery::dto::FrameDto;
use crate::domain::consumer_group_coordinator::TopicInterest;
use crate::domain::consumer_group_coordinator::member_scope::{
    AuthorizedGroup, CachedScope, Expected, ScopeContent, ScopeGeneration, ScopePrincipal,
    ScopeWrite, ScopeWriteOutcome,
};
use crate::domain::join_scope::GroupKey;
use crate::domain::model::BarrierMode;
use crate::domain::streaming::scope::{CompiledScope, TenantSetInterner};
use crate::domain::streaming::scope_memo::{MemoInsert, MemoKey, MemoResult};
use crate::domain::streaming::session::FrameStream;
use crate::test_support::response::TestResponse;
use crate::test_support::{
    EventBrokerHarness, Json, SpecQuery, SpecQueryHook, StaticTypesRegistry,
};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const CONSUMER_GROUP_RESOURCE: &str = "gts.cf.core.events.consumer_group.v1~";
const TOPIC_A: &str = "gts.cf.core.events.topic.v1~x.eb.streamopen.a.v1";
/// Four partitions, so a group of two members splits it in halves.
const TOPIC_WIDE: &str = "gts.cf.core.events.topic.v1~x.eb.streamopen.wide.v1";
const CREATED: &str = "gts.cf.core.events.event.v1~x.eb.streamopen.created.v1~";
const SHIPPED: &str = "gts.cf.core.events.event.v1~x.eb.streamopen.shipped.v1~";
const PLACED: &str = "gts.cf.core.events.event.v1~x.eb.streamopen.placed.v1~";
const ORDER: &str = "gts.x.eb.streamopen.order.v1~";
const INVOICE: &str = "gts.x.eb.streamopen.invoice.v1~";

const SUBJECT: Uuid = Uuid::from_u128(0x456);
const OTHER_SUBJECT: Uuid = Uuid::from_u128(0x457);
const CALLER_TENANT: Uuid = Uuid::from_u128(0x123);
const TENANT: Uuid = Uuid::from_u128(0x234);

/// The harness's `streaming.authz_scope_ttl_secs`, the production default.
const TTL: Duration = Duration::from_secs(60);

/// The harness's `subscription.join_timeout_secs`, the production default.
const JOIN_TIMEOUT: Duration = Duration::from_secs(60);

/// The `session_timeout` of a member a test joins itself; no test here
/// waits it out.
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Bounds every open: one that loops never answers, and over REST one that
/// succeeds never ends.
const BOUND: Duration = Duration::from_secs(10);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// Which call a request is. The group check of JOIN, seek and stream open is
/// on a consumer-group resource; the tenant gate of seek and of stream open and
/// R are on the tenant-scope resource, where only R requires constraints.
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
    F: Fn(Call) -> Answer + Send + Sync,
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
        Ok(match (self.answer)(Call::of(&request)) {
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
                let constraints = match root {
                    Some(root) if declared => vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [root],
                        ))],
                    }],
                    _ => Vec::new(),
                };
                EvaluationResponse {
                    decision: true,
                    context: EvaluationResponseContext {
                        constraints,
                        ..Default::default()
                    },
                }
            }
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

/// The caller with token scopes of its own: the same subject and tenant, but
/// another principal, since the token scopes are part of it.
fn caller_with_scopes() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(CALLER_TENANT)
        .token_scopes(vec!["events:read".to_owned()])
        .build()
        .expect("valid security context")
}

fn another_principal() -> ScopePrincipal {
    ScopePrincipal::from(
        &SecurityContext::builder()
            .subject_id(OTHER_SUBJECT)
            .subject_tenant_id(CALLER_TENANT)
            .build()
            .expect("valid security context"),
    )
}

fn registry() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": TOPIC_A, "partitions": 1 },
        { "id": CREATED, "topic": TOPIC_A, "allowed_subject_types": [ORDER] },
        { "id": SHIPPED, "topic": TOPIC_A, "allowed_subject_types": [ORDER, INVOICE] },
        { "id": TOPIC_WIDE, "partitions": 4 },
        { "id": PLACED, "topic": TOPIC_WIDE, "allowed_subject_types": [ORDER] },
    ]))
}

fn respect_interest() -> Value {
    json!([{ "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED] }])
}

fn ignore_interest() -> Value {
    json!([{ "topic": TOPIC_A, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [CREATED], "barrier_mode": "ignore" }])
}

fn wide_interest() -> Value {
    json!([{ "topic": TOPIC_WIDE, "subject_tenant": TENANT, "ownership_filter": "entire_tenant", "types": [PLACED] }])
}

/// `TOPIC_WIDE` and its partitions, for a member a test joins through the
/// coordinator.
fn wide_topic() -> TopicInterest {
    TopicInterest {
        id: GtsInstanceId::try_new(TOPIC_WIDE).unwrap(),
        partitions: 4,
    }
}

/// A subscription the caller joined and seeked, with the PDP list
/// cleared, so it holds what the open under test asks.
struct Joined {
    harness: EventBrokerHarness,
    enforcer: PolicyEnforcer,
    pdp: Recorded,
    hook: SpecQueryHook,
    group: GtsInstanceId,
    id: Uuid,
}

async fn joined(
    interests: Value,
    answer: impl Fn(Call) -> Answer + Send + Sync + 'static,
) -> Joined {
    joined_seeking(
        interests,
        answer,
        json!({ (TOPIC_A): [{ "partition": 0, "value": "earliest" }] }),
    )
    .await
}

/// The caller joined alone on `TOPIC_WIDE`, at v1, every partition seeked.
async fn joined_wide() -> Joined {
    let earliest = (0..4)
        .map(|partition| json!({ "partition": partition, "value": "earliest" }))
        .collect::<Vec<_>>();
    joined_seeking(
        wide_interest(),
        |_| Answer::Allow,
        json!({ (TOPIC_WIDE): earliest }),
    )
    .await
}

/// [`joined`], seeking `positions` at v1, the JOIN's topology version in a
/// new group.
async fn joined_seeking(
    interests: Value,
    answer: impl Fn(Call) -> Answer + Send + Sync + 'static,
    positions: Value,
) -> Joined {
    let pdp = Recorded::default();
    let enforcer = PolicyEnforcer::new(Arc::new(ScriptedPdp {
        recorded: Arc::clone(&pdp),
        answer,
    }));
    let hook = SpecQueryHook::new();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .with_policy_enforcer(enforcer.clone())
        .with_security_context(caller())
        .with_spec_query_hook(hook.clone())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let joined = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group,
            "client_agent": "stream-open-authz-test",
            "interests": interests,
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
            "positions": positions,
        })))
        .send()
        .await
        .assert_status(200);
    pdp.lock().unwrap().clear();
    Joined {
        harness,
        enforcer,
        pdp,
        hook,
        group: GtsInstanceId::try_new(&group).unwrap(),
        id: Uuid::parse_str(&id).unwrap(),
    }
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

fn sent(requests: &[EvaluationRequest]) -> Vec<Value> {
    requests
        .iter()
        .map(|request| serde_json::to_value(request).unwrap())
        .collect()
}

/// G: the tenant gate's request, as `tenant_authorized` builds it for the
/// subscription's tenant, the caller's own: the gate names that tenant as its
/// root, in `root_only` mode with barriers respected.
fn gate_request(enforcer: &PolicyEnforcer, ctx: &SecurityContext) -> EvaluationRequest {
    enforcer.build_request_with(
        ctx,
        &ResourceType::from_static(REQUEST_RESOURCE, &[pep_properties::OWNER_TENANT_ID]),
        "consume",
        None,
        false,
        &AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, CALLER_TENANT)
            .context_tenant_id(CALLER_TENANT)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(PdpBarrierMode::Respect)
            .require_constraints(false),
    )
}

/// The group check `ctx` sends for the caller's anonymous `group`: `consume` on
/// the group family resource, naming the group and its tenant, the caller's,
/// which is also the root of a `root_only` context with barriers respected.
fn group_check(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    group: &GtsInstanceId,
) -> EvaluationRequest {
    enforcer.build_request_with(
        ctx,
        &ResourceType::from_static(CONSUMER_GROUP_RESOURCE, &[pep_properties::OWNER_TENANT_ID]),
        "consume",
        None,
        false,
        &AccessRequest::new()
            .resource_property("consumer_group_id", group.as_ref())
            .resource_property(pep_properties::OWNER_TENANT_ID, CALLER_TENANT)
            .context_tenant_id(CALLER_TENANT)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(PdpBarrierMode::Respect)
            .require_constraints(false),
    )
}

/// The I request of the `ignore` interest on `TOPIC_A`.
fn i_request(enforcer: &PolicyEnforcer, ctx: &SecurityContext) -> EvaluationRequest {
    enforcer.build_request_with(
        ctx,
        &ResourceType::from_static(TOPIC_A, &[pep_properties::OWNER_TENANT_ID]),
        "consume_ignore_barrier",
        None,
        false,
        &AccessRequest::new()
            .context_tenant_id(TENANT)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(PdpBarrierMode::Ignore)
            .resource_property("topic_id", TOPIC_A)
            .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
            .require_constraints(false),
    )
}

/// The E request of the interest on `TOPIC_A` for `CREATED`.
fn e_request(
    enforcer: &PolicyEnforcer,
    ctx: &SecurityContext,
    barrier_mode: PdpBarrierMode,
) -> EvaluationRequest {
    enforcer.build_request_with(
        ctx,
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

/// Makes the memo stop answering `request`: an existing key is replaced, and
/// an entry that expires at `now` reads as absent.
fn expire(harness: &EventBrokerHarness, request: &EvaluationRequest) {
    let now = Instant::now();
    let stored = harness
        .scope_memo()
        .insert(key(request), MemoResult::Decision(false), now, now);
    assert_eq!(stored, MemoInsert::Stored);
}

/// When the memo's live answer to `request` stops serving.
fn memo_expiry(harness: &EventBrokerHarness, request: &EvaluationRequest) -> Instant {
    let Some((_, expires_at)) = harness
        .scope_memo()
        .get_entry(&key(request), Instant::now())
    else {
        panic!("the memo holds no live answer to the request");
    };
    expires_at
}

/// The scope an allowed E compiles to: the interest tenant only.
fn kept_scope() -> Arc<CompiledScope> {
    Arc::new(
        CompiledScope::compile(
            &AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
                pep_properties::OWNER_TENANT_ID,
                vec![TENANT],
            )])),
            &TenantMode::RootOnly,
            &TenantSetInterner::new(),
        )
        .unwrap(),
    )
}

fn group_key(barrier_mode: BarrierMode) -> GroupKey {
    GroupKey {
        topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
        tenant: TENANT,
        mode: TenantMode::RootOnly,
        barrier_mode,
    }
}

fn created_only() -> Arc<HashSet<GtsTypeId>> {
    Arc::new(HashSet::from([GtsTypeId::try_new(CREATED).unwrap()]))
}

async fn open(harness: &EventBrokerHarness, ctx: &SecurityContext, id: Uuid) -> FrameStream {
    match tokio::time::timeout(BOUND, harness.delivery().stream(ctx, id))
        .await
        .expect("the open answers within the bound")
    {
        Ok(stream) => stream,
        Err(err) => panic!("the stream open is refused: {err:?}"),
    }
}

/// The first `count` frames `stream` hands over, fewer when it ends first,
/// as their wire bodies. One deadline for all of them: heartbeats keep a
/// per-frame bound from ever firing.
async fn first_frames(stream: &mut FrameStream, count: usize) -> Vec<Value> {
    tokio::time::timeout(BOUND, async {
        let mut frames = Vec::with_capacity(count);
        while frames.len() < count {
            let Some(frame) = stream.next().await else {
                break;
            };
            frames.push(serde_json::to_value(FrameDto::from(frame)).unwrap());
        }
        frames
    })
    .await
    .expect("the frames arrive within the bound")
}

/// The wire position of `TOPIC_WIDE`'s `partition`, seeked to the start of
/// an empty partition.
fn at_start(partition: i32) -> Value {
    json!({ "topic": TOPIC_WIDE, "partition": partition, "offset": 0, "last_examined": 0 })
}

/// An open over REST that is refused. An open that succeeds streams without
/// end, so the response is bounded and a success fails here.
async fn refused_over_rest(harness: &EventBrokerHarness, id: Uuid) -> TestResponse {
    tokio::time::timeout(
        BOUND,
        harness.api_v1().get_events_stream(&id.to_string()).send(),
    )
    .await
    .expect("the open is refused within the bound")
}

fn permission_denied(reason: &str, resource_type: &str) -> Value {
    json!({
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
        "title": "Permission Denied",
        "status": 403,
        "detail": "You do not have permission to perform this operation",
        "instance": "/event-broker/v1/events:stream",
        "context": {
            "reason": reason,
            "resource_type": resource_type,
        },
    })
}

fn internal_error() -> Value {
    json!({
        "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
        "title": "Internal",
        "status": 500,
        "detail": "An internal error occurred. Please retry later.",
        "instance": "/event-broker/v1/events:stream",
        "context": {},
    })
}

// -- guard --

#[tokio::test]
async fn a_stream_open_by_a_caller_with_no_identity_is_refused_before_any_pdp_call_over_rest() {
    let pdp = Recorded::default();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(ScriptedPdp {
            recorded: Arc::clone(&pdp),
            answer: |_| Answer::Allow,
        })))
        .with_security_context(
            SecurityContext::builder()
                .subject_id(Uuid::nil())
                .subject_tenant_id(Uuid::nil())
                .build()
                .unwrap(),
        )
        .build()
        .await;
    // No such subscription: the guard answers before the lookup, so the
    // caller learns nothing about which subscriptions exist.
    let id = Uuid::from_u128(0x2301);

    let resp = refused_over_rest(&harness, id).await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        permission_denied(
            "the caller has no identity: its subject or tenant is the nil UUID",
            REQUEST_RESOURCE,
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_eq!(harness.leases().held_count(), 0);
}

#[tokio::test]
async fn a_stream_open_by_a_caller_with_no_identity_is_refused_before_any_pdp_call_in_process() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let nil_caller = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .bearer_token("a-token-the-broker-does-not-read".to_owned())
        .build()
        .unwrap();

    let result = joined
        .harness
        .broker()
        .stream(&nil_caller, SubscriptionId(joined.id))
        .await;

    let Err(EventBrokerError::Unauthorized { detail }) = result else {
        panic!("expected Unauthorized");
    };
    assert_eq!(
        detail,
        "You do not have permission to perform this operation"
    );
    assert_eq!(recorded(&joined.pdp), Vec::<Value>::new());
    assert_eq!(joined.harness.leases().held_count(), 0);
    assert_eq!(joined.harness.groups().scope_of(joined.id), None);

    let _stream = open(&joined.harness, &caller(), joined.id).await;
    assert_eq!(joined.harness.leases().held_count(), 1);
}

// -- scope at open --

#[tokio::test]
async fn an_open_right_after_join_uses_the_memo_and_stores_the_scope() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let e_request = e_request(&joined.enforcer, &caller(), PdpBarrierMode::Respect);
    let remembered_until = memo_expiry(&joined.harness, &e_request);

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        recorded(&joined.pdp),
        sent(&[gate_request(&joined.enforcer, &caller())])
    );
    assert_eq!(
        joined.harness.groups().scope_of(joined.id).as_deref(),
        Some(&CachedScope {
            generation: ScopeGeneration::nth(1),
            principal: ScopePrincipal::from(&caller()),
            groups: Arc::from([AuthorizedGroup {
                key: group_key(BarrierMode::Respect),
                scope: kept_scope(),
                candidates: created_only(),
                candidates_stale: false,
                expires_at: remembered_until,
            }]),
        })
    );
}

#[tokio::test]
async fn an_open_after_the_memo_expired_asks_the_pdp_again() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let e_request = e_request(&joined.enforcer, &caller(), PdpBarrierMode::Respect);
    expire(&joined.harness, &e_request);
    let before = Instant::now();

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        recorded(&joined.pdp),
        sent(&[gate_request(&joined.enforcer, &caller()), e_request.clone()])
    );
    let Some(stored) = joined.harness.groups().scope_of(joined.id) else {
        panic!("the open stores no scope");
    };
    let [group] = &*stored.groups else {
        panic!("expected one group, got {:?}", stored.groups);
    };
    // One group of one: it expires one TTL after the evaluation started, no
    // later than the answer E wrote to the memo.
    assert!(before + TTL <= group.expires_at);
    assert!(group.expires_at <= memo_expiry(&joined.harness, &e_request));
    assert_eq!(
        *stored,
        CachedScope {
            generation: ScopeGeneration::nth(1),
            principal: ScopePrincipal::from(&caller()),
            groups: Arc::from([AuthorizedGroup {
                key: group_key(BarrierMode::Respect),
                scope: kept_scope(),
                candidates: created_only(),
                candidates_stale: false,
                expires_at: group.expires_at,
            }]),
        }
    );
}

#[tokio::test]
async fn a_second_open_with_a_live_member_scope_asks_nothing_beyond_the_tenant_gate() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    drop(open(&joined.harness, &caller(), joined.id).await);
    let Some(first) = joined.harness.groups().scope_of(joined.id) else {
        panic!("the first open stores no scope");
    };
    joined.pdp.lock().unwrap().clear();

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        recorded(&joined.pdp),
        sent(&[gate_request(&joined.enforcer, &caller())])
    );
    // The JOIN's and the first open's: the second open runs none.
    assert_eq!(joined.hook.calls(SpecQuery::EventTypeCandidates), 2);
    let Some(second) = joined.harness.groups().scope_of(joined.id) else {
        panic!("the second open leaves no scope");
    };
    assert!(Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn a_second_open_after_a_group_expired_evaluates_again() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    drop(open(&joined.harness, &caller(), joined.id).await);
    let Some(first) = joined.harness.groups().scope_of(joined.id) else {
        panic!("the first open stores no scope");
    };
    // The same scope with its one group expired.
    let expired_at = Instant::now();
    let expired = Arc::from([AuthorizedGroup {
        expires_at: expired_at,
        ..first.groups[0].clone()
    }]);
    assert_eq!(
        joined.harness.groups().write_scope(
            joined.id,
            ScopeWrite::Store {
                expected: Expected::Generation(first.generation),
                content: ScopeContent {
                    principal: ScopePrincipal::from(&caller()),
                    groups: expired,
                },
            },
        ),
        ScopeWriteOutcome::Applied
    );
    let remembered_until = memo_expiry(
        &joined.harness,
        &e_request(&joined.enforcer, &caller(), PdpBarrierMode::Respect),
    );

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    // The JOIN's, the first open's and this one's.
    assert_eq!(joined.hook.calls(SpecQuery::EventTypeCandidates), 3);
    assert_eq!(
        joined.harness.groups().scope_of(joined.id).as_deref(),
        Some(&CachedScope {
            generation: ScopeGeneration::nth(3),
            principal: ScopePrincipal::from(&caller()),
            groups: Arc::from([AuthorizedGroup {
                key: group_key(BarrierMode::Respect),
                scope: kept_scope(),
                candidates: created_only(),
                candidates_stale: false,
                expires_at: remembered_until,
            }]),
        })
    );
}

#[tokio::test]
async fn an_open_by_another_principal_does_not_reuse_the_member_scope() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    drop(open(&joined.harness, &caller(), joined.id).await);
    joined.pdp.lock().unwrap().clear();
    let scoped = caller_with_scopes();
    let e_request = e_request(&joined.enforcer, &scoped, PdpBarrierMode::Respect);
    let before = Instant::now();

    let _stream = open(&joined.harness, &scoped, joined.id).await;

    // The memo key holds the token scopes, so the group check and E are asked
    // again.
    assert_eq!(
        recorded(&joined.pdp),
        sent(&[
            gate_request(&joined.enforcer, &scoped),
            group_check(&joined.enforcer, &scoped, &joined.group),
            e_request.clone(),
        ])
    );
    let Some(stored) = joined.harness.groups().scope_of(joined.id) else {
        panic!("the open stores no scope");
    };
    let [group] = &*stored.groups else {
        panic!("expected one group, got {:?}", stored.groups);
    };
    assert!(before + TTL <= group.expires_at);
    assert!(group.expires_at <= memo_expiry(&joined.harness, &e_request));
    assert_eq!(
        *stored,
        CachedScope {
            generation: ScopeGeneration::nth(2),
            principal: ScopePrincipal::from(&scoped),
            groups: Arc::from([AuthorizedGroup {
                key: group_key(BarrierMode::Respect),
                scope: kept_scope(),
                candidates: created_only(),
                candidates_stale: false,
                expires_at: group.expires_at,
            }]),
        }
    );
}

#[tokio::test]
async fn an_ignore_group_skips_i_on_a_memo_hit() {
    let joined = joined(ignore_interest(), |_| Answer::Allow).await;
    let remembered_until = memo_expiry(&joined.harness, &i_request(&joined.enforcer, &caller()))
        .min(memo_expiry(
            &joined.harness,
            &e_request(&joined.enforcer, &caller(), PdpBarrierMode::Ignore),
        ));

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        recorded(&joined.pdp),
        sent(&[gate_request(&joined.enforcer, &caller())])
    );
    assert_eq!(
        joined.harness.groups().scope_of(joined.id).as_deref(),
        Some(&CachedScope {
            generation: ScopeGeneration::nth(1),
            principal: ScopePrincipal::from(&caller()),
            groups: Arc::from([AuthorizedGroup {
                key: group_key(BarrierMode::Ignore),
                scope: kept_scope(),
                candidates: created_only(),
                candidates_stale: false,
                expires_at: remembered_until,
            }]),
        })
    );
}

#[tokio::test]
async fn an_ignore_group_sends_i_on_a_miss() {
    let joined = joined(ignore_interest(), |_| Answer::Allow).await;
    let i_request = i_request(&joined.enforcer, &caller());
    expire(&joined.harness, &i_request);
    let remembered_until = memo_expiry(
        &joined.harness,
        &e_request(&joined.enforcer, &caller(), PdpBarrierMode::Ignore),
    );

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        recorded(&joined.pdp),
        sent(&[gate_request(&joined.enforcer, &caller()), i_request])
    );
    assert_eq!(
        joined.harness.groups().scope_of(joined.id).as_deref(),
        Some(&CachedScope {
            generation: ScopeGeneration::nth(1),
            principal: ScopePrincipal::from(&caller()),
            groups: Arc::from([AuthorizedGroup {
                key: group_key(BarrierMode::Ignore),
                scope: kept_scope(),
                candidates: created_only(),
                candidates_stale: false,
                expires_at: remembered_until,
            }]),
        })
    );
}

/// JOINs under an allowing PDP, then denies E and answers R as `r_allowed`
/// says, and makes the memo forget the JOIN's E.
async fn joined_then_e_denied(r_allowed: bool) -> (Joined, Arc<AtomicBool>, EvaluationRequest) {
    let deny = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&deny);
    let joined = joined(respect_interest(), move |call| {
        match (switch.load(Ordering::SeqCst), call) {
            (true, Call::E) => Answer::Deny(Some("no consume grant on this topic")),
            (true, Call::R) if !r_allowed => Answer::Deny(None),
            _ => Answer::Allow,
        }
    })
    .await;
    deny.store(true, Ordering::SeqCst);
    let e_request = e_request(&joined.enforcer, &caller(), PdpBarrierMode::Respect);
    expire(&joined.harness, &e_request);
    (joined, deny, e_request)
}

/// The R request of the interest on `TOPIC_A`.
fn r_request(enforcer: &PolicyEnforcer) -> EvaluationRequest {
    enforcer.build_request_with(
        &caller(),
        &ResourceType::from_static(REQUEST_RESOURCE, &[pep_properties::OWNER_TENANT_ID]),
        "consume",
        None,
        true,
        &AccessRequest::new()
            .context_tenant_id(TENANT)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(PdpBarrierMode::Respect)
            .resource_property(pep_properties::OWNER_TENANT_ID, TENANT)
            .require_constraints(true),
    )
}

#[tokio::test]
async fn a_denied_scope_at_open_answers_403_and_releases_the_lease() {
    let (joined, deny, e_request) = joined_then_e_denied(false).await;

    let resp = refused_over_rest(&joined.harness, joined.id).await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        permission_denied("access denied by policy", REQUEST_RESOURCE)
    );
    assert_eq!(
        recorded(&joined.pdp),
        sent(&[
            gate_request(&joined.enforcer, &caller()),
            e_request.clone(),
            r_request(&joined.enforcer),
        ])
    );
    assert_eq!(joined.harness.leases().held_count(), 0);
    assert_eq!(joined.harness.groups().scope_of(joined.id), None);
    assert_eq!(
        joined
            .harness
            .scope_memo()
            .get(&key(&e_request), Instant::now()),
        None,
        "a refusal writes nothing"
    );

    deny.store(false, Ordering::SeqCst);
    let _stream = open(&joined.harness, &caller(), joined.id).await;
    assert_eq!(joined.harness.leases().held_count(), 1);
}

#[tokio::test]
async fn a_denied_scope_with_a_reachable_tenant_at_open_answers_topic_not_authorized() {
    let (joined, deny, e_request) = joined_then_e_denied(true).await;

    let resp = refused_over_rest(&joined.harness, joined.id).await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        permission_denied(
            "no consume grant on this topic",
            "gts.cf.core.events.topic.v1~"
        )
    );
    assert_eq!(
        recorded(&joined.pdp),
        sent(&[
            gate_request(&joined.enforcer, &caller()),
            e_request.clone(),
            r_request(&joined.enforcer),
        ])
    );
    assert_eq!(joined.harness.leases().held_count(), 0);
    assert_eq!(joined.harness.groups().scope_of(joined.id), None);
    assert_eq!(
        joined
            .harness
            .scope_memo()
            .get(&key(&e_request), Instant::now()),
        None,
        "a refusal writes nothing"
    );

    deny.store(false, Ordering::SeqCst);
    let _stream = open(&joined.harness, &caller(), joined.id).await;
    assert_eq!(joined.harness.leases().held_count(), 1);
}

#[tokio::test]
async fn an_unavailable_pdp_at_open_answers_500_and_releases_the_lease() {
    let fail = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&fail);
    let joined = joined(respect_interest(), move |call| {
        match (switch.load(Ordering::SeqCst), call) {
            (true, Call::E) => Answer::Fail,
            _ => Answer::Allow,
        }
    })
    .await;
    fail.store(true, Ordering::SeqCst);
    let e_request = e_request(&joined.enforcer, &caller(), PdpBarrierMode::Respect);
    expire(&joined.harness, &e_request);

    let resp = refused_over_rest(&joined.harness, joined.id).await;

    resp.assert_status(500);
    assert_eq!(resp.json(), internal_error());
    assert_eq!(
        recorded(&joined.pdp),
        sent(&[gate_request(&joined.enforcer, &caller()), e_request.clone()])
    );
    assert_eq!(joined.harness.leases().held_count(), 0);
    assert_eq!(joined.harness.groups().scope_of(joined.id), None);
    assert_eq!(
        joined
            .harness
            .scope_memo()
            .get(&key(&e_request), Instant::now()),
        None,
        "a failed evaluation writes nothing"
    );

    fail.store(false, Ordering::SeqCst);
    let _stream = open(&joined.harness, &caller(), joined.id).await;
    assert_eq!(joined.harness.leases().held_count(), 1);
}

#[test]
fn staggered_expiry_spreads_the_groups() {
    let now = Instant::now();
    let expiry = ScopeExpiry::new(now, Duration::from_secs(60), 4).unwrap();

    assert_eq!(
        [0, 1, 2, 3].map(|k| expiry.of_group(k, Remembered::No).unwrap()),
        [
            now + Duration::from_secs(60),
            now + Duration::from_secs(75),
            now + Duration::from_secs(90),
            now + Duration::from_secs(105),
        ]
    );
    assert_eq!(
        expiry
            .of_group(1, Remembered::Until(now + Duration::from_secs(10)))
            .unwrap(),
        now + Duration::from_secs(10),
        "a remembered answer never outlives its memo entry"
    );
    assert_eq!(
        expiry
            .of_group(1, Remembered::Until(now + Duration::from_secs(200)))
            .unwrap(),
        now + Duration::from_secs(75),
        "a memo entry never extends the group's own expiry"
    );
}

// -- store races and removal --

#[tokio::test]
async fn a_member_reaped_during_scope_evaluation_answers_404_and_releases_the_lease() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let groups = joined.harness.shared_groups();
    let (group, id) = (joined.group.clone(), joined.id);
    let mut fired = false;
    joined.hook.arm(move |_| {
        if !fired {
            fired = true;
            assert!(groups.leave(&group, id).is_some(), "the member leaves");
        }
        Ok(())
    });

    let resp = refused_over_rest(&joined.harness, joined.id).await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{}' is no longer a group member", joined.id),
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "resource_name": joined.id.to_string(),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
    assert_eq!(joined.harness.leases().held_count(), 0);
    // The JOIN's and the open's one pass: a member that is gone is not
    // evaluated again.
    assert_eq!(joined.hook.calls(SpecQuery::EventTypeCandidates), 2);
}

#[tokio::test]
async fn a_superseded_store_retries_once() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let groups = joined.harness.shared_groups();
    let id = joined.id;
    let mut fired = false;
    joined.hook.arm(move |_| {
        if !fired {
            fired = true;
            assert_eq!(
                groups.write_scope(
                    id,
                    ScopeWrite::Store {
                        expected: Expected::Absent,
                        content: ScopeContent {
                            principal: another_principal(),
                            groups: Arc::from([]),
                        },
                    },
                ),
                ScopeWriteOutcome::Applied
            );
        }
        Ok(())
    });
    let remembered_until = memo_expiry(
        &joined.harness,
        &e_request(&joined.enforcer, &caller(), PdpBarrierMode::Respect),
    );

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        joined.harness.groups().scope_of(joined.id).as_deref(),
        Some(&CachedScope {
            generation: ScopeGeneration::nth(2),
            principal: ScopePrincipal::from(&caller()),
            groups: Arc::from([AuthorizedGroup {
                key: group_key(BarrierMode::Respect),
                scope: kept_scope(),
                candidates: created_only(),
                candidates_stale: false,
                expires_at: remembered_until,
            }]),
        })
    );
    // The JOIN's, and the open's two passes.
    assert_eq!(joined.hook.calls(SpecQuery::EventTypeCandidates), 3);
}

#[tokio::test]
async fn a_superseded_store_reuses_a_live_scope_of_the_same_principal() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let groups = joined.harness.shared_groups();
    let id = joined.id;
    let live = Arc::<[AuthorizedGroup]>::from([AuthorizedGroup {
        key: group_key(BarrierMode::Respect),
        scope: kept_scope(),
        candidates: created_only(),
        candidates_stale: false,
        expires_at: Instant::now() + Duration::from_secs(3_600),
    }]);
    let stored = Arc::clone(&live);
    let mut fired = false;
    joined.hook.arm(move |_| {
        if !fired {
            fired = true;
            assert_eq!(
                groups.write_scope(
                    id,
                    ScopeWrite::Store {
                        expected: Expected::Absent,
                        content: ScopeContent {
                            principal: ScopePrincipal::from(&caller()),
                            groups: Arc::clone(&stored),
                        },
                    },
                ),
                ScopeWriteOutcome::Applied
            );
        }
        Ok(())
    });

    let _stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        joined.harness.groups().scope_of(joined.id).as_deref(),
        Some(&CachedScope {
            generation: ScopeGeneration::nth(1),
            principal: ScopePrincipal::from(&caller()),
            groups: live,
        })
    );
    // The JOIN's, and the open's one pass.
    assert_eq!(joined.hook.calls(SpecQuery::EventTypeCandidates), 2);
}

#[tokio::test]
async fn a_second_supersession_answers_500() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let groups = joined.harness.shared_groups();
    let id = joined.id;
    joined.hook.arm(move |_| {
        let expected = groups.scope_of(id).map_or(Expected::Absent, |held| {
            Expected::Generation(held.generation)
        });
        assert_eq!(
            groups.write_scope(
                id,
                ScopeWrite::Store {
                    expected,
                    content: ScopeContent {
                        principal: another_principal(),
                        groups: Arc::from([]),
                    },
                },
            ),
            ScopeWriteOutcome::Applied
        );
        Ok(())
    });

    let resp = refused_over_rest(&joined.harness, joined.id).await;

    resp.assert_status(500);
    assert_eq!(resp.json(), internal_error());
    assert_eq!(joined.harness.leases().held_count(), 0);
    // The hook stored once per pass and the open stored nothing: its second
    // store was superseded too.
    assert_eq!(
        joined.harness.groups().scope_of(joined.id).as_deref(),
        Some(&CachedScope {
            generation: ScopeGeneration::nth(2),
            principal: another_principal(),
            groups: Arc::from([]),
        })
    );
    // The JOIN's, and the open's two passes: one retry, no more.
    assert_eq!(joined.hook.calls(SpecQuery::EventTypeCandidates), 3);
}

#[tokio::test]
async fn a_stream_open_after_reap_answers_404() {
    let joined = joined(respect_interest(), |_| Answer::Allow).await;
    let joined_by = Instant::now();
    let reaped = joined.harness.groups().sweep(joined_by + JOIN_TIMEOUT);
    assert_eq!(reaped.len(), 1, "the JOINed member is reaped");

    let resp = refused_over_rest(&joined.harness, joined.id).await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": format!("subscription '{}' does not exist", joined.id),
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "resource_name": joined.id.to_string(),
                "resource_type": "gts.cf.core.events.subscription.v1~",
            },
        })
    );
    assert_eq!(recorded(&joined.pdp), Vec::<Value>::new());
    assert_eq!(joined.harness.leases().held_count(), 0);
}

// -- a rebalance while the stream opens --
//
// The coordinator splits a topic's partitions in ranges over its members
// sorted by subscription id (`range_split`): of two members, the lower id
// takes partitions 0 and 1, the higher 2 and 3. A member a test joins itself
// takes the highest id, or the lowest, so the split is known whatever id the
// REST JOIN drew.

/// A peer joins while the member's stream opens, after the open read the
/// subscription: the stream opens on the four partitions it read, then
/// reports the two it lost at once.
#[tokio::test]
async fn a_member_that_loses_partitions_while_its_stream_opens_gets_the_loss_frame() {
    let joined = joined_wide().await;
    let groups = joined.harness.shared_groups();
    let group = joined.group.clone();
    let mut fired = false;
    joined.hook.arm(move |_| {
        if !fired {
            fired = true;
            groups.join_member(&group, Uuid::max(), &[wide_topic()], SESSION_TIMEOUT);
        }
        Ok(())
    });

    let mut stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        first_frames(&mut stream, 2).await,
        vec![
            json!({
                "kind": "topology",
                "topology_version": 1,
                "assigned": [at_start(0), at_start(1), at_start(2), at_start(3)],
            }),
            json!({
                "kind": "topology",
                "topology_version": 2,
                "assigned": [at_start(0), at_start(1)],
            }),
        ]
    );
}

/// The peer leaves while the member's stream opens, after the open read the
/// subscription: the stream opens on the half it read, then ends with a
/// rebalance, since it holds no cursor for the half it gained.
#[tokio::test]
async fn a_member_that_gains_partitions_while_its_stream_opens_ends_with_a_rebalance() {
    let joined = joined_wide().await;
    let groups = joined.harness.shared_groups();
    let (group, peer) = (joined.group.clone(), Uuid::max());
    groups.join_member(&group, peer, &[wide_topic()], SESSION_TIMEOUT);
    let mut fired = false;
    joined.hook.arm(move |_| {
        if !fired {
            fired = true;
            assert!(groups.leave(&group, peer).is_some(), "the peer leaves");
        }
        Ok(())
    });

    let mut stream = open(&joined.harness, &caller(), joined.id).await;

    assert_eq!(
        first_frames(&mut stream, 3).await,
        vec![
            json!({
                "kind": "topology",
                "topology_version": 2,
                "assigned": [at_start(0), at_start(1)],
            }),
            json!({
                "kind": "control",
                "code": "terminal",
                "positions": [at_start(0), at_start(1)],
                "reason": "rebalanced",
            }),
        ]
    );
}

/// The peer leaves while the member's stream opens, so the member's watch
/// holds every partition while its read set holds the half it read; a new
/// member then joins with the lowest id and takes that half. Classified
/// against the watch, that move is a loss that keeps only partitions the
/// read set never held, which leaves the stream open on no partition;
/// classified against what the stream read, it is a gain.
#[tokio::test]
async fn a_member_whose_peer_leaves_while_its_stream_opens_ends_with_a_rebalance_when_another_joins()
 {
    let joined = joined_wide().await;
    let groups = joined.harness.shared_groups();
    let (group, peer) = (joined.group.clone(), Uuid::max());
    groups.join_member(&group, peer, &[wide_topic()], SESSION_TIMEOUT);
    let leaving = Arc::clone(&groups);
    let left = group.clone();
    let mut fired = false;
    joined.hook.arm(move |_| {
        if !fired {
            fired = true;
            assert!(leaving.leave(&left, peer).is_some(), "the peer leaves");
        }
        Ok(())
    });

    let mut stream = open(&joined.harness, &caller(), joined.id).await;
    let opening = first_frames(&mut stream, 1).await;
    groups.join_member(&group, Uuid::nil(), &[wide_topic()], SESSION_TIMEOUT);

    assert_eq!(
        opening,
        vec![json!({
            "kind": "topology",
            "topology_version": 2,
            "assigned": [at_start(0), at_start(1)],
        })]
    );
    assert_eq!(
        first_frames(&mut stream, 2).await,
        vec![json!({
            "kind": "control",
            "code": "terminal",
            "positions": [at_start(0), at_start(1)],
            "reason": "rebalanced",
        })]
    );
}
