//! What a stream's consume scope refresh asks the PDP and stores on the member.
//!
//! Each pass test JOINs a subscription under a scripted PDP that records every
//! request, stores a member scope whose groups expire where the test needs
//! them, clears the recorded list, and runs single passes of a refresh action
//! whose clock is pinned, so a group is due, a retry comes or an expiry passes
//! without waiting. It asserts the full list of requests a pass sends, the full
//! scope it stores and the directive it returns. The caller sits in tenant
//! `0x123`; the interests name `0x234` and its siblings.
//!
//! The stream tests open a stream through the harness, whose open spawns the
//! production worker, and run a test-built action beside it where they need
//! a pass at a chosen time.
//!
//! The timed tests run on the paused clock with the action's own clock, a
//! bare coordinator and candidates from memory, and a PDP that answers late
//! or never: a pass then takes the time its PDP calls take, and the instants
//! it asks and clears at are exact.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AccessRequest, AuthZResolverApi, BarrierMode as PdpBarrierMode, Constraint, DenyReason,
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InPredicate, PolicyEnforcer,
    Predicate, ResourceType, TenantMode,
};
use event_broker_sdk::models::EventType;
use gts::{GtsIdPattern, GtsTypeId};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{
    AccessScope, PlatformSecurityContext, ScopeConstraint, ScopeFilter, SecurityContext,
    pep_properties,
};
use toolkit_taskward::{Directive, WorkerAction};
use uuid::Uuid;

use super::{REFRESH_CONCURRENCY, Retry, ScopeRefresh, Step, TurnEnd};
use crate::config::StreamingConfig;
use crate::domain::backend::BackendResolver;
use crate::domain::consumer_group_coordinator::member_scope::{
    AuthorizedGroup, CachedScope, Expected, ScopeContent, ScopeGeneration, ScopePrincipal,
    ScopeWrite, ScopeWriteOutcome,
};
use crate::domain::consumer_group_coordinator::{ConsumerGroupCoordinator, TopicInterest};
use crate::domain::delivery::ConsumeEvaluator;
use crate::domain::error::DomainError;
use crate::domain::join_scope::{ConsumeMemo, GroupKey};
use crate::domain::model::{BarrierMode, Interest, Joiner, OwnershipFilter, Subscription, Topic};
use crate::domain::specification::{
    EventTypeCandidate, EventTypeCandidates, ProvisioningLimits, SpecificationManager, TopicState,
};
use crate::domain::streaming::frames::{CloseReason, ControlCode, Frame};
use crate::domain::streaming::scope::{CompiledScope, TenantSetInterner};
use crate::domain::streaming::scope_memo::{MemoInsert, MemoKey, MemoResult, ScopeMemo};
use crate::domain::streaming::session::FrameStream;
use crate::test_support::{
    EventBrokerHarness, Json, SpecQuery, SpecQueryHook, StaticTypesRegistry,
};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const GROUP: &str = "gts.cf.core.events.consumer_group.v1~02943530-10da-4624-a3ae-b998c425847f";
const TOPIC_A: &str = "gts.cf.core.events.topic.v1~x.eb.refresh.a.v1";
const CREATED: &str = "gts.cf.core.events.event.v1~x.eb.refresh.created.v1~";
const ORDER: &str = "gts.x.eb.refresh.order.v1~";

const SUBJECT: Uuid = Uuid::from_u128(0x456);
const CALLER_TENANT: Uuid = Uuid::from_u128(0x123);
const TENANT: Uuid = Uuid::from_u128(0x234);
const TENANT_B: Uuid = Uuid::from_u128(0x235);
const TENANT_C: Uuid = Uuid::from_u128(0x236);

/// The harness's `streaming.authz_scope_ttl_secs`, the production default.
const TTL: Duration = Duration::from_secs(60);

/// The harness's `streaming.authz_eval_deadline_secs`, the production
/// default, as the timed tests' enforcer applies it to each PDP call.
const DEADLINE: Duration = Duration::from_secs(10);

/// The refresh lead of a `respect` group at the default 10 s deadline.
const LEAD: Duration = Duration::from_secs(12);

/// The refresh lead of an `ignore` group at the default 10 s deadline.
const IGNORE_LEAD: Duration = Duration::from_secs(22);

/// Bounds every await from a pass, an open, a frame or a gate until it
/// resolves: one that hangs fails the test instead.
const BOUND: Duration = Duration::from_secs(10);

/// When a `respect` group renewed for a full window is next due: `TTL`
/// less `LEAD`.
const RENEWED_DUE_IN: Duration = Duration::from_secs(48);

const MS: Duration = Duration::from_millis(1);

/// The most passes a timed test runs before it fails: a worker that never
/// idles would otherwise run without end.
const MAX_PASSES: usize = 32;

type Recorded = Arc<Mutex<Vec<Value>>>;

/// When each recorded request reached the PDP.
type AskedAt = Arc<Mutex<Vec<Instant>>>;

/// Which call a request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Define,
    Produce,
    Gate,
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
            ("produce" | "produce_ignore_barrier", _) => Self::Produce,
            ("consume_ignore_barrier", _) => Self::I,
            ("consume", REQUEST_RESOURCE) if !request.context.require_constraints => Self::Gate,
            ("consume", REQUEST_RESOURCE) => Self::R,
            ("consume", _) => Self::E,
            (action, resource) => panic!("unexpected PDP call {action} on {resource}"),
        }
    }
}

/// What the scripted PDP answers to one call.
#[derive(Debug, Clone, Copy)]
enum Answer {
    /// Allowed, with `In(owner_tenant_id, [root_id])` when the request names a
    /// root and declares the property, as tr-authz answers.
    Allow,
    /// Allowed as `Allow` is, after the wait, as a slow but healthy PDP
    /// answers.
    AllowAfter(Duration),
    /// Allowed with a constraint on `subject`, which no consume resource
    /// declares, so the enforcer cannot compile it.
    Uncompilable,
    /// Allowed with a constraint whose event type is not a type id, so the
    /// consume scope keeps nothing it can enforce.
    Unenforceable,
    /// Denied.
    Deny,
    /// Allowed with a scope that matches nothing, `In(owner_tenant_id, [])`,
    /// which the enforcer refuses as a denial.
    AllowNothing,
    /// The evaluation itself fails, as an unreachable PDP does.
    Fail,
    /// The evaluation fails after the wait, as a slow, failing PDP does.
    FailAfter(Duration),
    /// No answer ever comes; the enforcer's deadline ends the call.
    Hang,
}

type Script = Box<dyn Fn(Call, Option<Uuid>) -> Answer + Send>;

/// Holds the calls `holds` names while armed, so a test can act while a
/// refresh waits on one.
struct Gate {
    armed: AtomicBool,
    /// E unless a test sets another call.
    holds: Mutex<Call>,
    entered: Notify,
    released: Notify,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            armed: AtomicBool::new(false),
            holds: Mutex::new(Call::E),
            entered: Notify::new(),
            released: Notify::new(),
        }
    }
}

/// Records every request, and when it came, and answers it by the current
/// script, which gets the call and the request's tenant-context root.
struct ScriptedPdp {
    recorded: Recorded,
    asked_at: AskedAt,
    script: Arc<Mutex<Script>>,
    gate: Arc<Gate>,
}

#[async_trait]
impl AuthZResolverApi for ScriptedPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.recorded
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        self.asked_at.lock().unwrap().push(Instant::now());
        let call = Call::of(&request);
        let root = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tenant_context| tenant_context.root_id);
        let held = *self.gate.holds.lock().unwrap();
        if call == held && self.gate.armed.load(Ordering::SeqCst) {
            self.gate.entered.notify_one();
            self.gate.released.notified().await;
        }
        let answer = (self.script.lock().unwrap())(call, root);
        match answer {
            Answer::Hang => std::future::pending::<()>().await,
            Answer::FailAfter(wait) | Answer::AllowAfter(wait) => tokio::time::sleep(wait).await,
            _ => {}
        }
        let allowed = |predicates: Vec<Predicate>| EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: if predicates.is_empty() {
                    Vec::new()
                } else {
                    vec![Constraint { predicates }]
                },
                ..Default::default()
            },
        };
        Ok(match answer {
            Answer::Allow | Answer::AllowAfter(_) => {
                let declared = request
                    .context
                    .supported_properties
                    .iter()
                    .any(|property| property == pep_properties::OWNER_TENANT_ID);
                match root {
                    Some(root) if declared => allowed(vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [root],
                    ))]),
                    _ => allowed(Vec::new()),
                }
            }
            Answer::Uncompilable => allowed(vec![Predicate::In(InPredicate::new(
                "subject",
                [Uuid::from_u128(0x77)],
            ))]),
            Answer::Unenforceable => allowed(vec![
                Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    [root.unwrap_or(TENANT)],
                )),
                Predicate::In(InPredicate::new("event_type", ["not-a-type-id"])),
            ]),
            Answer::AllowNothing => allowed(vec![Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                Vec::<Uuid>::new(),
            ))]),
            Answer::Deny => EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    deny_reason: Some(DenyReason {
                        error_code: "denied".to_owned(),
                        details: Some("the grant was revoked".to_owned()),
                    }),
                    ..Default::default()
                },
            },
            Answer::Fail | Answer::FailAfter(_) | Answer::Hang => {
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
        { "id": CREATED, "topic": TOPIC_A, "allowed_subject_types": [ORDER] },
    ]))
}

fn respect_interest(tenant: Uuid) -> Value {
    json!({ "topic": TOPIC_A, "subject_tenant": tenant, "ownership_filter": "entire_tenant", "types": [CREATED] })
}

fn ignore_interest(tenant: Uuid) -> Value {
    json!({ "topic": TOPIC_A, "subject_tenant": tenant, "ownership_filter": "entire_tenant", "types": [CREATED], "barrier_mode": "ignore" })
}

/// A subscription the caller joined and seeked, with the PDP list cleared.
struct Joined {
    harness: EventBrokerHarness,
    enforcer: PolicyEnforcer,
    pdp: Recorded,
    script: Arc<Mutex<Script>>,
    gate: Arc<Gate>,
    hook: SpecQueryHook,
    group: GtsInstanceId,
    id: Uuid,
}

/// Joins `interests` with every call allowed. `timing` is the harness's
/// scope TTL and PDP deadline, in seconds.
async fn joined_with(interests: Value, timing: (u32, u32)) -> Joined {
    let pdp = Recorded::default();
    let script: Arc<Mutex<Script>> = Arc::new(Mutex::new(Box::new(|_, _| Answer::Allow)));
    let gate = Arc::new(Gate::default());
    let enforcer = PolicyEnforcer::new(Arc::new(ScriptedPdp {
        recorded: Arc::clone(&pdp),
        asked_at: AskedAt::default(),
        script: Arc::clone(&script),
        gate: Arc::clone(&gate),
    }));
    let hook = SpecQueryHook::new();
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .with_policy_enforcer(enforcer.clone())
        .with_security_context(caller())
        .with_spec_query_hook(hook.clone())
        .with_authz_timing(timing.0, timing.1)
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
            "client_agent": "scope-refresh-test",
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
            "positions": { (TOPIC_A): [{ "partition": 0, "value": "earliest" }] },
        })))
        .send()
        .await
        .assert_status(200);
    pdp.lock().unwrap().clear();
    Joined {
        harness,
        enforcer,
        pdp,
        script,
        gate,
        hook,
        group: GtsInstanceId::try_new(&group).unwrap(),
        id: Uuid::parse_str(&id).unwrap(),
    }
}

