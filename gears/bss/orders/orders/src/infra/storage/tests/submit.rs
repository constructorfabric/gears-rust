//! Submit on real PostgreSQL (S3-12; D-188 steps 1–6, D-192, D-216, M2-3): the submit service
//! over the S2-04 engine, the rules PDP, the authorized draft reads and the gate orchestrator
//! over the Task 8b stubs (the counted harness of `infra::gate::tests::orchestrate`), with
//! Pricing's acceptance answered by the development double (`stubs::SellabilityStub`, accepting
//! on the harness catalog's bindings) behind a scripted wrapper. Phase A refuses a blocking
//! assessment without reserving and reserves a passing one; phase B records each line's receipt
//! under the attempt's fence; phase C commits the candidate through the engine. Each D-188
//! failure-matrix row this milestone can exercise is one named test. Compiled only with
//! `gate-dev-stubs`. In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
use super::capture::{BUYER, T, buyer, create_meta, key, s, view, write};
use super::*;
use crate::authz::Caller;
use crate::authz::test_pdp::{RulesProvider, service, user};
use crate::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use crate::domain::gate::answers::{EvaluationChargeKind, TotalsDeclaration};
use crate::domain::gate::commercial::{
    ReceiptResult, decode_line_requests, deterministic_receipt, receipt_result_value,
};
use crate::domain::gate::ports::GatePort;
use crate::infra::capture::{CaptureService, CaptureSettings, Settled, SettledOutcome};
use crate::infra::dates::PrepareDatesError;
use crate::infra::engine::{
    Abort, Engine, FaultHook, FaultPoint, ReceiptOutcome, Recovery, TransitionRequest,
};
use crate::infra::gate::authority::{AuthorityError, CommercialAuthority, DeadlineAuthority};
use crate::infra::gate::commit::SubmitPreparation;
use crate::infra::gate::preview::PreviewDates;
use crate::infra::gate::stubs::{SellabilityStub, StubBindings, StubFixture};
use crate::infra::gate::submit::SubmitService;
use crate::infra::gate::tests::orchestrate::{
    Catalog, Harness, ITEM, PAYER, PLAN, REVISION, fixture, harness,
};
use crate::infra::read::ReadService;
use crate::infra::storage::entity::commercial_attempt;
use crate::infra::storage::repo::idempotency::{DurableExecution, Frozen, Recorded};
use crate::infra::throttle::{CallerLimitSettings, CallerLimiter, PerCallerLimiter};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, EvaluationRequest, EvaluationResponse};
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::authoring::{
    AddLine, AuthoredTerm, BillingCycle, Category, CreateOrder, Currency, OrderView, SelectedItem,
};
use bss_orders_sdk::catalog::{Reason, Trigger};
use bss_orders_sdk::models::{CallMeta, DraftRevision, OrderVersion};
use bss_pricing_sdk::acceptance::{
    AcceptanceReceipt, CommandMeta, CommercialReason, FulfilmentEligibility, FulfilmentQuery,
    NewSaleQuery, SellabilityV1,
};
use bss_pricing_sdk::read::ResolvedBindings;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::DbTx;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

const ORDER_RT: &str = "gts.cf.bss.orders.order.v1~";
const RESOURCE: u128 = 10;
const SELLER: u128 = 20;
/// The seller's configured Orders service principal (D-194).
const SERVICE: (u128, u128) = (700, 701);
/// A principal no rule names.
const STRANGER: (u128, u128) = (104, 10);

/// The buyer authors and submits on resource tenant 10 (`submit` absent when `revoked`).
fn policy_with(submit: bool) -> Value {
    let mut actions = vec!["create", "write", "edit", "read"];
    if submit {
        actions.push("submit");
    }
    json!({"vendor": "constructorfabric", "priority": 10, "policy_revision": "m2-6",
    "rules": [{
        "id": "buyer", "subject": {"id": s(u(BUYER.0))}, "resource_type": ORDER_RT,
        "actions": actions,
        "paths": [{"predicates": [{"property": "resource_tenant_id",
            "values": [s(u(RESOURCE))]}]}],
    }]})
}

/// The rules PDP, whose `submit` grant a test can revoke mid-flight.
struct Revocable {
    granted: Arc<RulesProvider>,
    revoked: Arc<RulesProvider>,
    is_revoked: AtomicBool,
    /// Every decision asked of this PDP, granted or not.
    evaluations: AtomicUsize,
}
impl Revocable {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            granted: RulesProvider::from_policy(policy_with(true)),
            revoked: RulesProvider::from_policy(policy_with(false)),
            is_revoked: AtomicBool::new(false),
            evaluations: AtomicUsize::new(0),
        })
    }
    fn revoke(&self) {
        self.is_revoked.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl AuthZResolverApi for Revocable {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.evaluations.fetch_add(1, Ordering::SeqCst);
        if self.is_revoked.load(Ordering::SeqCst) {
            self.revoked.evaluate(ctx, request).await
        } else {
            self.granted.evaluate(ctx, request).await
        }
    }
}

/// The frozen basis over the platform default (no date required), on today's UTC date, so the
/// engine's date-basis guard sees the proposed date it transitions on.
#[derive(Default)]
struct TodayDates {
    prepared: AtomicUsize,
}
#[async_trait]
impl PreviewDates for TodayDates {
    async fn prepare(
        &self,
        resource_tenant_id: Uuid,
        lines: Vec<(Uuid, SuppliedDates)>,
    ) -> Result<DateBasis, PrepareDatesError> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        let row = PolicyRow {
            policy_id: PLATFORM_DEFAULT_POLICY_ID,
            resource_tenant_id: None,
            service_activation_required: false,
            acceptance_due_required: false,
            revision: 1,
        };
        let policy = DatePolicySnapshot::effective(resource_tenant_id, &[row])?;
        let today = OffsetDateTime::now_utc().date();
        Ok(DateBasis::resolve(resource_tenant_id, policy, today, lines))
    }
}

/// The seller's service principal, counted.
#[derive(Default)]
struct Authority {
    calls: AtomicUsize,
    rotated: AtomicBool,
    /// The exchange never answers (a hung `IdP`).
    stall: AtomicBool,
}
#[async_trait]
impl CommercialAuthority for Authority {
    async fn service_context(&self, _seller: Uuid) -> Result<SecurityContext, AuthorityError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(service(
            if self.rotated.load(Ordering::SeqCst) {
                SERVICE.0 + 1
            } else {
                SERVICE.0
            },
            SERVICE.1,
        )
        .ctx()
        .clone())
    }
}

/// The harness catalog's bindings: what the double accepts on.
#[derive(Default)]
struct FixedBindings {
    kinds: Mutex<BTreeMap<Uuid, bss_pricing_sdk::read::ChargeKind>>,
    multikey: bool,
    /// When set, every binding's SKU name (an oversized pin).
    sku_name: Option<String>,
}
impl FixedBindings {
    fn answer(&self, line: Uuid) -> crate::domain::gate::pricing_map::CatalogAnswer {
        let mut answer = Catalog::answer();
        if self.multikey {
            let resolved = Arc::make_mut(answer.resolved.as_mut().unwrap());
            let mut second = resolved.cells[0].clone();
            second.selection.item_id = u(0x21);
            let binding = second.binding.as_mut().unwrap();
            binding.item_id = u(0x21);
            binding.sku_id = u(0xd2);
            resolved.cells.push(second);
            let mut sku = answer.skus.values().next().unwrap().clone();
            if let crate::domain::gate::ports::PortAnswer::Answered(sku) = &mut sku {
                sku.id = u(0xd2);
            }
            answer.skus.insert(u(0xd2), sku);
        }
        if let Some(name) = &self.sku_name {
            for cell in &mut Arc::make_mut(answer.resolved.as_mut().unwrap()).cells {
                cell.binding.as_mut().unwrap().sku_name.clone_from(name);
            }
        }

        if let Some(kind) = self.kinds.lock().get(&line) {
            for cell in &mut Arc::make_mut(answer.resolved.as_mut().unwrap()).cells {
                let binding = cell.binding.as_mut().unwrap();
                binding.kind = kind.clone();
                if *kind != bss_pricing_sdk::read::ChargeKind::Recurring {
                    binding.recurring_period = None;
                }
                if *kind == bss_pricing_sdk::read::ChargeKind::Usage {
                    use bss_pricing_sdk::terms::*;
                    let content = UsageRatingPolicyInput {
                        rating_window: RatingWindow::BillingCycle,
                        aggregation_scope: AggregationScope::SubscriptionLine,
                        reset: Reset::RatingWindowStart,
                        partial_window: PartialWindow::ActualQuantityFullThresholds,
                        fold: Fold::Sum,
                    };
                    binding.meter = Some(MeterRef {
                        usage_type_id: "cpu-hours".into(),
                        version: "1".into(),
                    });
                    binding.usage_rating_policy = Some(UsageRatingPolicy {
                        policy_id: Uuid::from_u128(0x1234),
                        version: 1,
                        digest: bss_pricing_sdk::digest::policy_digest(&content),
                        content,
                    });
                }
            }
        }
        answer
    }
}
#[async_trait]
impl crate::infra::gate::pricing_read::CatalogReads for FixedBindings {
    async fn read_line(
        &self,
        _seller: Uuid,
        req: &crate::domain::gate::pricing_map::LineCatalogRequest,
        _date: time::Date,
        _run: &crate::infra::gate::budget::RunBudget,
        _cancel: &CancellationToken,
    ) -> crate::domain::gate::pricing_map::CatalogAnswer {
        self.answer(req.line_id)
    }
}
#[async_trait]
impl StubBindings for FixedBindings {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        query: &NewSaleQuery,
    ) -> Result<ResolvedBindings, CanonicalError> {
        self.answer(query.line_id)
            .resolved
            .map(|rb| ResolvedBindings::clone(&rb))
            .map_err(|_| CanonicalError::service_unavailable().create())
    }
}

/// The development double behind a script: per-line refusals, lost replies (the double issues
/// and records its receipt, then the reply is lost), an already lapsed receipt, and a hook run
/// on every call. Every call is counted with its command key.
struct Scripted {
    stub: SellabilityStub,
    bindings: Arc<FixedBindings>,
    calls: Mutex<Vec<(String, Uuid)>>,
    refuse: Mutex<BTreeMap<Uuid, CommercialReason>>,
    lose_replies: AtomicUsize,
    lapsed: AtomicBool,
    on_call: Mutex<Option<OnCall>>,
}
/// A hook run with the call number on every scripted acceptance call.
type OnCall = Box<dyn Fn(usize) + Send + Sync>;
impl Scripted {
    fn new(bindings: Arc<FixedBindings>) -> Arc<Self> {
        Arc::new(Self {
            stub: SellabilityStub::new(&fixture(), bindings.clone()),
            bindings,
            calls: Mutex::new(Vec::new()),
            refuse: Mutex::new(BTreeMap::new()),
            lose_replies: AtomicUsize::new(0),
            lapsed: AtomicBool::new(false),
            on_call: Mutex::new(None),
        })
    }
    fn calls(&self) -> usize {
        self.calls.lock().len()
    }
    fn keys(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(k, _)| k.clone()).collect()
    }
}
#[async_trait]
impl SellabilityV1 for Scripted {
    async fn check(
        &self,
        ctx: &SecurityContext,
        query: NewSaleQuery,
        meta: CommandMeta,
    ) -> Result<AcceptanceReceipt, CanonicalError> {
        let n = {
            let mut calls = self.calls.lock();
            calls.push((meta.idempotency_key.clone(), query.line_id));
            calls.len()
        };
        if let Some(hook) = self.on_call.lock().as_ref() {
            hook(n);
        }
        if let Some(reason) = self.refuse.lock().get(&query.line_id) {
            return Err((*reason).into());
        }
        if self.lapsed.load(Ordering::SeqCst) {
            // Issued an hour ago with a one-minute hold: already past its activation deadline.
            let resolved = self.bindings.resolve(ctx, &query).await?;
            return deterministic_receipt(
                query,
                &resolved,
                Uuid::new_v4(),
                OffsetDateTime::now_utc() - time::Duration::hours(1),
                60,
            )
            .map_err(CanonicalError::from);
        }
        let receipt = self.stub.check(ctx, query, meta).await?;
        if self
            .lose_replies
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(CanonicalError::service_unavailable().create());
        }
        Ok(receipt)
    }
    async fn check_fulfilment(
        &self,
        ctx: &SecurityContext,
        query: FulfilmentQuery,
    ) -> Result<FulfilmentEligibility, CanonicalError> {
        self.stub.check_fulfilment(ctx, query).await
    }
}

struct Fixture {
    t: T,
    capture: CaptureService,
    submit: SubmitService,
    h: Harness,
    dates: Arc<TodayDates>,
    authority: Arc<Authority>,
    pdp: Arc<Revocable>,
    acceptance: Arc<Scripted>,
    engine: Arc<Engine>,
}

// Multi-line commercial scenarios intentionally admit both lines on the fixture's shared SKU.
// Cardinality refusal itself remains covered by the dedicated gate/overlap tests.
fn two_line_fixture() -> StubFixture {
    let mut f = fixture();
    for occupancy in f.occupancy.values_mut() {
        let crate::domain::gate::answers::OccupancyObservation::LegacyActiveV1(occupancy) =
            occupancy
        else {
            panic!("legacy fixture expected")
        };
        occupancy.max_concurrent_active =
            crate::domain::gate::answers::MaxConcurrentActive::Limit(2);
    }
    f
}

