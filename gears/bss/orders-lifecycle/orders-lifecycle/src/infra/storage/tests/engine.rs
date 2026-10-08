//! S2-04 on real PostgreSQL: the transition engine composing the real rules PDP (S2-03), the
//! registry gate (S2-05), the sealed v3 audit writer (S2-06), step-17 claims (S2-07) and the
//! real Event Broker managed producer (S2-08), through the restricted runtime role.
//!
//! Slice guard predicates and contributions are test bindings: the engine owns when and in
//! what order they run, and the slices that own their logic are later packages.
use super::events::Env;
use super::*;
use crate::authz::test_pdp::{RulesProvider, pep, service, user};
use crate::authz::{Arrangement, Caller};
use crate::domain::audit::{
    ActorIdentities, AdminAttribute, AdminChange, AdminField, AdminTextKey, Principal, ServiceRole,
};
use crate::domain::contributions::{
    AggregateContribution, ContributionKind, DraftHeaderEdit, VersionContribution, declared_kind,
};
use crate::domain::guards::GuardRegistry;
use crate::domain::idempotency::LeaseDuration;
use crate::domain::overlap::{OverlapScopeKey, ProposedClaims};
use crate::domain::state_table::{RowId, StateTable};
use crate::domain::transition::{GuardBindings, GuardSubject, GuardVerdict, InputState, Inputs};
use crate::infra::broker::BoundProducer;
use crate::infra::engine::documents::EventDetail;
use crate::infra::engine::{
    Abort, ChildDocuments, CreateRequest, DocumentWriter, Engine, EngineError, EngineOutcome,
    EngineParts, EventSpec, FaultHook, FaultPoint, OutcomeKind, Preparation, PreparationView,
    PrepareError, Prepared, PreparedExecution, Registries, TransitionRequest,
};
use crate::infra::events::payload::{
    Assertion, CompensationEvidence, FailureReason, LineRef, LineSubscription, OperatorAttestation,
    OrderAcceptanceRecorded, OrderAmended, OrderApproved, OrderCancelled, OrderCompleted,
    OrderExpired, OrderFulfillmentFailed, OrderHeld, OrderRejected, OrderResumed, OrderSubmitted,
    RequirementSource, UnknownToken,
};
use crate::infra::storage::repo::idempotency::AttemptInputs;
use async_trait::async_trait;
use bss_orders_lifecycle_sdk::catalog::{OrderState as S, Reason, Trigger};
use bss_orders_lifecycle_sdk::models::IdempotencyKey;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_db::DbTx;
use toolkit_db::secure::ScopeError;

const ORDER_RT: &str = "gts.cf.bss.orders.order.v1~";
const ACCEPTANCE_RT: &str = "gts.cf.bss.orders.acceptance.v1~";
const NEW_SALE: &str = "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1";
/// Subjects: buyer/operator user 102 (tenant 10), Workflow service 106 (tenant 99), the
/// configured system worker 900 (tenant 901).
const BUYER: (u128, u128) = (102, 10);
const WORKFLOW: (u128, u128) = (106, 99);

fn s(id: Uuid) -> String {
    id.to_string()
}
fn path(pairs: &[(&str, Vec<Uuid>)]) -> Value {
    json!({"predicates": pairs.iter().map(|(p, v)| json!({
        "property": p, "values": v.iter().map(|x| s(*x)).collect::<Vec<_>>()
    })).collect::<Vec<_>>()})
}
/// Real rules policy: the buyer may do every user operation on resource tenant 10 (payer use
/// 30/31); Workflow may do the seam operations on exactly the listed orders of seller 20.
fn policy(workflow_orders: &[Uuid]) -> Value {
    let mut rules = vec![
        json!({
            "id": "buyer", "subject": {"id": s(u(BUYER.0))}, "resource_type": ORDER_RT,
            "actions": ["create", "write", "edit", "submit", "amend", "cancel", "hold", "resume",
                        "read", "force_fail_unreconciled"],
            "paths": [path(&[("resource_tenant_id", vec![u(10)])])],
            "payer_use": {"property": "payer_tenant_id", "values": [s(u(30)), s(u(31))]},
        }),
        json!({
            "id": "buyer-acceptance", "subject": {"id": s(u(BUYER.0))},
            "resource_type": ACCEPTANCE_RT, "actions": ["record"],
            "paths": [path(&[("resource_tenant_id", vec![u(10)])])],
        }),
    ];
    if !workflow_orders.is_empty() {
        rules.push(json!({
            "id": "workflow", "subject": {"id": s(u(WORKFLOW.0))}, "resource_type": ORDER_RT,
            "actions": ["read", "approval_reflection", "begin_fulfillment", "spawn_signal",
                        "fulfillment_acknowledgement", "workflow_cancel"],
            "paths": [path(&[("id", workflow_orders.to_vec()), ("seller_tenant_id", vec![u(20)])])],
        }));
    }
    json!({"vendor": "constructorfabric", "priority": 10, "policy_revision": "s2-04", "rules": rules})
}
fn identities() -> ActorIdentities {
    ActorIdentities::new(
        Some(Principal {
            subject_id: u(900),
            subject_tenant_id: u(901),
        }),
        [(
            ServiceRole::Workflow,
            Principal {
                subject_id: u(WORKFLOW.0),
                subject_tenant_id: u(WORKFLOW.1),
            },
        )],
    )
    .unwrap()
}
pub(super) fn buyer() -> Caller {
    user(BUYER.0, BUYER.1)
}
fn workflow() -> Caller {
    service(WORKFLOW.0, WORKFLOW.1)
}
fn arrangement() -> Arrangement {
    Arrangement {
        resource_tenant_id: u(10),
        seller_tenant_id: u(20),
        payer_tenant_id: u(30),
    }
}
pub(super) fn key(text: &str) -> IdempotencyKey {
    IdempotencyKey::try_from(text.to_owned()).unwrap()
}
fn at() -> time::OffsetDateTime {
    now()
}

pub(super) struct T {
    pub(super) env: Env,
    _bound: BoundProducer,
    sink: crate::infra::events::EventSink,
}
impl T {
    pub(super) async fn new() -> Self {
        let env = Env::new().await.unwrap();
        let bound = env.bind().await;
        let sink = bound.sink().clone();
        Self {
            env,
            _bound: bound,
            sink,
        }
    }
    pub(super) fn engine(&self, workflow_orders: &[Uuid]) -> Engine {
        self.engine_with(policy(workflow_orders))
    }
    fn engine_with(&self, policy: Value) -> Engine {
        Engine::new(
            EngineParts {
                db: self.env.db.clone(),
                pep: pep(RulesProvider::from_policy(policy)),
                sink: self.sink.clone(),
                identities: identities(),
                admin_key: Arc::new(AdminTextKey::new("k1", &[7u8; 32]).unwrap()),
                lease: LeaseDuration::from_seconds(30).unwrap(),
            },
            Registries::compile().unwrap(),
        )
    }
    pub(super) async fn n(&self, sql: &str) -> i64 {
        self.env.pg.scalar(sql).await.unwrap()
    }
    async fn order_row(&self, id: Uuid) -> Value {
        self.json(&format!(
            "SELECT to_jsonb(o) AS j FROM bss_orders__order o WHERE order_id='{id}'"
        ))
        .await
    }
    pub(super) async fn json(&self, sql: &str) -> Value {
        self.env
            .pg
            .raw
            .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await
            .unwrap()
            .map_or(Value::Null, |r| r.try_get::<Value>("", "j").unwrap())
    }
    async fn outbox(&self) -> i64 {
        self.n("SELECT count(*) AS n FROM toolkit_outbox_body")
            .await
    }
    async fn audits(&self, order: Uuid) -> i64 {
        self.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit WHERE order_id='{order}' OR requested_order_ref='{order}'"
        ))
        .await
    }
}

/// A preparation returning a fixed contribution, optionally running owner SQL first (to change
/// the order between authorization and the transaction).
pub(super) struct Prep {
    pub(super) prepared: Prepared,
    pub(super) side_effect: Option<(DatabaseConnection, String)>,
}
#[async_trait]
impl Preparation for Prep {
    async fn prepare(&self, _: &PreparationView) -> Result<Prepared, PrepareError> {
        if let Some((raw, sql)) = &self.side_effect {
            raw.execute_unprepared(sql).await.unwrap();
        }
        Ok(self.prepared.clone())
    }
}
pub(super) fn fixed(prepared: Prepared) -> Prep {
    Prep {
        prepared,
        side_effect: None,
    }
}

