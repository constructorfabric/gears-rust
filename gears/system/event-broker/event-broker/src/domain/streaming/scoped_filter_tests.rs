//! Delivery's consume decision per event, against a hand-built member scope.
//!
//! Each filter is built over one wildcard interest on `TOPIC` in `TENANT`
//! unless a test says otherwise, with `CREATED` and `PLACED` as the group's
//! candidates. `SHIPPED`, `RETURNED` and `ARCHIVED` are outside them, so each
//! needs a single-type evaluation; `ARCHIVED` is not registered. The PDP
//! double records every request and answers by the request's one event type.
//! The caller sits in tenant `0x123`; the interest names `0x234`.
//!
//! The doubles and builders are public to the crate because the session
//! tests open sessions over the same filter.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, BarrierMode as PdpBarrierMode, Constraint, EqPredicate, EvaluationRequest,
    EvaluationResponse, EvaluationResponseContext, InPredicate, PolicyEnforcer, Predicate,
    TenantMode,
};
use chrono::Utc;
use event_broker_sdk::models::EventType;
use gts::{GtsIdPattern, GtsTypeId};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::Instant;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{
    AccessScope, PlatformSecurityContext, ScopeConstraint, ScopeFilter, ScopeValue,
    SecurityContext, pep_properties,
};
use uuid::Uuid;

use super::{
    PLAN_CAP, SELECTOR_CACHE_CAP, SINGLE_CAP, SINGLE_TYPE_CONCURRENCY, ScopedFilter,
    SingleTypeAuthorizer,
};
use crate::domain::backend::BackendResolver;
use crate::domain::consumer_group_coordinator::member_scope::{
    AuthorizedGroup, CachedScope, ScopeGeneration, ScopePrincipal,
};
use crate::domain::error::DomainError;
use crate::domain::join_scope::GroupKey;
use crate::domain::model::{BarrierMode, Event, Interest, OwnershipFilter, Sequence, Topic};
use crate::domain::specification::{
    EventTypeCandidates, ProvisioningLimits, SpecificationManager, TopicState,
};
use crate::domain::streaming::filter::{EventFilter, Prepared};
use crate::domain::streaming::read::{EventBatch, EventSlice};
use crate::domain::streaming::scope::{CompiledScope, EVENT_TYPE, TenantSetInterner};
use crate::test_support::{SpecQuery, SpecQueryHook};

pub const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.scoped.orders.v1";
pub const FAMILY: &str = "gts.cf.core.events.event.v1~x.eb.scoped.*";
pub const CREATED: &str = "gts.cf.core.events.event.v1~x.eb.scoped.created.v1~";
pub const PLACED: &str = "gts.cf.core.events.event.v1~x.eb.scoped.placed.v1~";
pub const SHIPPED: &str = "gts.cf.core.events.event.v1~x.eb.scoped.shipped.v1~";
pub const RETURNED: &str = "gts.cf.core.events.event.v1~x.eb.scoped.returned.v1~";
pub const ARCHIVED: &str = "gts.cf.core.events.event.v1~x.eb.scoped.archived.v1~";
pub const ORDER: &str = "gts.x.eb.scoped.order.v1~";
pub const INVOICE: &str = "gts.x.eb.scoped.invoice.v1~";
/// A base subject type.
const CUSTOMER: &str = "gts.x.eb.scoped.customer.v1~";
/// A subject type derived from `CUSTOMER`, under the `acme` vendor.
const LOYALTY_MEMBER: &str = "gts.x.eb.scoped.customer.v1~acme.retail.loyalty.member.v1~";
/// A subject type derived from `CUSTOMER`, under another vendor than `acme`.
const PARTNER_MEMBER: &str = "gts.x.eb.scoped.customer.v1~globex.retail.loyalty.member.v1~";
/// A subject type outside the `CUSTOMER` subtree.
const SUPPLIER: &str = "gts.x.eb.scoped.supplier.v1~";

const SUBJECT: Uuid = Uuid::from_u128(0x456);
const CALLER_TENANT: Uuid = Uuid::from_u128(0x123);
pub const TENANT: Uuid = Uuid::from_u128(0x234);

/// The production default of `streaming.authz_eval_deadline_secs`, for the
/// tests that do not wait it out.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// Bounds every await on held work, so a fan-out that never finishes fails
/// the test instead of hanging it.
const BOUND: Duration = Duration::from_secs(5);

/// What the PDP double answers one single-type E.
#[derive(Debug, Clone, Copy)]
pub enum Answer {
    /// Allowed, with `In(owner_tenant_id, [root_id])`, as tr-authz answers.
    Allow,
    /// Allowed, but only for `CREATED`: `In(event_type, [CREATED])`.
    AllowCreatedOnly,
    /// Denied.
    Deny,
    /// Allowed with a scope that matches nothing, `In(owner_tenant_id, [])`,
    /// which the enforcer refuses as a denial.
    AllowNothing,
    /// Allowed with a constraint on a property the request does not declare,
    /// which the enforcer cannot compile.
    Uncompilable,
    /// Allowed with a type value that is not a GTS type id, which compiles
    /// at the enforcer and leaves no constraint the broker can enforce.
    Unenforceable,
    /// The evaluation fails, as an unreachable PDP does.
    Fail,
    /// Never answers.
    Never,
    /// Answers as [`Answer::Allow`] after the given time on the runtime's
    /// clock.
    AllowAfter(Duration),
}

/// The PDP double's script: what to answer one request.
type Script = Box<dyn Fn(&EvaluationRequest) -> Answer + Send + Sync>;

/// A PDP that records every request and the `x-request-id` its task holds,
/// logs one line per call, answers it by its script, holds each call on a
/// gate until the test opens it, and counts the calls in flight.
pub struct TypePdp {
    answer: Script,
    recorded: Mutex<Vec<Value>>,
    request_ids: Mutex<Vec<Option<String>>>,
    gate: watch::Sender<bool>,
    arrived: watch::Sender<usize>,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

impl TypePdp {
    /// A double whose gate is open.
    pub fn answering(
        answer: impl Fn(&EvaluationRequest) -> Answer + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            answer: Box::new(answer),
            recorded: Mutex::new(Vec::new()),
            request_ids: Mutex::new(Vec::new()),
            gate: watch::Sender::new(true),
            arrived: watch::Sender::new(0),
            in_flight: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    /// A double that holds every call until [`Self::open`].
    fn held(answer: impl Fn(&EvaluationRequest) -> Answer + Send + Sync + 'static) -> Arc<Self> {
        let pdp = Self::answering(answer);
        pdp.gate.send_replace(false);
        pdp
    }

    fn open(&self) {
        self.gate.send_replace(true);
    }

    /// Every request so far, serialized.
    pub fn recorded(&self) -> Vec<Value> {
        self.recorded.lock().unwrap().clone()
    }

    /// The `x-request-id` each call's task held, in arrival order.
    fn request_ids(&self) -> Vec<Option<String>> {
        self.request_ids.lock().unwrap().clone()
    }

    /// The one event type of each request so far, sorted.
    pub fn asked_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self
            .recorded()
            .iter()
            .map(|request| {
                let [event_type] = request["resource"]["properties"]["event_types"]
                    .as_array()
                    .expect("a single-type E names its types")
                    .as_slice()
                else {
                    panic!("a single-type E names one type: {request}");
                };
                event_type.as_str().unwrap().to_owned()
            })
            .collect();
        types.sort();
        types
    }

    /// Waits until `count` calls have arrived.
    async fn arrivals(&self, count: usize) {
        self.arrived
            .subscribe()
            .wait_for(|arrived| *arrived >= count)
            .await
            .expect("the double holds its own sender");
    }

    fn arrived(&self) -> usize {
        *self.arrived.borrow()
    }

    fn enforcer(self: &Arc<Self>) -> PolicyEnforcer {
        PolicyEnforcer::new(Arc::clone(self) as Arc<dyn AuthZResolverApi>)
    }
}

#[async_trait]
impl AuthZResolverApi for TypePdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.recorded
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        self.request_ids
            .lock()
            .unwrap()
            .push(toolkit::api::current_request_id());
        tracing::info!("the policy double answers a single-type evaluation");
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        self.arrived.send_modify(|arrived| *arrived += 1);
        let opened = self.gate.subscribe().wait_for(|open| *open).await.is_ok();
        assert!(opened, "the double holds its own gate");
        let answer = match (self.answer)(&request) {
            Answer::AllowAfter(delay) => {
                tokio::time::sleep(delay).await;
                Answer::Allow
            }
            answer => answer,
        };
        if !matches!(answer, Answer::Never) {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
        let allowed = |predicate: Predicate| EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![predicate],
                }],
                ..Default::default()
            },
        };
        let root = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tenant_context| tenant_context.root_id)
            .expect("a single-type E is rooted at the interest tenant");
        Ok(match answer {
            Answer::Allow => allowed(Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                [root],
            ))),
            Answer::AllowCreatedOnly => {
                allowed(Predicate::In(InPredicate::new(EVENT_TYPE, [CREATED])))
            }
            Answer::Deny => EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext::default(),
            },
            Answer::AllowNothing => allowed(Predicate::In(InPredicate::new(
                pep_properties::OWNER_TENANT_ID,
                Vec::<Uuid>::new(),
            ))),
            Answer::Uncompilable => allowed(Predicate::Eq(EqPredicate::new("subject", SUBJECT))),
            Answer::Unenforceable => {
                allowed(Predicate::In(InPredicate::new(EVENT_TYPE, ["not-a-type"])))
            }
            Answer::Fail => {
                return Err(CanonicalError::service_unavailable().create());
            }
            Answer::Never => std::future::pending().await,
            Answer::AllowAfter(_) => unreachable!("answered as `Allow` above"),
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// A specification manager that knows only the event types it was given.
/// Delivery reads nothing else from it.
pub struct KnownTypes(HashMap<String, EventType>);