async fn setup(stubs: StubFixture, per_minute: u32) -> Fixture {
    // Boxed: the whole container-backed fixture future is large (`clippy::large_futures`).
    Box::pin(setup_with(stubs, per_minute, false)).await
}

/// The service over a fresh container; `bound` binds the scripted acceptance double.
async fn setup_with(stubs: StubFixture, per_minute: u32, bound: bool) -> Fixture {
    setup_with_bindings(stubs, per_minute, bound, None).await
}

async fn setup_with_bindings(
    stubs: StubFixture,
    per_minute: u32,
    bound: bool,
    custom: Option<Arc<FixedBindings>>,
) -> Fixture {
    // Boxed: the container-backed `T` future is large (`clippy::large_futures`).
    let t = Box::pin(T::new()).await;
    let pdp = Revocable::new();
    let api: Arc<dyn AuthZResolverApi> = pdp.clone();
    let engine = Arc::new(t.engine(api.clone()));
    let slot: Arc<ArcSwapOption<Engine>> = Arc::new(ArcSwapOption::from(Some(Arc::clone(&engine))));
    let capture = CaptureService::new(Arc::clone(&slot), CaptureSettings { line_cap: 200 });
    let reads: Arc<ReadService> = Arc::new(t.reads_with(api));
    let mut h = harness(stubs, false);
    let bindings = custom.clone().unwrap_or_default();
    if custom.is_some() {
        h.orchestrator = h.orchestrator.with_catalog(bindings.clone());
    }
    let dates = Arc::new(TodayDates::default());
    let authority = Arc::new(Authority::default());
    let limiter = PerCallerLimiter::new(
        CallerLimiter::Submit,
        CallerLimitSettings {
            per_minute: NonZeroU32::new(per_minute).unwrap(),
            max_keys: 100,
        },
    );
    let acceptance = Scripted::new(bindings);
    let port: Arc<dyn SellabilityV1> = acceptance.clone();
    let submit = SubmitService::new(
        slot,
        reads,
        h.orchestrator.clone(),
        dates.clone(),
        authority.clone(),
        limiter,
        200,
    )
    .with_acceptance(bound.then_some(port));
    Fixture {
        t,
        capture,
        submit,
        h,
        dates,
        authority,
        pdp,
        acceptance,
        engine,
    }
}

/// The same service with the scripted double bound (a later process after a crash).
fn rebind(f: &Fixture) -> SubmitService {
    let port: Arc<dyn SellabilityV1> = f.acceptance.clone();
    SubmitService::new(
        Arc::new(ArcSwapOption::from(Some(Arc::clone(&f.engine)))),
        Arc::new(f.t.reads_with(f.pdp.clone())),
        f.h.orchestrator.clone(),
        f.dates.clone(),
        f.authority.clone(),
        PerCallerLimiter::new(
            CallerLimiter::Submit,
            CallerLimitSettings {
                per_minute: NonZeroU32::new(60).unwrap(),
                max_keys: 100,
            },
        ),
        200,
    )
    .with_acceptance(Some(port))
}

/// The same service over `engine` (a fault-injecting one), with the scripted double bound.
fn service_over(f: &Fixture, engine: Engine) -> SubmitService {
    let port: Arc<dyn SellabilityV1> = f.acceptance.clone();
    SubmitService::new(
        Arc::new(ArcSwapOption::from(Some(Arc::new(engine)))),
        Arc::new(f.t.reads_with(f.pdp.clone())),
        f.h.orchestrator.clone(),
        f.dates.clone(),
        f.authority.clone(),
        PerCallerLimiter::new(
            CallerLimiter::Submit,
            CallerLimitSettings {
                per_minute: NonZeroU32::new(60).unwrap(),
                max_keys: 100,
            },
        ),
        200,
    )
    .with_acceptance(Some(port))
}

/// Fails the commit at one named persistence boundary (`FaultPoint`), every time it is reached.
struct FailAt {
    point: FaultPoint,
    hits: AtomicUsize,
}
#[async_trait]
impl FaultHook for FailAt {
    async fn at(&self, point: FaultPoint, _tx: &DbTx<'_>) -> Result<(), Abort> {
        if point != self.point {
            return Ok(());
        }
        self.hits.fetch_add(1, Ordering::SeqCst);
        Err(Abort::Injected(point))
    }
}

/// The submit request the service builds for `v` under `text` (the engine-level tests).
fn submit_request(v: &OrderView, text: &str) -> TransitionRequest {
    TransitionRequest {
        order_id: v.order.order_id,
        trigger: Trigger::Submit,
        operation: "submit",
        expected_version: OrderVersion::try_from(1).unwrap(),
        expected_draft_revision: v.draft_revision,
        document: json!({}),
        proposed: None,
        caller_reason: None,
        idempotency_key: key(text),
        correlation_id: None,
        execution: None,
    }
}

/// Resume the reserved attempt of `request` (its lease lapsed): a new owner and fence.
async fn reclaim(
    f: &Fixture,
    request: &TransitionRequest,
) -> (DurableExecution, commercial_attempt::Model) {
    lapse_leases(f, request.order_id).await;
    let Recovery::Existing(existing) = f
        .engine
        .recover_commercial_attempt(&buyer(), request)
        .await
        .unwrap()
    else {
        panic!("the reserved attempt is resumed")
    };
    let (execution, Frozen::Commercial(attempt)) = *existing else {
        panic!("not a commercial attempt")
    };
    (execution, attempt)
}

/// The first line's accepted receipt for `attempt`, from the double, as phase B records it.
async fn accepted_result(f: &Fixture, attempt: &commercial_attempt::Model) -> (String, Value) {
    let line = decode_line_requests(&attempt.line_requests)
        .unwrap()
        .lines
        .remove(0);
    let receipt = f
        .acceptance
        .check(
            service(SERVICE.0, SERVICE.1).ctx(),
            line.query.clone(),
            CommandMeta {
                idempotency_key: line.key.clone(),
            },
        )
        .await
        .unwrap();
    let value =
        receipt_result_value(line.line_id, &ReceiptResult::Accepted(Box::new(receipt))).unwrap();
    (line.line_id.to_string(), value)
}

/// Committed version numbers of `order`, ascending, with their `supersedes_version`.
async fn versions(f: &Fixture, order: Uuid) -> Value {
    f.t.json(&format!(
        "SELECT coalesce(jsonb_agg(jsonb_build_array(version, supersedes_version) \
         ORDER BY version), '[]'::jsonb) AS j FROM bss_orders__order_version \
         WHERE order_id='{order}'"
    ))
    .await
}

/// A rolling monthly line on the fixture's plan revision.
fn line() -> AddLine {
    AddLine {
        plan_id: PLAN,
        plan_revision_id: REVISION,
        selected_items: vec![SelectedItem {
            item_id: ITEM,
            quantity: Some(serde_json::from_value(json!("1")).unwrap()),
            selected_dim_value: None,
        }],
        currency: Currency::try_from("USD".to_owned()).unwrap(),
        contract_effective_date: None,
        service_activation_date: None,
        acceptance_due_date: None,
        term_duration: Some(AuthoredTerm::Rolling),
        billing_cycle: Some(BillingCycle::Month),
    }
}

/// A draft of `lines` lines (draft revision = `lines`), as the buyer; `tag` keeps keys distinct.
async fn draft_tagged(f: &Fixture, lines: usize, tag: &str) -> OrderView {
    draft_tagged_items(f, lines, tag, false).await
}
async fn draft_tagged_items(f: &Fixture, lines: usize, tag: &str, multikey: bool) -> OrderView {
    let order = CreateOrder {
        resource_tenant_id: u(RESOURCE),
        payer_tenant_id: PAYER,
        seller_tenant_id: u(SELLER),
        category: Category::NewSale,
        contract_id: None,
    };
    let created = view(
        f.capture
            .create(&buyer(), order, &create_meta(&format!("{tag}-create")))
            .await
            .unwrap(),
    );
    let id = created.order.order_id;
    for n in 0..lines {
        let revision = i64::try_from(n).unwrap();
        let mut authored = line();
        if multikey {
            let mut second = authored.selected_items[0].clone();
            second.item_id = u(0x21);
            authored.selected_items.push(second);
        }
        f.capture
            .add_line(
                &buyer(),
                id,
                authored,
                &write(&format!("{tag}-line-{n}"), 1, Some(revision)),
            )
            .await
            .unwrap();
    }
    f.t.reads().get(&buyer(), id).await.unwrap()
}
async fn draft(f: &Fixture, lines: usize) -> OrderView {
    draft_tagged(f, lines, "m26").await
}

fn meta(text: &str) -> CallMeta {
    CallMeta {
        expected_version: OrderVersion::try_from(1).unwrap(),
        idempotency_key: key(text),
        correlation_id: None,
        delegation_proof_ref: None,
    }
}

async fn submit_with(
    svc: &SubmitService,
    caller: &Caller,
    v: &OrderView,
    text: &str,
) -> Result<Settled, OrdersError> {
    let revision = v
        .draft_revision
        .unwrap_or(DraftRevision::try_from(0).unwrap());
    svc.submit(
        caller,
        v.order.order_id,
        revision,
        &meta(text),
        &CancellationToken::new(),
    )
    .await
    .map_err(OrdersError::from)
}
async fn submit(
    f: &Fixture,
    caller: &Caller,
    v: &OrderView,
    text: &str,
) -> Result<Settled, OrdersError> {
    submit_with(&f.submit, caller, v, text).await
}

async fn attempts(f: &Fixture, order: Uuid) -> i64 {
    f.t.n(&format!(
        "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}'"
    ))
    .await
}
async fn attempt_status(f: &Fixture, order: Uuid) -> Value {
    f.t.json(&format!(
        "SELECT to_jsonb(a.status) AS j FROM bss_orders__commercial_attempt a WHERE order_id='{order}'"
    ))
    .await
}
async fn high_water(f: &Fixture, order: Uuid) -> i64 {
    f.t.n(&format!(
        "SELECT version_allocation_high_water::bigint AS n FROM bss_orders__order WHERE order_id='{order}'"
    ))
    .await
}
/// Committed versions, line rows, pinned lines and totals rows of `order`.
async fn committed(f: &Fixture, order: Uuid) -> (i64, i64, i64, i64) {
    let n = async |sql: String| f.t.n(&sql).await;
    (
        n(format!(
            "SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}'"
        ))
        .await,
        n(format!(
            "SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}'"
        ))
        .await,
        n(format!(
            "SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' AND order_pin IS NOT NULL"
        ))
        .await,
        n(format!(
            "SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}'"
        ))
        .await,
    )
}
/// Lapse the lease of every in-flight marker and attempt of `order` (a crashed owner).
async fn lapse_leases(f: &Fixture, order: Uuid) {
    f.t.env
        .pg
        .sql(&format!(
            "UPDATE bss_orders__idempotency SET lease_expires_at = now() - interval '1 minute' \
             WHERE order_id='{order}' AND status='in_flight'; \
             UPDATE bss_orders__commercial_attempt SET lease_until = now() - interval '1 minute' \
             WHERE order_id='{order}' AND status IN ('prepared','running')"
        ))
        .await
        .unwrap();
}

/// No gate port, no date preparation and no principal lookup happened.
fn no_port_called(f: &Fixture) {
    for port in GatePort::ALL {
        assert_eq!(f.h.calls.of(port), 0, "{port:?}");
    }
    assert_eq!(*f.h.calls.read_lines.lock(), 0);
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(f.authority.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_blocking_assessment_settles_through_the_engine_with_diagnostics_and_reserves_nothing() {
    let mut stubs = fixture();
    // The payer relationship is not confirmed: predicate 1 blocks submit (D-215).
    for profile in stubs.payer_profiles.values_mut() {
        profile.in_relationship_with_seller = false;
    }
    let f = setup_with(stubs, 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let settled = submit(&f, &buyer(), &v, "m25-refuse").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::PayerRelationshipUnconfirmed))
    );
    // The refusal carries the complete gate report; the run's rows are the order's.
    assert!(settled.response.body()["context"]["data"]["assessment"].is_object());
    // D-233: the stored Problem carries exactly the report the snapshot keeps, and the SDK
    // decodes it into the typed diagnostics member.
    assert_eq!(
        Some(&settled.response.body()["context"]["data"]["assessment"]),
        settled.response.assessment.as_ref()
    );
    match crate::api::local_client::sdk_result::<Value>(settled.clone()) {
        Err(OrdersError::Settled {
            reason,
            diagnostics,
        }) => {
            assert_eq!(reason, Reason::PayerRelationshipUnconfirmed);
            assert!(diagnostics.assessment.is_some(), "{diagnostics:?}");
        }
        other => panic!("not the settled gate refusal: {other:?}"),
    }
    assert!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__gate_outcome WHERE order_id='{order}'"
        ))
        .await
            > 0
    );
    // M2-3: no attempt, no allocated candidate, no Pricing command.
    assert_eq!(attempts(&f, order).await, 0);
    assert_eq!(high_water(&f, order).await, 1);
    assert_eq!(f.authority.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.acceptance.calls(), 0);
    // The same key replays the settled refusal.
    let replay = submit(&f, &buyer(), &v, "m25-refuse").await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.response, settled.response);
}