async fn joined(interests: Value) -> Joined {
    joined_with(interests, (60, 10)).await
}

impl Joined {
    /// Answers every later call by `script`.
    fn answer(&self, script: impl Fn(Call, Option<Uuid>) -> Answer + Send + 'static) {
        *self.script.lock().unwrap() = Box::new(script);
    }

    fn recorded(&self) -> Vec<Value> {
        self.pdp.lock().unwrap().clone()
    }

    fn clear_recorded(&self) {
        self.pdp.lock().unwrap().clear();
    }

    /// A refresh action for the member as the delivery service builds one,
    /// over the harness's memo and specification manager, with its clock
    /// pinned at `now`. `listed` says whether it starts with each group's
    /// candidate list, as after an open that evaluated them.
    async fn refresh(&self, now: Instant, listed: bool) -> ScopeRefresh {
        let evaluator = Arc::new(ConsumeEvaluator::new(
            self.enforcer.clone(),
            self.harness.spec_manager(),
            ConsumeMemo::new(Arc::clone(self.harness.scope_memo()), TTL),
            Arc::new(TenantSetInterner::new()),
        ));
        let subscription = self.harness.groups().get(self.id).expect("the member");
        let mut lists = Vec::new();
        if listed {
            for _ in &subscription.interests {
                lists.push(Arc::new(
                    evaluator
                        .get_candidates(&GtsInstanceId::try_new(TOPIC_A).unwrap())
                        .await
                        .expect("the candidates"),
                ));
            }
        }
        let mut refresh = ScopeRefresh::new(
            &subscription,
            caller(),
            lists,
            self.harness.shared_groups(),
            evaluator,
            &StreamingConfig::default(),
        )
        .expect("the refresh");
        refresh.pin_clock(now);
        refresh
    }

    /// Stores `groups` as the member's first scope, for the caller.
    fn store(&self, groups: Vec<AuthorizedGroup>) -> Arc<CachedScope> {
        assert_eq!(
            self.harness.groups().write_scope(
                self.id,
                ScopeWrite::Store {
                    expected: Expected::Absent,
                    content: ScopeContent {
                        principal: ScopePrincipal::from(&caller()),
                        groups: groups.into(),
                    },
                },
            ),
            ScopeWriteOutcome::Applied
        );
        self.held()
    }

    fn held(&self) -> Arc<CachedScope> {
        let Some(held) = self.harness.groups().scope_of(self.id) else {
            panic!("the member holds no scope");
        };
        held
    }

    /// Makes the memo stop answering `request`: an existing key is
    /// replaced, and an entry that expires at once reads as absent.
    fn forget(&self, request: &EvaluationRequest) {
        let now = Instant::now();
        self.remember(request, MemoResult::Decision(false), now);
    }

    /// Makes the memo answer `request` with `result` until `until`.
    fn remember(&self, request: &EvaluationRequest, result: MemoResult, until: Instant) {
        let stored = self.harness.scope_memo().insert(
            MemoKey::try_from(request).unwrap(),
            result,
            until,
            Instant::now(),
        );
        assert_eq!(stored, MemoInsert::Stored);
    }

    async fn open(&self) -> FrameStream {
        match tokio::time::timeout(BOUND, self.harness.delivery().stream(&caller(), self.id))
            .await
            .expect("the open answers within the bound")
        {
            Ok(stream) => stream,
            Err(err) => panic!("the stream open is refused: {err:?}"),
        }
    }

    async fn publish(&self, id: Uuid) {
        self.harness
            .api_v1()
            .post_events()
            .with_body(Json(&json!({
                "id": id,
                "type": CREATED,
                "subject_tenant": TENANT,
                "source": "scope-refresh-test",
                "subject": "order-1",
                "subject_type": ORDER,
                "occurred_at": "2026-01-02T03:04:05Z",
            })))
            .send()
            .await
            .assert_status(202);
    }
}

/// One pass of `refresh`, bounded.
async fn pass(refresh: &mut ScopeRefresh) -> Directive {
    pass_with(refresh, &CancellationToken::new()).await
}

/// One pass of `refresh` under the worker's `cancel`, bounded.
async fn pass_with(refresh: &mut ScopeRefresh, cancel: &CancellationToken) -> Directive {
    let Ok(directive) = tokio::time::timeout(BOUND, refresh.execute(cancel))
        .await
        .expect("the pass ends within the bound");
    directive
}

/// The next event frame of `stream`, skipping every other frame. Bounded as a
/// whole: heartbeats keep arriving while no event does.
async fn next_event(stream: &mut FrameStream) -> Uuid {
    tokio::time::timeout(BOUND, async {
        loop {
            match stream.next().await {
                Some(Frame::Event(event)) => return event.id,
                Some(_) => {}
                None => panic!("the stream ended"),
            }
        }
    })
    .await
    .expect("an event arrives within the bound")
}

fn sent(requests: &[EvaluationRequest]) -> Vec<Value> {
    requests
        .iter()
        .map(|request| serde_json::to_value(request).unwrap())
        .collect()
}

/// The I request of an `ignore` interest in `tenant`.
fn i_request(enforcer: &PolicyEnforcer, tenant: Uuid) -> EvaluationRequest {
    enforcer.build_request_with(
        &caller(),
        &ResourceType::from_static(TOPIC_A, &[pep_properties::OWNER_TENANT_ID]),
        "consume_ignore_barrier",
        None,
        false,
        &AccessRequest::new()
            .context_tenant_id(tenant)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(PdpBarrierMode::Ignore)
            .resource_property("topic_id", TOPIC_A)
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant)
            .require_constraints(false),
    )
}

/// The E request of an interest in `tenant` for `CREATED`.
fn e_request(
    enforcer: &PolicyEnforcer,
    tenant: Uuid,
    barrier_mode: PdpBarrierMode,
) -> EvaluationRequest {
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
            .context_tenant_id(tenant)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(barrier_mode)
            .resource_property("topic_id", TOPIC_A)
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant)
            .resource_property("event_type_patterns", json!([CREATED]))
            .resource_property("event_types", json!([CREATED]))
            .resource_property("event_subject_type_patterns", json!([ORDER]))
            .require_constraints(true),
    )
}

/// The scope an allowed E compiles to: the interest tenant only.
fn kept_scope(tenant: Uuid) -> Arc<CompiledScope> {
    Arc::new(
        CompiledScope::compile(
            &AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
                pep_properties::OWNER_TENANT_ID,
                vec![tenant],
            )])),
            &TenantMode::RootOnly,
            &TenantSetInterner::new(),
        )
        .unwrap(),
    )
}

fn created_only() -> Arc<HashSet<GtsTypeId>> {
    Arc::new(HashSet::from([GtsTypeId::try_new(CREATED).unwrap()]))
}

fn key(tenant: Uuid, barrier_mode: BarrierMode) -> GroupKey {
    GroupKey {
        topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
        tenant,
        mode: TenantMode::RootOnly,
        barrier_mode,
    }
}

/// The group of `tenant` as an allowed evaluation leaves it, expiring at
/// `expires_at`.
fn group(tenant: Uuid, barrier_mode: BarrierMode, expires_at: Instant) -> AuthorizedGroup {
    AuthorizedGroup {
        key: key(tenant, barrier_mode),
        scope: kept_scope(tenant),
        candidates: created_only(),
        candidates_stale: false,
        expires_at,
    }
}

fn scope(generation: u64, groups: Vec<AuthorizedGroup>) -> CachedScope {
    CachedScope {
        generation: ScopeGeneration::nth(generation),
        principal: ScopePrincipal::from(&caller()),
        groups: groups.into(),
    }
}

/// The candidates of `TOPIC_A` as the registry lists them: `CREATED`,
/// allowing `ORDER`.
fn listed() -> EventTypeCandidates {
    EventTypeCandidates::Listed(vec![EventTypeCandidate {
        id: GtsTypeId::try_new(CREATED).unwrap(),
        allowed_subject_types: vec![GtsIdPattern::try_new(ORDER).unwrap()],
    }])
}

/// A specification manager that answers the candidate query from memory, so
/// a timed pass waits on nothing but its PDP. A refresh reads nothing else.
#[derive(Default)]
struct ListedCandidates {
    /// When set, each candidate query fails this long after it is asked, as
    /// a slow, unavailable database does.
    failing_after: Option<Duration>,
    /// When each candidate query was asked.
    asked_at: AskedAt,
}

#[async_trait]
impl SpecificationManager for ListedCandidates {
    async fn get_topic(&self, _id: &GtsInstanceId) -> Result<Option<Topic>, DomainError> {
        Ok(None)
    }

    async fn get_event_type(&self, _id: &GtsTypeId) -> Result<Option<EventType>, DomainError> {
        Ok(None)
    }

    async fn get_event_type_candidates(
        &self,
        _topic: &GtsInstanceId,
    ) -> Result<EventTypeCandidates, DomainError> {
        self.asked_at.lock().unwrap().push(Instant::now());
        match self.failing_after {
            Some(wait) => {
                tokio::time::sleep(wait).await;
                Err(DomainError::StorageUnavailable {
                    reason: "the candidates cannot be read".to_owned(),
                    source: None,
                })
            }
            None => Ok(listed()),
        }
    }

    async fn get_registered_event_types(
        &self,
        _topic: &GtsInstanceId,
        _ids: &[GtsTypeId],
    ) -> Result<Vec<GtsTypeId>, DomainError> {
        Ok(Vec::new())
    }