fn registries() -> (StateTable, GuardRegistry) {
    let table = StateTable::registered().unwrap();
    let guards = GuardRegistry::registered(&table).unwrap();
    (table, guards)
}
/// Bind every guard of `row`; `fail` refuses with its first registered reason.
fn guards(row: u8, fail: Option<&str>) -> GuardBindings {
    let (_, registry) = registries();
    let mut bindings = GuardBindings::new();
    for g in registry.row_guards(RowId::of(row)) {
        let verdict = if Some(g.name) == fail {
            GuardVerdict::Fail(g.reasons[0])
        } else {
            GuardVerdict::Pass
        };
        bindings = bindings.bind(g.name, move |_: &GuardSubject<'_>| verdict);
    }
    bindings
}
fn resolved(row: u8) -> Inputs {
    let (_, registry) = registries();
    registry
        .row_inputs(RowId::of(row))
        .into_iter()
        .fold(Inputs::new(), |i, p| i.with(p, InputState::Resolved))
}
fn create_prepared(fail: bool) -> Prepared {
    let mut p = Prepared::new(AggregateContribution::None);
    p.guards = guards(1, fail.then_some("capture.category-admitted"));
    p
}
fn create_request(text: &str, document: Value) -> CreateRequest {
    CreateRequest {
        category: NEW_SALE.into(),
        arrangement: arrangement(),
        contract_id: None,
        document,
        idempotency_key: key(text),
        correlation_id: Some(u(777)),
    }
}
pub(super) async fn create(t: &T, engine: &Engine, text: &str) -> Uuid {
    let out = engine
        .create(
            &buyer(),
            create_request(text, json!({"category": NEW_SALE})),
            &fixed(create_prepared(false)),
        )
        .await
        .unwrap();
    let OutcomeKind::Committed { order_id, .. } = out.kind else {
        panic!("{out:?}")
    };
    let _ = t;
    order_id
}
/// The committed version a seeded order carries: drafts are version 1; every later state has
/// its submitted commercial version 2.
fn version_of(state: S) -> i32 {
    if state == S::Draft { 1 } else { 2 }
}
/// Seed an order in `state` (owner SQL over an engine-created draft, with the submitted
/// version 2 past draft); the dwell clock moves back an hour so a state change is observable.
async fn seed(t: &T, engine: &Engine, text: &str, state: S, pre_hold: Option<S>) -> Uuid {
    let id = create(t, engine, text).await;
    let token = |s: S| crate::domain::audit::state_token(s);
    if state != S::Draft {
        t.env
            .pg
            .sql(&format!(
                "INSERT INTO bss_orders__order_version (order_id, version, supersedes_version, market_currency, market_region, payer_tenant_id, category, contract_id, actor, actor_tenant_id, reason, amendment_reason, created_at) \
                 VALUES ('{id}', 2, 1, 'EUR', 'DE', '{}', '{NEW_SALE}', NULL, '{}', '{}', 'submit', NULL, now()); \
                 UPDATE bss_orders__order SET version_allocation_high_water=2, current_version=2 WHERE order_id='{id}'",
                u(30),
                u(BUYER.0),
                u(BUYER.1)
            ))
            .await
            .unwrap();
    }
    t.env
        .pg
        .sql(&format!(
            "UPDATE bss_orders__order SET state='{}', pre_hold_state={}, state_entered_at=state_entered_at - interval '1 hour' WHERE order_id='{id}'",
            token(state),
            pre_hold.map_or("NULL".to_owned(), |p| format!("'{}'", token(p)))
        ))
        .await
        .unwrap();
    id
}

fn operation(trigger: Trigger) -> &'static str {
    match trigger {
        Trigger::DraftMutate | Trigger::AdministrativeEdit => "patch_order",
        Trigger::Submit => "submit",
        Trigger::Cancel => "cancel",
        Trigger::AutoVoid => crate::domain::idempotency::AUTO_VOID_OPERATION,
        Trigger::Expire => crate::domain::idempotency::EXPIRE_OPERATION,
        Trigger::ReflectApprovalRequired
        | Trigger::ReflectApprovalNotRequired
        | Trigger::ReflectApprovalGranted
        | Trigger::ReflectApprovalDenied => "reflect_approval",
        Trigger::BeginFulfillment => "begin_fulfillment",
        Trigger::ReportSpawnSignal => "report_spawn_signal",
        Trigger::AcknowledgeCompleted | Trigger::AcknowledgeFailed => "acknowledge",
        Trigger::CancelWorkflowMediated => "workflow_cancel",
        Trigger::Amendment => "amend",
        Trigger::Hold => "hold",
        Trigger::Resume => "resume",
        Trigger::RecordAcceptance => "record_acceptance",
        Trigger::ForceFailUnreconciled => "force_fail",
        Trigger::Create => "create",
    }
}
fn caller_reason(trigger: Trigger) -> Option<String> {
    match trigger {
        Trigger::Cancel | Trigger::CancelWorkflowMediated | Trigger::Amendment => {
            Some("customer request".into())
        }
        Trigger::ForceFailUnreconciled => Some("stuck activation".into()),
        Trigger::AcknowledgeFailed => Some("overlap-presence-unevaluable".into()),
        _ => None,
    }
}
pub(super) fn request(
    order: Uuid,
    trigger: Trigger,
    text: &str,
    version: i32,
) -> TransitionRequest {
    TransitionRequest {
        order_id: order,
        trigger,
        operation: operation(trigger),
        expected_version: version,
        expected_draft_revision: crate::domain::transition::uses_draft_revisions(trigger)
            .then_some(0),
        document: json!({"request": text}),
        proposed: None,
        caller_reason: caller_reason(trigger),
        idempotency_key: key(text),
        correlation_id: Some(u(777)),
        execution: None,
    }
}
fn is_workflow(trigger: Trigger) -> bool {
    crate::domain::state_table::is_workflow_class(trigger)
}

fn ordinary_evidence() -> CompensationEvidence {
    CompensationEvidence {
        drafts_voided: vec![],
        activated_rolled_back: vec![],
        activation_dispatched: false,
        at_sale_facts_emitted: Assertion::Known(false),
        no_active_subscription_remains: Assertion::Known(true),
        operator_attestation: None,
    }
}
fn forced_evidence() -> CompensationEvidence {
    CompensationEvidence {
        activation_dispatched: true,
        at_sale_facts_emitted: Assertion::Unknown(UnknownToken::Unknown),
        no_active_subscription_remains: Assertion::Unknown(UnknownToken::Unknown),
        operator_attestation: Some(OperatorAttestation {
            requested_by: u(91),
            request_audit_id: u(92),
            requested_at: at(),
            approved_by: u(93),
        }),
        ..ordinary_evidence()
    }
}
fn detail<K: crate::infra::events::EventKind>(k: K) -> Arc<dyn EventSpec> {
    Arc::new(EventDetail(k))
}
/// The row's one event detail, valid for its committed post-state (version `v` after).
fn event(row: u8, from: S, v: i32) -> Option<Arc<dyn EventSpec>> {
    let cancelled = |evidence: Option<CompensationEvidence>| OrderCancelled {
        cancelling_actor: "actor-102".into(),
        cancel_reason: "customer request".into(),
        compensation_evidence: evidence,
    };
    Some(match row {
        4 => detail(OrderSubmitted {
            lines: vec![LineRef { line_id: u(5000) }],
            acceptance: None,
        }),
        5 | 15 | 17 | 23 => detail(cancelled(None)),
        16 | 27 => detail(cancelled(Some(ordinary_evidence()))),
        6 | 24 => detail(OrderExpired {
            expired_state: from,
            ttl: "P14D".into(),
            ttl_policy_id: u(181),
            ttl_policy_revision: 1,
            platform_policy_revision: 1,
        }),
        8 | 9 => detail(OrderApproved {
            deciding_authority: "approver-1".into(),
            approved_version: v,
        }),
        10 => detail(OrderRejected {
            deciding_authority: "approver-1".into(),
            denial_reason: "policy".into(),
        }),
        13 => detail(OrderCompleted {
            lines: vec![LineSubscription {
                line_id: u(5000),
                subscription_id: u(6000),
            }],
        }),
        14 | 26 => detail(OrderFulfillmentFailed {
            failure_reason: FailureReason::OverlapPresenceUnevaluable,
            forced_reason: None,
            compensation_evidence: ordinary_evidence(),
        }),
        28 | 29 => detail(OrderFulfillmentFailed {
            failure_reason: FailureReason::OperatorForcedUnreconciled,
            forced_reason: Some("stuck activation".into()),
            compensation_evidence: forced_evidence(),
        }),
        18..=20 => detail(OrderAmended {
            supersedes_version: v - 1,
        }),
        21 => detail(OrderHeld {
            previous_state: from,
            hold_reason: None,
        }),
        22 => detail(OrderResumed {
            restored_state: S::Approved,
        }),
        25 => detail(OrderAcceptanceRecorded {
            accepted_version: v,
            accepted_at: at(),
            recording_actor: "actor-102".into(),
            requirement_source: RequirementSource::PlatformDefault,
        }),
        _ => return None,
    })
}

/// Row 3's administrative document and row 12's D-198 grant: child writes through the bounded
/// handle, never the aggregate.
struct AdminDoc(Uuid);
#[async_trait]
impl DocumentWriter for AdminDoc {
    async fn write(&self, docs: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        docs.insert_order_admin(entity::order_admin::Model {
            order_id: self.0,
            external_reference: Some("PO-1".into()),
            display_label: None,
            internal_notes: None,
            updated_by: u(BUYER.0).to_string(),
            updated_at: docs.transition_time(),
        })
        .await?;
        Ok(())
    }
}
struct GrantDoc {
    order: Uuid,
    generation_offset: i64,
}
#[async_trait]
impl DocumentWriter for GrantDoc {
    async fn write(&self, docs: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        let (version, generation) = docs.locked_version_and_generation();
        docs.insert_fulfillment_grant(entity::fulfillment_grant::Model {
            grant_id: Uuid::new_v4(),
            order_id: self.order,
            order_version: version,
            fulfillment_attempt_id: u(4040),
            generation: generation + self.generation_offset,
            roster_receipt_digest: "sha256:roster".into(),
            roster: json!([]),
            authority_fact_digest: "sha256:authority".into(),
            predecessor_grant_id: None,
            created_at: docs.transition_time(),
            execution_id: docs.execution_id(),
            audit_id: docs.audit_id(),
            audit_outcome: "committed".into(),
        })
        .await?;
        Ok(())
    }
}

/// The admitted contribution of an existing-order row seeded in `from`.
pub(super) fn prepared(row: u8, order: Uuid, from: S, candidate: Option<i32>) -> Prepared {
    let aggregate = match declared_kind(RowId::of(row)) {
        ContributionKind::None => AggregateContribution::None,
        ContributionKind::DraftEdit => AggregateContribution::DraftEdit(DraftHeaderEdit::default()),
        ContributionKind::Version => AggregateContribution::Version(VersionContribution {
            candidate: candidate.unwrap(),
            payer_tenant_id: u(30),
            category: NEW_SALE.into(),
            contract_id: None,
            market_currency: Some("EUR".into()),
            market_region: Some("DE".into()),
            amendment_reason: (row != 4).then(|| "customer request".into()),
        }),
        ContributionKind::BeginFulfillment => {
            AggregateContribution::BeginFulfillment { tolerated: false }
        }
        ContributionKind::SpawnSignal => AggregateContribution::SpawnSignal,
        ContributionKind::CompensationEvidence => AggregateContribution::CompensationEvidence(
            serde_json::to_value(ordinary_evidence()).unwrap(),
        ),
        ContributionKind::ForcedFailure => {
            AggregateContribution::ForcedFailure(serde_json::to_value(forced_evidence()).unwrap())
        }
    };
    let mut p = Prepared::new(aggregate);
    p.guards = guards(row, None);
    p.inputs = resolved(row);
    p.event = event(row, from, candidate.unwrap_or(version_of(from)));
    if matches!(row, 4 | 18 | 19 | 20) {
        p.claims = Some(
            ProposedClaims::new(
                u(30),
                u(10),
                [OverlapScopeKey::try_from(format!("sku-{order}")).unwrap()],
            )
            .unwrap(),
        );
    }
    if row == 3 {
        p.admin_changes = vec![AdminChange {
            field: AdminField::Order(AdminAttribute::ExternalReference),
            prior: None,
            new: Some("PO-1".into()),
        }];
        p.documents = Some(Arc::new(AdminDoc(order)));
    }
    if row == 12 {
        p.documents = Some(Arc::new(GrantDoc {
            order,
            generation_offset: 1,
        }));
    }
    p
}

/// D-188 step 2 through the engine's specialized subflow: the reserved candidate.
async fn reserve(
    engine: &Engine,
    caller: &Caller,
    req: &TransitionRequest,
) -> (i32, TransitionRequest) {
    reserve_with_dates(engine, caller, req, json!({})).await
}
/// [`reserve`] freezing an S2-10 date basis in the attempt's `date_policy_basis`.
pub(super) async fn reserve_with_dates(
    engine: &Engine,
    caller: &Caller,
    req: &TransitionRequest,
    date_policy_basis: Value,
) -> (i32, TransitionRequest) {
    let inputs = AttemptInputs {
        prepared_draft_revision: (req.trigger == Trigger::Submit).then_some(0),
        proposed_arrangement: json!({"payer": s(u(30))}),
        authorization_fact_fingerprint: "facts".into(),
        original_principal: json!({"subject": s(u(BUYER.0))}),
        proof_reference: None,
        date_policy_basis,
        commercial_subject_id: u(BUYER.0),
        commercial_subject_type: "user".into(),
        commercial_subject_tenant_id: u(BUYER.1),
    };
    let PreparedExecution::Execution(execution) = engine
        .prepare_commercial_attempt(caller, req, inputs, json!({}))
        .await
        .unwrap()
    else {
        panic!("no commercial execution")
    };
    let (execution, crate::infra::storage::repo::idempotency::Frozen::Commercial(attempt)) =
        *execution
    else {
        panic!("not a commercial attempt")
    };
    let mut req = req.clone();
    req.execution = Some(execution.owner());
    (attempt.candidate_version, req)
}