/// Phase A alone (no acceptance port bound): the attempt is reserved with its frozen requests,
/// keys, assessment and evaluation, and the call answers unavailable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_passing_assessment_reserves_the_attempt_with_frozen_line_requests() {
    let f = setup(two_line_fixture(), 10).await;
    let v = draft(&f, 2).await;
    let order = v.order.order_id;
    let before = high_water(&f, order).await;
    let answer = submit(&f, &buyer(), &v, "m25-pass").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    assert_eq!(*f.h.calls.read_lines.lock(), 1, "one catalog call (M2-6)");
    let candidate = before + 1;
    // The attempt is reserved: `prepared`, at the next candidate, with the frozen basis and the
    // seller's service principal.
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
             AND status='prepared' AND candidate_version={candidate} \
             AND prepared_draft_revision=2 \
             AND commercial_subject_id='{}' \
             AND date_policy_basis ? 'proposed_utc_date' \
             AND line_requests->'order' ?& array['evaluation','report','vector'] \
             AND receipt_results = '{{}}'::jsonb",
            u(SERVICE.0)
        ))
        .await,
        1
    );
    // One frozen request per line, under its stable key, at the candidate, with its key.
    for line in &v.lines {
        let id = line.line_id;
        assert_eq!(
            f.t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
                 AND line_requests->'{id}'->>'key' = '{order}/{candidate}/{id}/submit' \
                 AND line_requests->'{id}'->'query'->>'order_version' = '{candidate}' \
                 AND line_requests->'{id}'->'item_keys_v1'->0->>'overlap_scope_key' = 'dev-stub-scope' \
                 AND line_requests->'{id}'->'item_keys_v1'->0->>'catalog_subscription_product_key' = 'dev-stub-product' \
                 AND length(line_requests->'{id}'->>'request_digest') = 64"
            ))
            .await,
            1,
            "{id}"
        );
    }
    // No order version was appended and the aggregate stays a draft.
    assert_eq!(committed(&f, order).await.0, 1);
}

/// A zero-line draft settles `no-lines` through the engine's `gate.has-lines` guard with no
/// port call, no date preparation and no attempt.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_empty_draft_settles_no_lines_without_any_port_call() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 0).await;
    let settled = submit(&f, &buyer(), &v, "m26-empty").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::NoLines)),
        "{settled:?}"
    );
    no_port_called(&f);
    assert_eq!(attempts(&f, v.order.order_id).await, 0);
    assert_eq!(f.acceptance.calls(), 0);
}

/// Selections across distinct lines exceed the order cap before dates, catalog ports or any
/// durable commercial reservation; a same-key retry replays the settled refusal.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_over_cap_draft_refuses_without_any_port_call_or_reservation() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 0).await;
    let order = v.order.order_id;
    for n in 0..2 {
        let mut add = line();
        add.selected_items = (0..101)
            .map(|item| SelectedItem {
                item_id: u(10_000 + item),
                quantity: Some(serde_json::from_value(json!("1")).unwrap()),
                selected_dim_value: None,
            })
            .collect();
        f.capture
            .add_line(
                &buyer(),
                order,
                add,
                &write(&format!("m3c-cap-line-{n}"), 1, Some(n)),
            )
            .await
            .unwrap();
    }
    let v = f.t.reads().get(&buyer(), order).await.unwrap();
    let high_water_before = high_water(&f, order).await;
    for _ in 0..2 {
        let settled = submit(&f, &buyer(), &v, "m3c-cap-submit").await.unwrap();
        assert_eq!(
            settled.outcome,
            SettledOutcome::Refused(Some(Reason::PurchaseCapacityExceeded)),
            "{settled:?}"
        );
    }
    no_port_called(&f);
    assert_eq!(f.acceptance.calls(), 0);
    assert_eq!(attempts(&f, order).await, 0);
    assert_eq!(high_water(&f, order).await, high_water_before);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_denied_caller_is_refused_before_any_port_call() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let stranger = user(STRANGER.0, STRANGER.1);
    let answer = submit(&f, &stranger, &v, "m25-denied").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Refused(_)), "{answer:?}");
    no_port_called(&f);
    assert_eq!(attempts(&f, v.order.order_id).await, 0);
    assert_eq!(f.acceptance.calls(), 0);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_eleventh_submit_in_a_minute_is_throttled() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let stranger = user(STRANGER.0, STRANGER.1);
    for n in 0..10 {
        let answer = submit(&f, &stranger, &v, &format!("m25-t-{n}")).await;
        assert!(matches!(answer, Err(OrdersError::Refused(_))), "{answer:?}");
    }
    let answer = submit(&f, &stranger, &v, "m25-t-10").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Other { .. }), "{answer:?}");
    no_port_called(&f);
}

/// D-188 success: one receipt per line under its frozen key, the schema-2 pin, the totals
/// (exact integers, canonical displays, the TCV on the order/recurring row), the claim, the
/// candidate version and exactly one `OrderSubmitted`; the attempt is committed and a same-key
/// replay runs no date preparation, no gate and no Pricing command (E2).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn success_commits_pins_totals_and_emits_exactly_one_order_submitted() {
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 2).await;
    let order = v.order.order_id;
    let candidate = high_water(&f, order).await + 1;
    let outbox = f.t.outbox().await;
    let settled = submit(&f, &buyer(), &v, "m26-ok").await.unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    assert_eq!(settled.response.body()["version"], json!(candidate));
    assert_eq!(settled.response.body()["state"], json!("submitted"));
    assert_eq!(
        settled.response.body()["assessment_id"],
        settled.response.assessment.as_ref().unwrap()["assessment_id"]
    );
    assert_eq!(f.acceptance.calls(), 2);
    for line in &v.lines {
        let id = line.line_id;
        assert!(
            f.acceptance
                .keys()
                .contains(&format!("{order}/{candidate}/{id}/submit"))
        );
        // The admitted line: commercial columns, the schema-2 pin naming this row, the key.
        assert_eq!(
            f.t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' \
                 AND version={candidate} AND line_id='{id}' \
                 AND commercial_attempt_id IS NOT NULL AND pricing_acceptance_id IS NOT NULL \
                 AND length(pricing_request_digest)=64 AND length(pricing_terms_digest)=64 \
                 AND order_pin->>'schema_version' = '2' \
                 AND order_pin->'accepted_version_ref'->>'line_id' = '{id}' \
                 AND overlap_scope_key IS NULL AND catalog_subscription_product_key IS NULL AND overlap_item_keys->0->>'overlap_scope_key' = 'dev-stub-scope' AND overlap_item_keys->0->>'catalog_subscription_product_key' = 'dev-stub-product'"
            ))
            .await,
            1,
            "{id}"
        );
    }
    // Two line rows and the order roll-up, the producer's values as canonical decimal text (the
    // fixture's 21/2 with display 10.50 is stored as 10.5), TCV on the carrier only.
    assert_eq!(committed(&f, order).await, (2, 2, 2, 3));
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}' \
             AND scope='order' AND charge_kind='recurring' AND tcv_exact='126' \
             AND tcv_display='126' AND tcv_basis='rolling_annualized' AND gross_exact='10.5' \
             AND gross_display='10.5'"
        ))
        .await,
        1
    );
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}' \
             AND scope='line' AND net_exact='10.5' AND net_display='10.5' AND tcv_exact IS NULL"
        ))
        .await,
        2
    );
    assert_eq!(attempt_status(&f, order).await, json!("committed"));
    assert_eq!(
        f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL")).await,
        1
    );
    assert_eq!(f.t.outbox().await, outbox + 1, "exactly one OrderSubmitted");
    assert!(
        settled.response.assessment.is_some(),
        "the settlement carries the report"
    );
    // The admission wrote the run's diagnostics naming the committed candidate (M2-2).
    assert!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__gate_outcome WHERE order_id='{order}' AND version={candidate}"
        ))
        .await
            > 0
    );
    // E2: the same key replays before any date, gate or Pricing work.
    let prepared = f.dates.prepared.load(Ordering::SeqCst);
    let reads = *f.h.calls.read_lines.lock();
    let replay = submit(&f, &buyer(), &v, "m26-ok").await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.response, settled.response);
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), prepared);
    assert_eq!(*f.h.calls.read_lines.lock(), reads);
    assert_eq!(f.acceptance.calls(), 2);
    assert_eq!(f.t.outbox().await, outbox + 1);
}

/// DESIGN §3.7: a usage-bearing line commits its committed component and an
/// `uncommitted_usage` row with NULL gross/net, an unavailable discount, no TCV and the
/// producer's usage declaration (exclusions included) verbatim, plus the roll-up's usage row;
/// every database check accepts them. The same rows altered past the money checks are refused
/// by the database itself (`ck_…resolved_total__5` for an amount on a usage row, `__9` for a
/// non-canonical display), so the checks are exercised at the DB on submit's own rows.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_usage_bearing_line_commits_an_uncommitted_usage_row_the_database_accepts() {
    let usage = TotalsDeclaration {
        charge_kind: EvaluationChargeKind::Usage,
        amount_basis: "usage_in_arrears".to_owned(),
        period_evidence: json!({"cycle": "month", "source": "dev-stub"}),
        item_breakdown: json!([]),
        recurring_by_cycle: json!({}),
        exclusions: json!({"usage": "metered; rated in arrears, not in this total"}),
    };
    let mut stubs = fixture();
    for line in stubs.evaluation.values_mut() {
        line.usage_flagged = true;
        line.usage_declaration = Some(usage.clone());
    }
    stubs
        .evaluation_totals
        .as_mut()
        .unwrap()
        .order_usage_declaration = Some(usage.clone());
    let f = setup_with(stubs, 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let settled = submit(&f, &buyer(), &v, "m26-usage").await.unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    // Line recurring + line usage + order recurring + order usage.
    assert_eq!(committed(&f, order).await, (2, 1, 1, 4));
    let exclusions = usage.exclusions.to_string();
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}' \
             AND charge_kind='usage' AND amount_status='uncommitted_usage' \
             AND gross_exact IS NULL AND gross_display IS NULL \
             AND net_exact IS NULL AND net_display IS NULL \
             AND discount_status='unavailable' AND discount_exact IS NULL \
             AND tcv_exact IS NULL AND tcv_basis IS NULL \
             AND amount_basis='usage_in_arrears' AND exclusions = '{exclusions}'::jsonb"
        ))
        .await,
        2
    );
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}' \
             AND charge_kind='recurring' AND amount_status='committed' \
             AND gross_exact='10.5' AND (tcv_exact IS NOT NULL) = (scope='order')"
        ))
        .await,
        2
    );
    // Submit's own rows, copied to the draft version and altered past one money check each.
    let copy = |kind: &str, change: Value| {
        format!(
            "INSERT INTO bss_orders__resolved_total SELECT (jsonb_populate_record(\
             NULL::bss_orders__resolved_total, to_jsonb(r) || '{change}'::jsonb)).* \
             FROM bss_orders__resolved_total r WHERE order_id='{order}' AND scope='line' \
             AND charge_kind='{kind}'"
        )
    };
    for (kind, change, check) in [
        (
            "usage",
            json!({"version": 1, "gross_exact": "0", "gross_display": "0", "net_exact": "0", "net_display": "0"}),
            "ck_bss_orders__resolved_total__5",
        ),
        (
            "recurring",
            json!({"version": 1, "gross_display": "10.50"}),
            "ck_bss_orders__resolved_total__9",
        ),
        (
            "recurring",
            json!({"version": 1, "net_exact": "1e1"}),
            "ck_bss_orders__resolved_total__9",
        ),
    ] {
        let error =
            f.t.env
                .pg
                .raw
                .execute_unprepared(&copy(kind, change.clone()))
                .await
                .unwrap_err();
        assert_eq!(
            violated_constraint(&error).as_deref(),
            Some(check),
            "{change}: {error}"
        );
    }
}

/// D-188: Pricing accepts line A and refuses line B: the refusal settles, the attempt is
/// refused, and no version, pin, total, claim or event is written; B's refusal stops phase B.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn accept_a_then_refuse_b_settles_the_refusal_and_writes_nothing_commercial() {
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 2).await;
    let order = v.order.order_id;
    let mut ids: Vec<Uuid> = v.lines.iter().map(|l| l.line_id).collect();
    ids.sort();
    // Phase B commands lines in line-id order: the second is refused.
    f.acceptance
        .refuse
        .lock()
        .insert(ids[1], CommercialReason::NotSellable);
    let outbox = f.t.outbox().await;
    let settled = submit(&f, &buyer(), &v, "m26-refuse-b").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::CatalogPredicateFailed))
    );
    assert_eq!(f.acceptance.calls(), 2);
    assert_eq!(attempt_status(&f, order).await, json!("refused"));
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
             AND receipt_results->'{}' ? 'accepted' \
             AND receipt_results->'{}'->'refused'->>'reason' = 'NOT_SELLABLE'",
            ids[0], ids[1]
        ))
        .await,
        1
    );
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(f.t.outbox().await, outbox);
    assert_eq!(
        f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL")).await,
        0
    );
    assert_eq!(f.t.order_row(order).await["state"], json!("draft"));
    // The candidate stays burned; the same key replays the refusal.
    let replay = submit(&f, &buyer(), &v, "m26-refuse-b").await.unwrap();
    assert!(replay.replayed);
    assert_eq!(f.acceptance.calls(), 2);
}