impl KnownTypes {
    /// `SHIPPED` allowing `ORDER` and `INVOICE`, and `RETURNED` allowing
    /// `ORDER`, on `TOPIC`, plus `extra` registered on `TOPIC` allowing
    /// `ORDER`.
    pub fn on_topic(extra: &[&str]) -> Arc<dyn SpecificationManager> {
        let registered = [(SHIPPED, vec![INVOICE, ORDER]), (RETURNED, vec![ORDER])]
            .into_iter()
            .chain(extra.iter().map(|id| (*id, vec![ORDER])))
            .map(|(id, allowed)| {
                (
                    id.to_owned(),
                    EventType {
                        id: GtsTypeId::new(id),
                        topic: GtsInstanceId::try_new(TOPIC).unwrap(),
                        description: None,
                        allowed_subject_types: allowed
                            .into_iter()
                            .map(|raw| GtsIdPattern::try_new(raw).unwrap())
                            .collect(),
                        partition_key: "/subject".to_owned(),
                        data_schema: json!({}),
                    },
                )
            })
            .collect();
        Arc::new(Self(registered))
    }
}

#[async_trait]
impl SpecificationManager for KnownTypes {
    async fn get_topic(&self, _id: &GtsInstanceId) -> Result<Option<Topic>, DomainError> {
        Ok(None)
    }

    async fn get_event_type(&self, id: &GtsTypeId) -> Result<Option<EventType>, DomainError> {
        Ok(self.0.get(id.as_ref()).cloned())
    }

    async fn get_event_type_candidates(
        &self,
        _topic: &GtsInstanceId,
    ) -> Result<EventTypeCandidates, DomainError> {
        Ok(EventTypeCandidates::Listed(Vec::new()))
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
        Ok(self.0.values().cloned().collect())
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

pub fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(CALLER_TENANT)
        .build()
        .expect("valid security context")
}

pub fn interest(topic: &str, types: &[&str], barrier_mode: BarrierMode) -> Interest {
    Interest {
        topic: GtsInstanceId::try_new(topic).unwrap(),
        subject_tenant: TENANT,
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode,
        types: types
            .iter()
            .map(|raw| GtsIdPattern::try_new(raw).unwrap())
            .collect(),
        subject_types: Vec::new(),
        filter: None,
    }
}

/// A root-only scope over `TENANT`, holding `filters` too in its one
/// constraint, compiled as E's answer is.
pub fn tenant_scope(filters: Vec<ScopeFilter>) -> Arc<CompiledScope> {
    let mut all = vec![ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, TENANT)];
    all.extend(filters);
    Arc::new(
        CompiledScope::compile(
            &AccessScope::single(ScopeConstraint::new(all)),
            &TenantMode::RootOnly,
            &TenantSetInterner::new(),
        )
        .expect("the scope compiles"),
    )
}

/// The authorized group of `interest`'s key, with `scope` and `candidates`.
pub fn group_of(
    interest: &Interest,
    scope: Arc<CompiledScope>,
    candidates: &[&str],
) -> AuthorizedGroup {
    AuthorizedGroup {
        key: GroupKey::new(interest, CALLER_TENANT),
        scope,
        candidates: Arc::new(candidates.iter().map(|id| GtsTypeId::new(id)).collect()),
        candidates_stale: false,
        expires_at: Instant::now() + Duration::from_mins(1),
    }
}

/// The member scope of `groups`, as the coordinator holds it under
/// `generation`.
pub fn held_scope(generation: u64, groups: Vec<AuthorizedGroup>) -> Arc<CachedScope> {
    Arc::new(CachedScope {
        generation: ScopeGeneration::nth(generation),
        principal: ScopePrincipal::from(&caller()),
        groups: groups.into(),
    })
}

pub fn authorizer(
    pdp: &Arc<TypePdp>,
    specs: Arc<dyn SpecificationManager>,
    deadline: Duration,
) -> SingleTypeAuthorizer {
    SingleTypeAuthorizer::new(
        pdp.enforcer(),
        specs,
        Arc::new(TenantSetInterner::new()),
        caller(),
        deadline,
    )
}

/// The usual interest: `FAMILY` on `TOPIC`, respecting barriers.
pub fn wildcard_interest() -> Interest {
    interest(TOPIC, &[FAMILY], BarrierMode::Respect)
}

/// The usual member scope under `generation`: the wildcard interest's group
/// with `CREATED` and `PLACED` as candidates, admitting every type in
/// `TENANT`.
pub fn wildcard_scope(generation: u64) -> Arc<CachedScope> {
    held_scope(
        generation,
        vec![group_of(
            &wildcard_interest(),
            tenant_scope(Vec::new()),
            &[CREATED, PLACED],
        )],
    )
}

/// The usual filter: the wildcard interest over [`wildcard_scope`].
fn wildcard_filter(pdp: &Arc<TypePdp>) -> ScopedFilter {
    ScopedFilter::new(
        &[wildcard_interest()],
        wildcard_scope(1),
        authorizer(pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .expect("the scope holds the interest's group")
}

pub fn event_of(topic: &str, event_type: &str, sequence: i64) -> Event {
    Event {
        id: Uuid::from_u128(u128::try_from(sequence).unwrap()),
        r#type: GtsTypeId::new(event_type),
        topic: GtsInstanceId::try_new(topic).unwrap(),
        subject_tenant: TENANT,
        source: "scoped-filter-test".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: GtsTypeId::new(ORDER),
        subject_owner: None,
        occurred_at: Utc::now(),
        trace_parent: None,
        data: json!({}),
        meta: None,
        ingest: None,
        partition: Some(0),
        sequence: Some(Sequence::assigned(sequence)),
        sequence_time: None,
    }
}

fn event(event_type: &str, sequence: i64) -> Event {
    event_of(TOPIC, event_type, sequence)
}

pub fn batch(events: Vec<Event>) -> EventBatch {
    let held = events.len();
    let frontier = events
        .iter()
        .filter_map(|event| event.sequence)
        .max()
        .unwrap_or(Sequence::assigned(0));
    EventBatch::from_runs(vec![
        EventSlice::builder(events.into())
            .range(0..held)
            .frontier(frontier)
            .build(),
    ])
}

/// Prepares `events`, which one wave decides whole, and decides each, in
/// order, as the session does.
async fn decide(filter: &ScopedFilter, events: Vec<Event>) -> Vec<bool> {
    let events = batch(events);
    let prepared = tokio::time::timeout(BOUND, filter.prepare(&events))
        .await
        .expect("prepare finishes")
        .expect("prepare succeeds");
    assert_eq!(prepared, Prepared::All);
    events
        .iter()
        .map(|event| filter.admits(event).expect("a prepared event is decided"))
        .collect()
}

/// `count` distinct event types under `FAMILY`, sorted.
pub fn many_types(count: usize) -> Vec<String> {
    (0..count)
        .map(|n| format!("gts.cf.core.events.event.v1~x.eb.scoped.t{n:04}.v1~"))
        .collect()
}

/// The full single-type E for `SHIPPED`, as the PDP receives it.
fn shipped_request() -> Value {
    json!({
        "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
        "action": { "name": "consume" },
        "resource": {
            "type": TOPIC,
            "properties": {
                "topic_id": TOPIC,
                "owner_tenant_id": TENANT,
                "event_type_patterns": [FAMILY],
                "event_types": [SHIPPED],
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
            "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
        },
    })
}

#[tokio::test]
async fn a_type_outside_the_candidates_is_evaluated_once_per_generation() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);

    let decided = decide(
        &filter,
        vec![event(SHIPPED, 1), event(SHIPPED, 2), event(SHIPPED, 3)],
    )
    .await;

    assert_eq!(decided, vec![true, true, true]);
    assert_eq!(pdp.recorded(), vec![shipped_request()]);

    let again = decide(&filter, vec![event(SHIPPED, 4)]).await;
    assert_eq!(again, vec![true]);
    assert_eq!(pdp.recorded().len(), 1);
}

#[tokio::test]
async fn alternating_types_cost_one_evaluation_each() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);

    let decided = decide(
        &filter,
        vec![
            event(SHIPPED, 1),
            event(RETURNED, 2),
            event(SHIPPED, 3),
            event(RETURNED, 4),
        ],
    )
    .await;

    assert_eq!(decided, vec![true, true, true, true]);
    assert_eq!(pdp.asked_types(), vec![RETURNED, SHIPPED]);
}