fn token(s: S) -> String {
    crate::domain::audit::state_token(s)
}

/// Every existing-order expanded key (45) admitted through the engine: target, dwell clock,
/// pre-hold, counters, version append, irreversible facts, one committed audit entry (one per
/// field for row 3), exactly the declared event cardinality, and the settled response.
#[tokio::test]
async fn every_expanded_key_commits_its_row_effects_audit_and_event_cardinality() {
    let t = T::new().await;
    let (table, _) = registries();
    let mut cases = 0;
    for row in table.rows().iter().filter(|r| r.id.number() != 1) {
        for from in row.from.iter().flatten().copied() {
            let n = row.id.number();
            let text = format!("row{n}-{}", token(from));
            let pre_hold = (from == S::OnHold).then_some(if matches!(n, 26 | 27 | 29) {
                S::InFulfillment
            } else {
                S::Approved
            });
            let probe = t.engine(&[]);
            let order = seed(&t, &probe, &format!("seed-{text}"), from, pre_hold).await;
            let engine = t.engine(&[order]);
            let caller = if is_workflow(row.trigger) {
                workflow()
            } else {
                buyer()
            };
            let before = t.order_row(order).await;
            let outbox_before = t.outbox().await;
            let audits_before = t.audits(order).await;
            let mut req = request(order, row.trigger, &text, version_of(from));
            let mut candidate = None;
            if row.is_versioning() {
                let (c, r) = reserve(&engine, &caller, &req).await;
                candidate = Some(c);
                req = r;
            }
            let prep = fixed(prepared(n, order, from, candidate));
            let out = if matches!(row.trigger, Trigger::Expire | Trigger::AutoVoid) {
                internal(&t, &engine, order, from, req, &prep).await
            } else {
                engine.transition(&caller, req.clone(), &prep).await
            }
            .unwrap_or_else(|e| panic!("{} from {from:?}: {e:?}", row.id));
            let OutcomeKind::Committed { order_id, audit_id } = out.kind else {
                panic!("{} from {from:?}: {out:?}", row.id)
            };
            assert_eq!(order_id, order);
            let after = t.order_row(order).await;
            let target = row.effective_target(from, pre_hold).unwrap();
            assert_eq!(after["state"], json!(token(target)), "{}", row.id);
            assert_eq!(
                after["state_entered_at"] != before["state_entered_at"],
                target != from,
                "{} dwell clock",
                row.id
            );
            let expected_pre_hold = match (target == S::OnHold, target != from) {
                (true, true) => json!(token(from)),
                (false, true) => Value::Null,
                (_, false) => before["pre_hold_state"].clone(),
            };
            assert_eq!(after["pre_hold_state"], expected_pre_hold, "{}", row.id);
            assert_eq!(
                after["resume_count"].as_i64().unwrap(),
                before["resume_count"].as_i64().unwrap()
                    + i64::from(row.trigger == Trigger::Resume)
            );
            assert_eq!(
                after["amendment_count"].as_i64().unwrap(),
                before["amendment_count"].as_i64().unwrap()
                    + i64::from(row.trigger == Trigger::Amendment)
            );
            assert_eq!(
                after["draft_revision"].as_i64().unwrap(),
                i64::from(n == 2),
                "{}",
                row.id
            );
            assert_eq!(
                after["current_version"].as_i64().unwrap(),
                i64::from(candidate.unwrap_or(version_of(from))),
                "{}",
                row.id
            );
            assert_eq!(!after["spawn_signal_at"].is_null(), n == 12, "{}", row.id);
            assert_eq!(
                !after["compensation_evidence"].is_null(),
                matches!(n, 14 | 16 | 26 | 27 | 28 | 29),
                "{}",
                row.id
            );
            // One committed entry per transition (row 3: one per changed field), sealed v3,
            // linked from the engine's before-facts.
            let committed: Value = t.json(&format!(
                "SELECT to_jsonb(a) AS j FROM bss_orders__transition_audit a WHERE audit_id='{audit_id}'"
            )).await;
            assert_eq!(committed["outcome"], json!("committed"));
            assert_eq!(committed["hash_version"], json!(3));
            // Tenant equality with the locked aggregate (D-99/D-104).
            assert_eq!(committed["audit_tenant_id"], after["audit_tenant_id"]);
            assert_eq!(committed["resource_tenant_id"], after["resource_tenant_id"]);
            assert_eq!(committed["from_state"], json!(token(from)), "{}", row.id);
            assert_eq!(committed["to_state"], json!(token(target)));
            assert_eq!(
                committed["sequence"].as_i64().unwrap(),
                after["audit_sequence"].as_i64().unwrap()
            );
            assert_eq!(t.audits(order).await, audits_before + 1, "{}", row.id);
            // Exactly the declared event cardinality, built from the committed post-state.
            assert_eq!(
                t.outbox().await - outbox_before,
                i64::from(row.event.is_some()),
                "{} event",
                row.id
            );
            // Settled success bound to the last audit entry; replay is verbatim.
            assert_eq!(
                t.n(&format!("SELECT count(*) AS n FROM bss_orders__idempotency WHERE status='settled' AND outcome='success' AND audit_id='{audit_id}'")).await,
                1
            );
            if row.is_versioning() {
                assert_eq!(
                    t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}' AND version={} AND supersedes_version={}", candidate.unwrap(), version_of(from))).await,
                    1
                );
                assert_eq!(
                    t.n(&format!("SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' AND status='committed'")).await,
                    1
                );
            } else {
                assert_eq!(
                    t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}'")).await,
                    i64::from(version_of(from))
                );
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 45);
}

/// The worker entry for rows 6 and 24: a real maintenance grant and discovered target scope.
async fn internal(
    t: &T,
    engine: &Engine,
    order: Uuid,
    from: S,
    mut req: TransitionRequest,
    prep: &dyn Preparation,
) -> Result<EngineOutcome, EngineError> {
    use crate::infra::maintenance::{MaintenanceAuthority, MaintenanceTask, ServiceActor, scope};
    let task = if req.trigger == Trigger::AutoVoid {
        MaintenanceTask::DraftAutoVoid
    } else {
        MaintenanceTask::StateExpiry
    };
    let authority =
        MaintenanceAuthority::configured(ServiceActor::configured(u(900), u(901)).unwrap(), [task]);
    let grant = authority.grant(task).unwrap();
    let found = scope::discover_orders(
        &t.env.db,
        &grant,
        &token(from),
        time::OffsetDateTime::now_utc() + time::Duration::days(1),
        100,
    )
    .await
    .unwrap();
    let discovered = found.iter().find(|d| d.order_id() == order).unwrap();
    let target = crate::infra::maintenance::TargetScope::from_discovered_order(discovered);
    req.caller_reason = None;
    engine.transition_internal(&grant, &target, req, prep).await
}

/// Every guarded row refuses at each of its guards in registration order: settled, resolved
/// refusal audit (no sequence), aggregate byte-identical, no version, no event; replay verbatim.
/// Engine refusals: not-admissible from every terminal state (no terminal exits) and
/// version-conflict.
#[tokio::test]
async fn every_row_refuses_as_a_committed_settled_outcome_without_effects() {
    let t = T::new().await;
    let (table, registry) = registries();
    let mut refusals = 0;
    for row in table.rows().iter().filter(|r| r.id.number() != 1) {
        let n = row.id.number();
        let from = row.from[0].unwrap();
        let pre_hold = (from == S::OnHold).then_some(S::InFulfillment);
        for (i, guard) in registry.row_guards(row.id).iter().enumerate() {
            let text = format!("refuse-{n}-{i}");
            let probe = t.engine(&[]);
            let order = seed(&t, &probe, &format!("seed-{text}"), from, pre_hold).await;
            let engine = t.engine(&[order]);
            let caller = if is_workflow(row.trigger) {
                workflow()
            } else {
                buyer()
            };
            let mut req = request(order, row.trigger, &text, version_of(from));
            let mut candidate = None;
            if row.is_versioning() {
                let (c, r) = reserve(&engine, &caller, &req).await;
                candidate = Some(c);
                req = r;
            }
            let mut p = prepared(n, order, from, candidate);
            p.guards = guards(n, Some(guard.name));
            let before = t.order_row(order).await;
            let outbox = t.outbox().await;
            let out = if matches!(row.trigger, Trigger::Expire | Trigger::AutoVoid) {
                internal(&t, &engine, order, from, req.clone(), &fixed(p.clone())).await
            } else {
                engine
                    .transition(&caller, req.clone(), &fixed(p.clone()))
                    .await
            }
            .unwrap();
            let OutcomeKind::Refused { reason, audit_id } = out.kind else {
                panic!("{} {}: {out:?}", row.id, guard.name)
            };
            assert_eq!(reason, guard.reasons[0]);
            assert_eq!(out.response.status, reason.mapping().http_status);
            // D-182/D-201: the refused force request is the two-person request reference.
            if reason == Reason::SecondApproverRequired {
                assert_eq!(
                    out.response.body["context"]["data"]["request_audit_id"],
                    json!(s(audit_id))
                );
                let observation = t
                    .json(&format!(
                        "SELECT force_request_observation AS j FROM bss_orders__transition_audit WHERE audit_id='{audit_id}'"
                    ))
                    .await;
                assert_eq!(observation["state"], json!(token(from)), "{}", row.id);
            }
            let after = t.order_row(order).await;
            // Allocation high-water moved only in the separately committed D-188 reservation.
            assert_eq!(
                after, before,
                "{} {} changed the aggregate",
                row.id, guard.name
            );
            assert_eq!(t.outbox().await, outbox, "{} refusal emitted", row.id);
            let audit: Value = t.json(&format!(
                "SELECT to_jsonb(a) AS j FROM bss_orders__transition_audit a WHERE audit_id='{audit_id}'"
            )).await;
            assert_eq!(audit["outcome"], json!("refused"));
            assert_eq!(audit["hash_version"], json!(3), "{} refusal is v3", row.id);
            assert_eq!(audit["audit_tenant_id"], before["audit_tenant_id"]);
            assert_eq!(audit["resource_tenant_id"], before["resource_tenant_id"]);
            assert_eq!(audit["sequence"], Value::Null);
            assert_eq!(audit["from_state"], audit["to_state"]);
            // Settled with exactly this refusal's audit entry; no claim or grant was taken.
            assert_eq!(
                t.n(&format!("SELECT count(*) AS n FROM bss_orders__idempotency WHERE status='settled' AND outcome='refused' AND audit_id='{audit_id}'")).await,
                1,
                "{} {} settlement",
                row.id,
                guard.name
            );
            assert_eq!(
                t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}'")).await
                    + t.n(&format!("SELECT count(*) AS n FROM bss_orders__fulfillment_grant WHERE order_id='{order}'")).await,
                0,
                "{} {} left a claim or grant",
                row.id,
                guard.name
            );
            assert_eq!(
                t.n(&format!(
                    "SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}'"
                ))
                .await,
                i64::from(version_of(from))
            );
            if row.is_versioning() {
                assert_eq!(
                    t.n(&format!("SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' AND status='refused'")).await,
                    1
                );
            }
            // Same key replays the stored refusal, without a new audit row.
            let audits = t.audits(order).await;
            let replay = if matches!(row.trigger, Trigger::Expire | Trigger::AutoVoid) {
                internal(&t, &engine, order, from, req.clone(), &fixed(p)).await
            } else {
                engine.transition(&caller, req.clone(), &fixed(p)).await
            }
            .unwrap();
            assert!(matches!(replay.kind, OutcomeKind::Replayed { .. }));
            assert_eq!(replay.response, out.response);
            assert_eq!(t.audits(order).await, audits);
            refusals += 1;
        }
    }
    let declared: usize = table
        .rows()
        .iter()
        .filter(|r| r.id.number() != 1)
        .map(|r| registry.row_guards(r.id).len())
        .sum();
    assert_eq!(refusals, declared, "every guard of every row");
    // No terminal exits: every trigger from every terminal state is not-admissible, settled.
    let probe = t.engine(&[]);
    for (i, terminal) in crate::domain::state_table::TERMINAL.iter().enumerate() {
        let order = seed(&t, &probe, &format!("terminal-{i}"), *terminal, None).await;
        let engine = t.engine(&[order]);
        for trigger in [
            Trigger::Cancel,
            Trigger::Hold,
            Trigger::Resume,
            Trigger::ForceFailUnreconciled,
            Trigger::AdministrativeEdit,
            Trigger::RecordAcceptance,
            Trigger::AcknowledgeFailed,
        ] {
            let caller = if is_workflow(trigger) {
                workflow()
            } else {
                buyer()
            };
            let text = format!("terminal-{i}-{}", operation(trigger));
            let p = Prepared::new(AggregateContribution::None);
            let out = engine
                .transition(&caller, request(order, trigger, &text, 2), &fixed(p))
                .await
                .unwrap();
            let OutcomeKind::Refused { reason, .. } = out.kind else {
                panic!("{terminal:?} {trigger:?}: {out:?}")
            };
            assert_eq!(reason, Reason::NotAdmissible);
            assert_eq!(
                out.response.body["context"]["data"]["state"],
                json!(token(*terminal))
            );
        }
    }
}