/// D-188: Pricing accepts line A and refuses line B: the refusal settles, the attempt is
/// refused, and no version, pin, total, claim or event is written; B's refusal stops phase B.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn unsupported_terms_in_phase_b_settles_burns_candidate_and_replays() {
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 2).await;
    let order = v.order.order_id;
    let mut ids: Vec<Uuid> = v.lines.iter().map(|l| l.line_id).collect();
    ids.sort();
    // Phase B commands lines in line-id order: the second is refused.
    f.acceptance
        .refuse
        .lock()
        .insert(ids[1], CommercialReason::UnsupportedTerms);
    let outbox = f.t.outbox().await;
    let settled = submit(&f, &buyer(), &v, "m3c-terms-refusal").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::BillingTermsUnsupported))
    );
    assert_eq!(f.acceptance.calls(), 2);
    assert_eq!(attempt_status(&f, order).await, json!("refused"));
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
             AND receipt_results->'{}' ? 'accepted' \
             AND receipt_results->'{}'->'refused'->>'reason' = 'UNSUPPORTED_TERMS'",
            ids[0], ids[1]
        ))
        .await,
        1
    );
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(f.t.outbox().await, outbox);
    assert_eq!(
        f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL")).await,
        0
    );
    assert_eq!(f.t.order_row(order).await["state"], json!("draft"));
    assert_eq!(high_water(&f, order).await, 2);
    // The candidate stays burned; the same key replays the refusal.
    let replay = submit(&f, &buyer(), &v, "m3c-terms-refusal").await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.response, settled.response);
    assert_eq!(high_water(&f, order).await, 2);
    assert_eq!(f.acceptance.calls(), 2);
}

/// D-188 lost reply: Pricing recorded the receipt but its reply was lost twice (the budget's
/// one retry included), so Orders recorded nothing and answered unavailable; after the lease
/// lapses the same-key retry re-sends under the same command key, gets Pricing's first receipt
/// back and commits. Recording the identical result again is `AlreadyRecorded`; a different one
/// fails closed.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_lost_reply_is_resent_under_the_same_key_and_never_recorded_twice() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    f.acceptance.lose_replies.store(2, Ordering::SeqCst);
    let answer = submit(&f, &buyer(), &v, "m26-lost").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    assert_eq!(
        f.acceptance.calls(),
        2,
        "the first command and its one retry"
    );
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
             AND status='prepared' AND receipt_results = '{{}}'::jsonb"
        ))
        .await,
        1,
        "nothing recorded for an unanswered command"
    );
    lapse_leases(&f, order).await;
    lapse_leases(&f, order).await;
    let settled = submit(&f, &buyer(), &v, "m26-lost").await.unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    let keys = f.acceptance.keys();
    assert_eq!(keys.len(), 3);
    assert!(
        keys.iter().all(|k| *k == keys[0]),
        "one command key: {keys:?}"
    );
    assert_eq!(committed(&f, order).await, (2, 1, 1, 2));
}

/// D-188 crash before any receipt: phase A reserved the attempt and the process stopped (no
/// acceptance port); a later process's same-key retry, after the lease lapses, resumes the
/// frozen attempt without re-assessing and commits the same candidate.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_crash_before_any_receipt_leaves_the_attempt_prepared_and_the_retry_completes() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let candidate = high_water(&f, order).await + 1;
    let answer = submit(&f, &buyer(), &v, "m26-crash").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    assert_eq!(attempt_status(&f, order).await, json!("prepared"));
    // The frozen request is the port's actual input, already persisted before dispatch.
    let frozen_before = f.t.json(&format!(
        "SELECT line_requests->'order' AS j FROM bss_orders__commercial_attempt WHERE order_id='{order}'"
    )).await;
    let sent = f.h.calls.evaluation_requests.lock().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        frozen_before["assessed_at"],
        serde_json::to_value(
            sent[0]
                .assessed_at
                .unwrap()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
        )
        .unwrap()
    );
    assert!(sent[0].tenant_axes.is_some());
    assert!(sent[0].acceptance_policy.is_some());
    assert!(sent[0].lines.iter().all(|line| {
        line.plan_id.is_some()
            && line.start_at.is_some()
            && line
                .resolved_terms
                .as_ref()
                .is_some_and(|terms| terms.snapshot.is_some())
    }));
    assert!(
        sent[0]
            .lines
            .iter()
            .flat_map(|line| &line.items)
            .flat_map(|item| &item.chains)
            .all(|chain| chain.binding.native_binding.is_some())
    );
    assert_eq!(
        frozen_before["evaluation_request"],
        serde_json::to_value(&sent[0]).unwrap()
    );
    // A live lease: the retry is still processing, nothing stolen.
    let later = rebind(&f);
    let busy = submit_with(&later, &buyer(), &v, "m26-crash")
        .await
        .unwrap_err();
    assert!(
        matches!(busy, OrdersError::Refused(Reason::StillProcessing)),
        "{busy:?}"
    );
    lapse_leases(&f, order).await;
    let reads = *f.h.calls.read_lines.lock();
    let prepared = f.dates.prepared.load(Ordering::SeqCst);
    let settled = submit_with(&later, &buyer(), &v, "m26-crash")
        .await
        .unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    assert_eq!(settled.response.body()["version"], json!(candidate));
    assert_eq!(f.t.n(&format!(
        "SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' AND version={candidate} AND overlap_scope_key IS NULL AND catalog_subscription_product_key IS NULL AND overlap_item_keys->0->>'overlap_scope_key'='dev-stub-scope' AND overlap_item_keys->0->>'catalog_subscription_product_key'='dev-stub-product'"
    )).await, 1);
    // E2: recovered from the frozen inputs, never re-assessed.
    assert_eq!(*f.h.calls.read_lines.lock(), reads);
    assert_eq!(*f.h.calls.evaluation_requests.lock(), sent);
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), prepared);
    assert_eq!(high_water(&f, order).await, candidate);
    assert_eq!(attempt_status(&f, order).await, json!("committed"));
    // D-235: the recovered commit settles the frozen gate report and writes the frozen outcome
    // vector as the run's diagnostics, under the frozen run id, naming the committed version.
    let frozen =
        f.t.json(&format!(
            "SELECT line_requests->'order' AS j FROM bss_orders__commercial_attempt \
             WHERE order_id='{order}'"
        ))
        .await;
    assert_eq!(
        frozen, frozen_before,
        "recovery retains the original Rating input"
    );
    assert_eq!(
        settled.response.assessment.as_ref(),
        Some(&frozen["report"]),
        "the settlement carries the frozen report"
    );
    let outcomes = frozen["vector"].as_array().unwrap().len();
    assert!(outcomes > 0);
    let run_id = frozen["assessment_id"].as_str().unwrap();
    assert_eq!(settled.response.body()["assessment_id"], json!(run_id));
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__gate_outcome WHERE order_id='{order}' \
             AND version={candidate} AND run_id='{run_id}'"
        ))
        .await,
        i64::try_from(outcomes).unwrap()
    );
}

/// A hung commercial exchange is bounded once, in `DeadlineAuthority` (D-244; `CatalogBinding::new`
/// wraps the production authority): phase A answers unavailable (the 503 wire) and reserves
/// nothing. Docker + `gate-dev-stubs`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_hung_commercial_exchange_answers_unavailable_without_reserving() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    f.authority.stall.store(true, Ordering::SeqCst);
    let bounded: Arc<dyn CommercialAuthority> = Arc::new(DeadlineAuthority::new(
        f.authority.clone(),
        std::time::Duration::from_millis(100),
    ));
    let port: Arc<dyn SellabilityV1> = f.acceptance.clone();
    let svc = SubmitService::new(
        Arc::new(ArcSwapOption::from(Some(Arc::clone(&f.engine)))),
        Arc::new(f.t.reads_with(f.pdp.clone())),
        f.h.orchestrator.clone(),
        f.dates.clone(),
        bounded,
        PerCallerLimiter::new(
            CallerLimiter::Submit,
            CallerLimitSettings {
                per_minute: NonZeroU32::new(60).unwrap(),
                max_keys: 100,
            },
        ),
        200,
    )
    .with_acceptance(Some(port));
    let answer = submit_with(&svc, &buyer(), &v, "t1-hung")
        .await
        .unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    assert_eq!(f.authority.calls.load(Ordering::SeqCst), 1);
    assert_eq!(attempts(&f, order).await, 0);
    assert_eq!(f.acceptance.calls(), 0);
}

/// D-188 step 4: the caller's submit authority is revoked after the receipts are recorded and
/// before the commit: the commit's authorization records the denial evidence, settles nothing
/// and the attempt is not committed.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn revoked_authority_before_the_commit_records_the_denial_and_commits_nothing() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let pdp = Arc::clone(&f.pdp);
    *f.acceptance.on_call.lock() = Some(Box::new(move |_| pdp.revoke()));
    let answer = submit(&f, &buyer(), &v, "m26-revoked").await.unwrap_err();
    // The buyer still reads the order, so the denial names the operation, not the order.
    assert!(
        matches!(
            answer,
            OrdersError::Refused(Reason::OperationNotPermittedForActor)
        ),
        "{answer:?}"
    );
    assert_eq!(f.acceptance.calls(), 1);
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
             AND status='running'"
        ))
        .await,
        1,
        "the receipt is recorded; the attempt is not committed"
    );
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit WHERE requested_order_ref='{order}' AND outcome='refused' AND sequence IS NULL AND trigger='submit'"
        ))
        .await,
        1,
        "the denial's unresolved evidence"
    );
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__idempotency WHERE order_id='{order}' AND operation='submit' AND status='settled'"
        ))
        .await,
        0
    );
}

/// The same key with an altered payload (another expected draft revision) is the engine's
/// unsettled `idempotency-mismatch`, found by the recovery probe before any assessment: no date
/// preparation, no gate run, the reserved attempt untouched and Pricing not commanded.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_same_key_with_an_altered_payload_is_an_idempotency_mismatch() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let answer = submit(&f, &buyer(), &v, "m26-mismatch").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    let mut altered = v.clone();
    altered.draft_revision = Some(DraftRevision::try_from(0).unwrap());
    let (prepared, reads) = (
        f.dates.prepared.load(Ordering::SeqCst),
        *f.h.calls.read_lines.lock(),
    );
    let answer = submit(&f, &buyer(), &altered, "m26-mismatch")
        .await
        .unwrap_err();
    assert!(
        matches!(answer, OrdersError::Refused(Reason::IdempotencyMismatch)),
        "{answer:?}"
    );
    // The recovery probe found the mismatch: no date preparation and no gate run.
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), prepared);
    assert_eq!(*f.h.calls.read_lines.lock(), reads);
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit \
             WHERE (order_id='{order}' OR requested_order_ref='{order}') \
             AND outcome='refused' AND reason='idempotency-mismatch'"
        ))
        .await,
        1,
        "the gate's resolved mismatch evidence"
    );
    assert_eq!(attempt_status(&f, order).await, json!("prepared"));
    assert_eq!(attempts(&f, order).await, 1);
    assert_eq!(f.acceptance.calls(), 0);
}

/// D-188 step 1 over a committed key: the order is no longer a draft, so the probe must run
/// before the draft-only read. The same key and payload replays the stored commit exactly
/// (`replayed`, the same response), and another payload under that key is the engine's unsettled
/// `idempotency-mismatch` with the gate's resolved evidence, never `Unavailable`; neither runs a
/// date preparation, a gate or a Pricing command, and nothing new is committed or emitted.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_committed_key_replays_its_commit_and_refuses_another_payload_as_a_mismatch() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let outbox = f.t.outbox().await;
    let settled = submit(&f, &buyer(), &v, "m26-committed-key").await.unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    assert!(!settled.replayed);
    assert_eq!(settled.response.body()["state"], json!("submitted"));
    assert_eq!(
        settled.response.body()["assessment_id"],
        settled.response.assessment.as_ref().unwrap()["assessment_id"]
    );
    let after_commit = committed(&f, order).await;
    let (prepared, reads, calls) = (
        f.dates.prepared.load(Ordering::SeqCst),
        *f.h.calls.read_lines.lock(),
        f.acceptance.calls(),
    );

    // The same key and payload: the stored commit, not the draft read's 503.
    let replay = submit(&f, &buyer(), &v, "m26-committed-key").await.unwrap();
    assert!(replay.replayed, "{replay:?}");
    assert_eq!(replay.outcome, SettledOutcome::Committed);
    assert_eq!(replay.response, settled.response);

    // Another payload (an older expected draft revision) under the committed key.
    let mut altered = v.clone();
    altered.draft_revision = Some(DraftRevision::try_from(0).unwrap());
    let answer = submit(&f, &buyer(), &altered, "m26-committed-key")
        .await
        .unwrap_err();
    assert!(
        matches!(answer, OrdersError::Refused(Reason::IdempotencyMismatch)),
        "{answer:?}"
    );
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__transition_audit \
             WHERE (order_id='{order}' OR requested_order_ref='{order}') \
             AND outcome='refused' AND reason='idempotency-mismatch'"
        ))
        .await,
        1,
        "the gate's resolved mismatch evidence"
    );

    // Neither probe answer assessed, commanded, committed or emitted anything.
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), prepared);
    assert_eq!(*f.h.calls.read_lines.lock(), reads);
    assert_eq!(f.acceptance.calls(), calls);
    assert_eq!(committed(&f, order).await, after_commit);
    assert_eq!(attempts(&f, order).await, 1);
    assert_eq!(attempt_status(&f, order).await, json!("committed"));
    assert_eq!(f.t.outbox().await, outbox + 1, "exactly one OrderSubmitted");
}

