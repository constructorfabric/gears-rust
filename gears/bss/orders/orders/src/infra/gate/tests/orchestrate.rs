//! The orchestrator shell over the Task 8b stubs and the example fixture, every port wrapped to
//! count its calls (S3-11/S3-12; D-215, D-180, D-227, M2-6). Compiled only with
//! `gate-dev-stubs`; the Preview service tests (`preview.rs`) reuse its harness.
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]
use super::doubles::{bound_cell, sku};
use crate::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use crate::domain::gate::answers::{
    AcceptancePolicy, AcceptancePolicyQuery, AssessmentAnswer, AssessmentQuery, BillingTermsQuery,
    ContractFacts, ContractQuery, EvaluationCycle, IndicativeTax, OccupancyObservation,
    OccupancyQuery, OrderEvaluation, OrderEvaluationRequest, OverlapKey, OverlapKeyLine,
    PayerProfile, PayerProfileQuery, ResolvedTerms, TaxQuery, TermIntentInput,
};
use crate::domain::gate::assess::BasketLine;
use crate::domain::gate::orchestrate::{Basket, GateTarget, LineTerms, TenantAxes};
use crate::domain::gate::ports::{GatePort, PortAnswer, Unavailable};
use crate::domain::gate::pricing_map::{CatalogAnswer, LineCatalogRequest, SelectedItem};
use crate::domain::gate::vector::Operation;
use crate::domain::overlap::ClaimParties;
use crate::infra::gate::GatePorts;
use crate::infra::gate::budget::{Budget, RunBudget};
use crate::infra::gate::orchestrate::{
    InMemoryInflightClaims, InflightClaims, InflightReadError, OrchestrateError, Orchestrator,
};
use crate::infra::gate::ports::{
    AcceptancePolicyReads, AssessmentReads, BillingTermsReads, ContractReads, EvaluationReads,
    IdentityReads, IndicativeTaxReads, OccupancyReads, OverlapKeyReads,
};
use crate::infra::gate::pricing_read::CatalogReads;
use crate::infra::gate::signals::{Flow, GateObservation, RecordingGateSignals};
use crate::infra::gate::stubs::{
    AcceptancePolicyStub, AssessmentStub, BillingTermsStub, ContractsStub, EvaluationStub,
    IdentityStub, IndicativeTaxStub, OccupancyStub, OverlapKeyStub, StubFixture,
};
use async_trait::async_trait;
use bss_orders_sdk::catalog::Reason;
use bss_pricing_sdk::read::{ResolvedBindings, RevisionRef};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use time::{Date, Month};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The example fixture shipped beside the gear config.
const EXAMPLE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/gate-dev-stubs.example.json"
);
// Keys the example fixture states.
pub const PAYER: Uuid = Uuid::from_u128(0xa1);
pub const CONTRACT: Uuid = Uuid::from_u128(0xc1);
pub const SKU: Uuid = Uuid::from_u128(0xd1);
pub const REVISION: Uuid = Uuid::from_u128(0xe1);
pub const LINE: Uuid = Uuid::from_u128(0xf1);
const SCOPE_KEY: &str = "dev-stub-scope";
// Keys it does not state.
pub const SELLER: Uuid = Uuid::from_u128(0x5e11);
pub const RESOURCE: Uuid = Uuid::from_u128(0x7e5);
pub const PLAN: Uuid = Uuid::from_u128(0x10);
pub const ITEM: Uuid = Uuid::from_u128(0x20);
const OTHER_ORDER: Uuid = Uuid::from_u128(0x0de7);
/// The fixture order a Submit or Amendment run belongs to.
pub const ORDER: Uuid = Uuid::from_u128(0x0de6);
/// Another customer of the same payer: the D-180 addend counts its orders (F03 predicate 7).
const OTHER_RESOURCE: Uuid = Uuid::from_u128(0x7e6);

pub fn day() -> Date {
    Date::from_calendar_date(2026, Month::October, 22).unwrap()
}
pub fn fixture() -> StubFixture {
    StubFixture::load(Path::new(EXAMPLE_FIXTURE)).unwrap()
}

/// Calls per port; the catalog counts `read_lines` as itself and `read_line` separately.
#[derive(Default)]
pub struct Calls {
    ports: Mutex<BTreeMap<GatePort, usize>>,
    pub(crate) read_lines: Mutex<usize>,
    pub(crate) evaluation_requests: Mutex<Vec<OrderEvaluationRequest>>,
    read_line: Mutex<usize>,
}
impl Calls {
    fn hit(&self, port: GatePort) {
        *self.ports.lock().entry(port).or_default() += 1;
    }
    pub(crate) fn of(&self, port: GatePort) -> usize {
        self.ports.lock().get(&port).copied().unwrap_or(0)
    }
}