/// D-110: a Workflow reflection carrying a superseded version gets version-conflict even when
/// the state moved too; an ordinary trigger gets not-admissible first.
#[tokio::test]
async fn workflow_version_precedes_admissibility_and_ordinary_admissibility_precedes_version() {
    let t = T::new().await;
    let probe = t.engine(&[]);
    let order = seed(&t, &probe, "seed-w", S::Approved, None).await;
    let engine = t.engine(&[order]);
    let mut p = Prepared::new(AggregateContribution::None);
    p.guards = guards(9, None);
    let out = engine
        .transition(
            &workflow(),
            request(order, Trigger::ReflectApprovalGranted, "w-1", 1),
            &fixed(p),
        )
        .await
        .unwrap();
    assert!(matches!(
        out.kind,
        OutcomeKind::Refused {
            reason: Reason::VersionConflict,
            ..
        }
    ));
    assert_eq!(
        out.response.body["context"]["data"]["current_version"],
        json!(2)
    );
    let out = engine
        .transition(
            &buyer(),
            request(order, Trigger::DraftMutate, "o-1", 1),
            &fixed(Prepared::new(AggregateContribution::DraftEdit(
                DraftHeaderEdit::default(),
            ))),
        )
        .await
        .unwrap();
    assert!(matches!(
        out.kind,
        OutcomeKind::Refused {
            reason: Reason::NotAdmissible,
            ..
        }
    ));
}

/// OL-4 / D-147: a stale client or prepared draft revision settles version-conflict naming the
/// current revision, with no draft write; an absent client revision in draft conflicts too.
#[tokio::test]
async fn stale_client_or_prepared_draft_revisions_settle_version_conflict() {
    let t = T::new().await;
    let engine = t.engine(&[]);
    let order = create(&t, &engine, "seed-d").await;
    let mut p = Prepared::new(AggregateContribution::DraftEdit(DraftHeaderEdit::default()));
    p.guards = guards(2, None);
    // Commit one draft edit (revision 0 -> 1).
    engine
        .transition(
            &buyer(),
            request(order, Trigger::DraftMutate, "d-1", 1),
            &fixed(p.clone()),
        )
        .await
        .unwrap();
    for (text, client) in [("d-2", Some(0)), ("d-3", None)] {
        let mut req = request(order, Trigger::DraftMutate, text, 1);
        req.expected_draft_revision = client;
        let out = engine
            .transition(&buyer(), req, &fixed(p.clone()))
            .await
            .unwrap();
        assert!(matches!(
            out.kind,
            OutcomeKind::Refused {
                reason: Reason::VersionConflict,
                ..
            }
        ));
        assert_eq!(
            out.response.body["context"]["data"]["draft_revision"],
            json!(1)
        );
    }
    // A prepared snapshot that a racing edit superseded: the client sends the current revision
    // but its inputs were resolved from revision 1 before another edit committed revision 2.
    let racing = Prep {
        prepared: p.clone(),
        side_effect: Some((
            t.env.pg.raw.clone(),
            format!("UPDATE bss_orders__order SET draft_revision=2 WHERE order_id='{order}'"),
        )),
    };
    let mut req = request(order, Trigger::DraftMutate, "d-4", 1);
    req.expected_draft_revision = Some(1);
    let out = engine.transition(&buyer(), req, &racing).await.unwrap();
    assert!(matches!(
        out.kind,
        OutcomeKind::Refused {
            reason: Reason::VersionConflict,
            ..
        }
    ));
    assert_eq!(t.order_row(order).await["draft_revision"], json!(2));
}

/// Create (D-105): one aggregate with the exact initial values, empty version 1, committed
/// create audit sequence 1, event-less; replay returns the original identity/number; a changed
/// payload is a mismatch; a refused create inserts no placeholder and replays its refusal.
#[tokio::test]
async fn create_initializes_exactly_and_replays_and_refusals_create_nothing() {
    let t = T::new().await;
    let engine = t.engine(&[]);
    let outbox = t.outbox().await;
    let out = engine
        .create(
            &buyer(),
            create_request("c-1", json!({"category": NEW_SALE})),
            &fixed(create_prepared(false)),
        )
        .await
        .unwrap();
    let OutcomeKind::Committed { order_id, audit_id } = out.kind else {
        panic!("{out:?}")
    };
    let row = t.order_row(order_id).await;
    assert_eq!(row["state"], json!("draft"));
    assert_eq!(row["current_version"], json!(1));
    assert_eq!(row["version_allocation_high_water"], json!(1));
    assert_eq!(row["draft_revision"], json!(0));
    assert_eq!(row["audit_sequence"], json!(1));
    assert_eq!(row["resume_count"], json!(0));
    assert_eq!(row["amendment_count"], json!(0));
    assert_eq!(row["audit_tenant_id"], row["resource_tenant_id"]);
    assert_eq!(row["initiating_actor"], json!(s(u(BUYER.0))));
    assert_eq!(row["sales_path"], json!("self_service"));
    assert_eq!(row["state_entered_at"], row["created_at"]);
    for null in [
        "pre_hold_state",
        "spawn_signal_at",
        "authorization_failure_tolerated_at",
        "compensation_evidence",
        "contract_id",
    ] {
        assert!(row[null].is_null(), "{null}");
    }
    let version: Value = t
        .json(&format!(
            "SELECT to_jsonb(v) AS j FROM bss_orders__order_version v WHERE order_id='{order_id}'"
        ))
        .await;
    assert_eq!(version["version"], json!(1));
    assert!(version["supersedes_version"].is_null());
    assert!(version["market_currency"].is_null());
    assert_eq!(version["reason"], json!("create"));
    assert_eq!(version["actor_tenant_id"], json!(s(u(BUYER.1))));
    assert_eq!(version["created_at"], row["created_at"]);
    let audit: Value = t
        .json(&format!(
            "SELECT to_jsonb(a) AS j FROM bss_orders__transition_audit a WHERE audit_id='{audit_id}'"
        ))
        .await;
    assert_eq!(audit["sequence"], json!(1));
    assert!(audit["from_state"].is_null());
    assert_eq!(audit["to_state"], json!("draft"));
    assert_eq!(t.outbox().await, outbox, "create is event-less");
    assert_eq!(out.response.status, 201);
    // Replay: identical response, no second order.
    let replay = engine
        .create(
            &buyer(),
            create_request("c-1", json!({"category": NEW_SALE})),
            &fixed(create_prepared(false)),
        )
        .await
        .unwrap();
    assert!(matches!(replay.kind, OutcomeKind::Replayed { .. }));
    assert_eq!(replay.response, out.response);
    assert_eq!(t.n("SELECT count(*) AS n FROM bss_orders__order").await, 1);
    // Changed payload under the same key: mismatch, audited unresolved, winner unchanged.
    let registry = t
        .json("SELECT to_jsonb(i) AS j FROM bss_orders__idempotency i WHERE operation='create'")
        .await;
    let err = engine
        .create(
            &buyer(),
            create_request("c-1", json!({"category": "other"})),
            &fixed(create_prepared(false)),
        )
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Refused(Reason::IdempotencyMismatch));
    assert_eq!(
        t.json("SELECT to_jsonb(i) AS j FROM bss_orders__idempotency i WHERE operation='create'")
            .await,
        registry
    );
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE reason='idempotency-mismatch' AND order_id IS NULL AND requested_order_ref IS NULL").await,
        1
    );
    // Guard refusal: no aggregate, version or placeholder; settled with NULL order; replayed.
    let refused = engine
        .create(
            &buyer(),
            create_request("c-2", json!({"category": "change"})),
            &fixed(create_prepared(true)),
        )
        .await
        .unwrap();
    assert!(matches!(
        refused.kind,
        OutcomeKind::Refused {
            reason: Reason::CategoryNotAdmitted,
            ..
        }
    ));
    assert_eq!(t.n("SELECT count(*) AS n FROM bss_orders__order").await, 1);
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__idempotency WHERE outcome='refused' AND order_id IS NULL").await,
        1
    );
    let again = engine
        .create(
            &buyer(),
            create_request("c-2", json!({"category": "change"})),
            &fixed(create_prepared(true)),
        )
        .await
        .unwrap();
    assert_eq!(again.response, refused.response);
    // Authorization denial: unresolved evidence, no registry probe or settlement.
    let stranger = user(555, 55);
    let registry_rows = t
        .n("SELECT count(*) AS n FROM bss_orders__idempotency")
        .await;
    let err = engine
        .create(
            &stranger,
            create_request("c-3", json!({"category": NEW_SALE})),
            &fixed(create_prepared(false)),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Refused(Reason::OperationNotPermittedForActor)
    );
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await,
        registry_rows
    );
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE subject_tenant_id='{}' AND outcome='refused' AND order_id IS NULL", u(55))).await,
        1
    );
}

/// Concurrent same-key creates: one aggregate, one version 1 and one create audit; the loser
/// replays the winner's identity and number.
#[tokio::test]
async fn concurrent_same_key_creates_commit_one_aggregate() {
    let t = T::new().await;
    let engine = t.engine(&[]);
    let (caller_a, caller_b) = (buyer(), buyer());
    let (prep_a, prep_b) = (fixed(create_prepared(false)), fixed(create_prepared(false)));
    let a = engine.create(
        &caller_a,
        create_request("same", json!({"category": NEW_SALE})),
        &prep_a,
    );
    let b = engine.create(
        &caller_b,
        create_request("same", json!({"category": NEW_SALE})),
        &prep_b,
    );
    let (a, b) = tokio::join!(a, b);
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.response, b.response);
    assert_eq!(t.n("SELECT count(*) AS n FROM bss_orders__order").await, 1);
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__order_version")
            .await,
        1
    );
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE outcome='committed'")
            .await,
        1
    );
}