#[tokio::test]
async fn a_point_read_answering_none_sends_no_subject_type_candidates() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);

    let decided = decide(&filter, vec![event(ARCHIVED, 1)]).await;

    assert_eq!(decided, vec![true]);
    assert_eq!(
        pdp.recorded(),
        vec![json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "consume" },
            "resource": {
                "type": TOPIC,
                "properties": {
                    "topic_id": TOPIC,
                    "owner_tenant_id": TENANT,
                    "event_type_patterns": [FAMILY],
                    "event_types": [ARCHIVED],
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
                "supported_properties": ["owner_tenant_id", "owner_id", "event_type", "event_subject_type"],
            },
        })]
    );
}

/// Every E is held until all three have arrived, which only a concurrent
/// fan-out reaches; then the gate opens and the one `prepare` returns.
#[tokio::test]
async fn a_batch_evaluates_its_new_types_concurrently_under_one_deadline() {
    let pdp = TypePdp::held(|_| Answer::Allow);
    let filter = Arc::new(wildcard_filter(&pdp));
    let events = batch(vec![
        event(SHIPPED, 1),
        event(RETURNED, 2),
        event(ARCHIVED, 3),
    ]);

    let preparing = tokio::spawn({
        let filter = Arc::clone(&filter);
        async move { filter.prepare(&events).await }
    });
    tokio::time::timeout(BOUND, pdp.arrivals(3))
        .await
        .expect("all three E calls are in flight at once");
    pdp.open();
    tokio::time::timeout(BOUND, preparing)
        .await
        .expect("prepare finishes once the gate opens")
        .expect("prepare does not panic")
        .expect("prepare succeeds");

    assert_eq!(pdp.asked_types(), vec![ARCHIVED, RETURNED, SHIPPED]);
}

/// The filter is built as a stream opens, inside the opening request's
/// `x-request-id` scope and span, and `prepare` runs in a task outside both,
/// as a REST body polls the stream after the handler returned. The runtime
/// is multi-thread, so the evaluations run on worker threads, where no span
/// the test entered is current.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[tracing_test::traced_test]
async fn single_type_evaluations_run_under_the_openers_request_id_and_span() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let opened = toolkit::api::with_request_id(Some("req-stream-open".to_owned()), async {
        tracing::info_span!("stream_open", opener = "req-stream-open")
            .in_scope(|| wildcard_filter(&pdp))
    })
    .await;
    let filter = Arc::new(opened);
    let events = batch(vec![
        event(SHIPPED, 1),
        event(RETURNED, 2),
        event(ARCHIVED, 3),
    ]);

    let prepared = tokio::time::timeout(
        BOUND,
        tokio::spawn({
            let filter = Arc::clone(&filter);
            async move { filter.prepare(&events).await }
        }),
    )
    .await
    .expect("prepare finishes")
    .expect("prepare does not panic")
    .expect("prepare succeeds");

    assert_eq!(prepared, Prepared::All);
    assert_eq!(pdp.asked_types(), vec![ARCHIVED, RETURNED, SHIPPED]);
    assert_eq!(
        pdp.request_ids(),
        vec![Some("req-stream-open".to_owned()); 3]
    );
    logs_assert(|lines: &[&str]| {
        let answered = lines
            .iter()
            .filter(|line| {
                line.contains("stream_open{opener=\"req-stream-open\"}")
                    && line.contains("the policy double answers a single-type evaluation")
            })
            .count();
        if answered == 3 {
            Ok(())
        } else {
            Err(format!(
                "expected 3 policy calls logged in the opener's span, got {answered}"
            ))
        }
    });
}

/// The double holds every call. Once the bound is reached the test yields
/// 16 times, enough for any further spawned call to arrive, before it counts.
/// The batch is cut after the wave, before the first event of the sixth type.
#[tokio::test]
async fn the_fan_out_never_exceeds_its_concurrency_bound() {
    let pdp = TypePdp::held(|_| Answer::Allow);
    let filter = Arc::new(wildcard_filter(&pdp));
    let types = many_types(SINGLE_TYPE_CONCURRENCY + 2);
    let events = batch(
        types
            .iter()
            .zip(1..)
            .map(|(event_type, sequence)| event(event_type, sequence))
            .collect(),
    );

    let preparing = tokio::spawn({
        let filter = Arc::clone(&filter);
        async move { filter.prepare(&events).await }
    });
    tokio::time::timeout(BOUND, pdp.arrivals(SINGLE_TYPE_CONCURRENCY))
        .await
        .expect("the bound's worth of calls arrives");
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(pdp.arrived(), SINGLE_TYPE_CONCURRENCY);
    pdp.open();
    let prepared = tokio::time::timeout(BOUND, preparing)
        .await
        .expect("prepare finishes once the gate opens")
        .expect("prepare does not panic")
        .expect("prepare succeeds");

    assert_eq!(prepared, Prepared::UpTo(SINGLE_TYPE_CONCURRENCY));
    assert_eq!(pdp.peak.load(Ordering::SeqCst), SINGLE_TYPE_CONCURRENCY);
    assert_eq!(pdp.asked_types(), types[..SINGLE_TYPE_CONCURRENCY].to_vec());
}

/// A filter of one more subtree interest on `TOPIC` than a wave holds, each
/// in its own group with no candidates, so one `CREATED` event needs one
/// single-type evaluation per group.
fn more_subtree_groups_than_a_wave(pdp: &Arc<TypePdp>) -> ScopedFilter {
    let interests: Vec<Interest> = (1..=u128::try_from(SINGLE_TYPE_CONCURRENCY + 1).unwrap())
        .map(|root| {
            owning(
                wildcard_interest(),
                OwnershipFilter::TenantSubtreeIncluded,
                Uuid::from_u128(0x5000 + root),
            )
        })
        .collect();
    let scope = compiled(
        vec![ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, TENANT)],
        &TenantMode::Subtree,
    );
    ScopedFilter::new(
        &interests,
        held_scope(
            1,
            interests
                .iter()
                .map(|interest| group_of(interest, Arc::clone(&scope), &[]))
                .collect(),
        ),
        authorizer(pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .expect("the scope holds every interest's group")
}

/// Every subtree group on a topic can select one event, so a batch's first
/// event may need more single-type evaluations than a wave holds. It is
/// still decided by one `prepare`, never cut before it, with no more than
/// the bound running at once.
#[tokio::test]
async fn a_first_event_more_groups_select_than_a_wave_holds_is_decided_within_the_bound() {
    let pdp = TypePdp::held(|_| Answer::Allow);
    let filter = Arc::new(more_subtree_groups_than_a_wave(&pdp));
    let events = batch(vec![event(CREATED, 1)]);

    let preparing = tokio::spawn({
        let filter = Arc::clone(&filter);
        async move { filter.prepare(&events).await }
    });
    tokio::time::timeout(BOUND, pdp.arrivals(SINGLE_TYPE_CONCURRENCY))
        .await
        .expect("the bound's worth of calls arrives");
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(pdp.arrived(), SINGLE_TYPE_CONCURRENCY);
    pdp.open();
    let prepared = tokio::time::timeout(BOUND, preparing)
        .await
        .expect("prepare finishes once the gate opens")
        .expect("prepare does not panic")
        .expect("prepare succeeds");

    assert_eq!(prepared, Prepared::All);
    assert_eq!(pdp.recorded().len(), SINGLE_TYPE_CONCURRENCY + 1);
    assert_eq!(pdp.peak.load(Ordering::SeqCst), SINGLE_TYPE_CONCURRENCY);
}

/// The first event's evaluations past one wave run in a second round with a
/// deadline of its own: each evaluation takes most of a deadline, so the two
/// rounds together outlast one deadline, and the batch is still decided.
#[tokio::test(start_paused = true)]
async fn each_round_of_a_first_events_evaluations_has_its_own_deadline() {
    let slow = DEADLINE * 6 / 10;
    let pdp = TypePdp::answering(move |_| Answer::AllowAfter(slow));
    let filter = more_subtree_groups_than_a_wave(&pdp);
    let events = batch(vec![event(CREATED, 1)]);

    let started = Instant::now();
    let prepared = tokio::time::timeout(DEADLINE * 2 + BOUND, filter.prepare(&events))
        .await
        .expect("prepare finishes");

    assert!(
        matches!(prepared, Ok(Prepared::All)),
        "expected Ok(All), got {prepared:?}"
    );
    assert_eq!(started.elapsed(), slow * 2);
    assert_eq!(pdp.recorded().len(), SINGLE_TYPE_CONCURRENCY + 1);
}

/// The barrier mode a request names.
fn barrier_mode_of(request: &Value) -> &Value {
    &request["context"]["tenant_context"]["barrier_mode"]
}

/// Two groups on `TOPIC`, `respect` (g0) and `ignore` (g1), each selecting
/// `SHIPPED`: g0 is denied it, g1 allowed.
#[tokio::test]
async fn a_swap_clears_the_changed_groups_plans_only() {
    let pdp = TypePdp::answering(|request| {
        let barrier_mode = request
            .context
            .tenant_context
            .as_ref()
            .map(|tenant_context| tenant_context.barrier_mode);
        if barrier_mode == Some(PdpBarrierMode::Respect) {
            Answer::Deny
        } else {
            Answer::Allow
        }
    });
    let respect = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let ignore = interest(TOPIC, &[FAMILY], BarrierMode::Ignore);
    let g1 = group_of(&ignore, tenant_scope(Vec::new()), &[CREATED]);
    let filter = ScopedFilter::new(
        &[respect.clone(), ignore.clone()],
        held_scope(
            1,
            vec![
                group_of(&respect, tenant_scope(Vec::new()), &[CREATED]),
                g1.clone(),
            ],
        ),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![true]);
    let recorded = pdp.recorded();
    let mut asked: Vec<&Value> = recorded.iter().map(barrier_mode_of).collect();
    asked.sort_by_key(ToString::to_string);
    assert_eq!(asked, vec![&json!("ignore"), &json!("respect")]);

    filter.swap_scope(held_scope(
        2,
        vec![group_of(&respect, tenant_scope(Vec::new()), &[CREATED]), g1],
    ));
    assert_eq!(decide(&filter, vec![event(SHIPPED, 2)]).await, vec![true]);

    let recorded = pdp.recorded();
    let [_, _, asked_again] = recorded.as_slice() else {
        panic!("only g0 asks again: {recorded:?}");
    };
    assert_eq!(barrier_mode_of(asked_again), &json!("respect"));
}

#[tokio::test]
async fn a_swap_never_lets_a_single_type_admit_outlive_a_revocation() {
    let revoked = Arc::new(AtomicBool::new(false));
    let pdp = TypePdp::answering({
        let revoked = Arc::clone(&revoked);
        move |_| {
            if revoked.load(Ordering::SeqCst) {
                Answer::Deny
            } else {
                Answer::Allow
            }
        }
    });
    let wildcard = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let filter = wildcard_filter(&pdp);
    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![true]);

    revoked.store(true, Ordering::SeqCst);
    filter.swap_scope(held_scope(
        2,
        vec![group_of(
            &wildcard,
            tenant_scope(Vec::new()),
            &[CREATED, PLACED],
        )],
    ));

    assert_eq!(decide(&filter, vec![event(SHIPPED, 2)]).await, vec![false]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED, SHIPPED]);
}