    async fn validate_event_data(
        &self,
        _event_type: &EventType,
        _data: &Value,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn list_topics(&self) -> Result<Vec<Topic>, DomainError> {
        Ok(Vec::new())
    }

    async fn list_event_types(&self) -> Result<Vec<EventType>, DomainError> {
        Ok(Vec::new())
    }

    async fn resolve_topic_id(&self, _id: &GtsInstanceId) -> Result<i64, DomainError> {
        Ok(0)
    }

    async fn resolve_event_type_id(&self, _id: &GtsTypeId) -> Result<i64, DomainError> {
        Ok(0)
    }

    async fn topic_state(&self, _topic: &Topic) -> Result<Option<TopicState>, DomainError> {
        Ok(None)
    }

    async fn provision_topics(
        &self,
        _backends: &dyn BackendResolver,
        _limits: ProvisioningLimits,
    ) -> usize {
        0
    }
}

/// An interest in `CREATED` on `TOPIC_A` in `tenant`, as JOIN stores
/// [`respect_interest`] and [`ignore_interest`].
fn interest_in(tenant: Uuid, barrier_mode: BarrierMode) -> Interest {
    Interest {
        topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
        subject_tenant: tenant,
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode,
        types: vec![GtsIdPattern::try_new(CREATED).unwrap()],
        subject_types: Vec::new(),
        filter: None,
    }
}

/// The tenant of interest `k` in the tests with many groups.
fn tenant_of(k: u32) -> Uuid {
    Uuid::from_u128(0x300 + u128::from(k))
}

/// One interest in `CREATED` per group, `count` of them, each in its own
/// tenant ([`tenant_of`]).
fn interests_in(count: u32, barrier_mode: BarrierMode) -> Vec<Interest> {
    (0..count)
        .map(|k| interest_in(tenant_of(k), barrier_mode))
        .collect()
}

/// A member of a bare coordinator, joined with `interests`, whose refresh
/// asks a scripted PDP through an enforcer with the default deadline and
/// reads its candidates from memory, writing what it asks to its own memo.
/// Every call is allowed until a test scripts otherwise.
struct Timed {
    coordinator: Arc<ConsumerGroupCoordinator>,
    enforcer: PolicyEnforcer,
    pdp: Recorded,
    asked_at: AskedAt,
    script: Arc<Mutex<Script>>,
    gate: Arc<Gate>,
    memo: Arc<ScopeMemo>,
    subscription: Subscription,
}

impl Timed {
    fn joined(interests: Vec<Interest>) -> Self {
        let pdp = Recorded::default();
        let asked_at = AskedAt::default();
        let script: Arc<Mutex<Script>> = Arc::new(Mutex::new(Box::new(|_, _| Answer::Allow)));
        let gate = Arc::new(Gate::default());
        let enforcer = PolicyEnforcer::new(Arc::new(ScriptedPdp {
            recorded: Arc::clone(&pdp),
            asked_at: Arc::clone(&asked_at),
            script: Arc::clone(&script),
            gate: Arc::clone(&gate),
        }))
        .with_deadline(DEADLINE);
        let coordinator = Arc::new(ConsumerGroupCoordinator::new(Duration::from_mins(1)));
        let topic = GtsInstanceId::try_new(TOPIC_A).unwrap();
        let outcome = coordinator.join(
            Subscription {
                id: Uuid::from_u128(0x5ab),
                tenant_id: CALLER_TENANT,
                joiner: Joiner::from(&caller()),
                consumer_group: GtsInstanceId::try_new(GROUP).unwrap(),
                client_agent: "scope-refresh-test".to_owned(),
                interests,
                topics: vec![topic.clone()],
                assigned: Vec::new(),
                topology_version: 0,
                session_timeout: Duration::from_secs(30),
                created_at: chrono::Utc::now(),
            },
            &[TopicInterest {
                id: topic,
                partitions: 1,
            }],
        );
        Self {
            coordinator,
            enforcer,
            pdp,
            asked_at,
            script,
            gate,
            memo: Arc::new(ScopeMemo::new()),
            subscription: outcome.subscription,
        }
    }

    fn answer(&self, script: impl Fn(Call, Option<Uuid>) -> Answer + Send + 'static) {
        *self.script.lock().unwrap() = Box::new(script);
    }

    fn recorded(&self) -> Vec<Value> {
        self.pdp.lock().unwrap().clone()
    }

    /// When each request reached the PDP, from `start`.
    fn asked_since(&self, start: Instant) -> Vec<Duration> {
        self.asked_at
            .lock()
            .unwrap()
            .iter()
            .map(|at| at.duration_since(start))
            .collect()
    }

    /// Stores `groups` as the member's first scope, for the caller.
    fn store(&self, groups: Vec<AuthorizedGroup>) {
        assert_eq!(
            self.coordinator.write_scope(
                self.subscription.id,
                ScopeWrite::Store {
                    expected: Expected::Absent,
                    content: ScopeContent {
                        principal: ScopePrincipal::from(&caller()),
                        groups: groups.into(),
                    },
                },
            ),
            ScopeWriteOutcome::Applied
        );
    }

    fn held(&self) -> Option<Arc<CachedScope>> {
        self.coordinator.scope_of(self.subscription.id)
    }

    /// When a stream's feed next sees the member's scope cleared.
    fn cleared_at(&self) -> tokio::task::JoinHandle<Instant> {
        let mut feed = self
            .coordinator
            .watch_scope(self.subscription.id)
            .expect("the member");
        feed.borrow_and_update();
        tokio::spawn(async move {
            loop {
                feed.changed().await.expect("the member stays");
                if feed.borrow_and_update().is_none() {
                    return Instant::now();
                }
            }
        })
    }

    /// The member's refresh as a stream open builds it, on the action's own
    /// clock, starting with each group's candidate list.
    fn refresh(&self) -> ScopeRefresh {
        self.refresh_over(
            Arc::new(ListedCandidates::default()),
            self.subscription
                .interests
                .iter()
                .map(|_| Arc::new(listed()))
                .collect(),
        )
    }

    /// The member's refresh reading its candidates from `specs`, starting
    /// with `lists`, one candidate list per group, or none as after an open
    /// that reused the held scope.
    fn refresh_over(
        &self,
        specs: Arc<ListedCandidates>,
        lists: Vec<Arc<EventTypeCandidates>>,
    ) -> ScopeRefresh {
        let evaluator = Arc::new(ConsumeEvaluator::new(
            self.enforcer.clone(),
            specs,
            ConsumeMemo::new(Arc::clone(&self.memo), TTL),
            Arc::new(TenantSetInterner::new()),
        ));
        ScopeRefresh::new(
            &self.subscription,
            caller(),
            lists,
            Arc::clone(&self.coordinator),
            evaluator,
            &StreamingConfig::default(),
        )
        .expect("the refresh")
    }
}

/// Runs passes of `refresh` as its worker does: a sleep is waited out and a
/// proceed runs the next pass at once, until a pass idles. On the paused
/// clock the waits take no real time; the run is bounded by `MAX_PASSES`
/// passes and by `TTL` plus `BOUND` of paused time.
async fn run_until_idle(refresh: &mut ScopeRefresh) {
    let cancel = CancellationToken::new();
    tokio::time::timeout(TTL + BOUND, async {
        for _ in 0..MAX_PASSES {
            let Ok(directive) = refresh.execute(&cancel).await;
            match directive {
                Directive::Sleep(wait, ()) => tokio::time::sleep(wait).await,
                Directive::Proceed(()) => {}
                Directive::Idle(()) => return,
            }
        }
        panic!("the worker does not idle within {MAX_PASSES} passes");
    })
    .await
    .expect("the worker idles within the bound");
}

/// Runs passes of `refresh` as its worker does, as [`run_until_idle`]
/// does, until `until` on the paused clock, and drops the pass then in
/// progress, with the renewals it runs. Returns early when a pass idles.
/// Bounded by `MAX_PASSES` passes: a pass that keeps renewals in flight
/// runs on without returning.
async fn run_until(refresh: &mut ScopeRefresh, until: Instant) {
    let cancel = CancellationToken::new();
    let _reached_or_idled = tokio::time::timeout_at(until, async {
        for _ in 0..MAX_PASSES {
            let Ok(directive) = refresh.execute(&cancel).await;
            match directive {
                Directive::Sleep(wait, ()) => tokio::time::sleep(wait).await,
                Directive::Proceed(()) => {}
                Directive::Idle(()) => return,
            }
        }
        panic!("the worker runs more than {MAX_PASSES} passes");
    })
    .await;
}

/// Lets every spawned task run as far as it can without time passing on the
/// paused clock: 16 yields, a bound on the hops from a pass to its renewals
/// and from a released renewal back to the pass (each hop is one poll).
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

/// When `cleared` saw the scope cleared, from `start`.
async fn cleared_since(cleared: tokio::task::JoinHandle<Instant>, start: Instant) -> Duration {
    tokio::time::timeout(BOUND, cleared)
        .await
        .expect("the scope is cleared within the bound")
        .unwrap()
        .duration_since(start)
}

// -- what a pass re-evaluates --

#[tokio::test]
async fn a_group_due_for_refresh_is_reevaluated_and_swapped() {
    let joined = joined(json!([
        respect_interest(TENANT),
        respect_interest(TENANT_B)
    ]))
    .await;
    let now = Instant::now();
    let held = joined.store(vec![
        group(TENANT, BarrierMode::Respect, now + LEAD - MS),
        group(
            TENANT_B,
            BarrierMode::Respect,
            now + Duration::from_secs(40),
        ),
    ]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    joined.forget(&e);
    let mut refresh = joined.refresh(now, true).await;

    let directive = pass(&mut refresh).await;

    assert_eq!(joined.recorded(), sent(&[e]));
    let stored = joined.held();
    assert_eq!(
        *stored,
        scope(
            2,
            vec![
                group(TENANT, BarrierMode::Respect, now + TTL),
                group(
                    TENANT_B,
                    BarrierMode::Respect,
                    now + Duration::from_secs(40)
                ),
            ]
        )
    );
    assert!(!Arc::ptr_eq(&stored.groups[0].scope, &held.groups[0].scope));
    assert!(!Arc::ptr_eq(
        &stored.groups[0].candidates,
        &held.groups[0].candidates
    ));
    assert!(Arc::ptr_eq(&stored.groups[1].scope, &held.groups[1].scope));
    assert!(Arc::ptr_eq(
        &stored.groups[1].candidates,
        &held.groups[1].candidates
    ));
    // The untouched group is next, one lead before its expiry.
    assert_eq!(directive, Directive::sleep(Duration::from_secs(28)));
}

#[tokio::test]
async fn a_refresh_never_reuses_the_memo_entry_it_is_replacing() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    let expires_at = now + LEAD - MS;
    joined.store(vec![group(TENANT, BarrierMode::Respect, expires_at)]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    // The answer the group was built from: live, but gone before the next
    // refresh would be due.
    joined.remember(&e, MemoResult::Scope(kept_scope(TENANT)), expires_at);
    let mut refresh = joined.refresh(now, true).await;

    let directive = pass(&mut refresh).await;

    assert_eq!(joined.recorded(), sent(&[e]));
    assert_eq!(
        *joined.held(),
        scope(2, vec![group(TENANT, BarrierMode::Respect, now + TTL)])
    );
    assert_eq!(directive, Directive::sleep(RENEWED_DUE_IN));
}

#[tokio::test]
async fn a_refresh_reuses_a_fresher_memo_entry() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    let fresher = kept_scope(TENANT);
    let until = now + Duration::from_secs(30);
    joined.remember(&e, MemoResult::Scope(Arc::clone(&fresher)), until);
    let mut refresh = joined.refresh(now, true).await;

    let directive = pass(&mut refresh).await;

    assert_eq!(joined.recorded(), Vec::<Value>::new());
    let stored = joined.held();
    assert_eq!(
        *stored,
        scope(2, vec![group(TENANT, BarrierMode::Respect, until)])
    );
    assert!(Arc::ptr_eq(&stored.groups[0].scope, &fresher));
    assert_eq!(directive, Directive::sleep(Duration::from_secs(18)));
}

/// I is asked before E, and not at all while the memo holds an allowed I
/// that outlives the next refresh, which then bounds the new window.
#[tokio::test]
async fn an_ignore_group_refresh_asks_i_then_e() {
    for i_remembered in [false, true] {
        let joined = joined(json!([ignore_interest(TENANT)])).await;
        let now = Instant::now();
        joined.store(vec![group(
            TENANT,
            BarrierMode::Ignore,
            now + IGNORE_LEAD - MS,
        )]);
        let i = i_request(&joined.enforcer, TENANT);
        let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Ignore);
        let i_until = now + Duration::from_secs(30);
        if i_remembered {
            joined.remember(&i, MemoResult::Decision(true), i_until);
        } else {
            joined.forget(&i);
        }
        joined.forget(&e);
        let mut refresh = joined.refresh(now, true).await;

        let directive = pass(&mut refresh).await;

        // The next refresh is one `ignore` lead (22 s) before the expiry.
        let (expected_calls, expires_at, due_in) = if i_remembered {
            (sent(&[e]), i_until, Duration::from_secs(8))
        } else {
            (sent(&[i, e]), now + TTL, Duration::from_secs(38))
        };
        assert_eq!(
            joined.recorded(),
            expected_calls,
            "I remembered: {i_remembered}"
        );
        assert_eq!(
            *joined.held(),
            scope(2, vec![group(TENANT, BarrierMode::Ignore, expires_at)]),
            "I remembered: {i_remembered}"
        );
        assert_eq!(
            directive,
            Directive::sleep(due_in),
            "I remembered: {i_remembered}"
        );
    }
}