/// Changed authorization facts after the decision: audited, never settled, no business write;
/// lost access stays non-disclosing. A later attempt under the same key runs fresh.
#[tokio::test]
async fn changed_facts_and_lost_access_audit_without_settling_the_key() {
    let t = T::new().await;
    let engine = t.engine(&[]);
    let order = seed(&t, &engine, "seed-f", S::Submitted, None).await;
    let mut p = Prepared::new(AggregateContribution::None);
    p.guards = guards(21, None);
    p.event = event(21, S::Submitted, 2);
    // Payer reassigned between authorization and lock (payer 31 is still within the policy).
    let stale = Prep {
        prepared: p.clone(),
        side_effect: Some((
            t.env.pg.raw.clone(),
            format!(
                "UPDATE bss_orders__order SET payer_tenant_id='{}' WHERE order_id='{order}'",
                u(31)
            ),
        )),
    };
    let before_registry = t
        .n("SELECT count(*) AS n FROM bss_orders__idempotency")
        .await;
    let err = engine
        .transition(&buyer(), request(order, Trigger::Hold, "h-1", 2), &stale)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Refused(Reason::AuthorizationContextChanged)
    );
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await,
        before_registry
    );
    assert_eq!(t.order_row(order).await["state"], json!("submitted"));
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE requested_order_ref='{order}' AND reason='authorization-context-changed' AND order_id IS NULL")).await,
        1
    );
    // Same key, fresh authorization: executes on its merits.
    let out = engine
        .transition(
            &buyer(),
            request(order, Trigger::Hold, "h-1", 2),
            &fixed(p.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }));
    // Resource tenant moved out of the caller's scope: not found, never a conflict.
    let order2 = seed(&t, &engine, "seed-g", S::Draft, None).await;
    let mut p2 = Prepared::new(AggregateContribution::None);
    p2.guards = guards(5, None);
    p2.event = event(5, S::Draft, 1);
    let lost = Prep {
        prepared: p2,
        side_effect: Some((
            t.env.pg.raw.clone(),
            format!(
                "UPDATE bss_orders__order SET resource_tenant_id='{}' WHERE order_id='{order2}'",
                u(12)
            ),
        )),
    };
    let err = engine
        .transition(&buyer(), request(order2, Trigger::Cancel, "x-1", 1), &lost)
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Refused(Reason::OrderNotFound));
}

/// §4.2 through the engine: a changed fingerprint is a mismatch and a live foreign lease is
/// still-processing; both audit (resolved) and leave the winner untouched.
#[tokio::test]
async fn mismatch_and_live_lease_audit_and_preserve_the_winner() {
    let t = T::new().await;
    let engine = t.engine(&[]);
    let order = seed(&t, &engine, "seed-m", S::Submitted, None).await;
    let mut p = Prepared::new(AggregateContribution::None);
    p.guards = guards(21, None);
    p.event = event(21, S::Submitted, 2);
    engine
        .transition(
            &buyer(),
            request(order, Trigger::Hold, "k-1", 2),
            &fixed(p.clone()),
        )
        .await
        .unwrap();
    let winner = t
        .json("SELECT to_jsonb(i) AS j FROM bss_orders__idempotency i WHERE idempotency_key='k-1'")
        .await;
    let mut changed = request(order, Trigger::Hold, "k-1", 2);
    changed.document = json!({"request": "different"});
    let err = engine
        .transition(&buyer(), changed, &fixed(p.clone()))
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Refused(Reason::IdempotencyMismatch));
    assert_eq!(
        t.json(
            "SELECT to_jsonb(i) AS j FROM bss_orders__idempotency i WHERE idempotency_key='k-1'"
        )
        .await,
        winner
    );
    // A live in-flight marker of an earlier crashed owner (lease far in the future).
    let order2 = seed(&t, &engine, "seed-n", S::Submitted, None).await;
    let principal = format!("{}/{}", u(BUYER.1), u(BUYER.0));
    let fp = {
        let req = request(order2, Trigger::Hold, "k-2", 2);
        crate::domain::idempotency::FingerprintInput {
            operation: req.operation,
            trigger: req.trigger,
            target: crate::domain::idempotency::FingerprintTarget::Order {
                order_id: order2,
                expected_version: 2,
            },
            axes: crate::domain::idempotency::FingerprintAxes {
                seller_tenant_id: u(20),
                resource_tenant_id: u(10),
                payer_tenant_id: u(30),
            },
            draft_revision: crate::domain::idempotency::DraftRevisionInput::NotApplicable,
            contribution: &req.document,
        }
        .fingerprint()
        .unwrap()
    };
    t.env.pg.sql(&format!(
        "INSERT INTO bss_orders__idempotency (operation, principal_scope, idempotency_key, order_id, request_fingerprint, status, execution_id, fencing_generation, lease_expires_at, created_at, expires_at) \
         VALUES ('hold', '{principal}', 'k-2', '{order2}', '{}', 'in_flight', '{}', 0, now() + interval '1 hour', now(), now() + interval '1 day')",
        fp.as_str(), Uuid::new_v4()
    )).await.unwrap();
    let err = engine
        .transition(
            &buyer(),
            request(order2, Trigger::Hold, "k-2", 2),
            &fixed(p),
        )
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Refused(Reason::StillProcessing));
    assert_eq!(t.order_row(order2).await["state"], json!("submitted"));
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE order_id='{order2}' AND reason='still-processing' AND sequence IS NULL")).await,
        1
    );
}

/// Steps 3/3.1 and D-113 through the engine: an unresolvable input settles its unevaluable
/// reason; an earlier-registered failing guard outranks it.
#[tokio::test]
async fn unresolvable_inputs_settle_unevaluable_but_never_mask_earlier_guards() {
    let t = T::new().await;
    let probe = t.engine(&[]);
    let order = seed(&t, &probe, "seed-u", S::Approved, None).await;
    let engine = t.engine(&[order]);
    let mut p = prepared(11, order, S::Approved, None);
    p.inputs = Inputs::new().with(
        crate::domain::guards::InputPort::AcceptanceRequirement,
        InputState::Unresolvable(Reason::AcceptanceRequirementUnevaluable),
    );
    let out = engine
        .transition(
            &workflow(),
            request(order, Trigger::BeginFulfillment, "u-1", 2),
            &fixed(p),
        )
        .await
        .unwrap();
    assert!(matches!(
        out.kind,
        OutcomeKind::Refused {
            reason: Reason::AcceptanceRequirementUnevaluable,
            ..
        }
    ));
    assert_eq!(out.response.status, 503);
    // Row 25: already-recorded fails ahead of the recording-party guard's unavailable input.
    let order2 = seed(&t, &probe, "seed-v", S::Submitted, None).await;
    let mut p = prepared(25, order2, S::Submitted, None);
    p.guards = guards(25, Some("preconditions.acceptance-not-recorded"));
    p.inputs = Inputs::new().with(
        crate::domain::guards::InputPort::AcceptanceRequirement,
        InputState::Unresolvable(Reason::AcceptanceRequirementUnevaluable),
    );
    let out = engine
        .transition(
            &buyer(),
            request(order2, Trigger::RecordAcceptance, "u-2", 2),
            &fixed(p),
        )
        .await
        .unwrap();
    assert!(matches!(
        out.kind,
        OutcomeKind::Refused {
            reason: Reason::AcceptanceAlreadyRecorded,
            ..
        }
    ));
}

/// Fails the attempt at one named boundary (or terminates the session after the last
/// statement, so COMMIT's acknowledgement is lost).
struct FailAt {
    point: FaultPoint,
    kill_before_commit: Option<(DatabaseConnection, Uuid)>,
    hits: AtomicUsize,
    /// Every boundary the attempt reached, in order: nothing may follow the injected one.
    visited: std::sync::Mutex<Vec<FaultPoint>>,
}
impl FailAt {
    fn visited(&self) -> Vec<FaultPoint> {
        self.visited.lock().unwrap().clone()
    }
}
#[async_trait]
impl FaultHook for FailAt {
    async fn at(&self, point: FaultPoint, tx: &DbTx<'_>) -> Result<(), Abort> {
        self.visited.lock().unwrap().push(point);
        if point != self.point {
            return Ok(());
        }
        self.hits.fetch_add(1, Ordering::SeqCst);
        if let Some((raw, order)) = &self.kill_before_commit {
            let pid = crate::infra::storage::repo::backend_pid_for_test(tx, *order)
                .await
                .unwrap();
            raw.execute_unprepared(&format!("SELECT pg_terminate_backend({pid}, 5000)"))
                .await
                .unwrap();
            return Ok(());
        }
        Err(Abort::Injected(point))
    }
}
fn fail_at(point: FaultPoint) -> Arc<FailAt> {
    Arc::new(FailAt {
        point,
        kill_before_commit: None,
        hits: AtomicUsize::new(0),
        visited: std::sync::Mutex::new(Vec::new()),
    })
}