#[tokio::test]
async fn a_swap_to_the_same_generation_keeps_every_plan() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let wildcard = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let filter = wildcard_filter(&pdp);
    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![true]);

    filter.swap_scope(held_scope(
        1,
        vec![group_of(
            &wildcard,
            tenant_scope(Vec::new()),
            &[CREATED, PLACED],
        )],
    ));

    assert_eq!(decide(&filter, vec![event(SHIPPED, 2)]).await, vec![true]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED]);
}

/// The swap lands while the E is held on the gate, as no session does: the
/// answer to the old generation's question is dropped, not kept.
#[tokio::test]
async fn a_swap_during_a_single_type_evaluation_fails_the_batch() {
    let pdp = TypePdp::held(|_| Answer::Allow);
    let wildcard = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let filter = Arc::new(wildcard_filter(&pdp));
    let events = batch(vec![event(SHIPPED, 1)]);

    let preparing = tokio::spawn({
        let filter = Arc::clone(&filter);
        async move { filter.prepare(&events).await }
    });
    tokio::time::timeout(BOUND, pdp.arrivals(1))
        .await
        .expect("the E is in flight");
    filter.swap_scope(held_scope(
        2,
        vec![group_of(
            &wildcard,
            tenant_scope(Vec::new()),
            &[CREATED, PLACED],
        )],
    ));
    pdp.open();
    let prepared = tokio::time::timeout(BOUND, preparing)
        .await
        .expect("prepare finishes once the gate opens")
        .expect("prepare does not panic");

    assert!(
        matches!(
            &prepared,
            Err(DomainError::Internal(message))
                if message == "the consume scope changed during a single-type evaluation"
        ),
        "got {prepared:?}"
    );
    assert_eq!(filter.single_count(0), 0);
}

#[tokio::test]
async fn a_scope_whose_groups_do_not_match_fails_every_later_batch() {
    let elsewhere = interest(TOPIC, &[FAMILY], BarrierMode::Ignore);
    let [held] = wildcard_scope(1).groups.to_vec().try_into().unwrap();
    let another_key = vec![group_of(&elsewhere, tenant_scope(Vec::new()), &[CREATED])];
    let one_more = vec![
        held,
        group_of(&elsewhere, tenant_scope(Vec::new()), &[CREATED]),
    ];
    for (row, groups) in [("another key", another_key), ("one more group", one_more)] {
        let pdp = TypePdp::answering(|_| Answer::Allow);
        let filter = wildcard_filter(&pdp);

        filter.swap_scope(held_scope(2, groups));

        for sequence in [1, 2] {
            let prepared = filter.prepare(&batch(vec![event(CREATED, sequence)])).await;
            assert!(
                matches!(
                    &prepared,
                    Err(DomainError::Internal(message))
                        if message == "the consume scope's groups do not match the subscription's interests"
                ),
                "{row}: got {prepared:?}"
            );
        }
        let decided = filter.admits(&event(CREATED, 3));
        assert!(
            matches!(
                &decided,
                Err(DomainError::Internal(message))
                    if message == "the consume scope's groups do not match the subscription's interests"
            ),
            "{row}: got {decided:?}"
        );
        assert_eq!(pdp.recorded(), Vec::<Value>::new(), "{row}");
    }
}

/// A scope stored for another caller under the generation the filter already
/// holds is not skipped as unchanged: the next batch fails, and so does every
/// decision after it, with no call.
#[tokio::test]
async fn a_scope_for_another_principal_with_the_same_generation_fails_the_next_batch() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);
    let someone_else = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x789))
        .subject_tenant_id(CALLER_TENANT)
        .build()
        .expect("valid security context");

    filter.swap_scope(Arc::new(CachedScope {
        generation: ScopeGeneration::nth(1),
        principal: ScopePrincipal::from(&someone_else),
        groups: wildcard_scope(1).groups.clone(),
    }));

    let prepared = filter.prepare(&batch(vec![event(CREATED, 1)])).await;
    assert!(
        matches!(
            &prepared,
            Err(DomainError::Internal(message))
                if message == "the consume scope belongs to another caller"
        ),
        "got {prepared:?}"
    );
    let decided = filter.admits(&event(CREATED, 1));
    assert!(
        matches!(
            &decided,
            Err(DomainError::Internal(message))
                if message == "the consume scope belongs to another caller"
        ),
        "got {decided:?}"
    );
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// Two interests of one group: the first selects only `ORDER` subjects, and
/// its event asks about `SHIPPED`; the second selects `SHIPPED` only for
/// `INVOICE` subjects, so only it selects its event, which reuses the group's
/// answer.
#[tokio::test]
async fn a_second_interest_of_the_group_reuses_its_single_type_answer() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let mut orders = wildcard_interest();
    orders.subject_types = vec![GtsIdPattern::try_new(ORDER).unwrap()];
    let mut invoices = interest(TOPIC, &[SHIPPED], BarrierMode::Respect);
    invoices.subject_types = vec![GtsIdPattern::try_new(INVOICE).unwrap()];
    let filter = ScopedFilter::new(
        &[orders, invoices],
        wildcard_scope(1),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let mut invoice = event(SHIPPED, 2);
    invoice.subject_type = GtsTypeId::new(INVOICE);

    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![true]);
    assert_eq!(decide(&filter, vec![invoice]).await, vec![true]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED]);
}

#[test]
fn an_interest_the_scope_holds_no_group_for_is_refused() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let wildcard = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let elsewhere = interest(TOPIC, &[FAMILY], BarrierMode::Ignore);

    let built = ScopedFilter::new(
        &[wildcard],
        held_scope(
            1,
            vec![group_of(&elsewhere, tenant_scope(Vec::new()), &[CREATED])],
        ),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    );

    assert!(
        matches!(
            &built,
            Err(DomainError::Internal(message))
                if message == "the consume scope holds no group for an interest"
        ),
        "got {:?}",
        built.err()
    );
}