/// A new key on an already committed order: the authorized read finds no draft, so the engine
/// settles the state table's `not-admissible` (`{"state": "submitted", "trigger": "submit"}`)
/// before any guard, never the draft read's 503. No date preparation, gate run or Pricing
/// command happens, nothing is committed or emitted, and the same new key replays the refusal.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_new_key_on_a_committed_order_settles_not_admissible_and_replays() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let settled = submit(&f, &buyer(), &v, "m26-committed-first")
        .await
        .unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    let after_commit = committed(&f, order).await;
    let outbox = f.t.outbox().await;
    let (prepared, reads, calls) = (
        f.dates.prepared.load(Ordering::SeqCst),
        *f.h.calls.read_lines.lock(),
        f.acceptance.calls(),
    );

    let refused = submit(&f, &buyer(), &v, "m26-committed-new").await.unwrap();
    assert!(!refused.replayed);
    assert_eq!(
        refused.outcome,
        SettledOutcome::Refused(Some(Reason::NotAdmissible)),
        "{refused:?}"
    );
    assert_eq!(
        refused.response.body()["context"]["data"],
        json!({"state": "submitted", "trigger": "submit"})
    );
    assert!(refused.response.assessment.is_none());
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__idempotency WHERE order_id='{order}' \
             AND operation='submit' AND status='settled'"
        ))
        .await,
        2,
        "the commit's key and the refusal's key are both settled"
    );

    // The same new key replays the settled refusal.
    let replay = submit(&f, &buyer(), &v, "m26-committed-new").await.unwrap();
    assert!(replay.replayed, "{replay:?}");
    assert_eq!(replay.response, refused.response);

    // Neither answer assessed, commanded, committed or emitted anything.
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), prepared);
    assert_eq!(*f.h.calls.read_lines.lock(), reads);
    assert_eq!(f.acceptance.calls(), calls);
    assert_eq!(committed(&f, order).await, after_commit);
    assert_eq!(attempts(&f, order).await, 1);
    assert_eq!(f.t.outbox().await, outbox);
}

/// D-188: candidate exhaustion fails the reservation before any Pricing command with the
/// canonical resource-exhaustion Problem (quota subject `version-allocation`): no attempt, no
/// receipt, nothing settled (an engine error mapping; no reason is registered for it).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn version_exhaustion_is_refused_before_any_pricing_call() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    f.t.env
        .pg
        .sql(&format!(
            "UPDATE bss_orders__order SET version_allocation_high_water=2147483647 WHERE order_id='{order}'"
        ))
        .await
        .unwrap();
    let answer = submit(&f, &buyer(), &v, "m26-exhausted").await.unwrap_err();
    // The canonical resource-exhaustion Problem, unsettled, never a business refusal.
    let OrdersError::Other { canonical } = answer else {
        panic!("not the canonical resource-exhaustion error: {answer:?}")
    };
    let problem = toolkit_canonical_errors::Problem::from(canonical);
    assert_eq!(
        problem.problem_type,
        "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
    );
    assert_eq!(
        problem.context["violations"][0]["subject"],
        json!("version-allocation")
    );
    assert_eq!(f.acceptance.calls(), 0);
    assert_eq!(attempts(&f, order).await, 0);
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__idempotency WHERE order_id='{order}' AND operation='submit'"
        ))
        .await,
        0
    );
}

/// D-152/D-188: a receipt whose exclusive activation deadline already passed at the commit's
/// transition time refuses `order-binding-expired`; nothing commercial is written.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_expired_receipt_refuses_order_binding_expired() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    f.acceptance.lapsed.store(true, Ordering::SeqCst);
    let settled = submit(&f, &buyer(), &v, "m26-expired").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::OrderBindingExpired))
    );
    assert_eq!(attempt_status(&f, order).await, json!("refused"));
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
}

/// D-235 item 7 (R6): a receipt whose schema-2 pin alone exceeds the version byte limit
/// refuses `purchase-capacity-exceeded` through `gate.binding-admission`, as an oversized
/// version does; nothing commercial is written and the same key replays the settled refusal.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_single_oversized_pin_refuses_purchase_capacity_exceeded() {
    let f = setup_with_bindings(
        fixture(),
        10,
        true,
        Some(Arc::new(FixedBindings {
            sku_name: Some("x".repeat(bss_orders_sdk::commercial::MAX_VERSION_BYTES)),
            ..Default::default()
        })),
    )
    .await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let settled = submit(&f, &buyer(), &v, "c8-oversized-pin").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::PurchaseCapacityExceeded)),
        "{settled:?}"
    );
    assert_eq!(attempt_status(&f, order).await, json!("refused"));
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    let replay = submit(&f, &buyer(), &v, "c8-oversized-pin").await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.response, settled.response);
}

/// D-179/D-188 step 17: a second order of the same payer, resource and overlap key submitted
/// while the first holds its claim is refused `order-in-flight-for-key`, naming the visible
/// holder and carrying the rebuilt gate report (D-240 item 4); no version is appended for it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_concurrent_claim_refuses_order_in_flight_for_key_with_no_version() {
    let f = setup_with(fixture(), 10, true).await;
    let holder = draft_tagged(&f, 1, "m26-holder").await;
    let contender = draft_tagged(&f, 1, "m26-contender").await;
    let settled = submit(&f, &buyer(), &holder, "m26-claim-1").await.unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    let outbox = f.t.outbox().await;
    let refused = submit(&f, &buyer(), &contender, "m26-claim-2")
        .await
        .unwrap();
    assert_eq!(
        refused.outcome,
        SettledOutcome::Refused(Some(Reason::OrderInFlightForKey))
    );
    assert_eq!(
        refused.response.body()["context"]["data"]["blocked"][0]["conflicting_order_id"],
        json!(s(holder.order.order_id))
    );
    // D-233 as amended by D-240 item 4: the step-17 refusal carries the rebuilt predicate-9
    // report in the Problem body, the same value the snapshot keeps.
    let report = &refused.response.body()["context"]["data"]["assessment"];
    assert!(report.is_object(), "{refused:?}");
    assert_eq!(Some(report), refused.response.assessment.as_ref());
    assert!(
        report["failures"]
            .as_array()
            .expect("failures")
            .iter()
            .any(|f| f["reason"] == "order-in-flight-for-key"),
        "{report}"
    );
    match crate::api::local_client::sdk_result::<Value>(refused.clone()) {
        Err(OrdersError::Settled {
            reason,
            diagnostics,
        }) => {
            assert_eq!(reason, Reason::OrderInFlightForKey);
            assert!(diagnostics.assessment.is_some(), "{diagnostics:?}");
        }
        other => panic!("not the settled collision refusal: {other:?}"),
    }
    // The same key replays the identical snapshot, body report included.
    let replay = submit(&f, &buyer(), &contender, "m26-claim-2")
        .await
        .unwrap();
    assert!(replay.replayed, "{replay:?}");
    assert_eq!(replay.response, refused.response);
    let order = contender.order.order_id;
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(attempt_status(&f, order).await, json!("refused"));
    assert_eq!(f.t.outbox().await, outbox);
}

/// D-216: a passing run whose evaluation has no storable form (a Rating fraction) refuses
/// `evaluation-unavailable` before reserving anything; Pricing is not commanded.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn an_unstorable_evaluation_refuses_evaluation_unavailable_before_reserving() {
    let mut stubs = fixture();
    for line in stubs.evaluation.values_mut() {
        line.totals.gross.exact =
            crate::domain::gate::answers::ExactMoney::new("1", "3", "USD", 2).unwrap();
    }
    let f = setup_with(stubs, 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let settled = submit(&f, &buyer(), &v, "m26-fraction").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::EvaluationUnavailable))
    );
    assert_eq!(attempts(&f, order).await, 0);
    assert_eq!(high_water(&f, order).await, 1);
    assert_eq!(f.acceptance.calls(), 0);
}

/// D-188 fencing: after a reclaim the old owner can neither record nor commit. Owner 1 resumes
/// the reserved attempt, owner 2 reclaims it (rotated fence, live lease) and records the receipt;
/// owner 1's record is `StaleOwner` and writes nothing, owner 1's commit is refused
/// `still-processing` with nothing committed, and owner 2's commit completes the candidate.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_reclaimed_attempt_fences_out_the_old_owner_and_the_new_owner_completes() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let candidate = high_water(&f, order).await + 1;
    let answer = submit(&f, &buyer(), &v, "m26-fence").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    let request = submit_request(&v, "m26-fence");
    let (old, _) = reclaim(&f, &request).await;
    let (new, attempt) = reclaim(&f, &request).await;
    assert_ne!(
        old.owner(),
        new.owner(),
        "the reclaim rotated owner and fence"
    );
    let recorder = f.engine.receipt_recorder(&buyer(), &request).await.unwrap();
    let (line, value) = accepted_result(&f, &attempt).await;
    // The old owner records nothing.
    assert_eq!(
        recorder.record(&old, &line, value.clone()).await.unwrap(),
        ReceiptOutcome::StaleOwner
    );
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
             AND receipt_results = '{{}}'::jsonb"
        ))
        .await,
        1
    );
    // The new owner records the receipt.
    assert_eq!(
        recorder.record(&new, &line, value).await.unwrap(),
        ReceiptOutcome::Recorded(Recorded::New)
    );
    // The old owner's commit is refused under the new owner's live lease; nothing commits.
    let preparation = SubmitPreparation::for_attempt(&attempt, 200).unwrap();
    let refused = f
        .engine
        .transition(
            &buyer(),
            request.clone().with_execution(old.owner()),
            &preparation,
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        crate::infra::engine::EngineError::Refused(Reason::StillProcessing)
    );
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(attempt_status(&f, order).await, json!("running"));
    // The new owner commits the reserved candidate.
    let outcome = f
        .engine
        .transition(&buyer(), request.with_execution(new.owner()), &preparation)
        .await
        .unwrap();
    let settled = Settled::from_outcome(outcome);
    assert_eq!(settled.outcome, SettledOutcome::Committed, "{settled:?}");
    assert_eq!(settled.response.body()["version"], json!(candidate));
    assert_eq!(attempt_status(&f, order).await, json!("committed"));
    // A finished attempt records nothing more, for any owner.
    let (line_again, value_again) = accepted_result(&f, &attempt).await;
    let after = recorder.record(&new, &line_again, value_again).await;
    // The settled key is no longer in flight, so `verify_owner` answers `StaleOwner`.
    assert!(matches!(after, Ok(ReceiptOutcome::StaleOwner)), "{after:?}");
}

/// D-188: a draft edit after the reservation makes the commit refuse `version-conflict` (the
/// engine's precedence refusal, before any guard): the refusal settles over the attempt, the
/// attempt is `refused`, the candidate stays burned (high-water unchanged) and nothing
/// commercial is written.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_draft_edit_after_the_reservation_refuses_version_conflict_and_burns_the_candidate() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let answer = submit(&f, &buyer(), &v, "m26-edit").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    let reserved = high_water(&f, order).await;
    assert_eq!(attempt_status(&f, order).await, json!("prepared"));
    // The buyer edits the draft while the attempt is reserved.
    f.capture
        .add_line(&buyer(), order, line(), &write("m26-edit-line", 1, Some(1)))
        .await
        .unwrap();
    lapse_leases(&f, order).await;
    let later = rebind(&f);
    let settled = submit_with(&later, &buyer(), &v, "m26-edit").await.unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::VersionConflict)),
        "{settled:?}"
    );
    assert_eq!(attempt_status(&f, order).await, json!("refused"));
    assert_eq!(
        high_water(&f, order).await,
        reserved,
        "the candidate is burned, not reused"
    );
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(f.t.order_row(order).await["state"], json!("draft"));
    // The same key replays the settled refusal.
    let replay = submit_with(&later, &buyer(), &v, "m26-edit").await.unwrap();
    assert!(replay.replayed);
}

/// D-188 step 5 atomicity: a failure injected at the commit's audit append, its outbox enqueue
/// and immediately before COMMIT (after settlement) rolls the whole commit back. The attempt
/// stays `running` with its recorded receipt, nothing commercial or settled survives, and the
/// same-key retry (after the lease lapses) commits from the recorded receipt without commanding
/// Pricing again.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_commit_fault_rolls_back_keeps_the_receipts_and_the_same_key_retry_completes() {
    for (n, point) in [FaultPoint::Audit, FaultPoint::Enqueue, FaultPoint::Settle]
        .into_iter()
        .enumerate()
    {
        // Each fault case gets an independent claim namespace.
        let f = setup_with(fixture(), 10, true).await;
        let tag = format!("m26-fault-{n}");
        let v = draft_tagged(&f, 1, &tag).await;
        let order = v.order.order_id;
        let candidate = high_water(&f, order).await + 1;
        let hook = Arc::new(FailAt {
            point,
            hits: AtomicUsize::new(0),
        });
        let faulty = service_over(&f, f.engine.with_fault(hook.clone()));
        let outbox = f.t.outbox().await;
        let calls = f.acceptance.calls();
        let answer = submit_with(&faulty, &buyer(), &v, &tag).await.unwrap_err();
        assert!(
            matches!(answer, OrdersError::Internal),
            "{point:?}: {answer:?}"
        );
        assert!(
            hook.hits.load(Ordering::SeqCst) >= 1,
            "{point:?} was reached"
        );
        assert_eq!(f.acceptance.calls(), calls + 1, "{point:?}");
        // Rolled back: no version, pin, total, claim, event or settlement; the receipt stays.
        assert_eq!(committed(&f, order).await, (1, 0, 0, 0), "{point:?}");
        assert_eq!(f.t.outbox().await, outbox, "{point:?}");
        assert_eq!(
            f.t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
                 AND status='running' AND receipt_results->'{}' ? 'accepted'",
                v.lines[0].line_id
            ))
            .await,
            1,
            "{point:?}"
        );
        assert_eq!(
            f.t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__idempotency WHERE order_id='{order}' AND operation='submit' \
                 AND status='settled'"
            ))
            .await,
            0,
            "{point:?}"
        );
        // The same-key retry commits from the recorded receipt.
        lapse_leases(&f, order).await;
        let settled = submit_with(&rebind(&f), &buyer(), &v, &tag).await.unwrap();
        assert_eq!(
            settled.outcome,
            SettledOutcome::Committed,
            "{point:?}: {settled:?}"
        );
        assert_eq!(settled.response.body()["version"], json!(candidate));
        assert_eq!(
            f.acceptance.calls(),
            calls + 1,
            "{point:?}: no second command"
        );
        assert_eq!(f.t.outbox().await, outbox + 1, "{point:?}");
        assert_eq!(attempt_status(&f, order).await, json!("committed"));
    }
}