/// Failure injection at every persistence boundary of an eventful transition with child
/// documents (row 12 grant + spawn), a refusal and create: everything rolls back, the key stays
/// unclaimed and a clean retry commits exactly once.
#[tokio::test]
async fn failure_at_every_persistence_boundary_rolls_back_everything() {
    let t = T::new().await;
    let probe = t.engine(&[]);
    for (i, point) in [
        FaultPoint::Gate,
        FaultPoint::Claims,
        FaultPoint::Version,
        FaultPoint::Documents,
        FaultPoint::Aggregate,
        FaultPoint::Audit,
        FaultPoint::Enqueue,
        FaultPoint::Settle,
    ]
    .into_iter()
    .enumerate()
    {
        // Row 14 (eventful, evidence) and row 12 (grant document) from in_fulfillment.
        for row in [14u8, 12] {
            let order = seed(
                &t,
                &probe,
                &format!("seed-f{i}-{row}"),
                S::InFulfillment,
                None,
            )
            .await;
            let engine = t.engine(&[order]);
            let trigger = if row == 14 {
                Trigger::AcknowledgeFailed
            } else {
                Trigger::ReportSpawnSignal
            };
            let text = format!("fault-{i}-{row}");
            let before = t.order_row(order).await;
            let (outbox, audits) = (t.outbox().await, t.audits(order).await);
            let hook = fail_at(point);
            let faulty = engine.with_fault(hook.clone());
            let err = faulty
                .transition(
                    &workflow(),
                    request(order, trigger, &text, 2),
                    &fixed(prepared(row, order, S::InFulfillment, None)),
                )
                .await
                .unwrap_err();
            assert!(
                hook.hits.load(Ordering::SeqCst) >= 1,
                "{point:?} not reached"
            );
            // No later algorithm step ran after the failed boundary.
            assert_eq!(hook.visited().last(), Some(&point), "{point:?} row {row}");
            assert_eq!(err, EngineError::Internal, "{point:?}");
            assert_eq!(t.order_row(order).await, before, "{point:?} row {row}");
            assert_eq!(t.outbox().await, outbox, "{point:?}");
            assert_eq!(t.audits(order).await, audits, "{point:?}");
            assert_eq!(
                t.n(&format!("SELECT count(*) AS n FROM bss_orders__idempotency WHERE idempotency_key='{text}'")).await,
                0,
                "{point:?} left a claim"
            );
            assert_eq!(
                t.n(&format!("SELECT count(*) AS n FROM bss_orders__fulfillment_grant WHERE order_id='{order}'")).await,
                0
            );
            let ok = engine
                .transition(
                    &workflow(),
                    request(order, trigger, &text, 2),
                    &fixed(prepared(row, order, S::InFulfillment, None)),
                )
                .await
                .unwrap();
            assert!(
                matches!(ok.kind, OutcomeKind::Committed { .. }),
                "{point:?}"
            );
            assert_eq!(t.outbox().await, outbox + i64::from(row == 14));
        }
    }
    // A versioning row (submit, D-188, with an overlap claim) at every boundary: no version,
    // claim, event, audit or settlement survives; the separately committed reservation keeps
    // its attempt and in-flight owner unchanged, and the same owner then commits exactly once.
    for (i, point) in [
        FaultPoint::Gate,
        FaultPoint::Claims,
        FaultPoint::Version,
        FaultPoint::Documents,
        FaultPoint::Aggregate,
        FaultPoint::Audit,
        FaultPoint::Enqueue,
        FaultPoint::Settle,
    ]
    .into_iter()
    .enumerate()
    {
        let order = create(&t, &probe, &format!("seed-v{i}")).await;
        let engine = t.engine(&[]);
        let text = format!("fault-v{i}");
        let (candidate, req) = reserve(
            &engine,
            &buyer(),
            &request(order, Trigger::Submit, &text, 1),
        )
        .await;
        let attempt = format!(
            "SELECT to_jsonb(a) AS j FROM bss_orders__commercial_attempt a WHERE order_id='{order}'"
        );
        let registry = format!(
            "SELECT to_jsonb(r) AS j FROM bss_orders__idempotency r WHERE idempotency_key='{text}'"
        );
        let (before, attempt_before, registry_before) = (
            t.order_row(order).await,
            t.json(&attempt).await,
            t.json(&registry).await,
        );
        assert_eq!(registry_before["status"], json!("in_flight"));
        let (outbox, audits) = (t.outbox().await, t.audits(order).await);
        let hook = fail_at(point);
        let err = engine
            .with_fault(hook.clone())
            .transition(
                &buyer(),
                req.clone(),
                &fixed(prepared(4, order, S::Draft, Some(candidate))),
            )
            .await
            .unwrap_err();
        assert_eq!(err, EngineError::Internal, "{point:?}");
        assert_eq!(hook.visited().last(), Some(&point), "{point:?} submit");
        assert_eq!(t.order_row(order).await, before, "{point:?} submit");
        assert_eq!(t.json(&attempt).await, attempt_before, "{point:?}");
        assert_eq!(t.json(&registry).await, registry_before, "{point:?}");
        assert_eq!(t.outbox().await, outbox, "{point:?}");
        assert_eq!(t.audits(order).await, audits, "{point:?}");
        assert_eq!(
            t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}'")).await
                + t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}'")).await,
            1,
            "{point:?} left a version or claim"
        );
        let ok = engine
            .transition(
                &buyer(),
                req,
                &fixed(prepared(4, order, S::Draft, Some(candidate))),
            )
            .await
            .unwrap();
        assert!(
            matches!(ok.kind, OutcomeKind::Committed { .. }),
            "{point:?}"
        );
        assert_eq!(t.outbox().await, outbox + 1);
        assert_eq!(
            t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}' AND version={candidate}")).await,
            1
        );
    }
    // A refusal's own audit/settlement boundaries roll back too.
    for point in [FaultPoint::Audit, FaultPoint::Settle] {
        let order = seed(&t, &probe, &format!("seed-r{point:?}"), S::Draft, None).await;
        let engine = t.engine(&[]).with_fault(fail_at(point));
        let mut p = prepared(5, order, S::Draft, None);
        p.guards = guards(5, Some("cancel.reason-required"));
        let err = engine
            .transition(
                &buyer(),
                request(order, Trigger::Cancel, "rf", 1),
                &fixed(p),
            )
            .await
            .unwrap_err();
        assert_eq!(err, EngineError::Internal);
        assert_eq!(t.audits(order).await, 1, "only the create entry");
    }
    // Create rolls back every effect, including the order number.
    for point in [
        FaultPoint::Gate,
        FaultPoint::Version,
        FaultPoint::Documents,
        FaultPoint::Audit,
        FaultPoint::Settle,
    ] {
        let orders = t.n("SELECT count(*) AS n FROM bss_orders__order").await;
        let engine = t.engine(&[]).with_fault(fail_at(point));
        let err = engine
            .create(
                &buyer(),
                create_request(&format!("cf-{point:?}"), json!({"category": NEW_SALE})),
                &fixed(create_prepared(false)),
            )
            .await
            .unwrap_err();
        assert_eq!(err, EngineError::Internal);
        assert_eq!(
            t.n("SELECT count(*) AS n FROM bss_orders__order").await,
            orders
        );
    }
}

/// A server-rejected COMMIT (deferred grant source check) is a confirmed rollback (500); a
/// terminated session after the last statement is an unknown outcome, never a rollback claim,
/// resolved by retrying the same key.
#[tokio::test]
async fn commit_rejection_is_internal_and_a_lost_acknowledgement_is_unknown() {
    let t = T::new().await;
    let probe = t.engine(&[]);
    let order = seed(&t, &probe, "seed-c", S::InFulfillment, None).await;
    let engine = t.engine(&[order]);
    let mut p = prepared(12, order, S::InFulfillment, None);
    p.documents = Some(Arc::new(GrantDoc {
        order,
        generation_offset: 2,
    }));
    let err = engine
        .transition(
            &workflow(),
            request(order, Trigger::ReportSpawnSignal, "c-1", 2),
            &fixed(p),
        )
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Internal);
    assert!(t.order_row(order).await["spawn_signal_at"].is_null());
    // Lost acknowledgement.
    let order2 = seed(&t, &probe, "seed-l", S::Draft, None).await;
    let hook = Arc::new(FailAt {
        point: FaultPoint::Settle,
        kill_before_commit: Some((t.env.pg.raw.clone(), order2)),
        hits: AtomicUsize::new(0),
        visited: std::sync::Mutex::new(Vec::new()),
    });
    let engine = t.engine(&[]);
    let err = engine
        .with_fault(hook)
        .transition(
            &buyer(),
            request(order2, Trigger::Cancel, "l-1", 1),
            &fixed(prepared(5, order2, S::Draft, None)),
        )
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::OutcomeUnknown);
    // The same key resolves the outcome: here the session died before COMMIT, so it executes
    // once now; had it committed, the retry would replay.
    let out = engine
        .transition(
            &buyer(),
            request(order2, Trigger::Cancel, "l-1", 1),
            &fixed(prepared(5, order2, S::Draft, None)),
        )
        .await
        .unwrap();
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }));
    let replay = engine
        .transition(
            &buyer(),
            request(order2, Trigger::Cancel, "l-1", 1),
            &fixed(prepared(5, order2, S::Draft, None)),
        )
        .await
        .unwrap();
    assert!(matches!(replay.kind, OutcomeKind::Replayed { .. }));
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE order_id='{order2}' AND trigger='cancel' AND outcome='committed'")).await,
        1
    );
}

/// Step 17 through the engine: a submit whose key another order holds settles
/// order-in-flight-for-key with no version, claims or event; a terminal row releases claims;
/// later D-188/D-198 paths are explicitly unavailable without their subflows.
#[tokio::test]
async fn claims_collide_before_version_append_and_terminal_rows_release() {
    let t = T::new().await;
    let probe = t.engine(&[]);
    let holder = create(&t, &probe, "holder").await;
    let order = create(&t, &probe, "contender").await;
    let engine = t.engine(&[]);
    let shared = OverlapScopeKey::try_from("sku-shared".to_owned()).unwrap();
    let claims = || Some(ProposedClaims::new(u(30), u(10), [shared.clone()]).unwrap());
    // The holder submits first and keeps its claim.
    let req = request(holder, Trigger::Submit, "s-holder", 1);
    let (candidate, req) = reserve(&engine, &buyer(), &req).await;
    let mut p = prepared(4, holder, S::Draft, Some(candidate));
    p.claims = claims();
    assert!(matches!(
        engine
            .transition(&buyer(), req, &fixed(p))
            .await
            .unwrap()
            .kind,
        OutcomeKind::Committed { .. }
    ));
    // The D-188 preparation subflow uses the shared gate: a changed payload under the settled
    // key is a mismatch with resolved evidence on the locked order, and creates no attempt.
    let mut changed = request(holder, Trigger::Submit, "s-holder", 1);
    changed.document = json!({"request": "changed"});
    let attempts = t
        .n("SELECT count(*) AS n FROM bss_orders__commercial_attempt")
        .await;
    let inputs = AttemptInputs {
        prepared_draft_revision: Some(0),
        proposed_arrangement: json!({}),
        authorization_fact_fingerprint: "facts".into(),
        original_principal: json!({}),
        proof_reference: None,
        date_policy_basis: json!({}),
        commercial_subject_id: u(BUYER.0),
        commercial_subject_type: "user".into(),
        commercial_subject_tenant_id: u(BUYER.1),
    };
    let err = engine
        .prepare_commercial_attempt(&buyer(), &changed, inputs, json!({}))
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Refused(Reason::IdempotencyMismatch));
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__commercial_attempt")
            .await,
        attempts
    );
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__transition_audit WHERE order_id='{holder}' AND reason='idempotency-mismatch' AND sequence IS NULL")).await,
        1
    );
    let outbox = t.outbox().await;
    let req = request(order, Trigger::Submit, "s-contender", 1);
    let (candidate, req) = reserve(&engine, &buyer(), &req).await;
    let mut p = prepared(4, order, S::Draft, Some(candidate));
    p.claims = claims();
    let out = engine.transition(&buyer(), req, &fixed(p)).await.unwrap();
    assert!(matches!(
        out.kind,
        OutcomeKind::Refused {
            reason: Reason::OrderInFlightForKey,
            ..
        }
    ));
    assert_eq!(
        out.response.body["context"]["data"]["blocked"][0]["conflicting_order_id"],
        json!(s(holder))
    );
    assert_eq!(t.order_row(order).await["state"], json!("draft"));
    assert_eq!(t.outbox().await, outbox);
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}'"
        ))
        .await,
        1
    );
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL")).await,
        0
    );
    // A terminal transition (row 17 cancel) releases every live claim of the holder.
    let mut p = prepared(17, holder, S::Submitted, None);
    p.event = event(17, S::Submitted, 2);
    engine
        .transition(
            &buyer(),
            request(holder, Trigger::Cancel, "c-holder", 2),
            &fixed(p),
        )
        .await
        .unwrap();
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{holder}' AND released_at IS NULL")).await,
        0
    );
    // A versioning row without its D-188 attempt and a post-spawn hold without its D-198
    // control are unavailable and change nothing.
    let order3 = create(&t, &probe, "no-attempt").await;
    let err = engine
        .transition(
            &buyer(),
            request(order3, Trigger::Submit, "s-3", 1),
            &fixed(prepared(4, order3, S::Draft, Some(2))),
        )
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Unavailable);
    let spawned = seed(&t, &probe, "spawned", S::InFulfillment, None).await;
    t.env
        .pg
        .sql(&format!(
            "UPDATE bss_orders__order SET spawn_signal_at=now() WHERE order_id='{spawned}'"
        ))
        .await
        .unwrap();
    let before = t.order_row(spawned).await;
    let err = engine
        .transition(
            &buyer(),
            request(spawned, Trigger::Hold, "h-spawned", 2),
            &fixed(prepared(21, spawned, S::InFulfillment, None)),
        )
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Unavailable);
    assert_eq!(t.order_row(spawned).await, before);
}