// -- what a refusal and a failure do --

/// Every refusal of I or E clears the scope at once, without R, and the
/// worker goes idle. A scope that matches nothing is a refusal, never an
/// evaluation failure to retry.
#[tokio::test]
async fn a_denied_refresh_clears_at_once() {
    for (barrier_mode, refused, answer) in [
        (BarrierMode::Ignore, Call::I, Answer::Deny),
        (BarrierMode::Ignore, Call::I, Answer::AllowNothing),
        (BarrierMode::Ignore, Call::I, Answer::Uncompilable),
        (BarrierMode::Respect, Call::E, Answer::Deny),
        (BarrierMode::Respect, Call::E, Answer::AllowNothing),
        (BarrierMode::Respect, Call::E, Answer::Uncompilable),
        (BarrierMode::Respect, Call::E, Answer::Unenforceable),
    ] {
        let row = format!("{barrier_mode:?} {refused:?} {answer:?}");
        let (interest, lead, pdp_mode) = match barrier_mode {
            BarrierMode::Respect => (respect_interest(TENANT), LEAD, PdpBarrierMode::Respect),
            BarrierMode::Ignore => (ignore_interest(TENANT), IGNORE_LEAD, PdpBarrierMode::Ignore),
        };
        let joined = joined(json!([interest])).await;
        let now = Instant::now();
        joined.store(vec![group(TENANT, barrier_mode, now + lead - MS)]);
        let i = i_request(&joined.enforcer, TENANT);
        let e = e_request(&joined.enforcer, TENANT, pdp_mode);
        joined.forget(&i);
        joined.forget(&e);
        let mut feed = joined.harness.groups().watch_scope(joined.id).unwrap();
        feed.borrow_and_update();
        joined.answer(move |call, _| {
            if call == refused {
                answer
            } else {
                Answer::Allow
            }
        });
        let mut refresh = joined.refresh(now, true).await;

        let directive = pass(&mut refresh).await;

        let expected_calls = match refused {
            Call::I => sent(&[i]),
            _ => sent(&[e]),
        };
        assert_eq!(joined.recorded(), expected_calls, "{row}");
        assert_eq!(directive, Directive::idle(), "{row}");
        assert_eq!(joined.harness.groups().scope_of(joined.id), None, "{row}");
        assert!(feed.has_changed().unwrap(), "{row}");
        assert_eq!(*feed.borrow_and_update(), None, "{row}");
    }
}

/// A failed E keeps the scope serving and retries after 1, 2, 4 and then 5
/// seconds. The group is `ignore`, so its 22 s lead leaves room for the
/// capped wait before its expiry.
#[tokio::test]
async fn an_evaluation_failure_retries_with_backoff() {
    let joined = joined(json!([ignore_interest(TENANT)])).await;
    let now = Instant::now();
    let held = joined.store(vec![group(TENANT, BarrierMode::Ignore, now + IGNORE_LEAD)]);
    let i = i_request(&joined.enforcer, TENANT);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Ignore);
    joined.forget(&i);
    joined.forget(&e);
    joined.answer(|call, _| {
        if call == Call::E {
            Answer::Fail
        } else {
            Answer::Allow
        }
    });
    let mut refresh = joined.refresh(now, true).await;

    let mut at = now;
    let mut directives = Vec::new();
    let mut calls = Vec::new();
    for _ in 0..4 {
        refresh.pin_clock(at);
        let directive = pass(&mut refresh).await;
        calls.push(joined.recorded());
        joined.clear_recorded();
        let Directive::Sleep(wait, ()) = directive else {
            panic!("expected a sleep, got {directive:?}");
        };
        directives.push(directive);
        at += wait;
    }

    assert_eq!(
        directives,
        [1, 2, 4, 5].map(|secs| Directive::sleep(Duration::from_secs(secs)))
    );
    // The allowed I is kept in the memo past the next refresh, so only the
    // first pass asks it.
    assert_eq!(
        calls,
        vec![
            sent(&[i, e.clone()]),
            sent(std::slice::from_ref(&e)),
            sent(std::slice::from_ref(&e)),
            sent(&[e]),
        ]
    );
    assert!(Arc::ptr_eq(&joined.held(), &held));
}

/// A retry that comes at the group's expiry clears the scope without
/// asking again. On the paused clock, so the pass half a second before the
/// expiry takes no time of its own.
#[tokio::test(start_paused = true)]
async fn a_failure_lasting_to_expiry_clears() {
    let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Respect)]);
    let start = Instant::now();
    let expires_in = Duration::from_millis(500);
    timed.store(vec![group(
        TENANT,
        BarrierMode::Respect,
        start + expires_in,
    )]);
    let held = timed.held().expect("the scope just stored");
    timed.answer(|call, _| {
        if call == Call::E {
            Answer::Fail
        } else {
            Answer::Allow
        }
    });
    let mut refresh = timed.refresh();

    // Half a second before the expiry: the 1 s retry is cut to the expiry.
    let first = pass(&mut refresh).await;
    let e = e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect);
    assert_eq!(first, Directive::sleep(expires_in));
    assert_eq!(timed.recorded(), sent(std::slice::from_ref(&e)));
    assert_eq!(timed.asked_since(start), [Duration::ZERO]);
    assert!(timed.held().is_some_and(|kept| Arc::ptr_eq(&kept, &held)));

    tokio::time::sleep(expires_in).await;
    let second = pass(&mut refresh).await;

    assert_eq!(second, Directive::idle());
    assert_eq!(timed.recorded(), sent(&[e]));
    assert_eq!(timed.held(), None);
}

/// A group the worker first reaches only after its expiry (a late wakeup,
/// or earlier groups that took its lead) clears the scope without asking:
/// the scope no longer serves.
#[tokio::test]
async fn a_group_first_reached_after_its_expiry_clears_the_scope() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    let expires_at = now + LEAD;
    joined.store(vec![group(TENANT, BarrierMode::Respect, expires_at)]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    joined.forget(&e);
    let late = expires_at + Duration::from_secs(1);
    let mut refresh = joined.refresh(late, true).await;

    let directive = pass(&mut refresh).await;

    assert_eq!(joined.recorded(), Vec::<Value>::new());
    assert_eq!(joined.harness.groups().scope_of(joined.id), None);
    assert_eq!(directive, Directive::idle());
}

#[tokio::test]
async fn a_success_resets_the_backoff() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD)]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    joined.forget(&e);
    joined.answer(|call, _| {
        if call == Call::E {
            Answer::Fail
        } else {
            Answer::Allow
        }
    });
    let mut refresh = joined.refresh(now, true).await;
    assert_eq!(
        pass(&mut refresh).await,
        Directive::sleep(Duration::from_secs(1))
    );
    refresh.pin_clock(now + Duration::from_secs(1));
    assert_eq!(
        pass(&mut refresh).await,
        Directive::sleep(Duration::from_secs(2))
    );

    joined.answer(|_, _| Answer::Allow);
    let renewed_at = now + Duration::from_secs(3);
    refresh.pin_clock(renewed_at);
    assert_eq!(pass(&mut refresh).await, Directive::sleep(RENEWED_DUE_IN));
    assert_eq!(
        *joined.held(),
        scope(
            2,
            vec![group(TENANT, BarrierMode::Respect, renewed_at + TTL)]
        )
    );

    joined.answer(|call, _| {
        if call == Call::E {
            Answer::Fail
        } else {
            Answer::Allow
        }
    });
    refresh.pin_clock(renewed_at + TTL - LEAD);
    joined.clear_recorded();
    let directive = pass(&mut refresh).await;

    assert_eq!(directive, Directive::sleep(Duration::from_secs(1)));
    assert_eq!(joined.recorded(), sent(&[e]));
}

// -- a refresh on the paused clock --

/// E never answers, so each try ends at the enforcer's 10 s deadline. The
/// first try, one lead (12 s) before the expiry, fails at 10 s and the retry
/// comes 1 s after; that retry is cut at the expiry, which clears the scope.
/// No call starts after the expiry.
#[tokio::test(start_paused = true)]
async fn a_group_whose_pdp_times_out_is_cleared_at_its_expiry() {
    let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Respect)]);
    let start = Instant::now();
    timed.store(vec![group(TENANT, BarrierMode::Respect, start + LEAD)]);
    timed.answer(|call, _| {
        if call == Call::E {
            Answer::Hang
        } else {
            Answer::Allow
        }
    });
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    run_until_idle(&mut refresh).await;

    let e = e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect);
    assert_eq!(timed.recorded(), sent(&[e.clone(), e]));
    assert_eq!(timed.asked_since(start), [0, 11].map(Duration::from_secs));
    assert_eq!(cleared_since(cleared, start).await, LEAD);
    assert_eq!(timed.held(), None);
}

/// An `ignore` group (lead 22 s) whose I, or whose E after an allowed I,
/// never answers: tries at 0 s and 11 s, each ended by the 10 s deadline,
/// and the scope cleared at the expiry. The I allowed at 0 s is kept in the
/// memo past the next try, so the retry asks E alone.
#[tokio::test(start_paused = true)]
async fn an_ignore_group_whose_pdp_times_out_is_cleared_at_its_expiry() {
    for hangs in [Call::I, Call::E] {
        let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Ignore)]);
        let start = Instant::now();
        timed.store(vec![group(
            TENANT,
            BarrierMode::Ignore,
            start + IGNORE_LEAD,
        )]);
        timed.answer(move |call, _| {
            if call == hangs {
                Answer::Hang
            } else {
                Answer::Allow
            }
        });
        let cleared = timed.cleared_at();
        let mut refresh = timed.refresh();

        run_until_idle(&mut refresh).await;

        let i = i_request(&timed.enforcer, TENANT);
        let e = e_request(&timed.enforcer, TENANT, PdpBarrierMode::Ignore);
        let (requests, asked) = match hangs {
            Call::I => (sent(&[i.clone(), i]), vec![0, 11]),
            _ => (sent(&[i, e.clone(), e]), vec![0, 0, 11]),
        };
        assert_eq!(timed.recorded(), requests, "{hangs:?} hangs");
        assert_eq!(
            timed.asked_since(start),
            asked
                .into_iter()
                .map(Duration::from_secs)
                .collect::<Vec<_>>(),
            "{hangs:?} hangs"
        );
        assert_eq!(
            cleared_since(cleared, start).await,
            IGNORE_LEAD,
            "{hangs:?} hangs"
        );
        assert_eq!(timed.held(), None, "{hangs:?} hangs");
    }
}