/// D-188 sparse versions: candidates 2 and 3 are refused (a Pricing refusal, then a lapsed
/// receipt) and burned; a fresh key then commits 4. The committed history is versions 1 and 4,
/// and version 4 supersedes 1, never an arithmetic predecessor.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn burned_candidates_leave_a_sparse_history_and_the_commit_supersedes_the_last_committed() {
    let f = setup_with(fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let line_id = v.lines[0].line_id;
    f.acceptance
        .refuse
        .lock()
        .insert(line_id, CommercialReason::NotSellable);
    let first = submit(&f, &buyer(), &v, "m26-sparse-1").await.unwrap();
    assert_eq!(
        first.outcome,
        SettledOutcome::Refused(Some(Reason::CatalogPredicateFailed))
    );
    assert_eq!(high_water(&f, order).await, 2);
    f.acceptance.refuse.lock().clear();
    f.acceptance.lapsed.store(true, Ordering::SeqCst);
    let second = submit(&f, &buyer(), &v, "m26-sparse-2").await.unwrap();
    assert_eq!(
        second.outcome,
        SettledOutcome::Refused(Some(Reason::OrderBindingExpired))
    );
    assert_eq!(high_water(&f, order).await, 3);
    f.acceptance.lapsed.store(false, Ordering::SeqCst);
    let third = submit(&f, &buyer(), &v, "m26-sparse-3").await.unwrap();
    assert_eq!(third.outcome, SettledOutcome::Committed, "{third:?}");
    assert_eq!(third.response.body()["version"], json!(4));
    assert_eq!(versions(&f, order).await, json!([[1, null], [4, 1]]));
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' \
             AND ((candidate_version IN (2, 3) AND status='refused') \
             OR (candidate_version = 4 AND status='committed'))"
        ))
        .await,
        3
    );
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' AND version=4"
        ))
        .await,
        1
    );
}

/// D-188 step 4 with the frozen basis (S2-10): an attempt reserved on one UTC day and recovered
/// on a later one refuses `date-cascade-invalid` at the commit's `capture.date-basis` guard
/// (the transition date is not the frozen proposed date); the attempt is refused and nothing
/// commercial is written. The day change is simulated by restating the frozen basis on the
/// previous day.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_recovered_attempt_on_a_later_utc_day_refuses_date_cascade_invalid() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let answer = submit(&f, &buyer(), &v, "m26-day").await.unwrap_err();
    assert!(matches!(answer, OrdersError::Unavailable), "{answer:?}");
    // The attempt was reserved "yesterday": its frozen basis proposes yesterday's date.
    let row = PolicyRow {
        policy_id: PLATFORM_DEFAULT_POLICY_ID,
        resource_tenant_id: None,
        service_activation_required: false,
        acceptance_due_required: false,
        revision: 1,
    };
    let policy = DatePolicySnapshot::effective(u(RESOURCE), &[row]).unwrap();
    let yesterday = OffsetDateTime::now_utc().date().previous_day().unwrap();
    let lines: Vec<(Uuid, SuppliedDates)> = v
        .lines
        .iter()
        .map(|l| {
            (
                l.line_id,
                SuppliedDates {
                    contract_effective: None,
                    service_activation: None,
                    acceptance_due: None,
                },
            )
        })
        .collect();
    let basis = DateBasis::resolve(u(RESOURCE), policy, yesterday, lines).frozen();
    f.t.env
        .pg
        .sql(&format!(
            "SET session_replication_role = replica; \
             UPDATE bss_orders__commercial_attempt SET date_policy_basis = '{basis}'::jsonb \
             WHERE order_id='{order}'; \
             SET session_replication_role = origin"
        ))
        .await
        .unwrap();
    lapse_leases(&f, order).await;
    let settled = submit_with(&rebind(&f), &buyer(), &v, "m26-day")
        .await
        .unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::DateCascadeInvalid)),
        "{settled:?}"
    );
    assert_eq!(attempt_status(&f, order).await, json!("refused"));
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
}

/// Q-C(a): receipt-bearing non-recurring lines commit with NULL overlap keys. Mixed
/// baskets offer only their recurring tuples to the real engine and claim repository.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
#[tracing_test::traced_test]
async fn non_recurring_receipts_commit_without_claims_and_mixed_orders_claim_recurring_only() {
    use bss_pricing_sdk::read::ChargeKind;
    for kind in [ChargeKind::OneTime, ChargeKind::Usage] {
        for mixed in [false, true] {
            let f = setup_with_bindings(
                fixture(),
                10,
                true,
                Some(Arc::new(FixedBindings::default())),
            )
            .await;
            let v = draft(&f, 2).await;
            for (index, line) in v.lines.iter().enumerate() {
                if !mixed || index == 0 {
                    f.acceptance
                        .bindings
                        .kinds
                        .lock()
                        .insert(line.line_id, kind.clone());
                }
            }
            let order = v.order.order_id;
            let settled = submit(&f, &buyer(), &v, "keyless-submit").await.unwrap();
            assert_eq!(
                settled.outcome,
                SettledOutcome::Committed,
                "{kind:?} mixed={mixed}"
            );
            assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL")).await, i64::from(mixed));
            assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' AND version=2 AND pricing_acceptance_id IS NOT NULL AND order_pin IS NOT NULL AND overlap_scope_key IS NULL AND catalog_subscription_product_key IS NULL")).await, 2);
            assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' AND version=2 AND overlap_item_keys='[]'::jsonb")).await, if mixed {1} else {2});
            let detail = f.t.reads().get(&buyer(), order).await.unwrap();
            assert_eq!(detail.lines.len(), 2, "{kind:?} mixed={mixed}");
            assert!(!detail.resolved_totals.unwrap().is_empty());
            assert_eq!(attempt_status(&f, order).await, json!("committed"));
        }
    }
}

/// D-194: credential rotation must not resend an uncertain command as another principal.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_changed_commercial_principal_cannot_resume_an_exact_retry() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    assert!(matches!(
        submit(&f, &buyer(), &v, "rotated").await,
        Err(OrdersError::Unavailable)
    ));
    let order = v.order.order_id;
    let candidate = high_water(&f, order).await;
    let reads = *f.h.calls.read_lines.lock();
    lapse_leases(&f, order).await;
    f.authority.rotated.store(true, Ordering::SeqCst);
    assert!(matches!(
        submit_with(&rebind(&f), &buyer(), &v, "rotated").await,
        Err(OrdersError::Unavailable)
    ));
    assert_eq!(
        f.acceptance.calls(),
        0,
        "no resend under the rotated identity"
    );
    assert_eq!(*f.h.calls.read_lines.lock(), reads, "no fresh assessment");
    assert_eq!(attempts(&f, order).await, 1);
    assert_eq!(high_water(&f, order).await, candidate);
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
}

/// Both phase-A reservations finish before either phase C: distinct candidates fence commits.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn different_keys_from_one_prior_version_reserve_unique_candidates_and_only_one_commits() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let actor = buyer();
    let (a, b) = tokio::join!(
        submit(&f, &actor, &v, "race-a"),
        submit(&f, &actor, &v, "race-b")
    );
    assert!(matches!(a, Err(OrdersError::Unavailable)), "{a:?}");
    assert!(matches!(b, Err(OrdersError::Unavailable)), "{b:?}");
    assert_eq!(attempts(&f, order).await, 2);
    assert_eq!(f.t.n(&format!("SELECT count(DISTINCT candidate_version) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' AND previous_committed_version=1")).await, 2);
    assert_eq!(high_water(&f, order).await, 3);
    lapse_leases(&f, order).await;
    let later = rebind(&f);
    let (a, b) = tokio::join!(
        submit_with(&later, &actor, &v, "race-a"),
        submit_with(&later, &actor, &v, "race-b")
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|s| s.outcome == SettledOutcome::Committed)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|s| s.outcome == SettledOutcome::Refused(Some(Reason::NotAdmissible)))
            .count(),
        1
    );
    assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' AND status='committed'")).await, 1);
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__order_version WHERE order_id='{order}'"
        ))
        .await,
        2
    );
}

/// A stored diagnostic uses the mapping that produced it, even after a deployment changes it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn diagnostic_replay_after_a_mapping_version_change_returns_the_stored_bytes() {
    let mut fx = fixture();
    fx.payer_profiles
        .get_mut(&PAYER)
        .unwrap()
        .in_relationship_with_seller = false;
    let f = setup(fx, 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let first = submit(&f, &buyer(), &v, "mapping-replay").await.unwrap();
    assert!(matches!(first.outcome, SettledOutcome::Refused(_)));
    // Emulate a snapshot written by a previous mapping deployment. Replay must not rebuild it.
    f.t.env.pg.sql(&format!("SET session_replication_role = replica; UPDATE bss_orders__idempotency SET settled_response = replace(settled_response::text, 'orders-gate-mapping/1', 'orders-gate-mapping/0')::jsonb WHERE order_id='{order}' AND operation='submit'; SET session_replication_role = origin" )).await.unwrap();
    let before = f.t.json(&format!("SELECT settled_response AS j FROM bss_orders__idempotency WHERE order_id='{order}' AND operation='submit'")).await;
    let reads = *f.h.calls.read_lines.lock();
    let replay = submit(&f, &buyer(), &v, "mapping-replay").await.unwrap();
    assert!(replay.replayed);
    assert_eq!(serde_json::to_value(&replay.response).unwrap(), before);
    assert_eq!(
        serde_json::to_vec(replay.response.body()).unwrap(),
        serde_json::to_vec(&before["body"]).unwrap(),
        "public diagnostic bytes are replayed without remapping"
    );
    assert_eq!(
        replay.response.assessment.as_ref().unwrap()["mapping_version"],
        "orders-gate-mapping/0"
    );
    assert_eq!(*f.h.calls.read_lines.lock(), reads);
}

/// A pre-product-capture attempt must retain absence on retry even though its scope is known.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn legacy_recovery_never_fabricates_product_identity_from_scope() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    let line = v.lines[0].line_id;
    assert!(matches!(
        submit(&f, &buyer(), &v, "legacy-product").await,
        Err(OrdersError::Unavailable)
    ));
    f.t.env.pg.sql(&format!(
        "SET session_replication_role = replica; UPDATE bss_orders__commercial_attempt SET line_requests = jsonb_set(line_requests, '{{{line}}}', ((line_requests->'{line}') - 'item_keys_v1'::text) || '{{\"overlap_scope_key\":\"dev-stub-scope\"}}'::jsonb) WHERE order_id='{order}'; SET session_replication_role = origin"
    )).await.unwrap();
    lapse_leases(&f, order).await;
    let later = rebind(&f);
    let settled = submit_with(&later, &buyer(), &v, "legacy-product")
        .await
        .unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed);
    assert_eq!(f.t.n(&format!(
        "SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' AND version=2 AND overlap_scope_key='dev-stub-scope' AND catalog_subscription_product_key IS NULL"
    )).await, 1);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn receipt_recorder_preserves_the_first_result_and_rejects_unfrozen_lines() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    assert!(matches!(
        submit(&f, &buyer(), &v, "m26-lost").await,
        Err(OrdersError::Unavailable)
    ));
    lapse_leases(&f, order).await;
    // The recorder: an identical first result is AlreadyRecorded, a different one fails closed.
    let request = TransitionRequest {
        order_id: order,
        trigger: Trigger::Submit,
        operation: "submit",
        expected_version: OrderVersion::try_from(1).unwrap(),
        expected_draft_revision: v.draft_revision,
        document: json!({}),
        proposed: None,
        caller_reason: None,
        idempotency_key: key("m26-lost"),
        correlation_id: None,
        execution: None,
    };
    let Recovery::Existing(existing) = f
        .engine
        .recover_commercial_attempt(&buyer(), &request)
        .await
        .unwrap()
    else {
        panic!("the reserved attempt is resumed")
    };
    let (execution, Frozen::Commercial(_)) = *existing else {
        panic!("not a commercial attempt")
    };
    let recorder = f.engine.receipt_recorder(&buyer(), &request).await.unwrap();
    let marker = json!({"refused": {"reason": "PRICE_CLOSED"}});
    // Only a frozen line records: another key, or the frozen order member, fails closed.
    for unfrozen in ["probe", "order", &Uuid::nil().to_string()] {
        assert_eq!(
            recorder
                .record(&execution, unfrozen, marker.clone())
                .await
                .unwrap_err(),
            crate::infra::engine::EngineError::Integration,
            "{unfrozen}"
        );
    }
    let probe_key = &v.lines[0].line_id.to_string();
    assert_eq!(
        recorder
            .record(&execution, probe_key, marker.clone())
            .await
            .unwrap(),
        ReceiptOutcome::Recorded(Recorded::New)
    );
    // A new result renews the owner's lease on the marker and the attempt (30 s lease).
    assert_eq!(
        f.t.n(&format!(
            "SELECT count(*) AS n FROM bss_orders__idempotency i \
             JOIN bss_orders__commercial_attempt a ON a.attempt_id = i.attempt_id \
             WHERE a.order_id='{order}' AND i.status='in_flight' \
             AND i.lease_expires_at > now() + interval '20 seconds' \
             AND a.lease_until = i.lease_expires_at"
        ))
        .await,
        1
    );
    assert_eq!(
        recorder
            .record(&execution, probe_key, marker)
            .await
            .unwrap(),
        ReceiptOutcome::Recorded(Recorded::AlreadyRecorded)
    );
    assert!(
        recorder
            .record(
                &execution,
                probe_key,
                json!({"refused": {"reason": "SKU_RETIRED"}})
            )
            .await
            .is_err()
    );
}