/// `SINGLE_CAP + 2` types, one wave per batch: storing the last two evicts
/// the first two, and the first is asked again on its next event. Two
/// evictions, one warning.
#[tokio::test]
#[tracing_test::traced_test]
async fn the_single_type_cap_evicts_the_oldest_and_warns_once() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);
    let types = many_types(SINGLE_CAP + 2);

    let mut decided = Vec::new();
    for (wave, chunk) in types.chunks(SINGLE_TYPE_CONCURRENCY).enumerate() {
        let first = i64::try_from(wave * SINGLE_TYPE_CONCURRENCY).unwrap() + 1;
        let events = chunk
            .iter()
            .zip(first..)
            .map(|(event_type, sequence)| event(event_type, sequence))
            .collect();
        decided.extend(decide(&filter, events).await);
    }

    assert_eq!(decided, vec![true; SINGLE_CAP + 2]);
    assert_eq!(filter.single_count(0), SINGLE_CAP);
    assert_eq!(pdp.recorded().len(), SINGLE_CAP + 2);

    assert_eq!(
        decide(&filter, vec![event(&types[0], 5_000)]).await,
        vec![true]
    );
    let recorded = pdp.recorded();
    let [.., asked_again] = recorded.as_slice() else {
        unreachable!()
    };
    assert_eq!(recorded.len(), SINGLE_CAP + 3);
    assert_eq!(
        asked_again["resource"]["properties"]["event_types"],
        json!([types[0]])
    );
    logs_assert(|lines: &[&str]| {
        match lines
            .iter()
            .filter(|line| line.contains("holds the most single-type decisions"))
            .count()
        {
            1 => Ok(()),
            n => Err(format!("expected one cap warning, got {n}")),
        }
    });
}

/// `PLAN_CAP + 2` candidate types: the last two are decided without a call
/// and without being kept, with one warning.
#[tokio::test]
#[tracing_test::traced_test]
async fn the_plan_cap_evaluates_without_storing() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let wildcard = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let types = many_types(PLAN_CAP + 2);
    let candidates: Vec<&str> = types.iter().map(String::as_str).collect();
    let filter = ScopedFilter::new(
        std::slice::from_ref(&wildcard),
        held_scope(
            1,
            vec![group_of(&wildcard, tenant_scope(Vec::new()), &candidates)],
        ),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();

    let decided = decide(
        &filter,
        types
            .iter()
            .zip(1..)
            .map(|(event_type, sequence)| event(event_type, sequence))
            .collect(),
    )
    .await;

    assert_eq!(decided, vec![true; PLAN_CAP + 2]);
    assert_eq!(filter.plan_count(0), PLAN_CAP);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
    logs_assert(|lines: &[&str]| {
        match lines
            .iter()
            .filter(|line| line.contains("holds the most event-type plans"))
            .count()
        {
            1 => Ok(()),
            n => Err(format!("expected one cap warning, got {n}")),
        }
    });
}

/// `SELECTOR_CACHE_CAP + 1` subject types of one candidate type: the last
/// is matched without being kept.
#[tokio::test]
async fn the_selector_cache_cap_evaluates_without_storing() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);
    let events: Vec<Event> = (0..=SELECTOR_CACHE_CAP)
        .map(|n| {
            let mut created = event(CREATED, i64::try_from(n).unwrap() + 1);
            created.subject_type = GtsTypeId::new(&format!("gts.x.eb.scoped.s{n:04}.v1~"));
            created
        })
        .collect();

    let decided = decide(&filter, events).await;

    assert_eq!(decided, vec![true; SELECTOR_CACHE_CAP + 1]);
    assert_eq!(filter.selector_count(0), SELECTOR_CACHE_CAP);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_candidate_type_is_decided_without_a_call() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);

    let decided = decide(&filter, vec![event(CREATED, 1), event(PLACED, 2)]).await;

    assert_eq!(decided, vec![true, true]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_candidate_type_the_scope_excludes_is_skipped_without_a_call() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let wildcard = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let created_only = tenant_scope(vec![ScopeFilter::r#in(
        EVENT_TYPE,
        vec![ScopeValue::String(CREATED.to_owned())],
    )]);
    let filter = ScopedFilter::new(
        std::slice::from_ref(&wildcard),
        held_scope(
            1,
            vec![group_of(&wildcard, created_only, &[CREATED, PLACED])],
        ),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();

    let decided = decide(&filter, vec![event(CREATED, 1), event(PLACED, 2)]).await;

    assert_eq!(decided, vec![true, false]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// Both interests select `CREATED`; the first one's group (`respect`) may not
/// read it, the second one's (`ignore`) may.
#[tokio::test]
async fn an_event_delivered_through_one_of_two_matching_interests() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let respect = interest(TOPIC, &[CREATED], BarrierMode::Respect);
    let ignore = interest(TOPIC, &[FAMILY], BarrierMode::Ignore);
    let placed_only = tenant_scope(vec![ScopeFilter::r#in(
        EVENT_TYPE,
        vec![ScopeValue::String(PLACED.to_owned())],
    )]);
    let filter = ScopedFilter::new(
        &[respect.clone(), ignore.clone()],
        held_scope(
            1,
            vec![
                group_of(&respect, placed_only, &[CREATED]),
                group_of(&ignore, tenant_scope(Vec::new()), &[CREATED, PLACED]),
            ],
        ),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();

    assert_eq!(decide(&filter, vec![event(CREATED, 1)]).await, vec![true]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// Events on `TOPIC` the wildcard interest does not select: another tenant,
/// another topic, a type outside `FAMILY`. None is asked about.
#[tokio::test]
async fn an_event_the_interest_does_not_select_is_skipped_without_a_call() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);
    let mut other_tenant = event(CREATED, 1);
    other_tenant.subject_tenant = Uuid::from_u128(0x999);
    let other_topic = event_of(
        "gts.cf.core.events.topic.v1~x.eb.scoped.other.v1",
        CREATED,
        2,
    );
    let other_family = event("gts.cf.core.events.event.v1~x.eb.unscoped.created.v1~", 3);

    assert!(!filter.matches(&other_tenant));
    assert!(!filter.matches(&other_topic));
    assert!(!filter.matches(&other_family));
    assert!(filter.matches(&event(CREATED, 4)));
    assert_eq!(
        decide(&filter, vec![other_tenant, other_topic, other_family]).await,
        vec![false, false, false]
    );
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// `interest` with `ownership` on `tenant`.
fn owning(interest: Interest, ownership: OwnershipFilter, tenant: Uuid) -> Interest {
    Interest {
        subject_tenant: tenant,
        ownership,
        ..interest
    }
}

/// A `CREATED` event at `sequence` in `tenant`, owned by `owner`.
fn owned_by(sequence: i64, tenant: Uuid, owner: Uuid) -> Event {
    Event {
        subject_tenant: tenant,
        subject_owner: Some(owner),
        ..event(CREATED, sequence)
    }
}

/// A `CREATED` event at `sequence` in `tenant`, owned by nobody.
fn unowned_in(sequence: i64, tenant: Uuid) -> Event {
    Event {
        subject_tenant: tenant,
        ..event(CREATED, sequence)
    }
}

/// A filter of the one `interest` over `scope`, with `CREATED` as candidate.
fn filter_of(pdp: &Arc<TypePdp>, interest: &Interest, scope: Arc<CompiledScope>) -> ScopedFilter {
    ScopedFilter::new(
        std::slice::from_ref(interest),
        held_scope(1, vec![group_of(interest, scope, &[CREATED])]),
        authorizer(pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .expect("the scope holds the interest's group")
}

/// `filters` as one constraint, compiled under `mode`.
fn compiled(filters: Vec<ScopeFilter>, mode: &TenantMode) -> Arc<CompiledScope> {
    Arc::new(
        CompiledScope::compile(
            &AccessScope::single(ScopeConstraint::new(filters)),
            mode,
            &TenantSetInterner::new(),
        )
        .expect("the scope compiles"),
    )
}

/// `only_mine` is the caller's mailbox: of the events in its own tenant, only
/// the ones it owns.
#[tokio::test]
async fn an_only_mine_interest_delivers_only_the_events_the_caller_owns_in_its_tenant() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let mine = owning(
        wildcard_interest(),
        OwnershipFilter::OnlyMine,
        CALLER_TENANT,
    );
    let filter = filter_of(
        &pdp,
        &mine,
        compiled(
            vec![ScopeFilter::eq(
                pep_properties::OWNER_TENANT_ID,
                CALLER_TENANT,
            )],
            &TenantMode::RootOnly,
        ),
    );

    let decided = decide(
        &filter,
        vec![
            owned_by(1, CALLER_TENANT, SUBJECT),
            owned_by(2, CALLER_TENANT, Uuid::from_u128(0x777)),
            unowned_in(3, CALLER_TENANT),
        ],
    )
    .await;

    assert_eq!(decided, vec![true, false, false]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// A PDP may answer an owner-only rule with no tenant predicate, which
/// compiles to any tenant; `only_mine` still reads the caller's own tenant
/// alone, so another tenant's event addressed to the caller stays out.
#[tokio::test]
async fn an_owner_rule_naming_no_tenant_never_lets_only_mine_read_another_tenant() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let mine = owning(
        wildcard_interest(),
        OwnershipFilter::OnlyMine,
        CALLER_TENANT,
    );
    let filter = filter_of(
        &pdp,
        &mine,
        compiled(
            vec![ScopeFilter::eq(pep_properties::OWNER_ID, SUBJECT)],
            &TenantMode::RootOnly,
        ),
    );

    let decided = decide(
        &filter,
        vec![
            owned_by(1, Uuid::from_u128(0x999), SUBJECT),
            owned_by(2, CALLER_TENANT, SUBJECT),
        ],
    )
    .await;

    assert_eq!(decided, vec![false, true]);
}

/// `entire_tenant` reads every event of its tenant, owned by anyone or by
/// nobody, and none of another tenant.
#[tokio::test]
async fn an_entire_tenant_interest_delivers_every_event_of_its_tenant_owned_or_not() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);

    let decided = decide(
        &filter,
        vec![
            owned_by(1, TENANT, SUBJECT),
            owned_by(2, TENANT, Uuid::from_u128(0x777)),
            unowned_in(3, TENANT),
            owned_by(4, Uuid::from_u128(0xC), SUBJECT),
        ],
    )
    .await;

    assert_eq!(decided, vec![true, true, true, false]);
}

/// `tenant_subtree_included` narrows nothing itself: the subtree scope the
/// PDP answered decides each event's tenant.
#[tokio::test]
async fn a_subtree_interest_delivers_the_tenants_its_subtree_scope_names() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let child = Uuid::from_u128(0xC);
    let subtree = owning(
        wildcard_interest(),
        OwnershipFilter::TenantSubtreeIncluded,
        TENANT,
    );
    let filter = filter_of(
        &pdp,
        &subtree,
        compiled(
            vec![ScopeFilter::in_uuids(
                pep_properties::OWNER_TENANT_ID,
                vec![TENANT, child],
            )],
            &TenantMode::Subtree,
        ),
    );

    let decided = decide(
        &filter,
        vec![
            unowned_in(1, TENANT),
            owned_by(2, child, Uuid::from_u128(0x777)),
            unowned_in(3, Uuid::from_u128(0xB)),
        ],
    )
    .await;

    assert_eq!(decided, vec![true, true, false]);
}

/// An interest selecting only `INVOICE` subjects skips the `ORDER` event.
#[tokio::test]
async fn a_subject_type_the_selector_excludes_is_skipped_without_a_call() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let mut invoices = wildcard_interest();
    invoices.subject_types = vec![GtsIdPattern::try_new(INVOICE).unwrap()];
    let filter = ScopedFilter::new(
        std::slice::from_ref(&invoices),
        wildcard_scope(1),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let mut invoice = event(SHIPPED, 2);
    invoice.subject_type = GtsTypeId::new(INVOICE);

    assert_eq!(
        decide(&filter, vec![event(SHIPPED, 1), invoice]).await,
        vec![false, true]
    );
    assert_eq!(pdp.asked_types(), vec![SHIPPED]);
}

/// A new generation whose group keeps its candidates but holds a new scope,
/// or keeps its scope but holds new candidates, starts that group over.
#[tokio::test]
async fn a_swap_clears_a_group_whose_scope_or_candidates_changed() {
    let revoked = Arc::new(AtomicBool::new(false));
    let pdp = TypePdp::answering({
        let revoked = Arc::clone(&revoked);
        move |_| {
            if revoked.load(Ordering::SeqCst) {
                Answer::Deny
            } else {
                Answer::Allow
            }
        }
    });
    let scope = wildcard_scope(1);
    let filter = ScopedFilter::new(
        &[wildcard_interest()],
        Arc::clone(&scope),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let [held] = scope.groups.to_vec().try_into().unwrap();
    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![true]);

    revoked.store(true, Ordering::SeqCst);
    let new_scope = AuthorizedGroup {
        scope: tenant_scope(Vec::new()),
        ..held
    };
    filter.swap_scope(held_scope(2, vec![new_scope.clone()]));
    assert_eq!(decide(&filter, vec![event(SHIPPED, 2)]).await, vec![false]);

    filter.swap_scope(held_scope(
        3,
        vec![AuthorizedGroup {
            candidates: Arc::new([SHIPPED].iter().map(|id| GtsTypeId::new(id)).collect()),
            ..new_scope
        }],
    ));
    assert_eq!(decide(&filter, vec![event(SHIPPED, 3)]).await, vec![true]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED, SHIPPED]);
}