/// Three groups expiring 0, 1 and 2 s apart, and E never answers. Each group
/// is tried at its due instant (0, 1 and 2 s), beside the others, and each
/// try ends at its 10 s deadline. The first group's retry, 1 s after its
/// failure, is cut at its expiry 1 s later, which clears the scope while the
/// other groups wait for theirs: no group serves past its own expiry.
#[tokio::test(start_paused = true)]
async fn groups_due_in_one_pass_are_cleared_at_the_first_expiry() {
    let timed = Timed::joined(vec![
        interest_in(TENANT, BarrierMode::Respect),
        interest_in(TENANT_B, BarrierMode::Respect),
        interest_in(TENANT_C, BarrierMode::Respect),
    ]);
    let start = Instant::now();
    let first_expiry = start + LEAD;
    timed.store(vec![
        group(TENANT, BarrierMode::Respect, first_expiry),
        group(
            TENANT_B,
            BarrierMode::Respect,
            first_expiry + Duration::from_secs(1),
        ),
        group(
            TENANT_C,
            BarrierMode::Respect,
            first_expiry + Duration::from_secs(2),
        ),
    ]);
    timed.answer(|call, _| {
        if call == Call::E {
            Answer::Hang
        } else {
            Answer::Allow
        }
    });
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    run_until_idle(&mut refresh).await;

    let e = e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect);
    assert_eq!(
        timed.recorded(),
        sent(&[
            e.clone(),
            e_request(&timed.enforcer, TENANT_B, PdpBarrierMode::Respect),
            e_request(&timed.enforcer, TENANT_C, PdpBarrierMode::Respect),
            e,
        ])
    );
    assert_eq!(
        timed.asked_since(start),
        [0, 1, 2, 11].map(Duration::from_secs)
    );
    assert_eq!(cleared_since(cleared, start).await, LEAD);
    assert_eq!(timed.held(), None);
}

/// Two groups due together, at the same expiry, and E never answers. Both
/// are tried at once, both tries end at the 10 s deadline, and both retries,
/// 1 s later, are cut at the shared expiry 1 s after that, which clears the
/// scope: each retry is timed from the clock read when its failure came.
#[tokio::test(start_paused = true)]
async fn groups_due_together_are_cleared_at_their_expiry() {
    let timed = Timed::joined(vec![
        interest_in(TENANT, BarrierMode::Respect),
        interest_in(TENANT_B, BarrierMode::Respect),
    ]);
    let start = Instant::now();
    timed.store(vec![
        group(TENANT, BarrierMode::Respect, start + LEAD),
        group(TENANT_B, BarrierMode::Respect, start + LEAD),
    ]);
    timed.answer(|call, _| {
        if call == Call::E {
            Answer::Hang
        } else {
            Answer::Allow
        }
    });
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    run_until_idle(&mut refresh).await;

    let e = e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect);
    let e_b = e_request(&timed.enforcer, TENANT_B, PdpBarrierMode::Respect);
    assert_eq!(timed.recorded(), sent(&[e.clone(), e_b.clone(), e, e_b]));
    assert_eq!(
        timed.asked_since(start),
        [0, 0, 11, 11].map(Duration::from_secs)
    );
    assert_eq!(cleared_since(cleared, start).await, LEAD);
    assert_eq!(timed.held(), None);
}

/// E fails 2 s after each call. The retries wait their backoff from the
/// failure: the second try starts 1 s after the first failed (at 3 s), the
/// third 2 s after the second failed (at 7 s). The third fails at 9 s, its
/// 4 s wait is cut to the expiry at 12 s, and the scope is cleared there.
#[tokio::test(start_paused = true)]
async fn a_retry_waits_its_backoff_after_a_slow_failure() {
    let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Respect)]);
    let start = Instant::now();
    timed.store(vec![group(TENANT, BarrierMode::Respect, start + LEAD)]);
    timed.answer(|call, _| {
        if call == Call::E {
            Answer::FailAfter(Duration::from_secs(2))
        } else {
            Answer::Allow
        }
    });
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    run_until_idle(&mut refresh).await;

    let e = e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect);
    assert_eq!(timed.recorded(), sent(&[e.clone(), e.clone(), e]));
    assert_eq!(timed.asked_since(start), [0, 3, 7].map(Duration::from_secs));
    assert_eq!(cleared_since(cleared, start).await, LEAD);
    assert_eq!(timed.held(), None);
}

/// With no candidate list yet, as after an open that reused the held scope,
/// a candidate query that answers gives the group its list: E is asked with
/// it and the group is renewed.
#[tokio::test(start_paused = true)]
async fn a_group_with_no_list_yet_is_evaluated_with_the_candidates_it_reads() {
    let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Respect)]);
    let start = Instant::now();
    timed.store(vec![group(TENANT, BarrierMode::Respect, start + LEAD)]);
    let mut refresh = timed.refresh_over(Arc::new(ListedCandidates::default()), Vec::new());

    let directive = pass(&mut refresh).await;

    assert_eq!(
        timed.recorded(),
        sent(&[e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect)])
    );
    assert_eq!(
        timed.held(),
        Some(Arc::new(scope(
            2,
            vec![group(TENANT, BarrierMode::Respect, start + TTL)]
        )))
    );
    assert_eq!(directive, Directive::sleep(RENEWED_DUE_IN));
}

/// With no candidate list yet, the candidate query fails 2 s after each ask.
/// Each retry waits its backoff from the failure: the second query at 3 s
/// (1 s after the first failed), the third at 7 s (2 s after the second).
/// The third fails at 9 s, its 4 s wait is cut to the expiry at 12 s, and the
/// scope is cleared there. Without a list no PDP call is asked.
#[tokio::test(start_paused = true)]
async fn a_retry_with_no_list_waits_its_backoff_after_a_slow_query_failure() {
    let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Respect)]);
    let start = Instant::now();
    timed.store(vec![group(TENANT, BarrierMode::Respect, start + LEAD)]);
    let candidates = Arc::new(ListedCandidates {
        failing_after: Some(Duration::from_secs(2)),
        ..ListedCandidates::default()
    });
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh_over(Arc::clone(&candidates), Vec::new());

    run_until_idle(&mut refresh).await;

    let queried: Vec<Duration> = candidates
        .asked_at
        .lock()
        .unwrap()
        .iter()
        .map(|at| at.duration_since(start))
        .collect();
    assert_eq!(queried, [0, 3, 7].map(Duration::from_secs));
    assert_eq!(timed.recorded(), Vec::<Value>::new());
    assert_eq!(cleared_since(cleared, start).await, LEAD);
    assert_eq!(timed.held(), None);
}

/// The worker's cancel comes before a pass that would clear an expired
/// group: the pass writes nothing and idles.
#[tokio::test(start_paused = true)]
async fn a_pass_run_after_its_cancel_writes_nothing() {
    let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Respect)]);
    let start = Instant::now();
    timed.store(vec![group(TENANT, BarrierMode::Respect, start)]);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut refresh = timed.refresh();

    let directive = pass_with(&mut refresh, &cancel).await;

    assert_eq!(directive, Directive::idle());
    assert_eq!(
        timed.held(),
        Some(Arc::new(scope(
            1,
            vec![group(TENANT, BarrierMode::Respect, start)]
        )))
    );
    assert_eq!(timed.recorded(), Vec::<Value>::new());
}

/// One group due at once, and its E held on the gate. The test releases E
/// and cancels the worker without yielding between, so both wake the
/// renewal together: its `biased` race takes the cancel, the turn ends
/// cancelled without taking E's answer, and the pass idles. E is asked
/// once, and nothing is written. Rows: E allows (no store) and E denies (no
/// clear).
///
/// The turn's race decides it here, and is pinned by
/// `a_renewal_that_finds_its_pass_cancelled_after_the_candidate_query_asks_no_pdp`
/// and `a_renewal_cancelled_while_its_barrier_right_is_asked_asks_no_consume_scope`.
/// A turn that takes its answer before it sees the cancel is kept from
/// writing by the pass's `biased` order and by `finish`'s checks on a
/// renewal and a refusal; the checks are pinned by
/// `a_turn_finished_after_the_cancel_ends_the_pass_writing_nothing`.
#[tokio::test(start_paused = true)]
async fn a_completion_ready_with_the_cancel_writes_nothing() {
    for answer in [Answer::Allow, Answer::Deny] {
        let timed = Timed::joined(vec![interest_in(TENANT, BarrierMode::Respect)]);
        let start = Instant::now();
        timed.store(vec![group(TENANT, BarrierMode::Respect, start + LEAD)]);
        timed.answer(move |_, _| answer);
        timed.gate.armed.store(true, Ordering::SeqCst);
        let mut refresh = timed.refresh();
        let cancel = CancellationToken::new();
        let worker = cancel.clone();
        let refreshing = tokio::spawn(async move { pass_with(&mut refresh, &worker).await });
        tokio::time::timeout(BOUND, timed.gate.entered.notified())
            .await
            .expect("the renewal reaches E within the bound");

        timed.gate.released.notify_one();
        cancel.cancel();
        let directive = tokio::time::timeout(BOUND, refreshing)
            .await
            .expect("the pass ends within the bound")
            .unwrap();

        let row = format!("{answer:?}");
        assert_eq!(directive, Directive::idle(), "{row}");
        assert_eq!(
            timed.held(),
            Some(Arc::new(scope(
                1,
                vec![group(TENANT, BarrierMode::Respect, start + LEAD)]
            ))),
            "{row}"
        );
        assert_eq!(
            timed.recorded(),
            sent(&[e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect)]),
            "{row}"
        );
    }
}

/// One group whose E never answers, and one due only at 28 s. The first
/// group's retry starts at 11 s while a slot is free for the second, and the
/// pass waits for the first expiry at 12 s, not for the second group: the
/// scope is cleared there.
#[tokio::test(start_paused = true)]
async fn a_wait_for_a_later_group_ends_at_the_earliest_expiry() {
    let timed = Timed::joined(vec![
        interest_in(TENANT, BarrierMode::Respect),
        interest_in(TENANT_B, BarrierMode::Respect),
    ]);
    let start = Instant::now();
    timed.store(vec![
        group(TENANT, BarrierMode::Respect, start + LEAD),
        group(
            TENANT_B,
            BarrierMode::Respect,
            start + Duration::from_secs(40),
        ),
    ]);
    timed.answer(|call, root| {
        if call == Call::E && root == Some(TENANT) {
            Answer::Hang
        } else {
            Answer::Allow
        }
    });
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    run_until_idle(&mut refresh).await;

    let e = e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect);
    assert_eq!(timed.recorded(), sent(&[e.clone(), e]));
    assert_eq!(timed.asked_since(start), [0, 11].map(Duration::from_secs));
    assert_eq!(cleared_since(cleared, start).await, LEAD);
    assert_eq!(timed.held(), None);
}

// -- many groups on the paused clock --

/// How many groups the tests with many groups hold.
const MANY: u32 = 16;

/// The spacing a stream open leaves between the expiries of `MANY` groups:
/// `TTL / MANY`.
const SPACING: Duration = Duration::from_millis(3_750);

/// `MANY` groups of `barrier_mode`, the first due at `start`, one `lead`
/// before its expiry, and each next one `SPACING` later.
fn spaced(start: Instant, barrier_mode: BarrierMode, lead: Duration) -> Vec<AuthorizedGroup> {
    (0..MANY)
        .map(|k| group(tenant_of(k), barrier_mode, start + lead + SPACING * k))
        .collect()
}