/// New producer declarations survive frozen attempt JSON and the actual SQL insert unchanged.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn explicit_components_and_non_recurring_tcv_carriers_commit_on_postgres() {
    use crate::domain::gate::answers::CommittedComponent;
    use bss_pricing_sdk::read::ChargeKind;
    for mode in ["mixed", "one_time", "usage"] {
        let mut stubs = fixture();
        let base = stubs.evaluation.values().next().unwrap().clone();
        let component = |kind| {
            let mut declaration = base.declaration.clone().unwrap();
            declaration.charge_kind = kind;
            CommittedComponent {
                totals: base.totals.clone(),
                declaration,
            }
        };
        let components = match mode {
            "mixed" => vec![
                component(EvaluationChargeKind::Recurring),
                component(EvaluationChargeKind::OneTime),
            ],
            "one_time" => vec![component(EvaluationChargeKind::OneTime)],
            _ => vec![],
        };
        let mut usage = base.declaration.clone().unwrap();
        usage.charge_kind = EvaluationChargeKind::Usage;
        usage.exclusions = json!({"usage": "uncommitted"});
        for line in stubs.evaluation.values_mut() {
            line.declaration = None;
            line.committed_components = Some(components.clone());
            line.usage_flagged = mode == "usage";
            line.usage_declaration = (mode == "usage").then_some(usage.clone());
        }
        let order = stubs.evaluation_totals.as_mut().unwrap();
        order.order_declaration = None;
        order.order_committed_components = Some(components);
        order.order_usage_declaration = (mode == "usage").then_some(usage);
        order.order_tcv_carrier =
            (mode != "mixed").then(|| component(EvaluationChargeKind::Recurring));
        let f =
            setup_with_bindings(stubs, 10, true, Some(Arc::new(FixedBindings::default()))).await;
        let v = draft(&f, 1).await;
        if mode != "mixed" {
            f.acceptance.bindings.kinds.lock().insert(
                v.lines[0].line_id,
                if mode == "usage" {
                    ChargeKind::Usage
                } else {
                    ChargeKind::OneTime
                },
            );
        }
        let order = v.order.order_id;
        let settled = submit(&f, &buyer(), &v, "components").await.unwrap();
        assert_eq!(
            settled.outcome,
            SettledOutcome::Committed,
            "{mode}: {settled:?}"
        );
        let detail = f.t.reads().get(&buyer(), order).await.unwrap();
        assert_eq!(detail.lines.len(), 1);
        assert_eq!(
            detail.resolved_totals.unwrap().len(),
            if mode == "mixed" { 4 } else { 3 }
        );
        let count = if mode == "mixed" { 4 } else { 3 };
        assert_eq!(
            f.t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}'"
            ))
            .await,
            count
        );
        assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}' AND scope='order' AND charge_kind='recurring' AND tcv_exact='126' AND gross_exact='10.5'")).await, 1);
        assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__resolved_total WHERE order_id='{order}' AND charge_kind='{}' AND {}",
            if mode == "usage" { "usage" } else { "one_time" },
            if mode == "usage" { "gross_exact IS NULL AND net_exact IS NULL" } else { "gross_exact='10.5' AND tcv_exact IS NULL" })).await, 2);
        assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__commercial_attempt WHERE order_id='{order}' AND line_requests->'order'->'evaluation'->'lines'->0 ? 'committed_components' AND line_requests->'order'->'evaluation' ? 'order_committed_components'")).await, 1);
    }
}