/// The D-198 subflow: a post-spawn hold commits only through its owned receiver control, which
/// settles and clears the pending pointer in the same transaction.
#[tokio::test]
async fn post_spawn_hold_commits_through_its_receiver_control() {
    use crate::infra::storage::repo::idempotency::ControlInputs;
    let t = T::new().await;
    let probe = t.engine(&[]);
    let order = seed(&t, &probe, "seed-s", S::InFulfillment, None).await;
    t.env
        .pg
        .sql(&format!("UPDATE bss_orders__order SET spawn_signal_at=now(), fulfillment_control_generation=1 WHERE order_id='{order}'"))
        .await
        .unwrap();
    let engine = t.engine(&[]);
    let req = request(order, Trigger::Hold, "ph-1", 2);
    let inputs = ControlInputs {
        expected_version: 2,
        fulfillment_attempt_id: u(4040),
        generation: 1,
        original_actor: json!({"subject": s(u(BUYER.0))}),
        proof_reference: None,
        authorization_fact_fingerprint: "facts".into(),
        roster: json!([]),
        roster_digest: "sha256:roster".into(),
        receiver_commands: json!({}),
    };
    let PreparedExecution::Execution(execution) = engine
        .prepare_receiver_control(&buyer(), &req, inputs)
        .await
        .unwrap()
    else {
        panic!("no control")
    };
    assert!(!t.order_row(order).await["fulfillment_control_pending"].is_null());
    let mut req = req;
    req.execution = Some(execution.0.owner());
    let out = engine
        .transition(
            &buyer(),
            req,
            &fixed(prepared(21, order, S::InFulfillment, None)),
        )
        .await
        .unwrap();
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }));
    let row = t.order_row(order).await;
    assert_eq!(row["state"], json!("on_hold"));
    assert!(row["fulfillment_control_pending"].is_null());
    assert_eq!(
        t.n(&format!("SELECT count(*) AS n FROM bss_orders__fulfillment_control WHERE order_id='{order}' AND status='settled'")).await,
        1
    );
}

/// Row 3 writes the administrative value from the engine's locked read (last write wins, §4.6).
struct AdminSet(Option<&'static str>);
#[async_trait]
impl DocumentWriter for AdminSet {
    async fn write(&self, docs: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        let next = |current: Option<entity::order_admin::Model>| entity::order_admin::Model {
            order_id: docs.order_id(),
            external_reference: self.0.map(str::to_owned),
            display_label: current.as_ref().and_then(|c| c.display_label.clone()),
            internal_notes: current.and_then(|c| c.internal_notes),
            updated_by: u(BUYER.0).to_string(),
            updated_at: docs.transition_time(),
        };
        match docs.order_admin().await? {
            Some(current) => {
                docs.replace_order_admin(&current.clone(), next(Some(current)))
                    .await
            }
            None => docs.insert_order_admin(next(None)).await.map(|_| ()),
        }
    }
}

/// S2-04 gap review: row 3's change guard and per-field audit compare the named values with
/// those stored at the engine's locked read (04 §3.6 step 2; D-117, D-142, D-149). A concurrent
/// edit after preparation can neither falsify the audited prior value nor be overwritten as a
/// no-op, an unchanged named field writes no entry, and an all-unchanged edit is the audited,
/// settled `administrative-edit-unchanged` refusal.
#[tokio::test]
async fn administrative_edits_compare_with_the_locked_values() {
    use crate::domain::audit::{AdminAttribute as A, AdminField as F};
    let t = T::new().await;
    let engine = t.engine(&[]);
    let order = create(&t, &engine, "admin-seed").await;
    let edit = |changes: Vec<AdminChange>, set: Option<&'static str>| {
        let mut p = Prepared::new(AggregateContribution::None);
        p.guards = guards(3, None).bind("versioning.admin-changed", |s: &GuardSubject<'_>| {
            if s.administrative.is_empty() {
                GuardVerdict::Fail(Reason::AdministrativeEditUnchanged)
            } else {
                GuardVerdict::Pass
            }
        });
        p.inputs = resolved(3);
        p.admin_changes = changes;
        p.documents = Some(Arc::new(AdminSet(set)));
        p
    };
    let change = |field, prior: Option<&str>, new: Option<&str>| AdminChange {
        field,
        prior: prior.map(str::to_owned),
        new: new.map(str::to_owned),
    };
    let committed = |key: &'static str| {
        format!(
            "SELECT coalesce(jsonb_agg(jsonb_build_array(changed_field, prior_value, new_value) ORDER BY sequence), '[]'::jsonb) AS j \
             FROM bss_orders__transition_audit WHERE order_id='{order}' AND outcome='committed' AND idempotency_key='{key}'"
        )
    };
    let stored = format!(
        "SELECT to_jsonb(a.external_reference) AS j FROM bss_orders__order_admin a WHERE order_id='{order}'"
    );
    // 1. A first edit stores the value.
    let out = engine
        .transition(
            &buyer(),
            request(order, Trigger::AdministrativeEdit, "ae-1", 1),
            &fixed(edit(
                vec![change(F::Order(A::ExternalReference), None, Some("PO-1"))],
                Some("PO-1"),
            )),
        )
        .await
        .unwrap();
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }), "{out:?}");
    assert_eq!(
        t.json(&committed("ae-1")).await,
        json!([["external_reference", null, "PO-1"]])
    );
    // 2. A concurrent edit commits PO-2 after this request prepared against PO-1. The audited
    //    prior value is the locked PO-2, and the display label the slice believed was "x" is
    //    already NULL at the lock: unchanged, so it writes no entry.
    let mut prep = fixed(edit(
        vec![
            change(F::Order(A::ExternalReference), Some("PO-1"), Some("PO-3")),
            change(F::Order(A::DisplayLabel), Some("x"), None),
        ],
        Some("PO-3"),
    ));
    prep.side_effect = Some((
        t.env.pg.raw.clone(),
        format!(
            "UPDATE bss_orders__order_admin SET external_reference='PO-2' WHERE order_id='{order}'"
        ),
    ));
    let out = engine
        .transition(
            &buyer(),
            request(order, Trigger::AdministrativeEdit, "ae-2", 1),
            &prep,
        )
        .await
        .unwrap();
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }), "{out:?}");
    assert_eq!(
        t.json(&committed("ae-2")).await,
        json!([["external_reference", "PO-2", "PO-3"]])
    );
    assert_eq!(t.json(&stored).await, json!("PO-3"));
    // 3. Every named value already holds its new value at the lock: the change guard refuses,
    //    settled and audited, with no committed entry and no administrative write.
    let audits = t.audits(order).await;
    let out = engine
        .transition(
            &buyer(),
            request(order, Trigger::AdministrativeEdit, "ae-3", 1),
            &fixed(edit(
                vec![change(
                    F::Order(A::ExternalReference),
                    Some("PO-1"),
                    Some("PO-3"),
                )],
                Some("PO-3"),
            )),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            out.kind,
            OutcomeKind::Refused {
                reason: Reason::AdministrativeEditUnchanged,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(t.json(&committed("ae-3")).await, json!([]));
    assert_eq!(t.audits(order).await, audits + 1);
    assert_eq!(t.json(&stored).await, json!("PO-3"));
    assert_eq!(t.order_row(order).await["draft_revision"], json!(0));
}

/// S2-04 gap review: the step-17 holder is named only through the caller's order-read scope
/// (D-179), and a caller without read authority is neither refused the submit nor told which
/// order holds the key.
#[tokio::test]
async fn a_collision_never_names_the_holder_to_a_caller_without_read_authority() {
    let t = T::new().await;
    let mut no_read = policy(&[]);
    let actions = no_read["rules"][0]["actions"].as_array_mut().unwrap();
    actions.retain(|a| a != "read");
    let engine = t.engine_with(no_read);
    let holder = create(&t, &engine, "nr-holder").await;
    let order = create(&t, &engine, "nr-contender").await;
    let shared = OverlapScopeKey::try_from("sku-no-read".to_owned()).unwrap();
    let claims = || Some(ProposedClaims::new(u(30), u(10), [shared.clone()]).unwrap());
    let req = request(holder, Trigger::Submit, "nr-s-holder", 1);
    let (candidate, req) = reserve(&engine, &buyer(), &req).await;
    let mut p = prepared(4, holder, S::Draft, Some(candidate));
    p.claims = claims();
    let out = engine.transition(&buyer(), req, &fixed(p)).await.unwrap();
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }), "{out:?}");
    let req = request(order, Trigger::Submit, "nr-s-contender", 1);
    let (candidate, req) = reserve(&engine, &buyer(), &req).await;
    let mut p = prepared(4, order, S::Draft, Some(candidate));
    p.claims = claims();
    let out = engine.transition(&buyer(), req, &fixed(p)).await.unwrap();
    assert!(
        matches!(
            out.kind,
            OutcomeKind::Refused {
                reason: Reason::OrderInFlightForKey,
                ..
            }
        ),
        "{out:?}"
    );
    assert_eq!(
        out.response.body["context"]["data"]["blocked"][0]["conflicting_order_id"],
        Value::Null
    );
    assert!(!out.response.body.to_string().contains(&s(holder)));
    assert_eq!(t.order_row(order).await["state"], json!("draft"));
}

/// Row 2's working-set edits through the bounded handle: add a line, then remove it; each
/// committed edit increments `draft_revision` once and the removed identity stays reserved.
struct DraftLine {
    order: Uuid,
    line: Uuid,
    remove: bool,
}
#[async_trait]
impl DocumentWriter for DraftLine {
    async fn write(&self, docs: &ChildDocuments<'_, '_>) -> Result<(), ScopeError> {
        if self.remove {
            if !docs.remove_draft_line(self.line).await? {
                return Err(ScopeError::Invalid("line is not a draft member"));
            }
            return Ok(());
        }
        docs.add_draft_line(entity::draft_content::Model {
            order_id: self.order,
            line_id: self.line,
            plan_id: u(200),
            plan_revision_id: u(201),
            selected_items: json!([]),
            currency: "EUR".into(),
            contract_effective_date: None,
            service_activation_date: None,
            acceptance_due_date: None,
            term_duration: None,
            term_kind: "rolling".into(),
            authored_term: json!({"kind": "rolling"}),
            billing_cycle: Some("month".into()),
        })
        .await?;
        Ok(())
    }
}