/// Sixteen `respect` groups due `SPACING` (3.75 s) apart, as a stream open
/// spaces them, and every E allowed 9 s after it is asked: inside the bound
/// of `REFRESH_CONCURRENCY x TTL / 16` (15 s). At most four renewals overlap,
/// so each group is asked at its due instant and the scope is never
/// cleared.
///
/// An answer reaches the memo 9 s into the window it opens and outlives that
/// window by 9 s, so the group's next renewal, 48 s later, reuses it without
/// a call and serves to its end, and the one after that asks again: each
/// group asks every 57 s (48 + 9) and is stored twice in that time. Over
/// three windows, to 180.5 s: 51 asks (three rounds of 16, then the first
/// three groups at 171, 174.75 and 178.5 s), 49 of them answered and stored
/// (the ask at 171 s answers at 180 s), and 37 renewals from the memo (at
/// `48 + 57 j + 3.75 k` s: 16, 16 and 5), so the scope reaches generation
/// 1 + 86.
#[tokio::test(start_paused = true)]
async fn many_groups_with_a_slow_pdp_keep_the_stream_alive() {
    let slow = Duration::from_secs(9);
    let horizon = 3 * TTL + Duration::from_millis(500);
    let timed = Timed::joined(interests_in(MANY, BarrierMode::Respect));
    let start = Instant::now();
    timed.store(spaced(start, BarrierMode::Respect, LEAD));
    timed.answer(move |_, _| Answer::AllowAfter(slow));
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    run_until(&mut refresh, start + horizon).await;

    let cycle = RENEWED_DUE_IN + slow;
    let asks: Vec<(u32, Duration)> = (0..4)
        .flat_map(|j| (0..MANY).map(move |k| (k, cycle * j + SPACING * k)))
        .filter(|(_, at)| *at < horizon)
        .collect();
    assert_eq!(asks.len(), 51);
    assert_eq!(
        timed.asked_since(start),
        asks.iter().map(|(_, at)| *at).collect::<Vec<_>>()
    );
    assert_eq!(
        timed.recorded(),
        sent(
            &asks
                .iter()
                .map(|(k, _)| e_request(&timed.enforcer, tenant_of(*k), PdpBarrierMode::Respect))
                .collect::<Vec<_>>()
        )
    );
    assert!(!cleared.is_finished(), "the scope was cleared");
    assert_eq!(
        timed.held().map(|held| held.generation),
        Some(ScopeGeneration::nth(87))
    );
}

/// Sixteen `ignore` groups due `SPACING` apart, and I and E each allowed
/// 9.5 s after they are asked: 19 s a renewal, past the bound of 15 s. The
/// first four renewals take every slot, so the fifth group, due at 15 s,
/// starts only when the first renewal ends at 19 s, and is still running at
/// its expiry, 37 s (its 22 s lead after 15 s): the scope is cleared there,
/// the renewals in flight are dropped, and no PDP call starts after it.
#[tokio::test(start_paused = true)]
async fn past_the_concurrency_bound_the_scope_clears_at_the_first_missed_expiry() {
    let half = Duration::from_millis(9_500);
    let timed = Timed::joined(interests_in(MANY, BarrierMode::Ignore));
    let start = Instant::now();
    timed.store(spaced(start, BarrierMode::Ignore, IGNORE_LEAD));
    timed.answer(move |_, _| Answer::AllowAfter(half));
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    run_until(&mut refresh, start + TTL).await;

    let i = |k| i_request(&timed.enforcer, tenant_of(k));
    let e = |k| e_request(&timed.enforcer, tenant_of(k), PdpBarrierMode::Ignore);
    assert_eq!(
        timed.recorded(),
        sent(&[
            i(0),
            i(1),
            i(2),
            e(0),
            i(3),
            e(1),
            e(2),
            i(4),
            e(3),
            i(5),
            i(6),
            e(4),
            i(7),
            e(5),
            e(6),
        ])
    );
    assert_eq!(
        timed.asked_since(start),
        [
            0, 3_750, 7_500, 9_500, 11_250, 13_250, 17_000, 19_000, 20_750, 22_750, 26_500, 28_500,
            30_250, 32_250, 36_000,
        ]
        .map(Duration::from_millis)
    );
    assert_eq!(
        cleared_since(cleared, start).await,
        IGNORE_LEAD + SPACING * 4
    );
    assert_eq!(timed.held(), None);
}

/// Eight groups due at once, and every E held on the gate. Exactly
/// `REFRESH_CONCURRENCY` renewals ask E before any answers; releasing one
/// starts the next group, and no other. The paused clock stands still while
/// the test only yields, so every call is asked at the start.
#[tokio::test(start_paused = true)]
async fn at_most_the_cap_of_renewals_run_at_once() {
    let timed = Timed::joined(interests_in(8, BarrierMode::Respect));
    let start = Instant::now();
    timed.store(
        (0..8)
            .map(|k| group(tenant_of(k), BarrierMode::Respect, start + LEAD))
            .collect(),
    );
    timed.gate.armed.store(true, Ordering::SeqCst);
    let mut refresh = timed.refresh();
    let refreshing = tokio::spawn(async move { pass(&mut refresh).await });

    settle().await;
    let before_any_release = timed.recorded().len();
    timed.gate.released.notify_one();
    settle().await;
    let after_one_release = timed.recorded().len();
    timed.gate.armed.store(false, Ordering::SeqCst);
    timed.gate.released.notify_waiters();
    let directive = tokio::time::timeout(BOUND, refreshing)
        .await
        .expect("the pass ends within the bound")
        .unwrap();

    assert_eq!(before_any_release, REFRESH_CONCURRENCY);
    assert_eq!(after_one_release, REFRESH_CONCURRENCY + 1);
    assert_eq!(
        timed.recorded(),
        sent(
            &(0..8)
                .map(|k| e_request(&timed.enforcer, tenant_of(k), PdpBarrierMode::Respect))
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(timed.asked_since(start), [Duration::ZERO; 8]);
    assert_eq!(
        timed.held(),
        Some(Arc::new(scope(
            9,
            (0..8)
                .map(|k| group(tenant_of(k), BarrierMode::Respect, start + TTL))
                .collect()
        )))
    );
    assert_eq!(directive, Directive::sleep(RENEWED_DUE_IN));
}

/// `REFRESH_CONCURRENCY + 1` groups due at once, every E held on the gate,
/// and the last group the nearest its expiry, 1 s before the others'. The
/// first `REFRESH_CONCURRENCY` renewals start earliest expiry first, so the
/// last group takes a slot and the fourth group waits; releasing E renews
/// all of them.
#[tokio::test(start_paused = true)]
async fn a_group_near_its_expiry_starts_before_groups_due_later() {
    let count = u32::try_from(REFRESH_CONCURRENCY + 1).unwrap();
    let last = count - 1;
    let timed = Timed::joined(interests_in(count, BarrierMode::Respect));
    let start = Instant::now();
    let expiry = |k| {
        if k == last {
            start + LEAD - Duration::from_secs(1)
        } else {
            start + LEAD
        }
    };
    timed.store(
        (0..count)
            .map(|k| group(tenant_of(k), BarrierMode::Respect, expiry(k)))
            .collect(),
    );
    timed.gate.armed.store(true, Ordering::SeqCst);
    let mut refresh = timed.refresh();
    let refreshing = tokio::spawn(async move { pass(&mut refresh).await });

    settle().await;
    let started = timed.recorded();
    timed.gate.armed.store(false, Ordering::SeqCst);
    timed.gate.released.notify_waiters();
    let directive = tokio::time::timeout(BOUND, refreshing)
        .await
        .expect("the pass ends within the bound")
        .unwrap();

    let e = |k| e_request(&timed.enforcer, tenant_of(k), PdpBarrierMode::Respect);
    assert_eq!(started, sent(&[e(4), e(0), e(1), e(2)]));
    assert_eq!(timed.recorded(), sent(&[e(4), e(0), e(1), e(2), e(3)]));
    assert_eq!(timed.asked_since(start), [Duration::ZERO; 5]);
    assert_eq!(
        timed.held(),
        Some(Arc::new(scope(
            6,
            (0..count)
                .map(|k| group(tenant_of(k), BarrierMode::Respect, start + TTL))
                .collect()
        )))
    );
    assert_eq!(directive, Directive::sleep(RENEWED_DUE_IN));
}

/// A `respect` group whose E is denied and an `ignore` group whose I takes
/// 5 s, both due at once. The denial clears the scope at once and drops the
/// other renewal: its I never answers into the memo and its E is never
/// asked, however long the test waits after.
#[tokio::test(start_paused = true)]
async fn a_refusal_drops_the_other_renewals_in_flight() {
    let timed = Timed::joined(vec![
        interest_in(TENANT, BarrierMode::Respect),
        interest_in(TENANT_B, BarrierMode::Ignore),
    ]);
    let start = Instant::now();
    timed.store(vec![
        group(TENANT, BarrierMode::Respect, start + LEAD),
        group(TENANT_B, BarrierMode::Ignore, start + IGNORE_LEAD),
    ]);
    timed.answer(|call, root| {
        if call == Call::E && root == Some(TENANT) {
            Answer::Deny
        } else if call == Call::I {
            Answer::AllowAfter(Duration::from_secs(5))
        } else {
            Answer::Allow
        }
    });
    let cleared = timed.cleared_at();
    let mut refresh = timed.refresh();

    let directive = pass(&mut refresh).await;
    tokio::time::sleep(DEADLINE).await;

    let i_b = i_request(&timed.enforcer, TENANT_B);
    assert_eq!(directive, Directive::idle());
    assert_eq!(cleared_since(cleared, start).await, Duration::ZERO);
    assert_eq!(
        timed.recorded(),
        sent(&[
            e_request(&timed.enforcer, TENANT, PdpBarrierMode::Respect),
            i_b.clone(),
        ])
    );
    assert_eq!(timed.asked_since(start), [Duration::ZERO; 2]);
    assert_eq!(
        timed
            .memo
            .get_entry(&MemoKey::try_from(&i_b).unwrap(), Instant::now()),
        None
    );
    assert_eq!(timed.held(), None);
}

// -- candidates --

#[tokio::test]
async fn a_candidate_query_failure_keeps_the_previous_candidates_stale() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    let held = joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    joined.forget(&e);
    let mut refresh = joined.refresh(now, true).await;
    joined.hook.arm(|query| match query {
        SpecQuery::EventTypeCandidates => Err(DomainError::StorageUnavailable {
            reason: "the candidates cannot be read".to_owned(),
            source: None,
        }),
        _ => Ok(()),
    });

    let directive = pass(&mut refresh).await;

    // E is asked with the candidate list the worker already had.
    assert_eq!(joined.recorded(), sent(&[e]));
    let stored = joined.held();
    assert_eq!(
        *stored,
        scope(
            2,
            vec![AuthorizedGroup {
                candidates_stale: true,
                ..group(TENANT, BarrierMode::Respect, now + TTL)
            }]
        )
    );
    assert!(Arc::ptr_eq(
        &stored.groups[0].candidates,
        &held.groups[0].candidates
    ));
    assert_eq!(directive, Directive::sleep(RENEWED_DUE_IN));
}

/// With no candidate list yet, a failed candidate query asks nothing and is
/// tried again like a failed evaluation.
#[tokio::test]
async fn a_candidate_query_failure_with_no_list_yet_is_tried_again() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    let held = joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
    let mut refresh = joined.refresh(now, false).await;
    joined.hook.arm(|query| match query {
        SpecQuery::EventTypeCandidates => Err(DomainError::StorageUnavailable {
            reason: "the candidates cannot be read".to_owned(),
            source: None,
        }),
        _ => Ok(()),
    });

    let directive = pass(&mut refresh).await;

    assert_eq!(joined.recorded(), Vec::<Value>::new());
    assert!(Arc::ptr_eq(&joined.held(), &held));
    assert_eq!(directive, Directive::sleep(Duration::from_secs(1)));
}