/// A stub that counts its calls and, when `cancel_on_call`, cancels the run after answering.
struct Counted<T> {
    inner: T,
    calls: Arc<Calls>,
    cancel_on_call: bool,
    delay: std::time::Duration,
}
impl<T> Counted<T> {
    async fn hit(&self, port: GatePort, cancel: &CancellationToken) {
        self.calls.hit(port);
        if self.cancel_on_call {
            cancel.cancel();
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
    }
}

#[async_trait]
impl IdentityReads for Counted<IdentityStub> {
    async fn payer_profile(
        &self,
        ctx: &SecurityContext,
        q: &PayerProfileQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<PayerProfile> {
        self.hit(GatePort::IdentityProfile, cancel).await;
        self.inner.payer_profile(ctx, q, run, cancel).await
    }
}
#[async_trait]
impl ContractReads for Counted<ContractsStub> {
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        q: &[ContractQuery],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<BTreeMap<Uuid, ContractFacts>> {
        self.hit(GatePort::ContractResolution, cancel).await;
        self.inner.resolve(ctx, q, run, cancel).await
    }
}
#[async_trait]
impl OverlapKeyReads for Counted<OverlapKeyStub> {
    async fn keys(
        &self,
        ctx: &SecurityContext,
        payer: Uuid,
        resource: Option<Uuid>,
        lines: &[OverlapKeyLine],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Vec<OverlapKey>> {
        self.hit(GatePort::OverlapKey, cancel).await;
        self.inner
            .keys(ctx, payer, resource, lines, run, cancel)
            .await
    }
}
#[async_trait]
impl OccupancyReads for Counted<OccupancyStub> {
    async fn get_overlap_occupancy(
        &self,
        ctx: &SecurityContext,
        q: &[OccupancyQuery],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Vec<(OccupancyQuery, OccupancyObservation)>> {
        self.hit(GatePort::Occupancy, cancel).await;
        self.inner.get_overlap_occupancy(ctx, q, run, cancel).await
    }
}
#[async_trait]
impl BillingTermsReads for Counted<BillingTermsStub> {
    async fn resolve(
        &self,
        q: &[BillingTermsQuery],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Vec<ResolvedTerms>> {
        self.hit(GatePort::BillingTerms, cancel).await;
        self.inner.resolve(q, run, cancel).await
    }
}
#[async_trait]
impl AcceptancePolicyReads for Counted<AcceptancePolicyStub> {
    async fn policy(
        &self,
        q: &AcceptancePolicyQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Option<AcceptancePolicy>> {
        self.hit(GatePort::AcceptancePolicy, cancel).await;
        self.inner.policy(q, run, cancel).await
    }
}
#[async_trait]
impl AssessmentReads for Counted<AssessmentStub> {
    async fn assess(
        &self,
        q: &AssessmentQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<AssessmentAnswer> {
        self.hit(GatePort::Assessment, cancel).await;
        self.inner.assess(q, run, cancel).await
    }
}
#[async_trait]
impl EvaluationReads for Counted<EvaluationStub> {
    async fn evaluate(
        &self,
        ctx: &SecurityContext,
        q: &OrderEvaluationRequest,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<OrderEvaluation> {
        self.calls.evaluation_requests.lock().push(q.clone());
        self.hit(GatePort::Evaluation, cancel).await;
        self.inner.evaluate(ctx, q, run, cancel).await
    }
}
#[async_trait]
impl IndicativeTaxReads for Counted<IndicativeTaxStub> {
    async fn estimate(
        &self,
        ctx: &SecurityContext,
        q: &TaxQuery,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<IndicativeTax> {
        self.hit(GatePort::IndicativeTax, cancel).await;
        self.inner.estimate(ctx, q, run, cancel).await
    }
}

/// The catalog: every line composes `ITEM` on the fixture's revision, bound to its SKU.
pub struct Catalog(Arc<Calls>);
impl Catalog {
    /// The one catalog answer every line gets (the submit tests accept on its bindings).
    pub(crate) fn answer() -> CatalogAnswer {
        CatalogAnswer {
            current: Ok(RevisionRef {
                plan_id: PLAN,
                revision_id: REVISION,
                revision_no: 1,
            }),
            resolved: Ok(Arc::new(ResolvedBindings {
                plan_id: PLAN,
                revision_id: REVISION,
                cells: vec![bound_cell(ITEM, None, SKU)],
            })),
            skus: BTreeMap::from([(SKU, PortAnswer::Answered(sku(SKU, SELLER)))]),
        }
    }
}
#[async_trait]
impl CatalogReads for Catalog {
    async fn read_line(
        &self,
        _seller: Uuid,
        _req: &LineCatalogRequest,
        _resolve_date: Date,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> CatalogAnswer {
        *self.0.read_line.lock() += 1;
        Self::answer()
    }
    async fn read_lines(
        &self,
        _seller: Uuid,
        lines: &[LineCatalogRequest],
        _resolve_date: Date,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> BTreeMap<Uuid, CatalogAnswer> {
        *self.0.read_lines.lock() += 1;
        lines.iter().map(|l| (l.line_id, Self::answer())).collect()
    }
}

pub struct Harness {
    pub(crate) orchestrator: Orchestrator,
    pub(crate) calls: Arc<Calls>,
    pub(crate) inflight: Arc<InMemoryInflightClaims>,
}

/// Every port counted over `fixture`; the identity stub cancels the run when `cancel_identity`.
pub fn harness(fixture: StubFixture, cancel_identity: bool) -> Harness {
    harness_with(
        fixture,
        cancel_identity,
        Arc::new(Budget::baseline()),
        |_| {},
    )
}

/// [`harness`] over `budget` (a recording signal sink, say), `replace` substituting ports
/// after the counted stubs are bound.
pub fn harness_with(
    fixture: StubFixture,
    cancel_identity: bool,
    budget: Arc<Budget>,
    replace: impl FnOnce(&mut GatePorts),
) -> Harness {
    harness_over(fixture, cancel_identity, budget, replace, None)
}

/// Test-only constructor substituting local storage reads without production fault knobs.
pub fn harness_with_inflight(
    fixture: StubFixture,
    budget: Arc<Budget>,
    inflight: Arc<dyn InflightClaims>,
) -> Harness {
    harness_over(fixture, false, budget, |_| {}, Some(inflight))
}

fn harness_over(
    fixture: StubFixture,
    cancel_identity: bool,
    budget: Arc<Budget>,
    replace: impl FnOnce(&mut GatePorts),
    custom_inflight: Option<Arc<dyn InflightClaims>>,
) -> Harness {
    let f = Arc::new(fixture);
    let calls = Arc::new(Calls::default());
    macro_rules! wrap {
        ($stub:ident) => {
            Arc::new(Counted {
                inner: $stub::new(Arc::clone(&f)),
                calls: Arc::clone(&calls),
                cancel_on_call: false,
                delay: std::time::Duration::ZERO,
            })
        };
    }
    let mut ports = GatePorts::unavailable();
    ports.identity = Arc::new(Counted {
        inner: IdentityStub::new(Arc::clone(&f)),
        calls: Arc::clone(&calls),
        cancel_on_call: cancel_identity,
        delay: std::time::Duration::ZERO,
    });
    ports.contracts = wrap!(ContractsStub);
    ports.overlap_keys = wrap!(OverlapKeyStub);
    ports.occupancy = wrap!(OccupancyStub);
    ports.billing_terms = wrap!(BillingTermsStub);
    ports.acceptance_policy = wrap!(AcceptancePolicyStub);
    ports.assessment = wrap!(AssessmentStub);
    ports.evaluation = wrap!(EvaluationStub);
    ports.indicative_tax = wrap!(IndicativeTaxStub);
    replace(&mut ports);
    let inflight = Arc::new(InMemoryInflightClaims::default());
    let reader = custom_inflight.unwrap_or_else(|| Arc::clone(&inflight) as _);
    let orchestrator = Orchestrator::new(Arc::new(ports), budget, reader)
        .with_catalog(Arc::new(Catalog(Arc::clone(&calls))));
    Harness {
        orchestrator,
        calls,
        inflight,
    }
}

fn basket(operation: Operation) -> Basket {
    let line = BasketLine {
        line_id: LINE,
        catalog: LineCatalogRequest {
            line_id: LINE,
            plan_id: PLAN,
            revision_id: REVISION,
            items: vec![SelectedItem {
                item_id: ITEM,
                quantity: "1".into(),
                selected_dim_value: None,
            }],
        },
        currency: Some("USD".into()),
        contract_ref: Some(CONTRACT),
    };
    let row = PolicyRow {
        policy_id: PLATFORM_DEFAULT_POLICY_ID,
        resource_tenant_id: None,
        service_activation_required: false,
        acceptance_due_required: false,
        revision: 1,
    };
    let policy = DatePolicySnapshot::effective(RESOURCE, &[row]).unwrap();
    let dates = DateBasis::resolve(RESOURCE, policy, day(), [(LINE, SuppliedDates::default())]);
    Basket {
        assessed_at: time::OffsetDateTime::UNIX_EPOCH,
        target: target(operation),
        assessment_id: Uuid::from_u128(0xa55),
        axes: TenantAxes {
            resource: RESOURCE,
            payer: PAYER,
            seller: SELLER,
        },
        contract_id: None,
        lines: vec![line],
        terms: BTreeMap::from([(
            LINE,
            LineTerms {
                intent: crate::domain::gate::orchestrate::LineTermIntent::Stated(
                    TermIntentInput::Rolling,
                ),
                cycle: Some(EvaluationCycle::Month),
            },
        )]),
        resolve_date: day(),
        dates,
    }
}

/// The run target of `operation` for the fixture order (a Preview has none).
pub fn target(operation: Operation) -> GateTarget {
    match operation {
        Operation::Preview => GateTarget::Preview,
        Operation::Submit => GateTarget::Submit { order_id: ORDER },
        Operation::Amendment => GateTarget::Amendment { order_id: ORDER },
    }
}

async fn run(
    h: &Harness,
    b: &Basket,
) -> Result<crate::infra::gate::orchestrate::Resolved, OrchestrateError> {
    // The D-180 count takes no caller scope: the reader builds its own private payer-count
    // scope (`PrivateScope::for_payer_count`).
    h.orchestrator
        .run(&SecurityContext::anonymous(), b, &CancellationToken::new())
        .await
}

#[tokio::test]
async fn a_full_preview_calls_every_expected_port_exactly_once() {
    let h = harness(fixture(), false);
    h.inflight
        .claims
        .lock()
        .push((PAYER, OTHER_RESOURCE, SCOPE_KEY.to_owned(), OTHER_ORDER));
    let resolved = run(&h, &basket(Operation::Preview)).await.unwrap();
    for port in [
        GatePort::IdentityProfile,
        GatePort::ContractResolution,
        GatePort::OverlapKey,
        GatePort::Occupancy,
        GatePort::BillingTerms,
        GatePort::AcceptancePolicy,
        GatePort::Assessment,
        GatePort::Evaluation,
        GatePort::IndicativeTax,
    ] {
        assert_eq!(h.calls.of(port), 1, "{port:?}");
    }
    assert_eq!(*h.calls.read_lines.lock(), 1);
    assert_eq!(*h.calls.read_line.lock(), 0);
    assert_eq!(h.inflight.reads.load(Ordering::SeqCst), 1);
    assert!(
        resolved.unavailable_kinds.is_empty(),
        "{:?}",
        resolved.unavailable_kinds
    );
    assert_eq!(resolved.bindings[&LINE].len(), 1);
    assert_eq!(resolved.keys[&LINE][0].overlap_scope_key, SCOPE_KEY);
    assert!(resolved.terms.contains_key(&LINE));
    assert!(resolved.policy.is_some());
    let sent = h.calls.evaluation_requests.lock();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].assessed_at, Some(time::OffsetDateTime::UNIX_EPOCH));
    assert_eq!(sent[0].tenant_axes, Some(basket(Operation::Preview).axes));
    assert_eq!(sent[0].acceptance_policy, resolved.policy);
    assert_eq!(resolved.evaluation_request.as_ref(), Some(&sent[0]));
    assert!(
        sent[0].lines[0].items[0].chains[0]
            .binding
            .native_binding
            .is_some()
    );
    assert!(resolved.evaluation.is_some());
    assert!(resolved.tax.is_some());
    assert!(
        !resolved
            .assessment
            .failures
            .iter()
            .any(|f| f.reason == Reason::OrderInFlightForKey)
    );
    // The fixture's scope admits one subscription; the other customer's in-flight line fills
    // it.
    assert_eq!(
        resolved.assessment.primary.and_then(|o| o.reason),
        Some(Reason::OverlapCardinalityExceeded)
    );
}

#[tokio::test]
async fn an_order_contract_is_resolved_and_no_contract_skips_the_port() {
    for contract in [None, Some(CONTRACT)] {
        let h = harness(fixture(), false);
        let mut b = basket(Operation::Preview);
        b.contract_id = contract;
        b.lines[0].contract_ref = None;
        let resolved = run(&h, &b).await.unwrap();
        assert_eq!(
            h.calls.of(GatePort::ContractResolution),
            usize::from(contract.is_some())
        );
        let rows: Vec<_> = resolved
            .assessment
            .vector
            .outcomes()
            .iter()
            .filter(|o| {
                o.key.predicate == crate::domain::gate::predicates::PredicateId::ContractActive
            })
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key.line_id, None);
        assert_eq!(
            rows[0].verdict,
            crate::domain::gate::vector::Verdict::Passed
        );
    }
}

#[tokio::test]
async fn a_same_resource_sibling_fails_predicate_nine_without_adding_to_d180() {
    let h = harness(fixture(), false);
    // Same payer/resource: the pre-check fails predicate 9 independently of predicate 7.
    h.inflight
        .claims
        .lock()
        .push((PAYER, RESOURCE, SCOPE_KEY.to_owned(), OTHER_ORDER));
    let resolved = run(&h, &basket(Operation::Preview)).await.unwrap();
    assert_eq!(h.inflight.reads.load(Ordering::SeqCst), 1);
    let row = resolved
        .assessment
        .vector
        .outcomes()
        .iter()
        .find(|o| o.key.predicate == crate::domain::gate::predicates::PredicateId::InFlightPerKey)
        .unwrap();
    assert_eq!(
        (row.verdict, row.reason),
        (
            crate::domain::gate::vector::Verdict::Failed,
            Some(Reason::OrderInFlightForKey)
        )
    );
    let mut own = basket(Operation::Amendment);
    own.target = GateTarget::Amendment {
        order_id: OTHER_ORDER,
    };
    let resolved = run(&h, &own).await.unwrap();
    assert!(
        !resolved
            .assessment
            .failures
            .iter()
            .any(|f| f.reason == Reason::OrderInFlightForKey)
    );
    assert!(
        !resolved
            .assessment
            .failures
            .iter()
            .any(|f| f.reason == Reason::OverlapCardinalityExceeded),
        "{:?}",
        resolved.assessment.failures
    );
}

#[tokio::test]
async fn submit_never_asks_for_tax() {
    let h = harness(fixture(), false);
    let resolved = run(&h, &basket(Operation::Submit)).await.unwrap();
    assert_eq!(h.calls.of(GatePort::IndicativeTax), 0);
    assert_eq!(h.calls.of(GatePort::Evaluation), 1);
    assert!(resolved.tax.is_none());
    assert!(
        resolved.assessment.primary.is_none(),
        "{:?}",
        resolved.assessment.primary
    );
}

#[tokio::test]
async fn an_unconfirmed_payer_asks_no_payer_dependent_port() {
    let mut f = fixture();
    f.payer_profiles
        .get_mut(&PAYER)
        .unwrap()
        .in_relationship_with_resource = false;
    let h = harness(f, false);
    let mut b = basket(Operation::Preview);
    b.contract_id = Some(CONTRACT);
    b.lines[0].contract_ref = None;
    let resolved = run(&h, &b).await.unwrap();
    for port in [
        GatePort::ContractResolution,
        GatePort::OverlapKey,
        GatePort::Occupancy,
        // The terms query carries the payer and the contract (D-215 item 2).
        GatePort::BillingTerms,
        // No confirmed market: no evaluation, so no tax either.
        GatePort::Evaluation,
        GatePort::IndicativeTax,
    ] {
        assert_eq!(h.calls.of(port), 0, "{port:?}");
    }
    assert_eq!(h.inflight.reads.load(Ordering::SeqCst), 0);
    assert!(resolved.terms.is_empty());
    assert_eq!(h.calls.of(GatePort::IdentityProfile), 1);
    assert_eq!(*h.calls.read_lines.lock(), 1);
    // Ports not asked are never logged as unavailable.
    assert!(
        resolved.unavailable_kinds.is_empty(),
        "{:?}",
        resolved.unavailable_kinds
    );
    assert_eq!(
        resolved.assessment.primary.and_then(|o| o.reason),
        Some(Reason::PayerRelationshipUnconfirmed)
    );
}

#[tokio::test]
async fn cancelling_wave_a_stops_before_payer_dependent_ports() {
    let h = harness(fixture(), true);
    let out = run(&h, &basket(Operation::Preview)).await;
    assert!(matches!(out, Err(OrchestrateError::Cancelled)), "{out:?}");
    assert_eq!(h.calls.of(GatePort::IdentityProfile), 1);
    assert_eq!(*h.calls.read_lines.lock(), 1);
    assert_eq!(h.calls.of(GatePort::AcceptancePolicy), 1);
    assert_eq!(h.calls.of(GatePort::ContractResolution), 0);
    assert_eq!(h.calls.of(GatePort::Assessment), 0);
}

#[tokio::test]
#[tracing_test::traced_test]
async fn an_unavailable_port_is_logged_once_with_its_kind() {
    let mut f = fixture();
    f.acceptance_policy = None;
    let h = harness(f, false);
    let resolved = run(&h, &basket(Operation::Submit)).await.unwrap();
    assert_eq!(
        resolved.unavailable_kinds,
        vec![(GatePort::AcceptancePolicy, Unavailable::Missing)]
    );
    assert!(logs_contain("orders.gate.unavailable_kind"));
    assert!(logs_contain("port=\"acceptance-policy\""));
    assert!(logs_contain("kind=Missing"));
}

/// A budget whose signals land in the returned recorder.
fn recording_budget() -> (Arc<Budget>, Arc<RecordingGateSignals>) {
    let sink = Arc::new(RecordingGateSignals::default());
    let budget = Budget::from_config(&crate::config::GateConfig::default(), sink.clone());
    (Arc::new(budget), sink)
}

/// An assessment port whose provider is down.
struct DownAssessment;
#[async_trait]
impl AssessmentReads for DownAssessment {
    async fn assess(
        &self,
        _q: &AssessmentQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<AssessmentAnswer> {
        PortAnswer::Unavailable(Unavailable::Outage)
    }
}

#[tokio::test]
async fn an_unavailable_port_and_an_unevaluable_run_are_signalled() {
    let (budget, sink) = recording_budget();
    let h = harness_with(fixture(), false, budget, |ports| {
        ports.assessment = Arc::new(DownAssessment);
    });
    run(&h, &basket(Operation::Submit)).await.unwrap();
    let seen = sink.observations();
    assert!(
        seen.contains(&GateObservation::PortUnavailable {
            port: GatePort::Assessment,
            kind: Unavailable::Outage,
        }),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|o| matches!(
            o,
            GateObservation::Run { flow: Flow::Submit, signals } if !signals.unevaluable.is_empty()
        )),
        "{seen:?}"
    );
}

/// R11 (D-194 R08 matrix): the policy, terms and assessment reads are service-principal reads
/// with no buyer-context fallback. The ports take no caller context; each is asked for the
/// basket's seller only, whose principal the adapter resolves (as `PricingCatalogReads` does).
#[derive(Default)]
struct SellerReads(Mutex<Vec<(GatePort, Uuid)>>);
#[async_trait]
impl AcceptancePolicyReads for SellerReads {
    async fn policy(
        &self,
        q: &AcceptancePolicyQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Option<AcceptancePolicy>> {
        self.0
            .lock()
            .push((GatePort::AcceptancePolicy, q.seller_tenant_id));
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}
#[async_trait]
impl BillingTermsReads for SellerReads {
    async fn resolve(
        &self,
        q: &[BillingTermsQuery],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<ResolvedTerms>> {
        self.0.lock().extend(
            q.iter()
                .map(|q| (GatePort::BillingTerms, q.seller_tenant_id)),
        );
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}
#[async_trait]
impl AssessmentReads for SellerReads {
    async fn assess(
        &self,
        q: &AssessmentQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<AssessmentAnswer> {
        self.0
            .lock()
            .push((GatePort::Assessment, q.seller_tenant_id));
        PortAnswer::Unavailable(Unavailable::Missing)
    }
}

#[tokio::test]
async fn service_principal_reads_are_asked_for_the_seller_with_no_caller_context() {
    let reads = Arc::new(SellerReads::default());
    let bound = Arc::clone(&reads);
    let h = harness_with(
        fixture(),
        false,
        Arc::new(Budget::baseline()),
        move |ports| {
            ports.acceptance_policy = bound.clone();
            ports.billing_terms = bound.clone();
            ports.assessment = bound;
        },
    );
    run(&h, &basket(Operation::Preview)).await.unwrap();
    let seen = reads.0.lock().clone();
    for port in [
        GatePort::AcceptancePolicy,
        GatePort::BillingTerms,
        GatePort::Assessment,
    ] {
        assert!(
            seen.contains(&(port, SELLER)),
            "{port:?} not asked for the seller: {seen:?}"
        );
    }
    assert!(seen.iter().all(|(_, seller)| *seller == SELLER), "{seen:?}");
}

#[tokio::test]
async fn a_revoked_seller_grant_is_publicly_unavailable_and_operator_only_denied() {
    use super::doubles::{FakePricing, FakeProducts, FakeResolver};
    use crate::config::CommercialIdentityConfig;
    use crate::infra::gate::authority::{AuthnAuthority, CommercialAuthority};
    use crate::infra::gate::pricing_read::PricingCatalogReads;

    let (budget, sink) = recording_budget();
    let h = harness_with(fixture(), false, Arc::clone(&budget), |_| {});
    let pricing = FakePricing::new(
        RevisionRef {
            plan_id: PLAN,
            revision_id: REVISION,
            revision_no: 1,
        },
        vec![bound_cell(ITEM, None, SKU)],
    );
    let pricing = Arc::new(pricing);
    let identity = CommercialIdentityConfig {
        seller_tenant_id: SELLER,
        client_id: "orders-gate".to_owned(),
        client_secret: secrecy::SecretString::from("x"),
        scopes: Vec::new(),
    };
    let authority: Arc<dyn CommercialAuthority> = Arc::new(AuthnAuthority::new(
        Arc::new(FakeResolver::new(Uuid::from_u128(0x0bd5), SELLER)),
        &[identity],
    ));
    let catalog = PricingCatalogReads::new(
        pricing.clone(),
        Arc::new(FakeProducts::default()),
        authority,
        budget,
    );
    let orchestrator = h.orchestrator.clone().with_catalog(Arc::new(catalog));
    let before = orchestrator
        .run(
            &SecurityContext::anonymous(),
            &basket(Operation::Submit),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        !before
            .assessment
            .failures
            .iter()
            .any(|f| f.reason == Reason::PricingRevisionUnavailable)
    );
    pricing.fail_with(Some(super::doubles::Fault::Denied));
    let result = orchestrator
        .run(
            &SecurityContext::anonymous(),
            &basket(Operation::Submit),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let report = result.assessment.vector.report(Uuid::new_v4());
    let public = serde_json::to_value(&report).unwrap();
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.reason == Reason::PricingRevisionUnavailable)
    );
    assert!(
        !public.to_string().contains("denied"),
        "operator classification must stay private"
    );
    let seen = sink.observations();
    assert!(
        seen.contains(&GateObservation::PortUnavailable {
            port: GatePort::CurrentRevision,
            kind: Unavailable::Denied,
        }),
        "{seen:?}"
    );
    assert_eq!(
        seen.iter()
            .filter(|o| **o
                == GateObservation::CatalogDenial {
                    catalog_tenant: SELLER
                })
            .count(),
        1,
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_preview_shaped_run_is_signalled_as_preview() {
    let (budget, sink) = recording_budget();
    let h = harness_with(fixture(), false, budget, |_| {});
    run(&h, &basket(Operation::Preview)).await.unwrap();
    let flows: Vec<Flow> = sink
        .observations()
        .into_iter()
        .filter_map(|o| match o {
            GateObservation::Run { flow, .. } => Some(flow),
            _ => None,
        })
        .collect();
    assert_eq!(flows, vec![Flow::Preview]);
}

/// Delay the batched catalog boundary without changing its fixture answers.
struct DelayedCatalog(Catalog);
#[async_trait]
impl CatalogReads for DelayedCatalog {
    async fn read_line(
        &self,
        seller: Uuid,
        req: &LineCatalogRequest,
        date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> CatalogAnswer {
        self.0.read_line(seller, req, date, run, cancel).await
    }
    async fn read_lines(
        &self,
        seller: Uuid,
        reqs: &[LineCatalogRequest],
        date: Date,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> BTreeMap<Uuid, CatalogAnswer> {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.0.read_lines(seller, reqs, date, run, cancel).await
    }
}

// Serializing identity and catalog would take 400 ms and fail this test.
#[tokio::test(start_paused = true)]
async fn independent_identity_and_catalog_complete_in_one_delay() {
    let (budget, _) = recording_budget();
    let mut h = harness_with(fixture(), false, budget, |ports| {
        ports.identity = Arc::new(Counted {
            inner: IdentityStub::new(Arc::new(fixture())),
            calls: Arc::new(Calls::default()),
            cancel_on_call: false,
            delay: std::time::Duration::from_millis(200),
        });
    });
    h.orchestrator = h
        .orchestrator
        .clone()
        .with_catalog(Arc::new(DelayedCatalog(Catalog(Arc::clone(&h.calls)))));
    let start = tokio::time::Instant::now();
    let resolved = run(&h, &basket(Operation::Preview)).await.unwrap();
    assert_eq!(start.elapsed(), std::time::Duration::from_millis(200));
    assert!(resolved.assessment.primary.is_none());
    assert!(resolved.evaluation.is_some());
}

#[tokio::test]
async fn cancelling_wave_b_stops_before_assessment_and_evaluation() {
    let (budget, sink) = recording_budget();
    let h = harness_with(fixture(), false, budget, |ports| {
        ports.billing_terms = Arc::new(Counted {
            inner: BillingTermsStub::new(Arc::new(fixture())),
            calls: Arc::new(Calls::default()),
            cancel_on_call: true,
            delay: std::time::Duration::ZERO,
        });
    });
    assert!(matches!(
        run(&h, &basket(Operation::Preview)).await,
        Err(OrchestrateError::Cancelled)
    ));
    assert_eq!(h.calls.of(GatePort::Assessment), 0);
    assert_eq!(h.calls.of(GatePort::Evaluation), 0);
    assert_eq!(h.calls.of(GatePort::IndicativeTax), 0);
    assert!(
        !sink
            .observations()
            .iter()
            .any(|o| matches!(o, GateObservation::Run { .. }))
    );
}

#[tokio::test]
async fn cancelling_wave_c_stops_before_tax() {
    let (budget, sink) = recording_budget();
    let h = harness_with(fixture(), false, budget, |ports| {
        ports.assessment = Arc::new(Counted {
            inner: AssessmentStub::new(Arc::new(fixture())),
            calls: Arc::new(Calls::default()),
            cancel_on_call: true,
            delay: std::time::Duration::ZERO,
        });
    });
    assert!(matches!(
        run(&h, &basket(Operation::Preview)).await,
        Err(OrchestrateError::Cancelled)
    ));
    assert_eq!(h.calls.of(GatePort::IndicativeTax), 0);
    assert!(
        !sink
            .observations()
            .iter()
            .any(|o| matches!(o, GateObservation::Run { .. }))
    );
}

// A serial contracts/terms/keys wave or assessment/evaluation wave takes >=400 ms.
#[tokio::test(start_paused = true)]
async fn independent_payer_and_assessment_branches_overlap() {
    for wave in ["B", "C"] {
        let (budget, _) = recording_budget();
        let h = harness_with(fixture(), false, budget, |ports| {
            macro_rules! delayed {
                ($stub:ident) => {
                    Arc::new(Counted {
                        inner: $stub::new(Arc::new(fixture())),
                        calls: Arc::new(Calls::default()),
                        cancel_on_call: false,
                        delay: std::time::Duration::from_millis(200),
                    })
                };
            }
            if wave == "B" {
                ports.contracts = delayed!(ContractsStub);
                ports.billing_terms = delayed!(BillingTermsStub);
                ports.overlap_keys = delayed!(OverlapKeyStub);
            } else {
                ports.assessment = delayed!(AssessmentStub);
                ports.evaluation = delayed!(EvaluationStub);
            }
        });
        let start = tokio::time::Instant::now();
        let resolved = run(&h, &basket(Operation::Preview)).await.unwrap();
        assert_eq!(
            start.elapsed(),
            std::time::Duration::from_millis(200),
            "wave {wave}"
        );
        assert!(resolved.assessment.primary.is_none());
        assert!(resolved.tax.is_some());
    }
}

// Acceptance policy completes in A, yet is diagnosed after B's payer-dependent reads.
#[tokio::test]
async fn unavailable_answers_keep_historical_order_across_waves() {
    let mut f = fixture();
    f.contracts.clear();
    f.occupancy.clear();
    f.terms.clear();
    f.acceptance_policy = None;
    f.assessment_profile = None;
    let (budget, sink) = recording_budget();
    let h = harness_with(f, false, budget, |_| {});
    let resolved = run(&h, &basket(Operation::Preview)).await.unwrap();
    let expected = vec![
        (GatePort::ContractResolution, Unavailable::Missing),
        (GatePort::Occupancy, Unavailable::Missing),
        (GatePort::BillingTerms, Unavailable::Missing),
        (GatePort::AcceptancePolicy, Unavailable::Missing),
        (GatePort::Assessment, Unavailable::Missing),
    ];
    assert_eq!(resolved.unavailable_kinds, expected);
    let signalled: Vec<_> = sink
        .observations()
        .iter()
        .filter_map(|o| match o {
            GateObservation::PortUnavailable { port, kind } => Some((*port, *kind)),
            _ => None,
        })
        .collect();
    assert_eq!(signalled, expected);
    assert_eq!(h.inflight.reads.load(Ordering::SeqCst), 1);
}

/// Publish a successor after Wave A, while Rating evaluates the already frozen selection.
struct PublishDuringEvaluation {
    pricing: Arc<super::doubles::FakePricing>,
    inner: EvaluationStub,
    seen: Mutex<Vec<OrderEvaluationRequest>>,
}
#[async_trait]
impl EvaluationReads for PublishDuringEvaluation {
    async fn evaluate(
        &self,
        ctx: &SecurityContext,
        request: &OrderEvaluationRequest,
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<OrderEvaluation> {
        self.seen.lock().push(request.clone());
        let mut replacement = bound_cell(ITEM, None, Uuid::from_u128(0xd2));
        let price = &mut replacement.binding.as_mut().unwrap().price;
        price.price_id = Uuid::from_u128(0x22);
        price.model = bss_pricing_sdk::read::PriceModel::Flat {
            amount: bss_money::parse_decimal("999").unwrap(),
        };
        price.money_digest = bss_pricing_sdk::digest::money_digest(price);
        *self.pricing.publication.lock() = Some((
            RevisionRef {
                plan_id: PLAN,
                revision_id: Uuid::from_u128(0xe2),
                revision_no: 2,
            },
            vec![replacement],
        ));
        self.inner.evaluate(ctx, request, run, cancel).await
    }
}

#[tokio::test]
async fn mid_run_publication_does_not_mix_bindings_and_totals() {
    use super::doubles::{FakePricing, pricing_catalog};
    let budget = Arc::new(Budget::baseline());
    let pricing = Arc::new(FakePricing::new(
        RevisionRef {
            plan_id: PLAN,
            revision_id: REVISION,
            revision_no: 1,
        },
        vec![bound_cell(ITEM, None, SKU)],
    ));
    let evaluator = Arc::new(PublishDuringEvaluation {
        pricing: pricing.clone(),
        inner: EvaluationStub::new(Arc::new(fixture())),
        seen: Mutex::new(Vec::new()),
    });
    let h = harness_with(fixture(), false, budget.clone(), |ports| {
        ports.evaluation = evaluator.clone();
    });
    let orchestrator =
        h.orchestrator
            .with_catalog(Arc::new(pricing_catalog(pricing.clone(), SELLER, budget)));
    let result = orchestrator
        .run(
            &SecurityContext::anonymous(),
            &basket(Operation::Preview),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        pricing.publication.lock().is_some(),
        "publication happened before evaluation answered"
    );
    assert_eq!(
        pricing.resolves(),
        1,
        "no latest-binding reread after Rating"
    );
    assert_eq!(pricing.currents(), 1);
    assert_eq!(result.resolve_answers[&LINE].revision_id, REVISION);
    assert_eq!(result.bindings[&LINE][0].sku_id, SKU);
    let seen = evaluator.seen.lock();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].lines[0].plan_revision_id, REVISION);
    assert_eq!(seen[0].lines[0].items[0].chains[0].binding.sku_id, SKU);
    assert_eq!(
        seen[0].lines[0].items[0].chains[0].binding.price_id,
        result.bindings[&LINE][0].price.price_id
    );
    assert_eq!(
        result.evaluation.unwrap().order_totals,
        fixture().evaluation_totals.unwrap().order_totals
    );
}

struct InvalidItemKeys(u8);
#[async_trait]
impl OverlapKeyReads for InvalidItemKeys {
    async fn keys(
        &self,
        _ctx: &SecurityContext,
        _payer: Uuid,
        _resource: Option<Uuid>,
        lines: &[OverlapKeyLine],
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Vec<OverlapKey>> {
        let request = lines[0].clone();
        let mut key = OverlapKey {
            line_id: request.line_id,
            catalog_subscription_product_key: "test-product".into(),
            overlap_scope_key: SCOPE_KEY.into(),
            item_key: Some(crate::domain::gate::item_keys::ItemOverlapKey {
                profile: "item_keys_v1".into(),
                selection: request,
                catalog_subscription_product_key: "test-product".into(),
                overlap_scope_key: SCOPE_KEY.into(),
                classification: crate::domain::gate::item_keys::ClassificationProvenance {
                    source: "test-provider".into(),
                    reference: "classification".into(),
                    version: "1".into(),
                },
            }),
        };
        PortAnswer::Answered(match self.0 {
            0 => vec![],
            1 => vec![key.clone(), key],
            2 => {
                key.line_id = Uuid::new_v4();
                vec![key]
            }
            3 => {
                key.item_key.as_mut().unwrap().selection.sku_version += 1;
                vec![key]
            }
            _ => {
                key.item_key.as_mut().unwrap().classification.source.clear();
                vec![key]
            }
        })
    }
}
#[tokio::test]
async fn malformed_or_missing_item_batches_never_feed_occupancy_or_counts() {
    for mode in 0..5 {
        let h = harness_with(fixture(), false, Arc::new(Budget::baseline()), |ports| {
            ports.overlap_keys = Arc::new(InvalidItemKeys(mode));
        });
        let resolved = run(&h, &basket(Operation::Submit)).await.unwrap();
        assert_eq!(h.calls.of(GatePort::Occupancy), 0, "mode {mode}");
        assert!(resolved.keys.is_empty());
        assert!(!resolved.assessment.failures.is_empty());
    }
}

/// R3: the billing-terms port states the first line twice with different terms.
struct DuplicatedTerms(Arc<StubFixture>);
#[async_trait]
impl BillingTermsReads for DuplicatedTerms {
    async fn resolve(
        &self,
        queries: &[BillingTermsQuery],
        run: &RunBudget,
        cancel: &CancellationToken,
    ) -> PortAnswer<Vec<ResolvedTerms>> {
        let PortAnswer::Answered(mut terms) = BillingTermsStub::new(Arc::clone(&self.0))
            .resolve(queries, run, cancel)
            .await
        else {
            panic!("the stub answers the fixture terms")
        };
        let mut second = terms[0].clone();
        second.term = crate::domain::gate::answers::EvaluationTerm::FixedPeriods { count: 24 };
        terms.push(second);
        PortAnswer::Answered(terms)
    }
}

#[tokio::test]
async fn a_duplicated_terms_entry_is_malformed_and_never_frozen() {
    let h = harness_with(fixture(), false, Arc::new(Budget::baseline()), |ports| {
        ports.billing_terms = Arc::new(DuplicatedTerms(Arc::new(fixture())));
    });
    let resolved = run(&h, &basket(Operation::Submit)).await.unwrap();
    assert!(
        resolved
            .unavailable_kinds
            .contains(&(GatePort::BillingTerms, Unavailable::Malformed)),
        "{:?}",
        resolved.unavailable_kinds
    );
    assert!(
        resolved.terms.is_empty(),
        "no entry reaches the frozen query"
    );
    assert!(
        resolved.evaluation_request.is_none(),
        "no entry reaches Rating"
    );
    assert!(
        resolved
            .assessment
            .failures
            .iter()
            .any(|f| f.reason == Reason::CatalogPinCompositionUnavailable),
        "{:?}",
        resolved.assessment.failures
    );
}

/// R5: the acceptance-policy port answers a version no hold policy can be frozen from.
struct PolicyVersion(&'static str);
#[async_trait]
impl AcceptancePolicyReads for PolicyVersion {
    async fn policy(
        &self,
        _query: &AcceptancePolicyQuery,
        _run: &RunBudget,
        _cancel: &CancellationToken,
    ) -> PortAnswer<Option<AcceptancePolicy>> {
        PortAnswer::Answered(Some(AcceptancePolicy {
            version: self.0.into(),
            hold_seconds: 600,
        }))
    }
}

#[tokio::test]
async fn a_non_canonical_policy_version_is_malformed_before_any_consumer() {
    let h = harness_with(fixture(), false, Arc::new(Budget::baseline()), |ports| {
        ports.acceptance_policy = Arc::new(PolicyVersion("v1"));
    });
    let resolved = run(&h, &basket(Operation::Submit)).await.unwrap();
    assert!(
        resolved
            .unavailable_kinds
            .contains(&(GatePort::AcceptancePolicy, Unavailable::Malformed)),
        "{:?}",
        resolved.unavailable_kinds
    );
    assert!(resolved.policy.is_none());
    assert!(
        resolved
            .assessment
            .failures
            .iter()
            .any(|f| f.reason == Reason::CatalogPinCompositionUnavailable),
        "{:?}",
        resolved.assessment.failures
    );
}

#[tokio::test]
async fn protected_serialized_fixture_reaches_real_orchestration_and_refuses_foreign_tuple() {
    use crate::domain::gate::predicates::PredicateId;
    use crate::domain::gate::vector::Verdict;
    for (suspended, reserved, foreign, expect) in [
        (1, 0, false, Verdict::Failed),
        (0, 1, false, Verdict::Failed),
        (0, 0, false, Verdict::Passed),
        (0, 0, true, Verdict::Unevaluable),
    ] {
        let basket = basket(Operation::Submit);
        let mut fixture = fixture();
        fixture.occupancy.insert(SCOPE_KEY.into(), serde_json::from_value(serde_json::json!({
            "profile":"protected_full_tuple_v1","demand_unit":"line_scope_v1","observation_revision":"integration-snapshot",
            "scope":{"payer_tenant_id":basket.axes.payer,"resource_tenant_id":if foreign { Uuid::from_u128(999) } else { basket.axes.resource },"overlap_scope_key":SCOPE_KEY},
            "policy":{"source":"catalog","reference":"integration-policy","version":"1"},
            "active_count":0,"retained_suspended_count":suspended,"order_reserved_count":reserved,
            "max_concurrent_active":1,"own_membership":null
        })).unwrap());
        let h = harness_with(fixture, false, Arc::new(Budget::baseline()), |_| {});
        let resolved = run(&h, &basket).await.unwrap();
        assert_eq!(h.calls.of(GatePort::Occupancy), 1);
        let rows: Vec<_> = resolved
            .assessment
            .vector
            .outcomes()
            .iter()
            .filter(|o| o.key.predicate == PredicateId::OverlapUniqueness)
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, expect);
    }
}

/// Which local read fails; bounded categories match the orchestrator's existing errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedInflightRead {
    Count,
    Tuple,
}

pub const PRIVATE_DB_CAUSE: &str = "private-database-host-and-query-do-not-disclose";

/// Each attempted read is counted; optional cancellation occurs with its injected failure.
pub struct FailingInflight {
    fail: FailedInflightRead,
    cancel: Option<CancellationToken>,
    pub(crate) counts: std::sync::atomic::AtomicUsize,
    pub(crate) tuples: std::sync::atomic::AtomicUsize,
}
impl FailingInflight {
    pub(crate) fn new(fail: FailedInflightRead, cancel: Option<CancellationToken>) -> Self {
        Self {
            fail,
            cancel,
            counts: std::sync::atomic::AtomicUsize::default(),
            tuples: std::sync::atomic::AtomicUsize::default(),
        }
    }
    fn error(&self) -> InflightReadError {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        InflightReadError::Store(toolkit_db::secure::ScopeError::Db(sea_orm::DbErr::Custom(
            PRIVATE_DB_CAUSE.into(),
        )))
    }
}
#[async_trait]
impl InflightClaims for FailingInflight {
    async fn count_other_orders(
        &self,
        _tuple: ClaimParties,
        keys: &[String],
        _exclude: Option<Uuid>,
    ) -> Result<BTreeMap<String, u32>, InflightReadError> {
        self.counts.fetch_add(1, Ordering::SeqCst);
        if self.fail == FailedInflightRead::Count {
            return Err(self.error());
        }
        Ok(keys.iter().map(|key| (key.clone(), 0)).collect())
    }
    async fn held_same_tuple(
        &self,
        _tuple: ClaimParties,
        _keys: &[String],
        _exclude: Option<Uuid>,
    ) -> Result<std::collections::BTreeSet<String>, InflightReadError> {
        self.tuples.fetch_add(1, Ordering::SeqCst);
        Err(self.error())
    }
}

#[tokio::test]
async fn failed_local_reads_preserve_completed_unavailable_diagnostics_before_aborting() {
    for fail in [FailedInflightRead::Count, FailedInflightRead::Tuple] {
        for cancelled in [false, true] {
            let mut f = fixture();
            f.contracts.clear();
            f.occupancy.clear();
            f.terms.clear();
            f.acceptance_policy = None;
            let cancel = CancellationToken::new();
            let reads = Arc::new(FailingInflight::new(
                fail,
                cancelled.then(|| cancel.clone()),
            ));
            let (budget, sink) = recording_budget();
            let h = harness_with_inflight(f, budget, reads.clone());
            let result = h
                .orchestrator
                .run(
                    &SecurityContext::anonymous(),
                    &basket(Operation::Preview),
                    &cancel,
                )
                .await;
            let error = result.unwrap_err();
            if cancelled {
                assert!(matches!(error, OrchestrateError::Cancelled));
            } else {
                let OrchestrateError::Integration { what, source } = error else {
                    panic!("expected local storage error")
                };
                assert_eq!(
                    what,
                    match fail {
                        FailedInflightRead::Count => "in-flight line count",
                        FailedInflightRead::Tuple => "in-flight tuple lookup",
                    }
                );
                // Typed, with the cause on the source chain, not in the message.
                assert_eq!(source.to_string(), "in-flight store read");
                let read = source
                    .downcast_ref::<InflightReadError>()
                    .expect("the typed in-flight read error");
                assert!(matches!(read, InflightReadError::Store(_)));
                let cause = std::error::Error::source(read).expect("the store cause");
                assert!(cause.to_string().contains(PRIVATE_DB_CAUSE));
            }
            let observed: Vec<_> = sink
                .observations()
                .into_iter()
                .filter_map(|o| match o {
                    GateObservation::PortUnavailable { port, kind } => Some((port, kind)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                observed,
                vec![
                    (GatePort::ContractResolution, Unavailable::Missing),
                    (GatePort::Occupancy, Unavailable::Missing),
                    (GatePort::BillingTerms, Unavailable::Missing),
                    (GatePort::AcceptancePolicy, Unavailable::Missing),
                ],
                "{fail:?}, cancellation={cancelled}"
            );
            assert_eq!(reads.counts.load(Ordering::SeqCst), 1);
            assert_eq!(
                reads.tuples.load(Ordering::SeqCst),
                usize::from(fail == FailedInflightRead::Tuple)
            );
            for port in [
                GatePort::Assessment,
                GatePort::Evaluation,
                GatePort::IndicativeTax,
            ] {
                assert_eq!(h.calls.of(port), 0);
            }
            assert!(
                !sink
                    .observations()
                    .iter()
                    .any(|o| matches!(o, GateObservation::Run { .. }))
            );
        }
    }
}