#[tokio::test]
async fn a_failed_point_read_fails_the_batch() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let hook = SpecQueryHook::new();
    hook.arm(|query| match query {
        SpecQuery::EventType => Err(DomainError::StorageUnavailable {
            reason: "the event type cannot be read".to_owned(),
            source: None,
        }),
        _ => Ok(()),
    });
    let wildcard = interest(TOPIC, &[FAMILY], BarrierMode::Respect);
    let filter = ScopedFilter::new(
        std::slice::from_ref(&wildcard),
        held_scope(
            1,
            vec![group_of(&wildcard, tenant_scope(Vec::new()), &[CREATED])],
        ),
        authorizer(&pdp, hook.wrap(KnownTypes::on_topic(&[])), DEADLINE),
    )
    .unwrap();

    let prepared = filter.prepare(&batch(vec![event(SHIPPED, 1)])).await;

    assert!(
        matches!(
            &prepared,
            Err(DomainError::StorageUnavailable { reason, .. })
                if reason == "the event type cannot be read"
        ),
        "got {prepared:?}"
    );
    assert_eq!(hook.calls(SpecQuery::EventType), 1);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// A refused type is decided `false` until the scope changes; nothing about
/// the refusal is kept apart from its plan. A permitted scope that matches
/// nothing skips the event like a denial and never fails the batch.
#[tokio::test]
async fn every_refusal_kind_denies_the_type_until_the_next_generation() {
    for refused in [
        Answer::Deny,
        Answer::AllowNothing,
        Answer::Uncompilable,
        Answer::Unenforceable,
    ] {
        let pdp = TypePdp::answering(move |_| refused);
        let filter = wildcard_filter(&pdp);

        let decided = decide(&filter, vec![event(SHIPPED, 1), event(CREATED, 2)]).await;
        let again = decide(&filter, vec![event(SHIPPED, 3)]).await;

        assert_eq!(
            (decided, again),
            (vec![false, true], vec![false]),
            "{refused:?}"
        );
        assert_eq!(pdp.asked_types(), vec![SHIPPED], "{refused:?}");
    }
}

#[tokio::test]
async fn a_single_type_scope_that_excludes_the_type_denies_it() {
    let pdp = TypePdp::answering(|_| Answer::AllowCreatedOnly);
    let filter = wildcard_filter(&pdp);

    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![false]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED]);
}

/// `SINGLE_CAP` types fill the group's single-type results, one wave per
/// batch. A batch of the oldest of them and a new type then evicts the oldest
/// while it stores the new one's answer: the batch still decides both, and
/// the oldest is asked again on its next event.
#[tokio::test]
async fn an_eviction_never_strands_a_type_the_same_batch_uses() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);
    let types = many_types(SINGLE_CAP + 1);
    let (filled, [new]) = types.split_at(SINGLE_CAP) else {
        unreachable!("one type more than the cap")
    };
    for (wave, chunk) in filled.chunks(SINGLE_TYPE_CONCURRENCY).enumerate() {
        let first = i64::try_from(wave * SINGLE_TYPE_CONCURRENCY).unwrap() + 1;
        let events = chunk
            .iter()
            .zip(first..)
            .map(|(event_type, sequence)| event(event_type, sequence))
            .collect();
        assert_eq!(decide(&filter, events).await, vec![true; chunk.len()]);
    }
    assert_eq!(filter.single_count(0), SINGLE_CAP);
    assert_eq!(pdp.recorded().len(), SINGLE_CAP);

    let events = batch(vec![event(&types[0], 5_000), event(new, 5_001)]);
    let prepared = tokio::time::timeout(BOUND, filter.prepare(&events))
        .await
        .expect("prepare finishes")
        .expect("prepare succeeds");
    let decided: Vec<Result<bool, String>> = events
        .iter()
        .map(|event| filter.admits(event).map_err(|error| error.to_string()))
        .collect();

    assert_eq!(prepared, Prepared::All);
    assert_eq!(decided, vec![Ok(true), Ok(true)]);
    assert_eq!(filter.single_count(0), SINGLE_CAP);
    let recorded = pdp.recorded();
    let [.., asked] = recorded.as_slice() else {
        unreachable!("the fill asked")
    };
    assert_eq!(recorded.len(), SINGLE_CAP + 1);
    assert_eq!(asked["resource"]["properties"]["event_types"], json!([new]));

    assert_eq!(
        decide(&filter, vec![event(&types[0], 5_002)]).await,
        vec![true]
    );
    let recorded = pdp.recorded();
    let [.., asked_again] = recorded.as_slice() else {
        unreachable!("the fill asked")
    };
    assert_eq!(recorded.len(), SINGLE_CAP + 2);
    assert_eq!(
        asked_again["resource"]["properties"]["event_types"],
        json!([types[0]])
    );
}