// -- races and removal --

impl Joined {
    /// Makes another writer store `others` as generation 2 while the next
    /// pass waits on its candidate query, after it read generation 1.
    fn store_first_on_the_candidate_query(&self, others: Vec<AuthorizedGroup>) {
        let groups = self.harness.shared_groups();
        let id = self.id;
        self.hook.arm(move |query| {
            if query == SpecQuery::EventTypeCandidates {
                assert_eq!(
                    groups.write_scope(
                        id,
                        ScopeWrite::Store {
                            expected: Expected::Generation(ScopeGeneration::nth(1)),
                            content: ScopeContent {
                                principal: ScopePrincipal::from(&caller()),
                                groups: others.clone().into(),
                            },
                        },
                    ),
                    ScopeWriteOutcome::Applied
                );
            }
            Ok(())
        });
    }
}

/// Another writer stores between the pass's read and its store: the pass
/// writes nothing, and the next one schedules from the other writer's scope.
#[tokio::test]
async fn a_superseded_store_is_not_retried() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    joined.forget(&e);
    let mut refresh = joined.refresh(now, true).await;
    let others = vec![group(
        TENANT,
        BarrierMode::Respect,
        now + Duration::from_secs(100),
    )];
    joined.store_first_on_the_candidate_query(others.clone());

    let first = pass(&mut refresh).await;

    assert_eq!(first, Directive::proceed());
    assert_eq!(*joined.held(), scope(2, others.clone()));
    joined.hook.arm(|_| Ok(()));
    joined.clear_recorded();
    let second = pass(&mut refresh).await;
    assert_eq!(joined.recorded(), Vec::<Value>::new());
    assert_eq!(second, Directive::sleep(Duration::from_secs(88)));
    assert_eq!(*joined.held(), scope(2, others));
}

/// A refused group whose clear loses to another writer's store is asked
/// again on the very next pass, against the newer scope. Rows: E refuses
/// again (the newer scope is cleared), and E fails (a retry after 1 s, while
/// the newer scope serves).
#[tokio::test]
async fn a_refusal_that_loses_its_write_is_asked_again_at_once() {
    for refused_again in [true, false] {
        let joined = joined(json!([respect_interest(TENANT)])).await;
        let now = Instant::now();
        joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
        let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
        joined.forget(&e);
        joined.answer(|call, _| {
            if call == Call::E {
                Answer::Deny
            } else {
                Answer::Allow
            }
        });
        let mut refresh = joined.refresh(now, true).await;
        let others = vec![group(
            TENANT,
            BarrierMode::Respect,
            now + Duration::from_secs(100),
        )];
        joined.store_first_on_the_candidate_query(others.clone());

        let first = pass(&mut refresh).await;

        let row = format!("refused again: {refused_again}");
        assert_eq!(first, Directive::proceed(), "{row}");
        assert_eq!(*joined.held(), scope(2, others.clone()), "{row}");
        joined.hook.arm(|_| Ok(()));
        joined.clear_recorded();
        if !refused_again {
            joined.answer(|call, _| {
                if call == Call::E {
                    Answer::Fail
                } else {
                    Answer::Allow
                }
            });
        }
        let second = pass(&mut refresh).await;

        let (directive, held) = if refused_again {
            (Directive::idle(), None)
        } else {
            (
                Directive::sleep(Duration::from_secs(1)),
                Some(Arc::new(scope(2, others))),
            )
        };
        assert_eq!(joined.recorded(), sent(&[e]), "{row}");
        assert_eq!(second, directive, "{row}");
        assert_eq!(joined.harness.groups().scope_of(joined.id), held, "{row}");
    }
}

/// The stream is dropped while a pass's renewal waits on its candidate
/// query: the renewal sees the cancel once its candidate query returns and
/// asks no E, and the pass ends at the cancel, so nothing is written,
/// whether E would allow or deny, and it idles.
#[tokio::test]
async fn a_cancelled_pass_writes_nothing() {
    for answer in [Answer::Allow, Answer::Deny] {
        let joined = joined(json!([respect_interest(TENANT)])).await;
        let now = Instant::now();
        let held = joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
        let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
        joined.forget(&e);
        joined.answer(move |call, _| {
            if call == Call::E {
                answer
            } else {
                Answer::Allow
            }
        });
        let mut refresh = joined.refresh(now, true).await;
        let cancel = CancellationToken::new();
        let dropped = cancel.clone();
        joined.hook.arm(move |query| {
            if query == SpecQuery::EventTypeCandidates {
                dropped.cancel();
            }
            Ok(())
        });

        let directive = pass_with(&mut refresh, &cancel).await;

        let row = format!("{answer:?}");
        let kept = joined.harness.groups().scope_of(joined.id);
        assert!(
            kept.as_ref().is_some_and(|kept| Arc::ptr_eq(kept, &held)),
            "{row}: {kept:?}"
        );
        assert_eq!(joined.recorded(), Vec::<Value>::new(), "{row}");
        assert_eq!(directive, Directive::idle(), "{row}");
    }
}

/// Two groups are due, and the stream is dropped while their renewals wait
/// on their candidate queries: a renewal sees the cancel once its candidate
/// query returns and asks no E, and the pass drops the renewals still
/// running, so neither asks E, nothing is written and the pass idles.
#[tokio::test]
async fn a_cancelled_pass_asks_no_further_group() {
    let joined = joined(json!([
        respect_interest(TENANT),
        respect_interest(TENANT_B)
    ]))
    .await;
    let now = Instant::now();
    let held = joined.store(vec![
        group(TENANT, BarrierMode::Respect, now + LEAD - MS),
        group(TENANT_B, BarrierMode::Respect, now + LEAD - MS),
    ]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    let e_b = e_request(&joined.enforcer, TENANT_B, PdpBarrierMode::Respect);
    joined.forget(&e);
    joined.forget(&e_b);
    joined.answer(|call, root| {
        if call == Call::E && root == Some(TENANT) {
            Answer::Fail
        } else {
            Answer::Allow
        }
    });
    let mut refresh = joined.refresh(now, true).await;
    let cancel = CancellationToken::new();
    let dropped = cancel.clone();
    joined.hook.arm(move |query| {
        if query == SpecQuery::EventTypeCandidates {
            dropped.cancel();
        }
        Ok(())
    });

    let directive = pass_with(&mut refresh, &cancel).await;

    assert_eq!(joined.recorded(), Vec::<Value>::new());
    assert_eq!(directive, Directive::idle());
    assert!(Arc::ptr_eq(&joined.held(), &held));
}

/// One group is due, and the pass's cancel comes while its renewal waits on
/// the candidate query. The renewal runs with no pass `select!` to drop it,
/// so only its own race against the cancel stops it: it ends cancelled once
/// the query returns, and asks no E.
///
/// When the query returns, the cancel is already set and the evaluation
/// would send E at its first poll, so only the turn's `biased` order keeps E
/// unasked on every run. Unbiased, `select!` starts at a random one of its
/// two arms and asks E half the time, so the scenario runs `RUNS` (20) times
/// with fresh state: no E on every run comes at about 1 in 1,000,000.
#[tokio::test]
async fn a_renewal_that_finds_its_pass_cancelled_after_the_candidate_query_asks_no_pdp() {
    const RUNS: usize = 20;
    for run in 0..RUNS {
        let joined = joined(json!([respect_interest(TENANT)])).await;
        let now = Instant::now();
        let held = joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
        let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
        joined.forget(&e);
        let refresh = joined.refresh(now, true).await;
        let cancel = CancellationToken::new();
        let dropped = cancel.clone();
        joined.hook.arm(move |query| {
            if query == SpecQuery::EventTypeCandidates {
                dropped.cancel();
            }
            Ok(())
        });
        let mut running = JoinSet::new();
        let mut in_flight = vec![false; refresh.refreshing.len()];

        refresh.start_due(&held, now, &mut running, &mut in_flight, &cancel);
        let ended = tokio::time::timeout(BOUND, running.join_next())
            .await
            .expect("the renewal ends within the bound");

        let Some(Ok(end)) = ended else {
            panic!("run {run}: one renewal ran and ended");
        };
        assert_eq!(joined.recorded(), Vec::<Value>::new(), "run {run}");
        assert!(
            matches!(end.step, Step::Cancelled),
            "run {run}: the turn ends cancelled"
        );
        assert_eq!(end.k, 0, "run {run}");
    }
}

/// An `ignore` group is due, and the pass's cancel comes while its renewal
/// waits on I; the gate then lets I on to an allowing answer. The renewal
/// runs with no pass `select!` to drop it, so only its own race against the
/// cancel stops it: it ends cancelled before it takes I's answer, and asks
/// no E.
#[tokio::test]
async fn a_renewal_cancelled_while_its_barrier_right_is_asked_asks_no_consume_scope() {
    let joined = joined(json!([ignore_interest(TENANT)])).await;
    let now = Instant::now();
    let held = joined.store(vec![group(
        TENANT,
        BarrierMode::Ignore,
        now + IGNORE_LEAD - MS,
    )]);
    let i = i_request(&joined.enforcer, TENANT);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Ignore);
    joined.forget(&i);
    joined.forget(&e);
    let refresh = joined.refresh(now, true).await;
    let cancel = CancellationToken::new();
    *joined.gate.holds.lock().unwrap() = Call::I;
    joined.gate.armed.store(true, Ordering::SeqCst);
    let mut running = JoinSet::new();
    let mut in_flight = vec![false; refresh.refreshing.len()];

    refresh.start_due(&held, now, &mut running, &mut in_flight, &cancel);
    tokio::time::timeout(BOUND, joined.gate.entered.notified())
        .await
        .expect("I reaches the PDP within the bound");
    cancel.cancel();
    joined.gate.released.notify_one();
    let ended = tokio::time::timeout(BOUND, running.join_next())
        .await
        .expect("the renewal ends within the bound");

    let Some(Ok(end)) = ended else {
        panic!("one renewal ran and ended");
    };
    assert_eq!(joined.recorded(), sent(&[i]));
    assert!(
        matches!(end.step, Step::Cancelled),
        "the turn ends cancelled"
    );
    assert_eq!(end.k, 0);
    assert_eq!(end.fresh.as_deref(), Some(&listed()));
}