fn multikey_fixture() -> StubFixture {
    let mut fixture = fixture();
    let mut classification = fixture
        .item_classifications
        .values()
        .next()
        .unwrap()
        .clone();
    classification.catalog_subscription_product_key = "dev-second-product".into();
    classification.overlap_scope_key = "dev-second-scope".into();
    classification.classification.reference = "explicit-second-sku-classification".into();
    fixture.item_classifications.insert(u(0xd2), classification);
    let occupancy = fixture.occupancy.values().next().unwrap().clone();
    fixture
        .occupancy
        .insert("dev-second-scope".into(), occupancy);
    fixture
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn multikey_submit_recovery_persists_both_items_and_claims_without_requery() {
    let f = setup_with_bindings(
        multikey_fixture(),
        10,
        false,
        Some(Arc::new(FixedBindings {
            multikey: true,
            ..Default::default()
        })),
    )
    .await;
    let v = draft_tagged_items(&f, 1, "multikey", true).await;
    let order = v.order.order_id;
    assert!(matches!(
        submit(&f, &buyer(), &v, "multi-submit").await,
        Err(OrdersError::Unavailable)
    ));
    let line = v.lines[0].line_id;
    let frozen =
        f.t.json(&format!(
            "SELECT line_requests AS j FROM bss_orders__commercial_attempt WHERE order_id='{order}'"
        ))
        .await;
    assert_eq!(
        frozen[line.to_string()]["item_keys_v1"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let reads = *f.h.calls.read_lines.lock();
    let key_reads = f.h.calls.of(GatePort::OverlapKey);
    lapse_leases(&f, order).await;
    let settled = submit_with(&rebind(&f), &buyer(), &v, "multi-submit")
        .await
        .unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed);
    assert_eq!(*f.h.calls.read_lines.lock(), reads);
    assert_eq!(f.h.calls.of(GatePort::OverlapKey), key_reads);
    assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__order_line WHERE order_id='{order}' AND overlap_scope_key IS NULL AND catalog_subscription_product_key IS NULL AND jsonb_array_length(overlap_item_keys)=2")).await,1);
    assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL AND overlap_scope_key IN ('dev-stub-scope','dev-second-scope')")).await,2);
    assert_eq!(
        f.t.json(&format!(
            "SELECT line_requests AS j FROM bss_orders__commercial_attempt WHERE order_id='{order}'"
        ))
        .await,
        frozen
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn second_item_scope_conflict_acquires_no_partial_claim_or_version() {
    let f = setup_with_bindings(
        multikey_fixture(),
        10,
        false,
        Some(Arc::new(FixedBindings {
            multikey: true,
            ..Default::default()
        })),
    )
    .await;
    let v = draft_tagged_items(&f, 1, "multikey-conflict", true).await;
    let order = v.order.order_id;
    assert!(matches!(
        submit(&f, &buyer(), &v, "multi-conflict").await,
        Err(OrdersError::Unavailable)
    ));
    let holder = 0x9876;
    super::claims::create_for(&f.t.env.db, holder, PAYER.as_u128(), RESOURCE)
        .await
        .unwrap();
    f.t.env.pg.sql(&format!("INSERT INTO bss_orders__inflight_overlap_claim VALUES(gen_random_uuid(),'{PAYER}','{}','dev-stub-scope','{}',1,clock_timestamp(),NULL)",u(RESOURCE),u(holder))).await.unwrap();
    lapse_leases(&f, order).await;
    let settled = submit_with(&rebind(&f), &buyer(), &v, "multi-conflict")
        .await
        .unwrap();
    assert_eq!(
        settled.outcome,
        SettledOutcome::Refused(Some(Reason::OrderInFlightForKey))
    );
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL")).await,0);
    assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{}' AND released_at IS NULL",u(holder))).await,1);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn legacy_scalar_multirecurring_recovery_fails_without_changing_frozen_evidence() {
    let f = setup_with_bindings(
        multikey_fixture(),
        10,
        false,
        Some(Arc::new(FixedBindings {
            multikey: true,
            ..Default::default()
        })),
    )
    .await;
    let v = draft_tagged_items(&f, 1, "legacy-multikey", true).await;
    let order = v.order.order_id;
    let line = v.lines[0].line_id;
    assert!(matches!(
        submit(&f, &buyer(), &v, "legacy-multi").await,
        Err(OrdersError::Unavailable)
    ));
    f.t.env.pg.sql(&format!("SET session_replication_role = replica; UPDATE bss_orders__commercial_attempt SET line_requests = jsonb_set(line_requests, '{{{line}}}', ((line_requests->'{line}') - 'item_keys_v1'::text) || '{{\"overlap_scope_key\":\"dev-stub-scope\"}}'::jsonb) WHERE order_id='{order}'; SET session_replication_role = origin")).await.unwrap();
    let frozen =
        f.t.json(&format!(
            "SELECT line_requests AS j FROM bss_orders__commercial_attempt WHERE order_id='{order}'"
        ))
        .await;
    let key_reads = f.h.calls.of(GatePort::OverlapKey);
    lapse_leases(&f, order).await;
    assert!(matches!(
        submit_with(&rebind(&f), &buyer(), &v, "legacy-multi").await,
        Err(OrdersError::Integration)
    ));
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
    assert_eq!(
        f.t.json(&format!(
            "SELECT line_requests AS j FROM bss_orders__commercial_attempt WHERE order_id='{order}'"
        ))
        .await,
        frozen
    );
    assert_eq!(f.h.calls.of(GatePort::OverlapKey), key_reads);
    assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}' AND released_at IS NULL")).await,0);
}

/// D-197 / R3: a frozen query that differs from the frozen Rating request never commits, even
/// on recovery; nothing is written.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_frozen_query_that_differs_from_the_evaluated_request_never_commits() {
    let f = setup(fixture(), 10).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    assert!(matches!(
        submit(&f, &buyer(), &v, "c2-evaluated").await,
        Err(OrdersError::Unavailable)
    ));
    f.t.env.pg.sql(&format!("SET session_replication_role = replica; UPDATE bss_orders__commercial_attempt SET line_requests = jsonb_set(line_requests, '{{order,evaluation_request,market,region}}', '\"elsewhere\"'::jsonb) WHERE order_id='{order}'; SET session_replication_role = origin")).await.unwrap();
    lapse_leases(&f, order).await;
    assert!(matches!(
        submit_with(&rebind(&f), &buyer(), &v, "c2-evaluated").await,
        Err(OrdersError::Integration)
    ));
    assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
}
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
#[tracing_test::traced_test]
async fn failed_local_inflight_reads_refuse_before_attempt_acceptance_or_claims() {
    use crate::infra::gate::budget::Budget;
    use crate::infra::gate::tests::orchestrate::{
        FailedInflightRead, FailingInflight, PRIVATE_DB_CAUSE, harness_with_inflight,
    };
    let mut f = setup_with(fixture(), 60, true).await;
    for (fail, tag) in [
        (FailedInflightRead::Count, "a26-count"),
        (FailedInflightRead::Tuple, "a26-tuple"),
    ] {
        let reads = Arc::new(FailingInflight::new(fail, None));
        f.h = harness_with_inflight(fixture(), Arc::new(Budget::baseline()), reads);
        f.submit = rebind(&f);
        let v = draft_tagged(&f, 1, tag).await;
        let order = v.order.order_id;
        let error = submit(&f, &buyer(), &v, tag).await.unwrap_err();
        assert!(matches!(error, OrdersError::Unavailable));
        let problem = crate::api::rest::error::problem(error);
        assert_eq!(problem.status, Some(503));
        assert!(
            !serde_json::to_string(&problem)
                .unwrap()
                .contains(PRIVATE_DB_CAUSE)
        );
        assert_eq!(attempts(&f, order).await, 0);
        assert_eq!(high_water(&f, order).await, 1);
        assert_eq!(committed(&f, order).await, (1, 0, 0, 0));
        assert_eq!(f.t.n(&format!("SELECT count(*) AS n FROM bss_orders__inflight_overlap_claim WHERE order_id='{order}'")).await,0);
        assert_eq!(
            f.t.n(&format!(
                "SELECT count(*) AS n FROM bss_orders__gate_outcome WHERE order_id='{order}'"
            ))
            .await,
            0
        );
        assert_eq!(f.authority.calls.load(Ordering::SeqCst), 0);
        assert_eq!(f.acceptance.calls(), 0);
        assert_eq!(f.h.calls.of(GatePort::Assessment), 0);
        assert_eq!(f.h.calls.of(GatePort::Evaluation), 0);
    }
    assert!(logs_contain("in-flight line count"));
    assert!(logs_contain("in-flight tuple lookup"));
    assert!(!logs_contain(PRIVATE_DB_CAUSE));
}

/// Regression: successful submit must leave its immutable commercial version readable.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn submitted_reads_use_committed_content_and_page_current_version() {
    use bss_orders_sdk::reads::{LineList, PageSize};
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 2).await;
    let order = v.order.order_id;
    let settled = submit(&f, &buyer(), &v, "submitted-read").await.unwrap();
    assert_eq!(settled.outcome, SettledOutcome::Committed);
    // Deliberately remove any retained working rows: committed reads must not depend on them.
    f.t.env.pg.sql(&format!("ALTER TABLE bss_orders__draft_content DISABLE TRIGGER USER; DELETE FROM bss_orders__draft_content WHERE order_id='{order}'; ALTER TABLE bss_orders__draft_content ENABLE TRIGGER USER;")).await.unwrap();
    let reads = f.t.reads();
    let result = reads.get(&buyer(), order).await.expect("submitted detail");
    assert_eq!(result.order.state.token(), "submitted");
    assert_eq!(i64::from(result.version.version), 2);
    assert!(result.draft_revision.is_none());
    assert_eq!(result.lines.len(), 2);
    let body = serde_json::to_value(&result).unwrap();
    let persisted = f.t.json(&format!("SELECT order_pin AS j FROM bss_orders__order_line WHERE order_id='{order}' AND version=2 AND line_id='{}'", result.lines[0].line_id)).await;
    assert_eq!(body["lines"][0]["committed"]["order_pin"], persisted);
    assert!(body["resolved_totals"].as_array().unwrap().len() >= 2);
    let first = reads
        .list_lines(
            &buyer(),
            order,
            &LineList {
                limit: Some(PageSize::try_from(1).unwrap()),
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let second = reads
        .list_lines(
            &buyer(),
            order,
            &LineList {
                limit: Some(PageSize::try_from(1).unwrap()),
                cursor: first.page_info.next_cursor,
            },
        )
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(first.items[0].line_id, second.items[0].line_id);
    assert!(second.page_info.next_cursor.is_none());
    assert_eq!(first.current_version, result.version.version);
    assert!(first.draft_revision.is_none());
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn submitted_line_read_rejects_a_cursor_from_the_draft_version() {
    use bss_orders_sdk::reads::{LineList, PageSize};
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 2).await;
    let reads = f.t.reads();
    let first = reads
        .list_lines(
            &buyer(),
            v.order.order_id,
            &LineList {
                limit: Some(PageSize::try_from(1).unwrap()),
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(first.page_info.next_cursor.is_some());
    assert_eq!(
        submit(&f, &buyer(), &v, "cursor-submit")
            .await
            .unwrap()
            .outcome,
        SettledOutcome::Committed
    );
    assert!(matches!(
        reads
            .list_lines(
                &buyer(),
                v.order.order_id,
                &LineList {
                    limit: Some(PageSize::try_from(1).unwrap()),
                    cursor: first.page_info.next_cursor,
                }
            )
            .await,
        Err(OrdersError::Refused(Reason::CursorInvalid))
    ));
}

/// D-246: a line token `main` issued is bound to the creation version, so once the draft commits
/// it is refused like any token from the draft version.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_line_cursor_issued_by_main_is_refused_after_the_draft_commits() {
    use bss_orders_sdk::reads::{LineList, PageSize};
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 2).await;
    let reads = f.t.reads();
    let request = |cursor| LineList {
        limit: Some(PageSize::try_from(1).unwrap()),
        cursor,
    };
    let first = reads
        .list_lines(&buyer(), v.order.order_id, &request(None))
        .await
        .unwrap();
    let main_token = main_line_cursor(first.page_info.next_cursor.as_ref().unwrap());
    assert_eq!(
        submit(&f, &buyer(), &v, "main-cursor-submit")
            .await
            .unwrap()
            .outcome,
        SettledOutcome::Committed
    );
    assert!(matches!(
        reads
            .list_lines(&buyer(), v.order.order_id, &request(Some(main_token)))
            .await,
        Err(OrdersError::Refused(Reason::CursorInvalid))
    ));
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn submitted_reads_hide_denied_orders_and_reject_inconsistent_committed_selection() {
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    assert_eq!(
        submit(&f, &buyer(), &v, "integrity-submit")
            .await
            .unwrap()
            .outcome,
        SettledOutcome::Committed
    );
    let reads = f.t.reads();
    let stranger = user(STRANGER.0, STRANGER.1);
    for id in [order, Uuid::new_v4()] {
        assert!(matches!(
            reads.get(&stranger, id).await,
            Err(OrdersError::Refused(Reason::OrderNotFound))
        ));
        assert!(matches!(
            reads
                .list_lines(&stranger, id, &bss_orders_sdk::reads::LineList::default())
                .await,
            Err(OrdersError::Refused(Reason::OrderNotFound))
        ));
    }
    // Bypass append-only protection solely to simulate corrupt persisted evidence.
    f.t.env.pg.sql(&format!("ALTER TABLE bss_orders__order_line DISABLE TRIGGER USER; UPDATE bss_orders__order_line SET selected_items='[]'::jsonb WHERE order_id='{order}' AND version=2; ALTER TABLE bss_orders__order_line ENABLE TRIGGER USER;")).await.unwrap();
    assert!(matches!(
        reads.get(&buyer(), order).await,
        Err(OrdersError::Integration)
    ));
    assert!(matches!(
        reads
            .list_lines(&buyer(), order, &bss_orders_sdk::reads::LineList::default())
            .await,
        Err(OrdersError::Integration)
    ));
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn submitted_detail_refuses_missing_or_mismatched_saved_totals() {
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    assert_eq!(
        submit(&f, &buyer(), &v, "totals-integrity")
            .await
            .unwrap()
            .outcome,
        SettledOutcome::Committed
    );
    let reads = f.t.reads();
    f.t.env.pg.sql(&format!("ALTER TABLE bss_orders__resolved_total DISABLE TRIGGER USER; UPDATE bss_orders__resolved_total SET assessment_id='{}' WHERE order_id='{order}'; ALTER TABLE bss_orders__resolved_total ENABLE TRIGGER USER;", Uuid::new_v4())).await.unwrap();
    assert!(
        matches!(
            reads.get(&buyer(), order).await,
            Err(OrdersError::Integration)
        ),
        "mismatched assessment must not be disclosed"
    );
    f.t.env.pg.sql(&format!("ALTER TABLE bss_orders__resolved_total DISABLE TRIGGER USER; DELETE FROM bss_orders__resolved_total WHERE order_id='{order}'; ALTER TABLE bss_orders__resolved_total ENABLE TRIGGER USER;")).await.unwrap();
    assert!(
        matches!(
            reads.get(&buyer(), order).await,
            Err(OrdersError::Integration)
        ),
        "missing committed totals must not appear complete"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn submitted_reads_refuse_pin_axes_different_from_the_authorized_parent() {
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    assert_eq!(
        submit(&f, &buyer(), &v, "axes-integrity")
            .await
            .unwrap()
            .outcome,
        SettledOutcome::Committed
    );
    f.t.env.pg.sql(&format!("ALTER TABLE bss_orders__order DISABLE TRIGGER USER; UPDATE bss_orders__order SET seller_tenant_id='{}' WHERE order_id='{order}'; ALTER TABLE bss_orders__order ENABLE TRIGGER USER;", Uuid::new_v4())).await.unwrap();
    let reads = f.t.reads();
    assert!(matches!(
        reads.get(&buyer(), order).await,
        Err(OrdersError::Integration)
    ));
    assert!(matches!(
        reads
            .list_lines(&buyer(), order, &bss_orders_sdk::reads::LineList::default())
            .await,
        Err(OrdersError::Integration)
    ));
}

/// n45: the committed read compares the row's stored digests with the decoded pin's receipt.
/// A terms digest restated on the immutable row (triggers disabled) refuses as `Integration`,
/// and the log carries no stored digest (D-44).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
#[tracing_test::traced_test]
async fn a_committed_line_whose_stored_digest_differs_is_integration() {
    let f = setup_with(two_line_fixture(), 10, true).await;
    let v = draft(&f, 1).await;
    let order = v.order.order_id;
    assert_eq!(
        submit(&f, &buyer(), &v, "digest-integrity")
            .await
            .unwrap()
            .outcome,
        SettledOutcome::Committed
    );
    let reads = f.t.reads();
    reads
        .get(&buyer(), order)
        .await
        .expect("the committed order reads");
    let tampered = "0".repeat(64);
    f.t.env.pg.sql(&format!("ALTER TABLE bss_orders__order_line DISABLE TRIGGER USER; UPDATE bss_orders__order_line SET pricing_terms_digest='{tampered}' WHERE order_id='{order}' AND pricing_terms_digest IS NOT NULL; ALTER TABLE bss_orders__order_line ENABLE TRIGGER USER;")).await.unwrap();
    assert!(matches!(
        reads.get(&buyer(), order).await,
        Err(OrdersError::Integration)
    ));
    // The test's own SQL is in the sqlx statement log; only the read's integration lines count.
    logs_assert(|lines: &[&str]| {
        let read: Vec<_> = lines
            .iter()
            .filter(|line| line.contains("orders.read.integration"))
            .collect();
        match (
            read.is_empty(),
            read.iter().any(|line| line.contains(&tampered)),
        ) {
            (true, _) => Err("no integration line".to_owned()),
            (_, true) => Err("a stored digest reached the log".to_owned()),
            _ => Ok(()),
        }
    });
}

/// D-202 on the SDK preview and submit paths: a credential-bearing delegation proof reference is
/// `RequestInvalid` before any authorization, so the PDP is never asked and the gate's catalog is
/// never read. The client is built over the delivered services (`with_preview`/`with_submit`),
/// so this reaches the real `LocalOrdersClient` paths, not the undelivered answer. Gated tier
/// (`gate-dev-stubs`) over Docker/PG.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn sdk_preview_and_submit_refuse_a_credential_bearing_proof_before_the_pdp() {
    use crate::api::local_client::LocalOrdersClient;
    use crate::authz::test_pdp::pep;
    use crate::infra::gate::preview::{PreviewRuns, PreviewService};
    use bss_orders_sdk::OrdersV1;
    use bss_orders_sdk::models::DelegationProofRef;
    use bss_orders_sdk::preview::PreviewBasket;
    use bss_orders_sdk::reads::ReadMeta;

    /// Never reached: the refusal precedes the run.
    struct NoRuns;
    #[async_trait]
    impl PreviewRuns for NoRuns {
        async fn write(
            &self,
            _: &toolkit_db::secure::AccessScope,
            _: &crate::infra::gate::writer::RunRecord<'_>,
        ) -> Result<Uuid, toolkit_db::secure::ScopeError> {
            panic!("the preview run must not be written");
        }
    }

    let f = setup(fixture(), 60).await;
    let limiter = |kind| {
        PerCallerLimiter::new(
            kind,
            CallerLimitSettings {
                per_minute: NonZeroU32::new(60).unwrap(),
                max_keys: 100,
            },
        )
    };
    let preview = Arc::new(PreviewService::new(
        pep(f.pdp.clone()),
        f.h.orchestrator.clone(),
        f.dates.clone(),
        Arc::new(NoRuns),
        limiter(CallerLimiter::Preview),
    ));
    let client = LocalOrdersClient::new(
        Arc::new(CaptureService::new(
            Arc::new(ArcSwapOption::from(Some(Arc::clone(&f.engine)))),
            CaptureSettings { line_cap: 200 },
        )),
        Arc::new(f.t.reads_with(f.pdp.clone())),
    )
    .with_preview(Some(preview))
    .with_submit(Some(Arc::new(rebind(&f))));
    let jwt = || {
        Some(
            DelegationProofRef::try_from("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig".to_owned())
                .unwrap(),
        )
    };
    let ctx = buyer().ctx().clone();
    let line = json!({
        "plan_id": PLAN, "plan_revision_id": REVISION,
        "selected_items": [{"item_id": ITEM, "quantity": "1"}],
        "currency": "EUR", "term_duration": {"kind": "rolling"}, "billing_cycle": "month",
    });
    let basket: PreviewBasket = serde_json::from_value(json!({
        "resource_tenant_id": u(RESOURCE), "payer_tenant_id": u(30),
        "seller_tenant_id": u(SELLER), "lines": [line],
    }))
    .unwrap();

    let error = client
        .preview(
            &ctx,
            basket,
            ReadMeta {
                correlation_id: None,
                delegation_proof_ref: jwt(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, OrdersError::Refused(Reason::RequestInvalid)),
        "{error:?}"
    );
    let error = client
        .submit(
            &ctx,
            u(99),
            DraftRevision::try_from(0).unwrap(),
            CallMeta {
                delegation_proof_ref: jwt(),
                ..meta("jwt-1")
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, OrdersError::Refused(Reason::RequestInvalid)),
        "{error:?}"
    );
    assert_eq!(f.pdp.evaluations.load(Ordering::SeqCst), 0, "no PDP call");
    assert_eq!(*f.h.calls.read_lines.lock(), 0);
    assert_eq!(f.dates.prepared.load(Ordering::SeqCst), 0);
}