/// A candidate first and the first new type twice, so the cut index differs
/// from the count of new types: the wave asks `t0`..`t4`, and the batch is
/// cut at `t5`, its eighth event.
#[tokio::test]
async fn a_batch_with_more_new_types_than_one_wave_is_cut_after_the_wave() {
    assert_eq!(
        SINGLE_TYPE_CONCURRENCY, 5,
        "the batch below is laid out for a wave of five"
    );
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);
    let types = many_types(SINGLE_TYPE_CONCURRENCY + 2);
    let events = vec![
        event(CREATED, 1),
        event(&types[0], 2),
        event(&types[0], 3),
        event(&types[1], 4),
        event(&types[2], 5),
        event(&types[3], 6),
        event(&types[4], 7),
        event(&types[5], 8),
        event(&types[6], 9),
    ];

    let first = batch(events.clone());
    let prepared = tokio::time::timeout(BOUND, filter.prepare(&first))
        .await
        .expect("prepare finishes")
        .expect("prepare succeeds");
    assert_eq!(prepared, Prepared::UpTo(7));
    assert_eq!(pdp.asked_types(), types[..5].to_vec());
    let decided: Vec<bool> = first
        .iter()
        .take(7)
        .map(|event| filter.admits(event).expect("a prepared event is decided"))
        .collect();
    assert_eq!(decided, vec![true; 7]);

    let rest = batch(events[7..].to_vec());
    let prepared = tokio::time::timeout(BOUND, filter.prepare(&rest))
        .await
        .expect("prepare finishes")
        .expect("prepare succeeds");
    assert_eq!(prepared, Prepared::All);
    assert_eq!(pdp.asked_types(), types);
    let decided: Vec<bool> = rest
        .iter()
        .map(|event| filter.admits(event).expect("a prepared event is decided"))
        .collect();
    assert_eq!(decided, vec![true, true]);
}

/// `SHIPPED`'s E never answers while the other two answer at once; the batch
/// fails at `DEADLINE` on the paused clock, and keeps no answer.
#[tokio::test(start_paused = true)]
async fn a_batchs_evaluations_end_by_the_deadline() {
    let pdp = TypePdp::answering(|request| {
        if request.resource.properties["event_types"] == json!([SHIPPED]) {
            Answer::Never
        } else {
            Answer::Allow
        }
    });
    let filter = wildcard_filter(&pdp);
    let events = batch(vec![
        event(SHIPPED, 1),
        event(RETURNED, 2),
        event(ARCHIVED, 3),
    ]);

    let started = Instant::now();
    let prepared = tokio::time::timeout(DEADLINE + BOUND, filter.prepare(&events))
        .await
        .expect("the deadline ends the batch");

    assert_eq!(started.elapsed(), DEADLINE);
    assert!(
        matches!(
            &prepared,
            Err(DomainError::Internal(message))
                if message == "the single-type consume evaluations did not finish within 10s"
        ),
        "got {prepared:?}"
    );
    assert_eq!(filter.single_count(0), 0);
    assert_eq!(pdp.asked_types(), vec![ARCHIVED, RETURNED, SHIPPED]);
}

/// The new generation's group keeps the scope and candidates `Arc`s but
/// carries a later expiry, as a refresh served from the decision memo does:
/// `SHIPPED`, admitted under the old generation, is asked again and is now
/// refused.
#[tokio::test]
async fn a_refreshed_group_with_the_same_scope_arc_starts_over() {
    let revoked = Arc::new(AtomicBool::new(false));
    let pdp = TypePdp::answering({
        let revoked = Arc::clone(&revoked);
        move |_| {
            if revoked.load(Ordering::SeqCst) {
                Answer::Deny
            } else {
                Answer::Allow
            }
        }
    });
    let scope = wildcard_scope(1);
    let filter = ScopedFilter::new(
        &[wildcard_interest()],
        Arc::clone(&scope),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let [held] = scope.groups.to_vec().try_into().unwrap();
    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![true]);

    revoked.store(true, Ordering::SeqCst);
    let refreshed = AuthorizedGroup {
        expires_at: held.expires_at + Duration::from_mins(1),
        ..held
    };
    filter.swap_scope(held_scope(2, vec![refreshed]));

    assert_eq!(decide(&filter, vec![event(SHIPPED, 2)]).await, vec![false]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED, SHIPPED]);
}

/// The new generation's group is the held one: same scope and candidates
/// `Arc`s, same expiry. `SHIPPED` keeps its decision, though the PDP would
/// now refuse it.
#[tokio::test]
async fn an_unchanged_group_keeps_its_decisions() {
    let revoked = Arc::new(AtomicBool::new(false));
    let pdp = TypePdp::answering({
        let revoked = Arc::clone(&revoked);
        move |_| {
            if revoked.load(Ordering::SeqCst) {
                Answer::Deny
            } else {
                Answer::Allow
            }
        }
    });
    let scope = wildcard_scope(1);
    let filter = ScopedFilter::new(
        &[wildcard_interest()],
        Arc::clone(&scope),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let [held] = scope.groups.to_vec().try_into().unwrap();
    assert_eq!(decide(&filter, vec![event(SHIPPED, 1)]).await, vec![true]);

    revoked.store(true, Ordering::SeqCst);
    filter.swap_scope(held_scope(2, vec![held]));

    assert_eq!(decide(&filter, vec![event(SHIPPED, 2)]).await, vec![true]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED]);
}

// -- Which types a selector admits --
//
// The selector matches an event's type with `GtsId::matches_pattern`: a bare
// type id covers its derived types, a trailing `*` covers its prefix, and a
// segment that names no minor version covers every minor version. Unless a
// test says otherwise, each group's candidates are the registered types its
// selector matches, as JOIN lists them.

/// Derived from `CREATED`.
pub const EXPRESS: &str =
    "gts.cf.core.events.event.v1~x.eb.scoped.created.v1~x.eb.scoped.express.v1~";
/// Derived from `CREATED`, a sibling of `EXPRESS`.
pub const RUSH: &str = "gts.cf.core.events.event.v1~x.eb.scoped.created.v1~x.eb.scoped.rush.v1~";
/// Derived from `EXPRESS`, two levels below `CREATED`.
const PRIORITY: &str = "gts.cf.core.events.event.v1~x.eb.scoped.created.v1~x.eb.scoped.express.v1~x.eb.scoped.priority.v1~";
/// Derived from `CREATED`; never a candidate here.
const CREATED_AUDIT: &str =
    "gts.cf.core.events.event.v1~x.eb.scoped.created.v1~x.eb.scoped.audit.v1~";
/// A type outside `FAMILY`.
const OUTSIDER: &str = "gts.cf.core.events.event.v1~x.eb.unscoped.created.v1~";
/// A subject type no event here carries.
const AUDIT_SUBJECT: &str = "gts.x.eb.scoped.audit.v1~";

/// A filter over one interest in `types` on `TOPIC`, whose group holds
/// `candidates` and a scope over `TENANT` that `filters` narrow. It asks
/// `pdp` about any other type, reading `registered` as registered too.
fn selecting(
    pdp: &Arc<TypePdp>,
    types: &[&str],
    filters: Vec<ScopeFilter>,
    candidates: &[&str],
    registered: &[&str],
) -> ScopedFilter {
    let selector = interest(TOPIC, types, BarrierMode::Respect);
    ScopedFilter::new(
        std::slice::from_ref(&selector),
        held_scope(
            1,
            vec![group_of(&selector, tenant_scope(filters), candidates)],
        ),
        authorizer(pdp, KnownTypes::on_topic(registered), DEADLINE),
    )
    .expect("the scope holds the interest's group")
}