/// S2-04 gap review: a commercial draft edit can insert and remove lines through the engine's
/// child-document handle only (OL-4: insertion, replacement or removal).
#[tokio::test]
async fn draft_edits_add_and_remove_lines_through_the_bounded_handle() {
    let t = T::new().await;
    let engine = t.engine(&[]);
    let order = create(&t, &engine, "dl-seed").await;
    let line = u(7100);
    let members =
        format!("SELECT count(*) AS n FROM bss_orders__draft_content WHERE order_id='{order}'");
    for (n, (text, remove)) in [("dl-add", false), ("dl-remove", true)]
        .into_iter()
        .enumerate()
    {
        let mut p = prepared(2, order, S::Draft, None);
        p.documents = Some(Arc::new(DraftLine {
            order,
            line,
            remove,
        }));
        let mut req = request(order, Trigger::DraftMutate, text, 1);
        req.expected_draft_revision = Some(i64::try_from(n).unwrap());
        let out = engine.transition(&buyer(), req, &fixed(p)).await.unwrap();
        assert!(matches!(out.kind, OutcomeKind::Committed { .. }), "{out:?}");
        assert_eq!(t.n(&members).await, i64::from(!remove));
        assert_eq!(t.order_row(order).await["draft_revision"], json!(n + 1));
    }
    assert_eq!(
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_line_identity WHERE order_id='{order}' AND line_id='{line}'"
        ))
        .await,
        1
    );
}

/// Hold the aggregate lock of `order` in an owner transaction while `work` (authorized on the
/// old facts) blocks on it; then change the facts and commit, so `work` observes the change
/// under its own lock. The S2-03 `concurrent_axis_change_blocks_then_conflicts_without_mutation`
/// race, driven through the engine.
async fn race_fact_change<Fut: std::future::Future>(
    t: &T,
    order: Uuid,
    change: &str,
    work: Fut,
) -> Fut::Output {
    use sea_orm::TransactionTrait;
    let held = t.env.pg.raw.begin().await.unwrap();
    held.execute_unprepared(&format!(
        "SELECT 1 FROM bss_orders__order WHERE order_id='{order}' FOR UPDATE"
    ))
    .await
    .unwrap();
    let writer = async {
        let mut waited = 0;
        while t
            .n("SELECT count(*) AS n FROM pg_stat_activity WHERE wait_event_type='Lock'")
            .await
            == 0
        {
            waited += 1;
            assert!(
                waited < 400,
                "the preparation never blocked on the aggregate lock"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        held.execute_unprepared(change).await.unwrap();
        held.commit().await.unwrap();
    };
    let (out, ()) = tokio::join!(work, writer);
    out
}

fn commercial_inputs() -> AttemptInputs {
    AttemptInputs {
        prepared_draft_revision: Some(0),
        proposed_arrangement: json!({"payer": s(u(30))}),
        authorization_fact_fingerprint: "facts".into(),
        original_principal: json!({"subject": s(u(BUYER.0))}),
        proof_reference: None,
        date_policy_basis: json!({}),
        commercial_subject_id: u(BUYER.0),
        commercial_subject_type: "user".into(),
        commercial_subject_tenant_id: u(BUYER.1),
    }
}

/// S2-FINAL cleanup: the D-188/D-198 preparation subflows recheck the authorization facts under
/// the aggregate lock exactly as the ordinary paths do (`lock`, `snapshot`): a payer moved under
/// the decision is the `authorization-context-changed` conflict, a resource tenant moved out of
/// the caller's scope is the non-disclosing `order-not-found` (D-114, D-141). Neither settles
/// the key, reserves a candidate, writes a control or names the order in its evidence, and the
/// same key runs fresh afterwards.
#[tokio::test]
async fn durable_preparation_maps_stale_facts_to_conflict_and_lost_access_to_not_found() {
    use crate::infra::storage::repo::idempotency::ControlInputs;
    let t = T::new().await;
    let engine = t.engine(&[]);
    let registry = || t.n("SELECT count(*) AS n FROM bss_orders__idempotency");
    let attempts = || t.n("SELECT count(*) AS n FROM bss_orders__commercial_attempt");
    let controls = || t.n("SELECT count(*) AS n FROM bss_orders__fulfillment_control");
    let unresolved = async |order: Uuid, reason: &str| {
        t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit WHERE requested_order_ref='{order}' AND reason='{reason}' AND order_id IS NULL AND outcome='refused' AND sequence IS NULL"
        ))
        .await
    };
    let payer_moved = |order: Uuid| {
        format!(
            "UPDATE bss_orders__order SET payer_tenant_id='{}' WHERE order_id='{order}'",
            u(31)
        )
    };
    let tenant_moved = |order: Uuid| {
        format!(
            "UPDATE bss_orders__order SET resource_tenant_id='{}' WHERE order_id='{order}'",
            u(12)
        )
    };

    // Commercial subflow, stale facts: payer 31 is still inside the policy, so the facts changed
    // but access remains; the conflict is audited unresolved and nothing durable is written.
    let order = seed(&t, &engine, "seed-dp1", S::Draft, None).await;
    let req = request(order, Trigger::Submit, "dp-1", 1);
    let (r0, a0) = (registry().await, attempts().await);
    let err = race_fact_change(
        &t,
        order,
        &payer_moved(order),
        engine.prepare_commercial_attempt(&buyer(), &req, commercial_inputs(), json!({})),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        EngineError::Refused(Reason::AuthorizationContextChanged)
    );
    assert_eq!((registry().await, attempts().await), (r0, a0));
    let row = t.order_row(order).await;
    assert_eq!(row["payer_tenant_id"], json!(s(u(31))));
    assert_eq!(row["version_allocation_high_water"], json!(1));
    assert_eq!(unresolved(order, "authorization-context-changed").await, 1);
    // Same key, fresh authorization on the committed facts: the reservation proceeds.
    let (candidate, _) = reserve(&engine, &buyer(), &req).await;
    assert_eq!(candidate, 2);
    assert_eq!((registry().await, attempts().await), (r0 + 1, a0 + 1));

    // Commercial subflow, lost access: the resource tenant left the caller's scope between the
    // decision and the lock; the answer is the non-disclosing not-found, never a conflict.
    let order2 = seed(&t, &engine, "seed-dp2", S::Draft, None).await;
    let req2 = request(order2, Trigger::Submit, "dp-2", 1);
    let (r1, a1) = (registry().await, attempts().await);
    let err = race_fact_change(
        &t,
        order2,
        &tenant_moved(order2),
        engine.prepare_commercial_attempt(&buyer(), &req2, commercial_inputs(), json!({})),
    )
    .await
    .unwrap_err();
    assert_eq!(err, EngineError::Refused(Reason::OrderNotFound));
    assert_eq!((registry().await, attempts().await), (r1, a1));
    assert_eq!(
        t.order_row(order2).await["version_allocation_high_water"],
        json!(1)
    );
    assert_eq!(unresolved(order2, "order-not-found").await, 1);
    assert_eq!(unresolved(order2, "authorization-context-changed").await, 0);
    // The hidden target stays hidden on a fresh attempt: the same answer, still nothing durable.
    let err = engine
        .prepare_commercial_attempt(&buyer(), &req2, commercial_inputs(), json!({}))
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Refused(Reason::OrderNotFound));
    assert_eq!((registry().await, attempts().await), (r1, a1));

    // Receiver-control subflow, stale facts on a post-spawn order: the same mapping, and no
    // pending control pointer or control row is written. Past `draft` the resource tenant is
    // immutable in the DDL (`bss_orders__aggregate_guard`), so lost access through an axis move
    // cannot arise there; the commercial arm above covers it on the one shared subflow.
    let order3 = seed(&t, &engine, "seed-dp3", S::InFulfillment, None).await;
    t.env
        .pg
        .sql(&format!("UPDATE bss_orders__order SET spawn_signal_at=now(), fulfillment_control_generation=1 WHERE order_id='{order3}'"))
        .await
        .unwrap();
    let req3 = request(order3, Trigger::Hold, "dp-3", 2);
    let control_inputs = ControlInputs {
        expected_version: 2,
        fulfillment_attempt_id: u(4041),
        generation: 1,
        original_actor: json!({"subject": s(u(BUYER.0))}),
        proof_reference: None,
        authorization_fact_fingerprint: "facts".into(),
        roster: json!([]),
        roster_digest: "sha256:roster".into(),
        receiver_commands: json!({}),
    };
    let (r3, c3) = (registry().await, controls().await);
    let err = race_fact_change(
        &t,
        order3,
        &payer_moved(order3),
        engine.prepare_receiver_control(&buyer(), &req3, control_inputs),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        EngineError::Refused(Reason::AuthorizationContextChanged)
    );
    assert_eq!((registry().await, controls().await), (r3, c3));
    let row = t.order_row(order3).await;
    assert!(row["fulfillment_control_pending"].is_null());
    assert_eq!(row["state"], json!("in_fulfillment"));
    assert_eq!(unresolved(order3, "authorization-context-changed").await, 1);
}

/// S2-FINAL cleanup (S2-12 "audit failure or outbox failure rolls back", 01 §6 "faults at …
/// enqueue … leave no partial effects"): the platform outbox insert itself fails inside the
/// engine's transaction (INSERT revoked on `toolkit_outbox_body` for the runtime role). The
/// eventful row's state change, dwell clock, committed audit entry and key claim all roll back,
/// nothing is enqueued, the caller sees a confirmed rollback (never an unknown outcome), and the
/// same key executes exactly once after the grant is restored.
#[tokio::test]
async fn an_outbox_insert_failure_rolls_back_the_business_effect_and_audit() {
    let t = T::new().await;
    let engine = t.engine(&[]);
    let order = seed(&t, &engine, "seed-ob", S::Submitted, None).await;
    let mut p = Prepared::new(AggregateContribution::None);
    p.guards = guards(21, None);
    p.event = event(21, S::Submitted, 2);
    let before = t.order_row(order).await;
    let audits = t.audits(order).await;
    let registry = t
        .n("SELECT count(*) AS n FROM bss_orders__idempotency")
        .await;
    let outbox = t.outbox().await;
    t.env
        .pg
        .sql("REVOKE INSERT ON toolkit_outbox_body FROM bss_orders_runtime")
        .await
        .unwrap();
    let err = engine
        .transition(
            &buyer(),
            request(order, Trigger::Hold, "ob-1", 2),
            &fixed(p.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Internal,
        "a refused outbox insert is a confirmed rollback, not an unknown outcome"
    );
    let after = t.order_row(order).await;
    assert_eq!(after["state"], json!("submitted"));
    assert_eq!(after["state_entered_at"], before["state_entered_at"]);
    assert_eq!(after["audit_sequence"], before["audit_sequence"]);
    assert!(after["pre_hold_state"].is_null());
    assert_eq!(
        t.audits(order).await,
        audits,
        "no committed or refused audit row survives the rollback"
    );
    assert_eq!(
        t.n("SELECT count(*) AS n FROM bss_orders__idempotency")
            .await,
        registry,
        "the key was neither claimed nor settled"
    );
    assert_eq!(t.outbox().await, outbox, "nothing was enqueued");
    t.env
        .pg
        .sql("GRANT INSERT ON toolkit_outbox_body TO bss_orders_runtime")
        .await
        .unwrap();
    let out = engine
        .transition(
            &buyer(),
            request(order, Trigger::Hold, "ob-1", 2),
            &fixed(p),
        )
        .await
        .unwrap();
    assert!(matches!(out.kind, OutcomeKind::Committed { .. }));
    assert_eq!(t.order_row(order).await["state"], json!("on_hold"));
    assert_eq!(t.audits(order).await, audits + 1);
    assert_eq!(t.outbox().await, outbox + 1);
}