/// The `finish` rows for a turn handed back after the pass's cancel: one
/// that ended cancelled, a renewal and a refusal. Each ends the pass idle
/// and writes nothing, neither the member's scope nor the group's retry.
///
/// A renewal or a refusal gets here when its turn took the answer before it
/// saw the cancel: on a multi-thread runtime the pass can poll its cancel
/// arm just before the cancel comes, then join that turn, and only
/// `finish`'s own checks keep its write out.
#[tokio::test]
async fn a_turn_finished_after_the_cancel_ends_the_pass_writing_nothing() {
    type StepAt = fn(Instant) -> Step;
    let rows: [(&str, StepAt); 3] = [
        ("cancelled", |_| Step::Cancelled),
        ("renewed", |now| {
            Step::Renewed(group(TENANT, BarrierMode::Respect, now + TTL))
        }),
        ("revoked", |_| Step::Revoked),
    ];
    for (row, step) in rows {
        let joined = joined(json!([respect_interest(TENANT)])).await;
        let now = Instant::now();
        let stored = joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
        let mut refresh = joined.refresh(now, true).await;
        let [refreshing] = refresh.refreshing.as_mut_slice() else {
            panic!("{row}: one group");
        };
        refreshing.retry = Some(Retry {
            at: now + Duration::from_secs(1),
            backoff: Duration::from_secs(1),
        });
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut held = Arc::clone(&stored);
        let end = TurnEnd {
            k: 0,
            fresh: None,
            step: step(now),
        };

        let flow = refresh.finish(end, &mut held, &cancel);

        assert_eq!(flow, ControlFlow::Break(Directive::idle()), "{row}");
        assert!(Arc::ptr_eq(&held, &stored), "{row}: {held:?}");
        let member = joined.harness.groups().scope_of(joined.id);
        assert!(
            member
                .as_ref()
                .is_some_and(|member| Arc::ptr_eq(member, &stored)),
            "{row}: {member:?}"
        );
        let [refreshing] = refresh.refreshing.as_slice() else {
            panic!("{row}: one group");
        };
        assert_eq!(
            refreshing.retry.map(|retry| (retry.at, retry.backoff)),
            Some((now + Duration::from_secs(1), Duration::from_secs(1))),
            "{row}"
        );
        assert_eq!(refreshing.listed.as_deref(), Some(&listed()), "{row}");
    }
}

/// Two groups due in one pass: the first is renewed from a fresher memo
/// entry, without a call, and the second's E waits on the gate. While it
/// waits, the member holds the first group's new entry; the second's
/// follows once E answers. Both renewals start together, so the second can
/// reach its E before the first is stored: the read waits for that store.
#[tokio::test]
async fn a_group_renewed_early_in_a_pass_is_visible_before_the_pass_ends() {
    let joined = joined(json!([
        respect_interest(TENANT),
        respect_interest(TENANT_B)
    ]))
    .await;
    let now = Instant::now();
    let due = now + LEAD - MS;
    joined.store(vec![
        group(TENANT, BarrierMode::Respect, due),
        group(TENANT_B, BarrierMode::Respect, due),
    ]);
    let e = e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect);
    let e_b = e_request(&joined.enforcer, TENANT_B, PdpBarrierMode::Respect);
    let until = now + Duration::from_secs(30);
    joined.remember(&e, MemoResult::Scope(kept_scope(TENANT)), until);
    joined.forget(&e_b);
    let mut refresh = joined.refresh(now, true).await;
    joined.gate.armed.store(true, Ordering::SeqCst);
    let refreshing = tokio::spawn(async move { pass(&mut refresh).await });
    tokio::time::timeout(BOUND, joined.gate.entered.notified())
        .await
        .expect("the refresh reaches the second group's E within the bound");
    let mut feed = joined.harness.groups().watch_scope(joined.id).unwrap();
    tokio::time::timeout(
        BOUND,
        feed.wait_for(|held| {
            held.as_ref()
                .is_some_and(|held| held.generation == ScopeGeneration::nth(2))
        }),
    )
    .await
    .expect("the first group is stored within the bound")
    .expect("the member stays");

    let while_waiting = joined.held();

    joined.gate.armed.store(false, Ordering::SeqCst);
    joined.gate.released.notify_one();
    let directive = tokio::time::timeout(BOUND, refreshing)
        .await
        .expect("the refresh ends within the bound")
        .unwrap();
    assert_eq!(
        *while_waiting,
        scope(
            2,
            vec![
                group(TENANT, BarrierMode::Respect, until),
                group(TENANT_B, BarrierMode::Respect, due),
            ]
        )
    );
    assert_eq!(
        *joined.held(),
        scope(
            3,
            vec![
                group(TENANT, BarrierMode::Respect, until),
                group(TENANT_B, BarrierMode::Respect, now + TTL),
            ]
        )
    );
    assert_eq!(joined.recorded(), sent(&[e_b]));
    // The first group is next, one lead before the memo entry it reused ends.
    assert_eq!(directive, Directive::sleep(Duration::from_secs(18)));
}

#[tokio::test]
async fn a_gone_member_idles_the_worker() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let now = Instant::now();
    joined.store(vec![group(TENANT, BarrierMode::Respect, now + LEAD - MS)]);
    let mut refresh = joined.refresh(now, true).await;
    assert!(
        joined
            .harness
            .groups()
            .leave(&joined.group, joined.id)
            .is_some()
    );

    let directive = pass(&mut refresh).await;

    assert_eq!(directive, Directive::idle());
    assert_eq!(joined.recorded(), Vec::<Value>::new());
}

/// Three groups: one due in half a second, one failing (retry in 1 s) and
/// one due in 18 s. The pass sleeps until the earliest.
#[tokio::test]
async fn the_next_wakeup_is_the_earliest_refresh_or_retry() {
    let joined = joined(json!([
        respect_interest(TENANT),
        respect_interest(TENANT_B),
        respect_interest(TENANT_C),
    ]))
    .await;
    let now = Instant::now();
    joined.store(vec![
        group(
            TENANT,
            BarrierMode::Respect,
            now + LEAD + Duration::from_millis(500),
        ),
        group(TENANT_B, BarrierMode::Respect, now + LEAD),
        group(
            TENANT_C,
            BarrierMode::Respect,
            now + Duration::from_secs(30),
        ),
    ]);
    let e_b = e_request(&joined.enforcer, TENANT_B, PdpBarrierMode::Respect);
    joined.forget(&e_b);
    joined.answer(|call, root| {
        if call == Call::E && root == Some(TENANT_B) {
            Answer::Fail
        } else {
            Answer::Allow
        }
    });
    let mut refresh = joined.refresh(now, true).await;

    let directive = pass(&mut refresh).await;

    assert_eq!(joined.recorded(), sent(&[e_b]));
    assert_eq!(directive, Directive::sleep(Duration::from_millis(500)));
}

// -- the worker of an open stream --

/// The worker is the stream's: dropping the stream cancels it, which drops
/// its action.
#[tokio::test]
async fn dropping_the_stream_cancels_its_refresh_worker() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let stream = joined.open().await;
    assert_eq!(ScopeRefresh::live_for(joined.id), 1);

    drop(stream);

    tokio::time::timeout(BOUND, async {
        while ScopeRefresh::live_for(joined.id) > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the worker ends within the bound");
}

/// A refresh held inside its E does not hold delivery: an event published
/// while it waits is delivered from the current scope.
#[tokio::test]
async fn delivery_never_waits_on_a_refresh() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let mut stream = joined.open().await;
    let first = Uuid::from_u128(0x0190_f1c2_7a3b_7c4d_8e5f_6a7b_8c9d_0e01);
    joined.publish(first).await;
    assert_eq!(next_event(&mut stream).await, first);
    let held = joined.held();
    let due = held.groups[0].expires_at - LEAD;
    let mut refresh = joined.refresh(due, true).await;
    joined.gate.armed.store(true, Ordering::SeqCst);
    let refreshing = tokio::spawn(async move { pass(&mut refresh).await });
    tokio::time::timeout(BOUND, joined.gate.entered.notified())
        .await
        .expect("the refresh reaches E within the bound");

    let second = Uuid::from_u128(0x0190_f1c2_7a3b_7c4d_8e5f_6a7b_8c9d_0e02);
    tokio::time::timeout(BOUND, joined.publish(second))
        .await
        .expect("the publish answers within the bound");
    assert_eq!(next_event(&mut stream).await, second);
    assert!(!refreshing.is_finished(), "the refresh still waits on E");

    joined.gate.armed.store(false, Ordering::SeqCst);
    joined.gate.released.notify_one();
    let directive = tokio::time::timeout(BOUND, refreshing)
        .await
        .expect("the refresh ends within the bound")
        .unwrap();
    assert_eq!(directive, Directive::sleep(RENEWED_DUE_IN));
    assert_eq!(joined.held().generation, ScopeGeneration::nth(2));
}

#[tokio::test]
async fn a_refresh_that_clears_tears_the_open_stream_down() {
    let joined = joined(json!([respect_interest(TENANT)])).await;
    let mut stream = joined.open().await;
    let Some(Frame::Topology { positions, .. }) = tokio::time::timeout(BOUND, stream.next())
        .await
        .expect("the topology frame arrives within the bound")
    else {
        panic!("the stream does not start with its topology");
    };
    let held = joined.held();
    let mut refresh = joined.refresh(held.groups[0].expires_at - LEAD, true).await;
    joined.answer(|call, _| {
        if call == Call::E {
            Answer::Deny
        } else {
            Answer::Allow
        }
    });

    assert_eq!(pass(&mut refresh).await, Directive::idle());

    // Bounded as a whole: heartbeats keep arriving while the stream lives.
    let terminal = tokio::time::timeout(BOUND, async {
        loop {
            match stream.next().await {
                Some(Frame::Heartbeat { .. }) => {}
                other => break other,
            }
        }
    })
    .await
    .expect("the stream ends within the bound");
    let Some(Frame::Control {
        code: ControlCode::Terminal,
        positions: at_close,
        reason: Some(CloseReason::Teardown),
    }) = terminal
    else {
        panic!("expected a teardown terminal frame, got {terminal:?}");
    };
    assert_eq!(at_close, positions);
    assert!(
        tokio::time::timeout(BOUND, stream.next())
            .await
            .expect("the stream ends within the bound")
            .is_none()
    );
}

/// The production worker in real time: with a 5 s TTL and a 1 s deadline a
/// `respect` group's lead is 3 s, so the open's scope (generation 1, expiring
/// 5 s after the JOIN's E answer it reused) is refreshed about 2 s after the
/// JOIN and the member holds generation 2 before the first window ends.
///
/// Margin: the refresh is due 3 s before the expiry and takes one candidate
/// query and one E from an in-process PDP (milliseconds), so it lands about
/// 3 s ahead of the expiry asserted against; the wait for it is bounded by
/// the full 5 s window plus `BOUND`.
#[tokio::test]
async fn a_stream_is_refreshed_ahead_of_expiry() {
    let joined = joined_with(json!([respect_interest(TENANT)]), (5, 1)).await;
    let mut feed = joined.harness.groups().watch_scope(joined.id).unwrap();
    let _stream = joined.open().await;
    let opened = joined.held();
    assert_eq!(opened.generation, ScopeGeneration::nth(1));
    let first_expiry = opened.groups[0].expires_at;
    feed.borrow_and_update();
    joined.clear_recorded();

    let refreshed = tokio::time::timeout(Duration::from_secs(5) + BOUND, async {
        loop {
            feed.changed().await.expect("the member stays");
            if let Some(scope) = feed.borrow_and_update().clone() {
                break (scope, Instant::now());
            }
        }
    })
    .await
    .expect("the scope is refreshed within the window and the bound");

    let (scope, seen_at) = refreshed;
    assert_eq!(scope.generation, ScopeGeneration::nth(2));
    assert!(
        seen_at < first_expiry,
        "the refresh lands before the first window ends"
    );
    assert!(scope.groups[0].expires_at > first_expiry);
    assert_eq!(
        joined.recorded(),
        sent(&[e_request(&joined.enforcer, TENANT, PdpBarrierMode::Respect)])
    );
}