#[tokio::test]
async fn an_exact_selector_admits_only_its_own_type() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(&pdp, &[CREATED], Vec::new(), &[CREATED], &[]);

    let decided = decide(&filter, vec![event(CREATED, 1), event(PLACED, 2)]).await;

    assert_eq!(decided, vec![true, false]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_base_selector_admits_the_base_and_its_derived_types() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(&pdp, &[CREATED], Vec::new(), &[CREATED, EXPRESS, RUSH], &[]);

    let decided = decide(
        &filter,
        vec![
            event(CREATED, 1),
            event(EXPRESS, 2),
            event(RUSH, 3),
            event(PLACED, 4),
        ],
    )
    .await;

    assert_eq!(decided, vec![true, true, true, false]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_selector_admits_types_two_levels_below_it() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(&pdp, &[CREATED], Vec::new(), &[EXPRESS, PRIORITY], &[]);

    let decided = decide(&filter, vec![event(EXPRESS, 1), event(PRIORITY, 2)]).await;

    assert_eq!(decided, vec![true, true]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// `EXPRESS` selects itself and `PRIORITY`, derived from it; not its base
/// `CREATED` and not its sibling `RUSH`, which are therefore never asked about.
#[tokio::test]
async fn a_derived_selector_admits_neither_its_base_nor_a_sibling() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(&pdp, &[EXPRESS], Vec::new(), &[EXPRESS, PRIORITY], &[]);

    let decided = decide(
        &filter,
        vec![
            event(CREATED, 1),
            event(EXPRESS, 2),
            event(RUSH, 3),
            event(PRIORITY, 4),
        ],
    )
    .await;

    assert_eq!(decided, vec![false, true, false, true]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// `SHIPPED` is under `FAMILY` but not a candidate, so it is asked about and
/// allowed; `OUTSIDER` is not under `FAMILY`.
#[tokio::test]
async fn a_prefix_wildcard_admits_every_type_under_it_and_no_other() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = wildcard_filter(&pdp);

    let decided = decide(
        &filter,
        vec![
            event(CREATED, 1),
            event(PLACED, 2),
            event(SHIPPED, 3),
            event(OUTSIDER, 4),
        ],
    )
    .await;

    assert_eq!(decided, vec![true, true, true, false]);
    assert_eq!(pdp.asked_types(), vec![SHIPPED]);
}

/// The all-types pattern is the topic-only subscription: every event type
/// matches it, `OUTSIDER` included, and only the topic limits it.
#[tokio::test]
async fn the_all_types_wildcard_admits_every_event_type_of_its_topic() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(
        &pdp,
        &["gts.cf.core.events.event.v1~*"],
        Vec::new(),
        &[CREATED, PLACED],
        &[],
    );
    let other_topic = event_of(
        "gts.cf.core.events.topic.v1~x.eb.scoped.other.v1",
        CREATED,
        4,
    );

    let decided = decide(
        &filter,
        vec![
            event(CREATED, 1),
            event(PLACED, 2),
            event(OUTSIDER, 3),
            other_topic,
        ],
    )
    .await;

    assert_eq!(decided, vec![true, true, true, false]);
    assert_eq!(pdp.asked_types(), vec![OUTSIDER]);
}

/// gts-id matches a pattern segment that names no minor version against any
/// minor version of it, and never against another major version.
#[tokio::test]
async fn a_selector_without_a_minor_version_admits_its_minor_versions() {
    const FIRST_MINOR: &str = "gts.cf.core.events.event.v1~x.eb.scoped.minor.v1.0~";
    const SECOND_MINOR: &str = "gts.cf.core.events.event.v1~x.eb.scoped.minor.v1.1~";
    const NEXT_MAJOR: &str = "gts.cf.core.events.event.v1~x.eb.scoped.minor.v2~";
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(
        &pdp,
        &["gts.cf.core.events.event.v1~x.eb.scoped.minor.v1~"],
        Vec::new(),
        &[FIRST_MINOR, SECOND_MINOR],
        &[],
    );

    let decided = decide(
        &filter,
        vec![
            event(FIRST_MINOR, 1),
            event(SECOND_MINOR, 2),
            event(NEXT_MAJOR, 3),
        ],
    )
    .await;

    assert_eq!(decided, vec![true, true, false]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_selector_matching_no_type_of_its_topic_admits_nothing() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(
        &pdp,
        &["gts.cf.core.events.event.v1~x.eb.nothing.*"],
        Vec::new(),
        &[],
        &[],
    );

    let decided = decide(
        &filter,
        vec![event(CREATED, 1), event(PLACED, 2), event(SHIPPED, 3)],
    )
    .await;

    assert_eq!(decided, vec![false, false, false]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// The selector admits all three candidates; the scope names `CREATED` and
/// `RUSH` only, so `EXPRESS` is skipped, decided from the scope with no call.
#[tokio::test]
async fn a_derived_candidate_the_scope_excludes_is_skipped_and_the_rest_admitted() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(
        &pdp,
        &[CREATED],
        vec![ScopeFilter::r#in(
            EVENT_TYPE,
            vec![
                ScopeValue::String(CREATED.to_owned()),
                ScopeValue::String(RUSH.to_owned()),
            ],
        )],
        &[CREATED, EXPRESS, RUSH],
        &[],
    );

    let decided = decide(
        &filter,
        vec![event(CREATED, 1), event(EXPRESS, 2), event(RUSH, 3)],
    )
    .await;

    assert_eq!(decided, vec![true, false, true]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// `CREATED_AUDIT` was not a candidate when the scope was evaluated, as for a
/// type registered after the stream opened: the selector admits it, and its
/// single-type evaluation decides it.
#[tokio::test]
async fn a_derived_type_outside_the_candidates_is_admitted_when_its_answer_allows() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let filter = selecting(&pdp, &[CREATED], Vec::new(), &[CREATED], &[CREATED_AUDIT]);

    let decided = decide(&filter, vec![event(CREATED, 1), event(CREATED_AUDIT, 2)]).await;

    assert_eq!(decided, vec![true, true]);
    assert_eq!(pdp.asked_types(), vec![CREATED_AUDIT]);
}

/// Every event carries `ORDER` or `INVOICE`, which the selector does not
/// match, so nothing is admitted and nothing is asked about.
#[tokio::test]
async fn a_subject_type_selector_matching_no_event_admits_nothing() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let mut audits = wildcard_interest();
    audits.subject_types = vec![GtsIdPattern::try_new(AUDIT_SUBJECT).unwrap()];
    let filter = ScopedFilter::new(
        std::slice::from_ref(&audits),
        wildcard_scope(1),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let mut invoice = event(PLACED, 2);
    invoice.subject_type = GtsTypeId::new(INVOICE);

    let decided = decide(&filter, vec![event(CREATED, 1), invoice, event(SHIPPED, 3)]).await;

    assert_eq!(decided, vec![false, false, false]);
    assert_eq!(pdp.recorded(), Vec::<Value>::new());
}

/// A bare subject type selects itself and every subject type derived from it,
/// as a bare event type does; a subject type outside its subtree is skipped.
/// Every event is `CREATED`, a candidate, so nothing is asked about.
#[tokio::test]
async fn a_base_subject_type_selector_admits_its_derived_subject_types() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let mut customers = wildcard_interest();
    customers.subject_types = vec![GtsIdPattern::try_new(CUSTOMER).unwrap()];
    let filter = ScopedFilter::new(
        std::slice::from_ref(&customers),
        wildcard_scope(1),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let mut member = event(CREATED, 1);
    member.subject_type = GtsTypeId::new(LOYALTY_MEMBER);
    let mut customer = event(CREATED, 2);
    customer.subject_type = GtsTypeId::new(CUSTOMER);
    let mut supplier = event(CREATED, 3);
    supplier.subject_type = GtsTypeId::new(SUPPLIER);

    let decided = decide(&filter, vec![member, customer, supplier]).await;

    assert_eq!(decided, vec![true, true, false]);
    assert_eq!(pdp.asked_types(), Vec::<String>::new());
}

/// `customer.v1~acme.*` selects the subject types derived from `CUSTOMER`
/// under the `acme` vendor only: not `CUSTOMER` itself, which has no segment
/// for the `*` to match, not a derived type of another vendor, and not a
/// subject type outside the subtree. Every event is `CREATED`, a candidate,
/// so nothing is asked about.
#[tokio::test]
async fn a_trailing_wildcard_subject_type_selector_admits_only_its_prefix() {
    let pdp = TypePdp::answering(|_| Answer::Allow);
    let mut acme_customers = wildcard_interest();
    acme_customers.subject_types =
        vec![GtsIdPattern::try_new("gts.x.eb.scoped.customer.v1~acme.*").unwrap()];
    let filter = ScopedFilter::new(
        std::slice::from_ref(&acme_customers),
        wildcard_scope(1),
        authorizer(&pdp, KnownTypes::on_topic(&[]), DEADLINE),
    )
    .unwrap();
    let mut member = event(CREATED, 1);
    member.subject_type = GtsTypeId::new(LOYALTY_MEMBER);
    let mut supplier = event(CREATED, 2);
    supplier.subject_type = GtsTypeId::new(SUPPLIER);
    let mut customer = event(CREATED, 3);
    customer.subject_type = GtsTypeId::new(CUSTOMER);
    let mut partner = event(CREATED, 4);
    partner.subject_type = GtsTypeId::new(PARTNER_MEMBER);

    let decided = decide(&filter, vec![member, supplier, customer, partner]).await;

    assert_eq!(decided, vec![true, false, false, false]);
    assert_eq!(pdp.asked_types(), Vec::<String>::new());
}
